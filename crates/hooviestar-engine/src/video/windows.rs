use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    mem::ManuallyDrop,
    os::windows::ffi::OsStrExt,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use uuid::Uuid;
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{GENERIC_READ, HMODULE, HWND},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
                },
                D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
                D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
                D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR, D2D1CreateDevice,
                ID2D1Bitmap1, ID2D1DeviceContext, ID2D1SolidColorBrush,
            },
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_11_1,
            },
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT as BGRA_SUPPORT,
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT as VIDEO_SUPPORT, D3D11_SDK_VERSION,
                D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
                D3D11_USAGE_IMMUTABLE, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
                ID3D11Texture2D,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWriteCreateFactory,
                IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
            },
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_FORMAT_R16G16B16A16_FLOAT as FLOAT16_FORMAT, DXGI_SAMPLE_DESC,
                },
                DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_PRESENT,
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
                DXGI_SWAP_EFFECT_FLIP_DISCARD as FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
                IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGIOutput5,
                IDXGIOutputDuplication, IDXGIResource, IDXGISurface, IDXGISwapChain1,
            },
            Imaging::{
                CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
                IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
                WICDecodeMetadataCacheOnLoad,
            },
        },
        Media::MediaFoundation::{
            IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE,
            MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE,
            MF_MT_SUBTYPE, MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
            MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
            MFAudioFormat_Float, MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateMediaType,
            MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video, MFSTARTUP_FULL,
            MFShutdown, MFStartup, MFVideoFormat_ARGB32,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
            },
            Variant::VT_I8,
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
                RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
            },
        },
    },
    core::{IInspectable, Interface, PCWSTR, factory},
};
use windows_numerics::Matrix3x2;

use super::MediaControlBus;
use crate::{
    audio::{MediaAudioBus, PcmRing, SAMPLE_RATE},
    engine::{DeviceRecoveryPhase, EngineEvent, NativeSurfaces},
    project::{DisplayBinding, OutputConfig, ProjectV1, Scene, Source, TextAlign, Transform},
};
use parking_lot::RwLock;

pub struct D3d11Device {
    pub device: ID3D11Device,
    pub immediate_context: ID3D11DeviceContext,
    pub feature_level: D3D_FEATURE_LEVEL,
}

impl D3d11Device {
    pub fn create_hardware() -> Result<Self, WindowsVideoError> {
        let flags = BGRA_SUPPORT | VIDEO_SUPPORT;
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut device = None;
        let mut immediate_context = None;
        let mut feature_level = D3D_FEATURE_LEVEL_11_0;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut immediate_context),
            )
        }
        .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let device = device
            .ok_or_else(|| WindowsVideoError::DeviceCreation("D3D11 returned no device".into()))?;
        device
            .cast::<IDXGIDevice>()
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        Ok(Self {
            device,
            immediate_context: immediate_context.ok_or_else(|| {
                WindowsVideoError::DeviceCreation("D3D11 returned no immediate context".into())
            })?,
            feature_level,
        })
    }

    pub fn create_scene_texture(
        &self,
        descriptor: SceneTextureDescriptor,
    ) -> Result<ID3D11Texture2D, WindowsVideoError> {
        if descriptor.width == 0
            || descriptor.height == 0
            || descriptor.format != FLOAT16_FORMAT.0 as u32
        {
            return Err(WindowsVideoError::InvalidSceneTexture);
        }
        let texture_descriptor = D3D11_TEXTURE2D_DESC {
            Width: descriptor.width,
            Height: descriptor.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: FLOAT16_FORMAT,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&texture_descriptor, None, Some(&mut texture))
        }
        .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        texture.ok_or_else(|| {
            WindowsVideoError::DeviceCreation("D3D11 returned no scene texture".into())
        })
    }

    pub fn load_image(&self, path: &Path) -> Result<ID3D11Texture2D, WindowsVideoError> {
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnLoad,
            )
        }
        .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        let frame = unsafe { decoder.GetFrame(0) }
            .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        let converter = unsafe { factory.CreateFormatConverter() }
            .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        let mut width = 0;
        let mut height = 0;
        unsafe { converter.GetSize(&mut width, &mut height) }
            .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| WindowsVideoError::UnsupportedImage("image stride overflow".into()))?;
        let size = stride
            .checked_mul(height)
            .ok_or_else(|| WindowsVideoError::UnsupportedImage("image size overflow".into()))?;
        let mut pixels = vec![0u8; size as usize];
        unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut pixels) }
            .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        let descriptor = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: stride,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&descriptor, Some(&initial), Some(&mut texture))
        }
        .map_err(|error| WindowsVideoError::UnsupportedImage(error.to_string()))?;
        texture.ok_or_else(|| {
            WindowsVideoError::UnsupportedImage("D3D11 returned no image texture".into())
        })
    }
}
pub struct SwapChainTarget {
    swap_chain: IDXGISwapChain1,
    back_buffer: ID3D11Texture2D,
}

impl SwapChainTarget {
    fn create(
        device: &D3d11Device,
        factory: &IDXGIFactory2,
        hwnd: usize,
        width: u32,
        height: u32,
    ) -> Result<Self, WindowsVideoError> {
        if hwnd == 0 {
            return Err(WindowsVideoError::SwapChain(
                "target window handle is null".into(),
            ));
        }
        let descriptor = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: FLOAT16_FORMAT,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        let swap_chain = unsafe {
            factory.CreateSwapChainForHwnd(
                &device.device,
                HWND(hwnd as *mut _),
                &descriptor,
                None,
                None::<&IDXGIOutput>,
            )
        }
        .map_err(|error| WindowsVideoError::SwapChain(error.to_string()))?;
        let back_buffer = unsafe { swap_chain.GetBuffer::<ID3D11Texture2D>(0) }
            .map_err(|error| WindowsVideoError::SwapChain(error.to_string()))?;
        Ok(Self {
            swap_chain,
            back_buffer,
        })
    }

    fn present(&self) -> Result<(), WindowsVideoError> {
        unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)) }
            .ok()
            .map_err(|error| WindowsVideoError::SwapChain(error.to_string()))
    }
}

pub struct TextDraw<'a> {
    pub text: &'a str,
    pub font_family: &'a str,
    pub font_size: f32,
    pub font_weight: u16,
    pub color: &'a str,
    pub background_color: &'a str,
    pub align: &'a TextAlign,
    pub transform: &'a Transform,
}

pub struct D3d11Compositor {
    pub device: D3d11Device,
    scene_texture: ID3D11Texture2D,
    d2d_context: ID2D1DeviceContext,
    _scene_bitmap: ID2D1Bitmap1,
    source_bitmaps: HashMap<usize, (u64, ID2D1Bitmap1)>,
    bitmap_cache_tick: u64,
    dwrite_factory: IDWriteFactory,
    text_formats: HashMap<(u64, u32, u16, u8), (String, IDWriteTextFormat)>,
    brushes: HashMap<String, ID2D1SolidColorBrush>,
    text_buffers: HashMap<String, Vec<u16>>,
    program: SwapChainTarget,
    preview: SwapChainTarget,
}

impl D3d11Compositor {
    pub fn create(
        program_hwnd: usize,
        preview_hwnd: usize,
        width: u32,
        height: u32,
    ) -> Result<Self, WindowsVideoError> {
        let device = D3d11Device::create_hardware()?;
        let scene_texture =
            device.create_scene_texture(SceneTextureDescriptor::float16(width, height))?;
        let dxgi_device = device
            .device
            .cast::<IDXGIDevice>()
            .map_err(|error| WindowsVideoError::SwapChain(error.to_string()))?;
        let d2d_device = unsafe { D2D1CreateDevice(&dxgi_device, None) }
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let d2d_context =
            unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) }
                .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let scene_surface = scene_texture
            .cast::<IDXGISurface>()
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let scene_properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: FLOAT16_FORMAT,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let scene_bitmap = unsafe {
            d2d_context.CreateBitmapFromDxgiSurface(&scene_surface, Some(&scene_properties))
        }
        .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        unsafe { d2d_context.SetTarget(&scene_bitmap) };
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|error| WindowsVideoError::SwapChain(error.to_string()))?;
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|error| WindowsVideoError::SwapChain(error.to_string()))?;
        let program = SwapChainTarget::create(&device, &factory, program_hwnd, width, height)?;
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let preview = SwapChainTarget::create(&device, &factory, preview_hwnd, width, height)?;
        Ok(Self {
            device,
            scene_texture,
            d2d_context,
            _scene_bitmap: scene_bitmap,
            source_bitmaps: HashMap::new(),
            bitmap_cache_tick: 0,
            program,
            dwrite_factory,
            text_formats: HashMap::new(),
            brushes: HashMap::new(),
            text_buffers: HashMap::new(),
            preview,
        })
    }

    pub fn render_scene<'a, I, J>(
        &mut self,
        color: [f32; 4],
        items: I,
        texts: J,
    ) -> Result<(), WindowsVideoError>
    where
        I: IntoIterator<Item = (&'a ID3D11Texture2D, &'a Transform)>,
        J: IntoIterator<Item = TextDraw<'a>>,
    {
        let clear = D2D1_COLOR_F {
            r: color[0],
            g: color[1],
            b: color[2],
            a: color[3],
        };
        unsafe {
            self.d2d_context.BeginDraw();
            self.d2d_context.Clear(Some(&clear));
        }
        for (texture, transform) in items {
            let bitmap = self.bitmap_for(texture)?;
            let mut description = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut description) };
            let destination = D2D_RECT_F {
                left: transform.x,
                top: transform.y,
                right: transform.x + transform.width,
                bottom: transform.y + transform.height,
            };
            let source = D2D_RECT_F {
                left: transform.crop_left,
                top: transform.crop_top,
                right: description.Width as f32 - transform.crop_right,
                bottom: description.Height as f32 - transform.crop_bottom,
            };
            let radians = transform.rotation_degrees.to_radians();
            let (sine, cosine) = radians.sin_cos();
            let center_x = transform.x + transform.width * 0.5;
            let center_y = transform.y + transform.height * 0.5;
            let matrix = Matrix3x2 {
                M11: cosine,
                M12: sine,
                M21: -sine,
                M22: cosine,
                M31: center_x - cosine * center_x + sine * center_y,
                M32: center_y - sine * center_x - cosine * center_y,
            };
            unsafe {
                self.d2d_context.SetTransform(&matrix);
                self.d2d_context.DrawBitmap(
                    &bitmap,
                    Some(&destination),
                    transform.opacity,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    Some(&source),
                    None,
                );
            }
        }
        let mut rendered_texts: HashSet<&str> = HashSet::new();
        for text in texts {
            let destination = D2D_RECT_F {
                left: text.transform.x,
                top: text.transform.y,
                right: text.transform.x + text.transform.width,
                bottom: text.transform.y + text.transform.height,
            };
            let radians = text.transform.rotation_degrees.to_radians();
            let (sine, cosine) = radians.sin_cos();
            let center_x = text.transform.x + text.transform.width * 0.5;
            let center_y = text.transform.y + text.transform.height * 0.5;
            let matrix = Matrix3x2 {
                M11: cosine,
                M12: sine,
                M21: -sine,
                M22: cosine,
                M31: center_x - cosine * center_x + sine * center_y,
                M32: center_y - sine * center_x - cosine * center_y,
            };
            let background = self.brush_for(text.background_color)?;
            let foreground = self.brush_for(text.color)?;
            let format = self.text_format_for(
                text.font_family,
                text.font_size,
                text.font_weight,
                text.align,
            )?;
            unsafe {
                background.SetOpacity(text.transform.opacity);
                foreground.SetOpacity(text.transform.opacity);
                self.d2d_context.SetTransform(&matrix);
                self.d2d_context.FillRectangle(&destination, &background);
            }
            rendered_texts.insert(text.text);
            if !self.text_buffers.contains_key(text.text) {
                self.text_buffers
                    .insert(text.text.to_string(), text.text.encode_utf16().collect());
            }
            let buffer = self
                .text_buffers
                .get_mut(text.text)
                .expect("text buffer was inserted above");
            unsafe {
                self.d2d_context.DrawText(
                    buffer,
                    &format,
                    &destination,
                    &foreground,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        // Puffer nicht mehr gezeichneter Texte freigeben: der Cache
        // bleibt auf den aktuellen Durchlauf begrenzt statt fuer die
        // Laufzeit des Compositors zu wachsen.
        self.text_buffers
            .retain(|key, _| rendered_texts.contains(key.as_str()));
        unsafe {
            self.d2d_context.SetTransform(&Matrix3x2::identity());
            self.d2d_context.EndDraw(None, None)
        }
        .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        unsafe {
            self.device
                .immediate_context
                .CopyResource(&self.program.back_buffer, &self.scene_texture);
            self.device
                .immediate_context
                .CopyResource(&self.preview.back_buffer, &self.scene_texture);
        }
        self.program.present()?;
        self.preview.present()
    }

    fn bitmap_for(&mut self, texture: &ID3D11Texture2D) -> Result<ID2D1Bitmap1, WindowsVideoError> {
        let key = Interface::as_raw(texture) as usize;
        // Ein Tick je Bitmap-Suche: Treffer und neue Eintraege schreiben
        // monoton steigende Recency, die Verdraengung unten erwischt
        // damit den echten least-recently-used-Eintrag.
        self.bitmap_cache_tick += 1;
        if let Some(entry) = self.source_bitmaps.get_mut(&key) {
            entry.0 = self.bitmap_cache_tick;
            return Ok(entry.1.clone());
        }
        // Gezielte Verdraengung statt Wholesale-Clear: jeder Treffer hebt
        // die Verwendung an, ueber der Kapazitaet weicht der least
        // recently verwendete Eintrag. Zwei Pool-Texturen je Live-Capture
        // bleiben so dauerhaft im Cache.
        while self.source_bitmaps.len() >= SOURCE_BITMAP_CACHE_CAPACITY {
            let Some(oldest) = self
                .source_bitmaps
                .iter()
                .min_by_key(|(_, (last_use, _))| *last_use)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.source_bitmaps.remove(&oldest);
        }
        let surface = texture
            .cast::<IDXGISurface>()
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let mut description = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut description) };
        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: description.Format,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let bitmap = unsafe {
            self.d2d_context
                .CreateBitmapFromDxgiSurface(&surface, Some(&properties))
        }
        .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        self.source_bitmaps
            .insert(key, (self.bitmap_cache_tick, bitmap.clone()));
        Ok(bitmap)
    }

    fn brush_for(&mut self, color: &str) -> Result<ID2D1SolidColorBrush, WindowsVideoError> {
        if let Some(brush) = self.brushes.get(color) {
            return Ok(brush.clone());
        }
        let parsed = parse_hex_color(color);
        let value = D2D1_COLOR_F {
            r: parsed[0],
            g: parsed[1],
            b: parsed[2],
            a: parsed[3],
        };
        let brush = unsafe { self.d2d_context.CreateSolidColorBrush(&value, None) }
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        self.brushes.insert(color.to_string(), brush.clone());
        Ok(brush)
    }

    fn text_format_for(
        &mut self,
        family: &str,
        size: f32,
        weight: u16,
        align: &TextAlign,
    ) -> Result<IDWriteTextFormat, WindowsVideoError> {
        // Copy-Key ohne Heap-Allokation: FNV-1a ueber den Familiennamen
        // plus Groesse/Gewicht/Ausrichtung. Der gespeicherte Name
        // verifiziert Treffer, sodass Hash-Kollisionen korrekt zum
        // Neuaufbau fallen statt ein fremdes Format zu liefern.
        let key = (
            fnv1a_utf8(family),
            size.to_bits(),
            weight,
            match align {
                TextAlign::Left => 0u8,
                TextAlign::Center => 1u8,
                TextAlign::Right => 2u8,
            },
        );
        if let Some((cached_family, format)) = self.text_formats.get(&key) {
            if cached_family == family {
                return Ok(format.clone());
            }
        }
        let family_name = family.to_string();
        let family: Vec<u16> = family.encode_utf16().chain(Some(0)).collect();
        let locale: Vec<u16> = "de-DE".encode_utf16().chain(Some(0)).collect();
        let format = unsafe {
            self.dwrite_factory.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT(i32::from(weight.clamp(1, 999))),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size.max(1.0),
                PCWSTR(locale.as_ptr()),
            )
        }
        .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        let alignment = match align {
            TextAlign::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            TextAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            TextAlign::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        };
        unsafe { format.SetTextAlignment(alignment) }
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        unsafe { format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER) }
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        self.text_formats.insert(key, (family_name, format.clone()));
        Ok(format)
    }
}

/// FNV-1a ueber UTF-8-Bytes: prozess-lokaler Cache-Key-Hash ohne
/// Heap-Allokation fuer Hot Paths.
fn fnv1a_utf8(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// WGC-Handler laufen auf ThreadPool-Threads (Send-Grenze). Der Zeiger wird
/// ausschliesslich innerhalb des MTA dereferenziert.
struct SendDirect3DDevice(IDirect3DDevice);

impl SendDirect3DDevice {
    fn recreate_pool(
        &self,
        pool: &Direct3D11CaptureFramePool,
        width: i32,
        height: i32,
    ) -> windows::core::Result<()> {
        pool.Recreate(
            &self.0,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            SizeInt32 {
                Width: width,
                Height: height,
            },
        )
    }
}

unsafe impl Send for SendDirect3DDevice {}

struct ComApartment {
    owned: bool,
}

impl ComApartment {
    fn init() -> Self {
        // RPC_E_CHANGED_MODE laesst das Aufruf-Ergebnis fehlschlagen; dann
        // gehoert die Apartment-Initialisierung einem anderen Besitzer und
        // wir rufen kein CoUninitialize.
        let owned = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self { owned }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

/// Obergrenze aufeinanderfolgender Recovery-Eingriffe nach Renderfehlern
/// — unabhaengig davon, ob das Create gelang —, bevor der Render-Thread
/// endgueltig aufgibt.
const MAX_RECOVERY_FAILURES: u32 = 8;

/// Gueltige Frames in Folge, nach denen der Recovery-Streak abgeklungen
/// ist und auf 0 faellt.
const RECOVERY_STREAK_RESET_FRAMES: u32 = 60;

/// Obergrenze der zwischengespeicherten Quell-Bitmaps pro Compositor;
/// deckt zwei Pool-Texturen je Live-Capture bis weit ueber den
/// ausgelegten Betriebsbereich ab. Bei Neuem weicht der least recently
/// verwendete Eintrag, statt den Cache komplett zu leeren.
const SOURCE_BITMAP_CACHE_CAPACITY: usize = 128;

pub struct RenderRuntime {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl RenderRuntime {
    pub fn start(
        surfaces: NativeSurfaces,
        project: Arc<RwLock<ProjectV1>>,
        events: mpsc::Sender<EngineEvent>,
        media_audio: MediaAudioBus,
        media_control: MediaControlBus,
    ) -> Result<Self, WindowsVideoError> {
        if surfaces.program == 0 || surfaces.preview == 0 {
            return Err(WindowsVideoError::SwapChain(
                "Programm- und Preview-Fensterhandles sind erforderlich".into(),
            ));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("d3d11-render".into())
            .spawn(move || {
                let _com_apartment = ComApartment::init();
                let mut output = project.read().output.clone();
                let compositor = D3d11Compositor::create(
                    surfaces.program,
                    surfaces.preview,
                    output.width,
                    output.height,
                );
                let mut compositor = match compositor {
                    Ok(compositor) => {
                        let _ = ready_sender.send(Ok(()));
                        compositor
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = ready_sender.send(Err(message.clone()));
                        let _ = events.send(EngineEvent::DeviceRecovery {
                            phase: DeviceRecoveryPhase::Failed,
                            detail: Some(message),
                        });
                        return;
                    }
                };
                let mut media_context = None;
                let mut mf_last_error: Option<String> = None;
                restart_media_context(&mut media_context, &mut mf_last_error, &compositor.device);
                let mut media_sources: HashMap<Uuid, (String, MediaVideoSource)> = HashMap::new();
                let mut captures: HashMap<Uuid, WindowCapture> = HashMap::new();
                let mut frames: HashMap<Uuid, D3d11CapturedFrame> = HashMap::new();
                let mut stale_windows: HashSet<Uuid> = HashSet::new();
                let mut image_textures: HashMap<Uuid, (String, ID3D11Texture2D)> = HashMap::new();
                let mut display_captures: HashMap<Uuid, DisplayCapture> = HashMap::new();
                let mut available_displays: HashSet<Uuid> = HashSet::new();
                let mut previous_visible: HashSet<Uuid> = HashSet::new();
                let mut last_media_state: HashMap<Uuid, (bool, f64)> = HashMap::new();
                let mut ended_media: HashSet<Uuid> = HashSet::new();
                let mut last_source_sync = Instant::now() - Duration::from_secs(2);
                let mut deadline = Instant::now();
                let mut recovery_streak: u32 = 0;
                let mut rendered_frames_since_recovery: u32 = 0;
                let mut last_failures: HashMap<(Uuid, FailureKind), String> = HashMap::new();
                let mut read_backoff: HashSet<Uuid> = HashSet::new();
                let mut resize_retry_failures: u32 = 0;
                let mut next_resize_attempt = Instant::now();
                let mut sync_tick_index: u32 = 0;
                while !thread_stop.load(Ordering::Acquire) {
                    let snapshot = project.read().clone();
                    let next_output = snapshot.output.clone();
                    try_resize_compositor(
                        next_output,
                        surfaces,
                        &mut output,
                        &mut compositor,
                        &mut media_context,
                        &mut mf_last_error,
                        &mut media_sources,
                        &mut last_media_state,
                        &mut last_failures,
                        &mut read_backoff,
                        &mut captures,
                        &mut frames,
                        &mut stale_windows,
                        &mut display_captures,
                        &mut image_textures,
                        &mut previous_visible,
                        &mut last_source_sync,
                        &mut resize_retry_failures,
                        &mut next_resize_attempt,
                        &events,
                    );

                    if last_source_sync.elapsed() >= Duration::from_secs(1) {
                        let mut creations_this_tick: u32 = 0;
                        // Fairness: Alle vier Synchronisierer teilen sich das
                        // Erstellungs-Budget dieses Ticks. Die Reihenfolge
                        // rotiert pro Tick, damit kein Quelltyp dauerhaft den
                        // ersten Slot bekommt und die anderen hungern.
                        let rotation = sync_tick_index % 4;
                        sync_tick_index = sync_tick_index.wrapping_add(1);
                        for pass in match rotation {
                            0 => [
                                SyncPass::Window,
                                SyncPass::Display,
                                SyncPass::Images,
                                SyncPass::Media,
                            ],
                            1 => [
                                SyncPass::Display,
                                SyncPass::Images,
                                SyncPass::Media,
                                SyncPass::Window,
                            ],
                            2 => [
                                SyncPass::Images,
                                SyncPass::Media,
                                SyncPass::Window,
                                SyncPass::Display,
                            ],
                            _ => [
                                SyncPass::Media,
                                SyncPass::Window,
                                SyncPass::Display,
                                SyncPass::Images,
                            ],
                        } {
                            match pass {
                                SyncPass::Window => synchronize_window_captures(
                                    &snapshot,
                                    surfaces,
                                    &compositor.device,
                                    &events,
                                    &mut captures,
                                    &mut frames,
                                    &mut last_failures,
                                    &mut creations_this_tick,
                                ),
                                SyncPass::Display => synchronize_display_captures(
                                    &snapshot,
                                    &compositor.device,
                                    &events,
                                    &mut display_captures,
                                    &mut last_failures,
                                    &mut creations_this_tick,
                                ),
                                SyncPass::Images => synchronize_images(
                                    &snapshot,
                                    &compositor.device,
                                    &events,
                                    &mut image_textures,
                                    &mut last_failures,
                                    &mut creations_this_tick,
                                ),
                                SyncPass::Media => {
                                    if media_context.is_none() {
                                        // Start fehlgeschlagen (z.B. nach
                                        // Device-Recovery): einmal pro
                                        // Sync-Tick erneut versuchen.
                                        restart_media_context(
                                            &mut media_context,
                                            &mut mf_last_error,
                                            &compositor.device,
                                        );
                                    }
                                    if let Some(context) = &media_context {
                                        synchronize_media(
                                            &snapshot,
                                            context,
                                            &events,
                                            &media_audio,
                                            &mut media_sources,
                                            &mut last_failures,
                                            &mut creations_this_tick,
                                        );
                                    }
                                }
                            }
                        }
                        last_source_sync = Instant::now();
                    }

                    let active_scene = snapshot
                        .scenes
                        .iter()
                        .find(|scene| scene.id == snapshot.active_scene_id);
                    pump_window_frames(
                        active_scene,
                        &captures,
                        &mut frames,
                        &mut stale_windows,
                        &events,
                    );
                    pump_displays(
                        &compositor.device,
                        &mut display_captures,
                        &mut available_displays,
                        &events,
                    );
                    pump_media(
                        &snapshot,
                        active_scene,
                        &media_control,
                        &mut media_sources,
                        &mut last_media_state,
                        &mut ended_media,
                        &mut last_failures,
                        &mut read_backoff,
                        &mut previous_visible,
                        &events,
                    );

                    let items = active_scene.into_iter().flat_map(|scene| {
                        scene.items.iter().filter_map(|item| {
                            if !item.visible {
                                return None;
                            }
                            if let Some(frame) = frames.get(&item.source_id) {
                                return Some((&frame.texture, &item.transform));
                            }
                            if let Some(texture) = media_sources
                                .get(&item.source_id)
                                .and_then(|(_, media)| media.texture())
                            {
                                return Some((texture, &item.transform));
                            }
                            if let Some(texture) = display_captures
                                .get(&item.source_id)
                                .and_then(DisplayCapture::texture)
                            {
                                return Some((texture, &item.transform));
                            }
                            image_textures
                                .get(&item.source_id)
                                .map(|(_, texture)| (texture, &item.transform))
                        })
                    });
                    let texts = active_scene.into_iter().flat_map(|scene| {
                        scene.items.iter().filter_map(|item| {
                            if !item.visible {
                                return None;
                            }
                            snapshot.sources.iter().find_map(|source| match source {
                                Source::Text {
                                    id,
                                    text,
                                    font_family,
                                    font_size_px,
                                    font_weight,
                                    color,
                                    background_color,
                                    align,
                                    ..
                                } if *id == item.source_id => Some(TextDraw {
                                    text,
                                    font_family,
                                    font_size: *font_size_px,
                                    font_weight: *font_weight,
                                    color,
                                    background_color,
                                    align,
                                    transform: &item.transform,
                                }),
                                _ => None,
                            })
                        })
                    });
                    let mut unavailable = Vec::new();
                    if let Some(scene) = active_scene {
                        for item in scene.items.iter().filter(|item| item.visible) {
                            let has_texture = frames.contains_key(&item.source_id)
                                || display_captures
                                    .get(&item.source_id)
                                    .and_then(DisplayCapture::texture)
                                    .is_some()
                                || image_textures.contains_key(&item.source_id)
                                || media_sources
                                    .get(&item.source_id)
                                    .is_some_and(|(_, media)| {
                                        // Audio-only-Medien (video_enabled =
                                        // false) haben nie eine Textur, sind
                                        // aber praesent: kein Offline-Schild.
                                        // Ebenso bleibt ein pausiertes Medium
                                        // nach einem Seek praesent, solange
                                        // mindestens ein Bild dekodiert wurde.
                                        !media.video_enabled
                                            || media.texture().is_some()
                                            || media.decoded_video()
                                    });
                            if has_texture {
                                continue;
                            }
                            if let Some(source) = snapshot
                                .sources
                                .iter()
                                .find(|source| source.id() == item.source_id)
                                && !matches!(
                                    source,
                                    Source::Text { .. } | Source::ApplicationAudio { .. }
                                )
                            {
                                unavailable.push((
                                    format!("Quelle nicht verfügbar: {}", source_name(source)),
                                    item.transform,
                                ));
                            }
                        }
                    }
                    let offline_alignment = TextAlign::Center;
                    let offline_texts = unavailable.iter().map(|(text, transform)| TextDraw {
                        text,
                        font_family: "Segoe UI",
                        font_size: 28.0,
                        font_weight: 600,
                        color: "#ffffff",
                        background_color: "#202736",
                        align: &offline_alignment,
                        transform,
                    });
                    if let Err(error) = compositor.render_scene(
                        parse_hex_color(&output.background),
                        items,
                        texts.chain(offline_texts),
                    ) {
                        if recover_after_render_error(
                            &error,
                            surfaces,
                            &output,
                            &mut compositor,
                            &mut media_context,
                            &mut mf_last_error,
                            &mut media_sources,
                            &mut last_media_state,
                            &mut last_failures,
                            &mut read_backoff,
                            &mut captures,
                            &mut frames,
                            &mut stale_windows,
                            &mut display_captures,
                            &mut image_textures,
                            &mut previous_visible,
                            &mut recovery_streak,
                            &mut rendered_frames_since_recovery,
                            &mut last_source_sync,
                            &thread_stop,
                            &events,
                        ) {
                            break;
                        }
                        continue;
                    }
                    // Gueltige Frames zaehlen gegen den Recovery-Streak:
                    // erst RECOVERY_STREAK_RESET_FRAMES Frames in Folge
                    // gilt die Quelle als stabil und der Streak faellt auf
                    // 0. Ein einzelner transienter Fehler klingt damit ab,
                    // eine Flatter-Serie terminiert am Streak-Limit.
                    if recovery_streak > 0 {
                        rendered_frames_since_recovery += 1;
                        if rendered_frames_since_recovery >= RECOVERY_STREAK_RESET_FRAMES {
                            recovery_streak = 0;
                            rendered_frames_since_recovery = 0;
                        }
                    }
                    let frame_time = Duration::from_secs_f64(1.0 / f64::from(output.fps.max(1)));
                    deadline += frame_time;
                    let now = Instant::now();
                    if deadline > now {
                        thread::sleep(deadline - now);
                    } else {
                        deadline = now;
                    }
                }
            })
            .map_err(|error| WindowsVideoError::DeviceCreation(error.to_string()))?;
        match ready_receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Mutex::new(Some(thread)),
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(WindowsVideoError::SwapChain(message))
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(WindowsVideoError::SwapChain(error.to_string()))
            }
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RenderRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.get_mut().take() {
            let _ = thread.join();
        }
    }
}

/// Media-Foundation-Kontext (neu) starten und die Fehlerlage nur bei
/// Zustandswechsel melden: Ein dauerhaft scheiternder Start flutet
/// sonst das Log, ein einmaliger wird nicht verschluckt.
fn restart_media_context(
    context: &mut Option<MediaFoundationContext>,
    last_error: &mut Option<String>,
    device: &D3d11Device,
) {
    match MediaFoundationContext::start(device) {
        Ok(started) => {
            *context = Some(started);
            *last_error = None;
        }
        Err(error) => {
            let reason = error.to_string();
            if last_error.as_deref() != Some(reason.as_str()) {
                eprintln!("[hooviestar] Media Foundation Kontext fehlgeschlagen: {reason}");
            }
            *last_error = Some(reason);
        }
    }
}

/// Geraeteabhaengigen Zustand nach Device-Verlust verwerfen; der
/// naechste Sync-Tick baut die Quellen neu auf. `available_displays`
/// bleibt absichtlich unberuehrt (historisches Verhalten).
fn clear_device_tied_state(
    media_sources: &mut HashMap<Uuid, (String, MediaVideoSource)>,
    last_media_state: &mut HashMap<Uuid, (bool, f64)>,
    last_failures: &mut HashMap<(Uuid, FailureKind), String>,
    read_backoff: &mut HashSet<Uuid>,
    media_context: &mut Option<MediaFoundationContext>,
    captures: &mut HashMap<Uuid, WindowCapture>,
    frames: &mut HashMap<Uuid, D3d11CapturedFrame>,
    stale_windows: &mut HashSet<Uuid>,
    display_captures: &mut HashMap<Uuid, DisplayCapture>,
    image_textures: &mut HashMap<Uuid, (String, ID3D11Texture2D)>,
    previous_visible: &mut HashSet<Uuid>,
) {
    media_sources.clear();
    last_media_state.clear();
    last_failures.clear();
    read_backoff.clear();
    drop(media_context.take());
    captures.clear();
    frames.clear();
    stale_windows.clear();
    display_captures.clear();
    image_textures.clear();
    previous_visible.clear();
}

/// Groessenaenderung lazily retryen: der alte Kompositor rendert in
/// der alten Groesse weiter, eine nicht erzeugbare Groesse kostet
/// nicht das fatale Recovery-Budget.
fn try_resize_compositor(
    next_output: OutputConfig,
    surfaces: NativeSurfaces,
    output: &mut OutputConfig,
    compositor: &mut D3d11Compositor,
    media_context: &mut Option<MediaFoundationContext>,
    mf_last_error: &mut Option<String>,
    media_sources: &mut HashMap<Uuid, (String, MediaVideoSource)>,
    last_media_state: &mut HashMap<Uuid, (bool, f64)>,
    last_failures: &mut HashMap<(Uuid, FailureKind), String>,
    read_backoff: &mut HashSet<Uuid>,
    captures: &mut HashMap<Uuid, WindowCapture>,
    frames: &mut HashMap<Uuid, D3d11CapturedFrame>,
    stale_windows: &mut HashSet<Uuid>,
    display_captures: &mut HashMap<Uuid, DisplayCapture>,
    image_textures: &mut HashMap<Uuid, (String, ID3D11Texture2D)>,
    previous_visible: &mut HashSet<Uuid>,
    last_source_sync: &mut Instant,
    resize_retry_failures: &mut u32,
    next_resize_attempt: &mut Instant,
    events: &mpsc::Sender<EngineEvent>,
) {
    // Nicht-Groessen-Aenderungen (fps, Hintergrund) greifen sofort,
    // unabhaengig vom ausstehenden Retry einer nicht erzeugbaren
    // Groesse; nur width/height bleiben an das gelungene Recreate
    // gebunden.
    output.fps = next_output.fps;
    output.background = next_output.background.clone();
    if next_output.width != output.width || next_output.height != output.height {
        // Groessenaenderung lazily retryen: der alte
        // Kompositor rendert in der alten Groesse weiter,
        // eine nicht erzeugbare Groesse kostet nicht das
        // fatale Recovery-Budget.
        if Instant::now() >= *next_resize_attempt {
            let _ = events.send(EngineEvent::DeviceRecovery {
                phase: DeviceRecoveryPhase::Started,
                detail: None,
            });
            match D3d11Compositor::create(
                surfaces.program,
                surfaces.preview,
                next_output.width,
                next_output.height,
            ) {
                Ok(recreated) => {
                    // Geraeteabhaengiger Zustand erst nach
                    // erfolgreichem Create verwerfen:
                    // scheitert das Create, bleibt der alte
                    // Kompositor mit allen Quellen
                    // weiter lauffaehig.
                    clear_device_tied_state(
                        media_sources,
                        last_media_state,
                        last_failures,
                        read_backoff,
                        media_context,
                        captures,
                        frames,
                        stale_windows,
                        display_captures,
                        image_textures,
                        previous_visible,
                    );
                    *compositor = recreated;
                    restart_media_context(media_context, mf_last_error, &compositor.device);
                    *output = next_output;
                    *last_source_sync = Instant::now() - Duration::from_secs(2);
                    *resize_retry_failures = 0;
                    *next_resize_attempt = Instant::now();
                    let _ = events.send(EngineEvent::DeviceRecovery {
                        phase: DeviceRecoveryPhase::Succeeded,
                        detail: None,
                    });
                }
                Err(error) => {
                    *resize_retry_failures += 1;
                    let _ = events.send(EngineEvent::DeviceRecovery {
                        phase: DeviceRecoveryPhase::Failed,
                        detail: Some(error.to_string()),
                    });
                    *next_resize_attempt =
                        Instant::now() + backoff_duration(*resize_retry_failures);
                }
            }
        }
    } else {
        *output = next_output;
        *resize_retry_failures = 0;
        *next_resize_attempt = Instant::now();
    }
}

/// Neueste WGC-Frames der sichtbaren Fensterquellen in den
/// Render-Cache uebernehmen.
fn pump_window_frames(
    active_scene: Option<&Scene>,
    captures: &HashMap<Uuid, WindowCapture>,
    frames: &mut HashMap<Uuid, D3d11CapturedFrame>,
    stale_windows: &mut HashSet<Uuid>,
    events: &mpsc::Sender<EngineEvent>,
) {
    if let Some(scene) = active_scene {
        for item in scene.items.iter().filter(|item| item.visible) {
            if let Some(capture) = captures.get(&item.source_id) {
                if let Some(frame) = capture.take_latest() {
                    frames.insert(item.source_id, frame);
                    if stale_windows.remove(&item.source_id) {
                        let _ = events.send(EngineEvent::SourceAvailable {
                            source_id: item.source_id,
                        });
                    }
                } else if capture.is_stale(Duration::from_millis(750)) {
                    // WGC liefert Frames nur bei sichtbarer Aenderung des
                    // Fensters. Ein statisches Fenster bleibt daher ohne
                    // neue Frames, ist aber nicht offline: Solange mindestens
                    // ein Frame angekommen ist, bleibt der letzte Texturstand
                    // in `frames` und wird weitergerendert. Erst ein
                    // GraphicsCaptureItem.Closed-Signal oder eine nie
                    // gelieferte Captive gilt als offline.
                    if !capture.has_delivered() || capture.is_closed() {
                        frames.remove(&item.source_id);
                        if stale_windows.insert(item.source_id) {
                            let reason = if capture.is_closed() {
                                "Fenster wurde geschlossen"
                            } else {
                                "Fenster liefert keine Live-Frames"
                            };
                            let _ = events.send(EngineEvent::SourceUnavailable {
                                source_id: item.source_id,
                                reason: reason.into(),
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Desktop-Duplication-Frames abholen; tote Duplikationen
/// (z.B. ACCESS_LOST) entfernen, der naechste Sync-Tick baut
/// sie neu auf.
fn pump_displays(
    device: &D3d11Device,
    display_captures: &mut HashMap<Uuid, DisplayCapture>,
    available_displays: &mut HashSet<Uuid>,
    events: &mpsc::Sender<EngineEvent>,
) {
    let mut failed_displays: Vec<Uuid> = Vec::new();
    for (source_id, capture) in &mut *display_captures {
        match capture.acquire_latest(&device.immediate_context, &device.device) {
            Ok(true) => {
                // Nur beim Uebergang melden, nicht bei jedem
                // erfassten Desktop-Frame (wie stale_windows).
                if available_displays.insert(*source_id) {
                    let _ = events.send(EngineEvent::SourceAvailable {
                        source_id: *source_id,
                    });
                }
            }
            Ok(false) => {}
            Err(error) => {
                available_displays.remove(source_id);
                let _ = events.send(EngineEvent::SourceUnavailable {
                    source_id: *source_id,
                    reason: error.to_string(),
                });
                failed_displays.push(*source_id);
            }
        }
    }
    // Tote Duplikation (z.B. ACCESS_LOST) wegwerfen; der naechste
    // synchronize_display_captures-Lauf baut sie neu auf.
    for source_id in failed_displays {
        display_captures.remove(&source_id);
    }
}

/// Medien-PTS-Pacing: Lesentscheid, Pausen, Seek-Wuensche und
/// MediaState-Events; Unsichtbarkeit pausiert, sofern die Quelle
/// nicht continue_when_hidden gesetzt hat.
fn pump_media(
    snapshot: &ProjectV1,
    active_scene: Option<&Scene>,
    media_control: &MediaControlBus,
    media_sources: &mut HashMap<Uuid, (String, MediaVideoSource)>,
    last_media_state: &mut HashMap<Uuid, (bool, f64)>,
    ended_media: &mut HashSet<Uuid>,
    last_failures: &mut HashMap<(Uuid, FailureKind), String>,
    read_backoff: &mut HashSet<Uuid>,
    previous_visible: &mut HashSet<Uuid>,
    events: &mpsc::Sender<EngineEvent>,
) {
    let visible_ids: HashSet<Uuid> = active_scene
        .into_iter()
        .flat_map(|scene| {
            scene
                .items
                .iter()
                .filter(|item| item.visible)
                .map(|item| item.source_id)
        })
        .collect();
    for source in &snapshot.sources {
        if let Source::Media {
            id,
            continue_when_hidden,
            restart_on_show,
            looped,
            ..
        } = source
        {
            let control = media_control.read().get(id).copied().unwrap_or_default();
            let Some((_, media)) = media_sources.get_mut(id) else {
                continue;
            };
            if *restart_on_show && visible_ids.contains(id) && !previous_visible.contains(id) {
                if media.seek(0.0).is_ok() {
                    // Explizites Replay-on-Show entkraeftet den
                    // Ended-Latch wie ein Nutzer-Seek.
                    ended_media.remove(id);
                }
                last_failures.remove(&(*id, FailureKind::MediaRead));
                read_backoff.remove(id);
            }
            let requested = media_control
                .write()
                .get_mut(id)
                .and_then(|control| control.seek_seconds.take());
            if let Some(position) = requested {
                if let Err(error) = media.seek(position) {
                    let _ = events.send(EngineEvent::UnsupportedMedia {
                        source_id: *id,
                        reason: error.to_string(),
                    });
                } else {
                    // Seek setzt einen haengenden Leserfehler
                    // zurueck: eine neue identische Stoerung
                    // wird erneut gemeldet.
                    last_failures.remove(&(*id, FailureKind::MediaRead));
                    read_backoff.remove(id);
                    // MediaState-Dedup verwerfen: der
                    // naechste Publish meldet das Seek-Ziel
                    // sofort, auch bei <0,25 s Sprung im
                    // pausierten Zustand.
                    last_media_state.remove(id);
                    // Nutzer-Seek entkraeftet den Ended-Latch
                    // (Linux: ended = false nach erfolgreichem Seek).
                    ended_media.remove(id);
                }
            }
            // Beendetes Medium bleibt pausiert, bis der Nutzer explizit
            // Play oder Seek sendet (Linux-Paritaet: der Neustart nach
            // natuerlichem Ende laeuft ueber den Ended-Latch, nicht
            // ueber einen stillen Autoplay — auch nicht nach
            // Device-Recovery, das die Quelle neu oeffnet).
            if ended_media.contains(id) {
                if control.playing {
                    // Play nach Ende: Neustart von null; scheitert der
                    // Seek, bleibt der Latch bestehen und der naechste
                    // Play retryt den Neustart.
                    match media.seek(0.0) {
                        Ok(()) => {
                            ended_media.remove(id);
                            last_failures.remove(&(*id, FailureKind::MediaRead));
                            read_backoff.remove(id);
                            last_media_state.remove(id);
                        }
                        Err(error) => {
                            // Scheiternder Neustart-Seek schreibt den
                            // Playing-Wunsch zurueck (wie beim EOF):
                            // nur ein neuer Nutzer-Play retryt den
                            // Neustart statt eines Versuchs pro Tick.
                            // Ein zwischenzeitlicher Nutzer-Play hat die
                            // Epoche erhoeht und wird hier nicht
                            // ueberschrieben.
                            let mut bus = media_control.write();
                            let entry = bus.entry(*id).or_default();
                            if entry.epoch == control.epoch {
                                entry.playing = false;
                            }
                            let _ = events.send(EngineEvent::UnsupportedMedia {
                                source_id: *id,
                                reason: error.to_string(),
                            });
                        }
                    }
                }
                if ended_media.contains(id) {
                    media.set_paused(true);
                    send_media_state(
                        events,
                        last_media_state,
                        *id,
                        false,
                        media.timestamp_100ns() as f64 / 10_000_000.0,
                    );
                    continue;
                }
            }
            if !control.playing {
                // Pausierte Medien liefern keinen neuen Stand:
                // MediaState nur bei Aenderung senden statt in
                // jedem Durchlauf. Die Wanduhr friert ein.
                media.set_paused(true);
                send_media_state(
                    events,
                    last_media_state,
                    *id,
                    false,
                    media.timestamp_100ns() as f64 / 10_000_000.0,
                );
                continue;
            }
            if visible_ids.contains(id) || *continue_when_hidden {
                if read_backoff.remove(id) {
                    // Ein-Tick-Backoff nach Leserfehler: auch
                    // diese Pause zaehlt nicht als Wiedergabe.
                    media.set_paused(true);
                } else {
                    media.set_paused(false);
                    match media.read_next() {
                        Ok(ReadOutcome::Eof) => {
                            if *looped {
                                if let Err(error) = media.seek(0.0) {
                                    // Dauerhaft scheiternder Loop-Seek
                                    // nicht jeden Tick still retryen:
                                    // beendet latching (Linux: ended =
                                    // true, Ring stumm) und den Neustart
                                    // dem naechsten Play ueberlassen.
                                    eprintln!("[hooviestar] Loop-Seek fehlgeschlagen: {error}");
                                    *media.audio_ring.lock() =
                                        PcmRing::new(SAMPLE_RATE as usize * 2);
                                    // Ein zwischenzeitlicher Nutzer-Play
                                    // hat die Epoche erhoeht und wird
                                    // nicht ueberschrieben.
                                    let mut bus = media_control.write();
                                    let entry = bus.entry(*id).or_default();
                                    if entry.epoch == control.epoch {
                                        entry.playing = false;
                                    }
                                    ended_media.insert(*id);
                                    send_media_state(
                                        events,
                                        last_media_state,
                                        *id,
                                        false,
                                        media.timestamp_100ns() as f64 / 10_000_000.0,
                                    );
                                }
                            } else {
                                // EOF eines nicht loopenden Mediums:
                                // einmalig "beendet" melden; read_next
                                // liefert bis zum naechsten Seek kein
                                // neues Sample mehr (kein ReadSample).
                                // Beendet-Wunsch im Bus persistieren
                                // (Linux: desired_playing = false),
                                // damit ein Device-Recovery die Quelle
                                // pausiert neu oeffnet statt das
                                // fertige Medium spontan neu zu starten.
                                // Ein zwischenzeitlicher Nutzer-Play hat
                                // die Epoche erhoeht und wird hier nicht
                                // ueberschrieben; der Ended-Latch plus
                                // Gate behandeln den Neustart konsistent.
                                let mut bus = media_control.write();
                                let entry = bus.entry(*id).or_default();
                                if entry.epoch == control.epoch {
                                    entry.playing = false;
                                }
                                ended_media.insert(*id);
                                send_media_state(
                                    events,
                                    last_media_state,
                                    *id,
                                    false,
                                    media.timestamp_100ns() as f64 / 10_000_000.0,
                                );
                                last_failures.remove(&(*id, FailureKind::MediaRead));
                            }
                        }
                        Ok(ReadOutcome::Advanced) => {
                            send_media_state(
                                events,
                                last_media_state,
                                *id,
                                true,
                                media.timestamp_100ns() as f64 / 10_000_000.0,
                            );
                            last_failures.remove(&(*id, FailureKind::MediaRead));
                        }
                        Ok(ReadOutcome::Paced) => {
                            // Pufferziel erreicht bzw. Video der
                            // Wanduhr voraus: kein Standwechsel und
                            // bewusst kein EOF (ein Loop wuerde
                            // sonst jede Sekunde neu starten).
                        }
                        Err(error) => {
                            read_backoff.insert(*id);
                            let reason = error.to_string();
                            let changed = should_report_failure(
                                last_failures,
                                *id,
                                FailureKind::MediaRead,
                                &reason,
                            );
                            if changed {
                                let _ = events.send(EngineEvent::UnsupportedMedia {
                                    source_id: *id,
                                    reason,
                                });
                            }
                        }
                    }
                }
            } else {
                // Unsichtbar ohne continue_when_hidden: Uhr
                // anhalten, damit das Resuemee nicht vorspringt.
                media.set_paused(true);
            }
        }
    }
    *previous_visible = visible_ids;
}

/// Renderfehler (z.B. D2DERR_RECREATE_TARGET nach TDR) beendet den
/// Thread nicht: Geraeteabhaengigen Zustand neu aufbauen und
/// weiterlaufen. Rueckgabe `true`: endgueltig aufgegeben, der
/// Aufrufer beendet die Schleife.
fn recover_after_render_error(
    error: &WindowsVideoError,
    surfaces: NativeSurfaces,
    output: &OutputConfig,
    compositor: &mut D3d11Compositor,
    media_context: &mut Option<MediaFoundationContext>,
    mf_last_error: &mut Option<String>,
    media_sources: &mut HashMap<Uuid, (String, MediaVideoSource)>,
    last_media_state: &mut HashMap<Uuid, (bool, f64)>,
    last_failures: &mut HashMap<(Uuid, FailureKind), String>,
    read_backoff: &mut HashSet<Uuid>,
    captures: &mut HashMap<Uuid, WindowCapture>,
    frames: &mut HashMap<Uuid, D3d11CapturedFrame>,
    stale_windows: &mut HashSet<Uuid>,
    display_captures: &mut HashMap<Uuid, DisplayCapture>,
    image_textures: &mut HashMap<Uuid, (String, ID3D11Texture2D)>,
    previous_visible: &mut HashSet<Uuid>,
    recovery_streak: &mut u32,
    rendered_frames_since_recovery: &mut u32,
    last_source_sync: &mut Instant,
    thread_stop: &AtomicBool,
    events: &mpsc::Sender<EngineEvent>,
) -> bool {
    // Jeder Eingriff zaehlt — unabhaengig vom Create-Ausgang: Eine
    // Flatter-Serie (Create gelingt, Rendern scheitert weiter) muss
    // ueber Backoff abgebremst werden und am Streak-Limit terminieren.
    *recovery_streak += 1;
    *rendered_frames_since_recovery = 0;
    let _ = events.send(EngineEvent::DeviceRecovery {
        phase: DeviceRecoveryPhase::Started,
        detail: Some(error.to_string()),
    });
    clear_device_tied_state(
        media_sources,
        last_media_state,
        last_failures,
        read_backoff,
        media_context,
        captures,
        frames,
        stale_windows,
        display_captures,
        image_textures,
        previous_visible,
    );
    *last_source_sync = Instant::now() - Duration::from_secs(2);
    match D3d11Compositor::create(
        surfaces.program,
        surfaces.preview,
        output.width,
        output.height,
    ) {
        Ok(recreated) => {
            *compositor = recreated;
            restart_media_context(media_context, mf_last_error, &compositor.device);
            let _ = events.send(EngineEvent::DeviceRecovery {
                phase: DeviceRecoveryPhase::Succeeded,
                detail: None,
            });
            if *recovery_streak > 1 {
                // Wiederholter Eingriff: auch nach gegluecktem Create
                // drosseln, sonst flutet eine Flatter-Serie den
                // Event-Kanal und der Thread rotiert heiss.
                sleep_backoff(thread_stop, *recovery_streak);
            }
            if *recovery_streak >= MAX_RECOVERY_FAILURES {
                // Endgueltig aufgeben: Terminal-Event, damit
                // das Frontend 'retryt' von 'Render-Thread
                // dauerhaft tot' unterscheiden kann.
                let _ = events.send(EngineEvent::DeviceRecovery {
                    phase: DeviceRecoveryPhase::Failed,
                    detail: Some(format!(
                        "Wiederherstellung endgültig aufgegeben: {} Eingriffe in Folge ohne stabilen Frame",
                        *recovery_streak
                    )),
                });
                return true;
            }
        }
        Err(create_error) => {
            let _ = events.send(EngineEvent::DeviceRecovery {
                phase: DeviceRecoveryPhase::Failed,
                detail: Some(create_error.to_string()),
            });
            if *recovery_streak >= MAX_RECOVERY_FAILURES {
                // Endgueltig aufgeben: Terminal-Event, damit
                // das Frontend 'retryt' von 'Render-Thread
                // dauerhaft tot' unterscheiden kann.
                let _ = events.send(EngineEvent::DeviceRecovery {
                    phase: DeviceRecoveryPhase::Failed,
                    detail: Some(format!(
                        "Wiederherstellung endgültig aufgegeben nach {} fehlgeschlagenen Versuchen",
                        MAX_RECOVERY_FAILURES
                    )),
                });
                return true;
            }
            sleep_backoff(thread_stop, *recovery_streak);
        }
    }
    false
}

/// Backoff-Dauer nach fehlgeschlagener Recovery: 200ms je Fehlschlag,
/// max. 2s.
fn backoff_duration(failures: u32) -> Duration {
    Duration::from_millis(200)
        .saturating_mul(failures)
        .min(Duration::from_secs(2))
}

/// Backoff nach fehlgeschlagener Recovery: durch Shutdown unterbrechbar
/// — der Thread rotiert nie heiss.
fn sleep_backoff(stop: &AtomicBool, failures: u32) {
    let deadline = Instant::now() + backoff_duration(failures);
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
    }
}

/// Fehlerart fuer das Transition-Dedup von Failure-Events: identische
/// Meldungen pro (Quelle, Art) werden nur einmal gesendet.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum FailureKind {
    MediaOpen,
    MediaRead,
    Image,
    Display,
    Window,
}

/// True, wenn ein Failure-Event gesendet werden soll: identische
/// Fehlermeldungen werden unterdrueckt, bis Erfolg oder Recovery den
/// Eintrag loescht.
fn should_report_failure(
    failures: &mut HashMap<(Uuid, FailureKind), String>,
    source_id: Uuid,
    kind: FailureKind,
    reason: &str,
) -> bool {
    if failures.get(&(source_id, kind)).map(String::as_str) == Some(reason) {
        return false;
    }
    failures.insert((source_id, kind), reason.to_string());
    true
}

/// MediaState nur bei Aenderung senden: ein Wechsel von `playing` geht
/// sofort raus, Positionsfortschritt ist auf 0,25 s quantisiert, damit
/// Wiedergabe nicht pro Frame ein Event erzeugt.
fn send_media_state(
    events: &mpsc::Sender<EngineEvent>,
    last_media_state: &mut HashMap<Uuid, (bool, f64)>,
    source_id: Uuid,
    playing: bool,
    position_seconds: f64,
) {
    const POSITION_EVENT_STEP_SECONDS: f64 = 0.25;
    let changed = match last_media_state.get(&source_id).copied() {
        None => true,
        Some((last_playing, last_position)) => {
            playing != last_playing
                || (position_seconds - last_position).abs() >= POSITION_EVENT_STEP_SECONDS
        }
    };
    if changed {
        last_media_state.insert(source_id, (playing, position_seconds));
        let _ = events.send(EngineEvent::MediaState {
            source_id,
            state: crate::engine::MediaRuntimeState {
                playing,
                position_seconds,
                duration_seconds: None,
            },
        });
    }
}

/// Rotationsreihenfolge der Quell-Synchronisierer pro Sync-Tick: alle
/// vier teilen sich das Erstellungs-Budget des Ticks; die rotierende
/// Reihenfolge verhindert dauerhafte Bevorzugung eines Quelltyps.
#[derive(Clone, Copy)]
enum SyncPass {
    Window,
    Display,
    Images,
    Media,
}

fn synchronize_media(
    project: &ProjectV1,
    context: &MediaFoundationContext,
    events: &mpsc::Sender<EngineEvent>,
    media_audio: &MediaAudioBus,
    sources: &mut HashMap<Uuid, (String, MediaVideoSource)>,
    failures: &mut HashMap<(Uuid, FailureKind), String>,
    creations: &mut u32,
) {
    let desired: HashMap<Uuid, &str> = project
        .sources
        .iter()
        .filter_map(|source| match source {
            Source::Media { id, path, .. } => Some((*id, path.as_str())),
            _ => None,
        })
        .collect();
    sources.retain(|source_id, _| desired.contains_key(source_id));
    media_audio
        .lock()
        .retain(|source_id, _| desired.contains_key(source_id));
    for (source_id, path) in desired {
        if sources
            .get(&source_id)
            .is_some_and(|(loaded, _)| loaded == path)
        {
            continue;
        }
        if *creations > 0 {
            // Hoechstens eine teure Quell-Neuanlage pro Sync-Tick:
            // Fehlschlaege retryen im naechsten Tick, nicht im selben.
            continue;
        }
        *creations += 1;
        let ring = media_audio
            .lock()
            .entry(source_id)
            .or_insert_with(|| Arc::new(Mutex::new(PcmRing::new(SAMPLE_RATE as usize * 2))))
            .clone();
        match context.open_video(Path::new(path), ring) {
            Ok(source) => {
                sources.insert(source_id, (path.to_string(), source));
                failures.remove(&(source_id, FailureKind::MediaOpen));
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let reason = error.to_string();
                if should_report_failure(failures, source_id, FailureKind::MediaOpen, &reason) {
                    let _ = events.send(EngineEvent::UnsupportedMedia { source_id, reason });
                }
            }
        }
    }
}

fn synchronize_images(
    project: &ProjectV1,
    device: &D3d11Device,
    events: &mpsc::Sender<EngineEvent>,
    textures: &mut HashMap<Uuid, (String, ID3D11Texture2D)>,
    failures: &mut HashMap<(Uuid, FailureKind), String>,
    creations: &mut u32,
) {
    let desired: HashMap<Uuid, &str> = project
        .sources
        .iter()
        .filter_map(|source| match source {
            Source::Image { id, path, .. } => Some((*id, path.as_str())),
            _ => None,
        })
        .collect();
    textures.retain(|source_id, _| desired.contains_key(source_id));
    for (source_id, path) in desired {
        if textures
            .get(&source_id)
            .is_some_and(|(loaded, _)| loaded == path)
        {
            continue;
        }
        if *creations > 0 {
            // Hoechstens eine teure Quell-Neuanlage pro Sync-Tick:
            // Fehlschlaege retryen im naechsten Tick, nicht im selben.
            continue;
        }
        *creations += 1;
        match device.load_image(Path::new(path)) {
            Ok(texture) => {
                textures.insert(source_id, (path.to_string(), texture));
                failures.remove(&(source_id, FailureKind::Image));
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let reason = error.to_string();
                if should_report_failure(failures, source_id, FailureKind::Image, &reason) {
                    let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                }
            }
        }
    }
}

fn synchronize_display_captures(
    project: &ProjectV1,
    device: &D3d11Device,
    events: &mpsc::Sender<EngineEvent>,
    captures: &mut HashMap<Uuid, DisplayCapture>,
    failures: &mut HashMap<(Uuid, FailureKind), String>,
    creations: &mut u32,
) {
    let desired: HashMap<Uuid, &DisplayBinding> = project
        .sources
        .iter()
        .filter_map(|source| match source {
            Source::Display { id, binding, .. } => Some((*id, binding)),
            _ => None,
        })
        .collect();
    captures.retain(|source_id, _| desired.contains_key(source_id));
    for (source_id, binding) in desired {
        if captures.contains_key(&source_id) {
            continue;
        }
        if *creations > 0 {
            // Hoechstens eine teure Quell-Neuanlage pro Sync-Tick:
            // Fehlschlaege retryen im naechsten Tick, nicht im selben.
            continue;
        }
        *creations += 1;
        match DisplayCapture::create(device, binding) {
            Ok(capture) => {
                captures.insert(source_id, capture);
                failures.remove(&(source_id, FailureKind::Display));
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let reason = error.to_string();
                if should_report_failure(failures, source_id, FailureKind::Display, &reason) {
                    let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                }
            }
        }
    }
}

fn synchronize_window_captures(
    project: &ProjectV1,
    surfaces: NativeSurfaces,
    device: &D3d11Device,
    events: &mpsc::Sender<EngineEvent>,
    captures: &mut HashMap<Uuid, WindowCapture>,
    frames: &mut HashMap<Uuid, D3d11CapturedFrame>,
    failures: &mut HashMap<(Uuid, FailureKind), String>,
    creations: &mut u32,
) {
    let desired: HashMap<Uuid, &crate::project::WindowBinding> = project
        .sources
        .iter()
        .filter_map(|source| match source {
            Source::Window { id, binding, .. } => Some((*id, binding)),
            _ => None,
        })
        .collect();
    let desired_ids: HashSet<Uuid> = desired.keys().copied().collect();
    captures.retain(|source_id, _| desired_ids.contains(source_id));
    frames.retain(|source_id, _| desired_ids.contains(source_id));
    let excluded = [surfaces.studio, surfaces.program, surfaces.preview];
    for (source_id, binding) in desired {
        if captures.contains_key(&source_id) {
            continue;
        }
        if *creations > 0 {
            // Hoechstens eine teure Quell-Neuanlage pro Sync-Tick:
            // Fehlschlaege retryen im naechsten Tick, nicht im selben.
            continue;
        }
        *creations += 1;
        let capture = crate::discovery::windows::resolve_window(binding, &excluded)
            .map_err(WindowsVideoError::SourceUnavailable)
            .and_then(|window| WindowCapture::start(device, window));
        match capture {
            Ok(capture) => {
                captures.insert(source_id, capture);
                failures.remove(&(source_id, FailureKind::Window));
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let reason = error.to_string();
                if should_report_failure(failures, source_id, FailureKind::Window, &reason) {
                    let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                }
            }
        }
    }
}

fn source_name(source: &Source) -> &str {
    match source {
        Source::Window { name, .. }
        | Source::Display { name, .. }
        | Source::Image { name, .. }
        | Source::Text { name, .. }
        | Source::Media { name, .. }
        | Source::ApplicationAudio { name, .. } => name,
    }
}

fn parse_hex_color(value: &str) -> [f32; 4] {
    let fallback = [0.0, 0.0, 0.0, 1.0];
    let Some(hex) = value.strip_prefix('#') else {
        return fallback;
    };
    let bytes = hex.as_bytes();
    if bytes.len() != 6 || !bytes.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return fallback;
    }
    let component = |start: usize| {
        std::str::from_utf8(&bytes[start..start + 2])
            .ok()
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .map_or(0.0, |component| f32::from(component) / 255.0)
    };
    [component(0), component(2), component(4), 1.0]
}

pub struct D3d11CapturedFrame {
    pub texture: ID3D11Texture2D,
    frame: Direct3D11CaptureFrame,
}

impl Drop for D3d11CapturedFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

pub struct WindowCapture {
    frame_pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    frame_token: i64,
    latest: Arc<Mutex<Option<D3d11CapturedFrame>>>,
    last_frame: Arc<Mutex<Option<Instant>>>,
    closed: Arc<AtomicBool>,
    created_at: Instant,
    ro_initialized: bool,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl WindowCapture {
    pub fn start(device: &D3d11Device, window: usize) -> Result<Self, WindowsVideoError> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let result = Self::start_initialized(device, window);
        if result.is_err() {
            unsafe { RoUninitialize() };
        }
        result
    }

    fn start_initialized(device: &D3d11Device, window: usize) -> Result<Self, WindowsVideoError> {
        if window == 0 {
            return Err(WindowsVideoError::SourceUnavailable(
                "window handle is null".into(),
            ));
        }
        let dxgi_device = device
            .device
            .cast::<IDXGIDevice>()
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let winrt_device = inspectable
            .cast::<IDirect3DDevice>()
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let interop: IGraphicsCaptureItemInterop =
            factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(HWND(window as *mut _)) }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let size = item
            .Size()
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(WindowsVideoError::SourceUnavailable(
                "window has no capturable client area".into(),
            ));
        }
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            SizeInt32 {
                Width: size.Width,
                Height: size.Height,
            },
        )
        .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let closed = Arc::new(AtomicBool::new(false));
        let callback_closed = closed.clone();
        let closed_handler =
            TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                callback_closed.store(true, Ordering::Release);
                Ok(())
            });
        let _closed_token = item
            .Closed(&closed_handler)
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let latest = Arc::new(Mutex::new(None));
        let callback_latest = latest.clone();
        let last_frame = Arc::new(Mutex::new(None));
        let callback_last_frame = last_frame.clone();
        let content_size = Arc::new(Mutex::new((size.Width, size.Height)));
        let callback_content_size = content_size.clone();
        let recreating = Arc::new(Mutex::new(()));
        let callback_recreating = recreating.clone();
        let callback_device = SendDirect3DDevice(winrt_device.clone());
        let handler =
            TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |sender, _| {
                let Some(pool) = sender.as_ref() else {
                    return Ok(());
                };
                let frame = pool.TryGetNextFrame()?;
                let content = frame.ContentSize()?;
                {
                    let mut last = callback_content_size.lock();
                    if (last.0, last.1) != (content.Width, content.Height)
                        && let Some(_guard) = callback_recreating.try_lock()
                        && callback_device
                            .recreate_pool(pool, content.Width, content.Height)
                            .is_ok()
                    {
                        // Erst nach erfolgreichem Recreate committen, damit
                        // der naechste Frame eine fehlgeschlagene Groessen-
                        // aenderung erneut versucht.
                        *last = (content.Width, content.Height);
                    }
                }
                let surface = frame.Surface()?;
                let access = surface.cast::<IDirect3DDxgiInterfaceAccess>()?;
                let texture = unsafe { access.GetInterface::<ID3D11Texture2D>()? };
                callback_latest
                    .lock()
                    .replace(D3d11CapturedFrame { texture, frame });
                *callback_last_frame.lock() = Some(Instant::now());
                Ok(())
            });
        let frame_token = frame_pool
            .FrameArrived(&handler)
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let session = frame_pool
            .CreateCaptureSession(&item)
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        session
            .StartCapture()
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        Ok(Self {
            frame_pool,
            session,
            frame_token,
            latest,
            last_frame,
            closed,
            created_at: Instant::now(),
            ro_initialized: true,
            _thread_affinity: PhantomData,
        })
    }

    pub fn take_latest(&self) -> Option<D3d11CapturedFrame> {
        self.latest.lock().take()
    }

    pub fn is_stale(&self, maximum_age: Duration) -> bool {
        match *self.last_frame.lock() {
            Some(arrival) => arrival.elapsed() > maximum_age,
            // Nie gelieferte Captives bekommen eine vierfach lange Gnadenfrist,
            // damit minimierte oder verdeckte Fenster nicht sofort als tot gelten.
            None => self.created_at.elapsed() > maximum_age * 4,
        }
    }

    pub fn has_delivered(&self) -> bool {
        self.last_frame.lock().is_some()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let _ = self.frame_pool.RemoveFrameArrived(self.frame_token);
        let _ = self.session.Close();
        let _ = self.frame_pool.Close();
        if self.ro_initialized {
            unsafe { RoUninitialize() };
        }
    }
}

pub struct DisplayCapture {
    duplication: IDXGIOutputDuplication,
    texture: Option<ID3D11Texture2D>,
}

impl DisplayCapture {
    pub fn create(
        device: &D3d11Device,
        binding: &DisplayBinding,
    ) -> Result<Self, WindowsVideoError> {
        let dxgi_device = device
            .device
            .cast::<IDXGIDevice>()
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let description = unsafe { adapter.GetDesc() }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let luid = format!(
            "{:08x}:{:08x}",
            description.AdapterLuid.HighPart as u32, description.AdapterLuid.LowPart
        );
        if !luid.eq_ignore_ascii_case(&binding.adapter_luid) {
            return Err(WindowsVideoError::SourceUnavailable(
                "display is connected to a different graphics adapter".into(),
            ));
        }
        let output = unsafe { adapter.EnumOutputs(binding.output_id) }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let output5: IDXGIOutput5 = output
            .cast()
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let duplication =
            unsafe { output5.DuplicateOutput1(&device.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) }
                .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        Ok(Self {
            duplication,
            texture: None,
        })
    }

    pub fn acquire_latest(
        &mut self,
        context: &ID3D11DeviceContext,
        device: &ID3D11Device,
    ) -> Result<bool, WindowsVideoError> {
        let mut information = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe {
            self.duplication
                .AcquireNextFrame(0, &mut information, &mut resource)
        } {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(error) => return Err(WindowsVideoError::CaptureApi(error.to_string())),
        }
        let result = (|| {
            let source: ID3D11Texture2D = resource
                .ok_or_else(|| {
                    WindowsVideoError::CaptureApi("Desktop Duplication returned no texture".into())
                })?
                .cast()
                .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
            let mut description = D3D11_TEXTURE2D_DESC::default();
            unsafe { source.GetDesc(&mut description) };
            let recreate = self.texture.as_ref().is_none_or(|texture| {
                let mut existing = D3D11_TEXTURE2D_DESC::default();
                unsafe { texture.GetDesc(&mut existing) };
                existing.Width != description.Width || existing.Height != description.Height
            });
            if recreate {
                description.MipLevels = 1;
                description.ArraySize = 1;
                description.Usage = D3D11_USAGE_DEFAULT;
                description.BindFlags =
                    (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32;
                description.CPUAccessFlags = 0;
                description.MiscFlags = 0;
                let mut texture = None;
                unsafe { device.CreateTexture2D(&description, None, Some(&mut texture)) }
                    .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
                self.texture = texture;
            }
            let target = self.texture.as_ref().ok_or_else(|| {
                WindowsVideoError::CaptureApi(
                    "failed to allocate Desktop Duplication texture".into(),
                )
            })?;
            unsafe { context.CopyResource(target, &source) };
            Ok(true)
        })();
        let release = unsafe { self.duplication.ReleaseFrame() }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()));
        result.and(release.map(|()| true))
    }

    pub fn texture(&self) -> Option<&ID3D11Texture2D> {
        self.texture.as_ref()
    }
}

pub struct MediaFoundationContext {
    manager: IMFDXGIDeviceManager,
}

impl MediaFoundationContext {
    pub fn start(device: &D3d11Device) -> Result<Self, WindowsVideoError> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        // MFStartup/MFShutdown sind prozessweit refcounted: Jeder
        // Fehlerpfad nach dem Startup muss die Referenz wieder ausgleichen,
        // sonst leckt jede fehlgeschlagene Initialisierung.
        let setup = || -> Result<IMFDXGIDeviceManager, WindowsVideoError> {
            let mut reset_token = 0;
            let mut manager = None;
            unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager) }
                .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
            let manager = manager.ok_or_else(|| {
                WindowsVideoError::UnsupportedMedia(
                    "Media Foundation returned no DXGI manager".into(),
                )
            })?;
            unsafe { manager.ResetDevice(&device.device, reset_token) }
                .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
            Ok(manager)
        };
        match setup() {
            Ok(manager) => Ok(Self { manager }),
            Err(error) => {
                let _ = unsafe { MFShutdown() };
                Err(error)
            }
        }
    }

    pub fn open_video(
        &self,
        path: &Path,
        audio_ring: Arc<Mutex<PcmRing>>,
    ) -> Result<MediaVideoSource, WindowsVideoError> {
        let mut attributes = None;
        unsafe { MFCreateAttributes(&mut attributes, 3) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let attributes = attributes.ok_or_else(|| {
            WindowsVideoError::UnsupportedMedia("Media Foundation returned no attributes".into())
        })?;
        unsafe { attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &self.manager) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        unsafe { attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let reader = unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attributes) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let video_enabled = (|| -> windows::core::Result<()> {
            let media_type = unsafe { MFCreateMediaType() }?;
            unsafe {
                media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
                reader.SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &media_type,
                )
            }
        })()
        .is_ok();
        let audio_enabled = (|| -> windows::core::Result<()> {
            let audio_type = unsafe { MFCreateMediaType() }?;
            unsafe {
                audio_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                audio_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
                audio_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                audio_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
                audio_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)?;
                reader.SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                    None,
                    &audio_type,
                )
            }
        })()
        .is_ok();
        if !video_enabled && !audio_enabled {
            return Err(WindowsVideoError::UnsupportedMedia(
                "Media Foundation found no supported audio or video stream".into(),
            ));
        }
        Ok(MediaVideoSource {
            reader,
            texture: None,
            timestamp_100ns: 0,
            audio_ring,
            audio_enabled,
            video_enabled,
            decoded_video: false,
            video_ended: false,
            audio_ended: false,
            // Uhr startet lazy mit dem ersten read_next: Das Medium kann
            // lange vor der ersten Sichtbarkeit geoeffnet werden.
            play_epoch: Instant::now(),
            play_offset_100ns: 0,
            clock_paused: true,
        })
    }
}

impl Drop for MediaFoundationContext {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

/// Audio-Nachschubziel pro Medium: 200 ms Ringfuellstand bei 48 kHz. Der
/// WASAPI-Mixer verbraucht Echtzeit; darunter drohen Underruns, drueber
/// wächst die A/V-Latenz ungebunden.
const AUDIO_TARGET_FRAMES: usize = SAMPLE_RATE as usize * 2 / 10;
/// PTS-Vorlauf fuer den Video-Leseentscheid: 50 ms in 100-ns-Einheiten.
const VIDEO_LEAD_100NS: i64 = 500_000;

/// Ergebnis eines Lesezyklus: Fortschritt, gepaced (nichts faellig) oder
/// Stream-Ende. Paced ist bewusst NICHT `false`: Der Aufrufer behandelt
/// `false` als EOF inklusive Loop-Restart per seek(0).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadOutcome {
    Advanced,
    Paced,
    Eof,
}

pub struct MediaVideoSource {
    reader: IMFSourceReader,
    texture: Option<ID3D11Texture2D>,
    timestamp_100ns: i64,
    audio_ring: Arc<Mutex<PcmRing>>,
    audio_enabled: bool,
    video_enabled: bool,
    // Einmal erfolgreich dekodiert: bleibt ueber einen Seek hinweg
    // gesetzt (der Seek leert nur die Textur bis zum naechsten
    // Frame); Reset ausschliesslich beim Neuoeffnen der Quelle.
    decoded_video: bool,
    video_ended: bool,
    audio_ended: bool,
    play_epoch: Instant,
    play_offset_100ns: i64,
    clock_paused: bool,
}

impl MediaVideoSource {
    pub(crate) fn read_next(&mut self) -> Result<ReadOutcome, WindowsVideoError> {
        if self.streams_ended() {
            // Beide Streams am Ende: bis zum naechsten Seek kein
            // ReadSample mehr — MF liefert sonst jeden Aufruf EOS.
            return Ok(ReadOutcome::Eof);
        }
        let mut advanced = false;
        if self.video_enabled && self.video_due() {
            advanced |= self.read_video()?;
        }
        if self.audio_enabled {
            advanced |= self.refill_audio()?;
        }
        if advanced {
            Ok(ReadOutcome::Advanced)
        } else if self.streams_ended() {
            // Beide Streams endeten genau in diesem Zyklus.
            Ok(ReadOutcome::Eof)
        } else {
            Ok(ReadOutcome::Paced)
        }
    }

    fn streams_ended(&self) -> bool {
        (!self.video_enabled || self.video_ended) && (!self.audio_enabled || self.audio_ended)
    }

    /// Wanduhr des Mediums in 100-ns-PTS-Einheiten; waehrend Pause eingefroren.
    fn media_clock_100ns(&self) -> i64 {
        let elapsed = if self.clock_paused {
            0
        } else {
            (self.play_epoch.elapsed().as_nanos() / 100) as i64
        };
        self.play_offset_100ns + elapsed
    }

    /// Pausiert/resumiert die Wanduhr; idempotent, Aufruf in jedem Tick ok.
    pub fn set_paused(&mut self, paused: bool) {
        if paused == self.clock_paused {
            return;
        }
        if paused {
            // Einfrieren: verstrichene Zeit in den Offset konsolidieren.
            self.play_offset_100ns = self.media_clock_100ns();
            self.clock_paused = true;
        } else {
            self.play_epoch = Instant::now();
            self.clock_paused = false;
        }
    }

    /// Video nur lesen, wenn die Wanduhr den letzten Frame eingeholt hat
    /// (plus Vorlauf); der erste Frame nach Open/Seek kommt immer sofort.
    fn video_due(&self) -> bool {
        if self.texture.is_none() {
            return true;
        }
        self.timestamp_100ns <= self.media_clock_100ns() + VIDEO_LEAD_100NS
    }

    fn read_video(&mut self) -> Result<bool, WindowsVideoError> {
        let mut stream = 0;
        let mut flags = 0;
        let mut timestamp = 0;
        let mut sample = None;
        unsafe {
            self.reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                Some(&mut stream),
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        }
        .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let Some(sample) = sample else {
            self.video_ended = true;
            return Ok(false);
        };
        let buffer = unsafe { sample.GetBufferByIndex(0) }
            .or_else(|_| unsafe { sample.ConvertToContiguousBuffer() })
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let dxgi: IMFDXGIBuffer = buffer
            .cast()
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let mut raw = std::ptr::null_mut();
        unsafe { dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        self.texture = Some(unsafe { ID3D11Texture2D::from_raw(raw) });
        self.decoded_video = true;
        self.timestamp_100ns = timestamp;
        Ok(true)
    }

    /// Nachschub nach Ringfuellstand (Echtzeit-Verbrauch des WASAPI-Mixers),
    /// nicht nach Tickrhythmus: fuellt auf bis zum 200-ms-Ziel oder EOF.
    fn refill_audio(&mut self) -> Result<bool, WindowsVideoError> {
        let mut advanced = false;
        let mut idle_probes = 0;
        while self.audio_ring.lock().filled_frames() < AUDIO_TARGET_FRAMES {
            match self.read_audio_once()? {
                None => {
                    self.audio_ended = true;
                    break;
                }
                Some(0) => {
                    // Entartete Probe ohne Frames: Schleife begrenzen.
                    idle_probes += 1;
                    if idle_probes >= 4 {
                        break;
                    }
                }
                Some(_) => {
                    idle_probes = 0;
                    advanced = true;
                }
            }
        }
        Ok(advanced)
    }

    /// Ein ReadSample-Zyklus: None = Stream-Ende, Some(n) = n Stereo-Frames
    /// in den Ring geschoben (ein Lock-Zyklus pro Sample-Batch).
    fn read_audio_once(&mut self) -> Result<Option<usize>, WindowsVideoError> {
        let mut stream = 0;
        let mut flags = 0;
        let mut timestamp = 0;
        let mut sample = None;
        unsafe {
            self.reader.ReadSample(
                MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                0,
                Some(&mut stream),
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        }
        .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let Some(sample) = sample else {
            return Ok(None);
        };
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let mut data = std::ptr::null_mut();
        let mut length = 0;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let frames = if !data.is_null() && length >= 8 {
            let samples =
                unsafe { std::slice::from_raw_parts(data.cast::<f32>(), length as usize / 4) };
            let stereo_frames = samples.len() / 2;
            self.audio_ring.lock().push_slice(samples);
            stereo_frames
        } else {
            0
        };
        unsafe { buffer.Unlock() }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        Ok(Some(frames))
    }

    pub fn seek(&mut self, seconds: f64) -> Result<(), WindowsVideoError> {
        let ticks = (seconds.max(0.0) * 10_000_000.0).round() as i64;
        let position = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_I8,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 { hVal: ticks },
                }),
            },
        };
        let format = windows::core::GUID::zeroed();
        unsafe { self.reader.SetCurrentPosition(&format, &position) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        if self.video_enabled {
            let _ = unsafe {
                self.reader
                    .Flush(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            };
        }
        if self.audio_enabled {
            let _ = unsafe {
                self.reader
                    .Flush(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32)
            };
            *self.audio_ring.lock() = PcmRing::new(SAMPLE_RATE as usize * 2);
        }
        self.texture = None;
        self.timestamp_100ns = ticks;
        self.video_ended = false;
        self.audio_ended = false;
        // Wallclock neu aufsetzen: die Wiedergabe gliedert ab `ticks`.
        self.play_offset_100ns = ticks;
        self.play_epoch = Instant::now();
        Ok(())
    }

    pub fn texture(&self) -> Option<&ID3D11Texture2D> {
        self.texture.as_ref()
    }

    pub fn decoded_video(&self) -> bool {
        self.decoded_video
    }

    pub fn timestamp_100ns(&self) -> i64 {
        self.timestamp_100ns
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneTextureDescriptor {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub render_target: bool,
    pub shader_resource: bool,
}

impl SceneTextureDescriptor {
    pub fn float16(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: FLOAT16_FORMAT.0 as u32,
            render_target: true,
            shader_resource: true,
        }
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum WindowsVideoError {
    #[error("source unavailable: {0}")]
    SourceUnavailable(String),
    #[error("D3D11 device creation failed: {0}")]
    DeviceCreation(String),
    #[error("invalid Float16 scene texture descriptor")]
    InvalidSceneTexture,
    #[error("Windows Graphics Capture failed: {0}")]
    CaptureApi(String),
    #[error("DXGI swapchain failed: {0}")]
    SwapChain(String),
    #[error("unsupported image: {0}")]
    UnsupportedImage(String),
    #[error("unsupported media: {0}")]
    UnsupportedMedia(String),
}
