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
        atomic::{AtomicBool, AtomicU64, Ordering},
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

use super::{GpuFrame, LatestFrame, MediaControlBus};
use crate::{
    audio::{MediaAudioBus, PcmRing, SAMPLE_RATE},
    engine::{DeviceRecoveryPhase, EngineEvent, NativeSurfaces},
    project::{DisplayBinding, ProjectV1, Source, TextAlign, Transform},
};
use parking_lot::RwLock;

pub const D3D11_CREATE_DEVICE_BGRA_SUPPORT: u32 = 0x20;
pub const D3D11_CREATE_DEVICE_VIDEO_SUPPORT: u32 = 0x800;
pub const DXGI_FORMAT_R16G16B16A16_FLOAT: u32 = 10;
pub const DXGI_SWAP_EFFECT_FLIP_DISCARD: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    pub creation_flags: u32,
    pub single_immediate_context: bool,
}

impl Default for DeviceDescriptor {
    fn default() -> Self {
        Self {
            creation_flags: D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            single_immediate_context: true,
        }
    }
}

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
            || descriptor.format != DXGI_FORMAT_R16G16B16A16_FLOAT
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
    source_bitmaps: HashMap<usize, ID2D1Bitmap1>,
    dwrite_factory: IDWriteFactory,
    text_formats: HashMap<String, IDWriteTextFormat>,
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
            let buffer = self
                .text_buffers
                .entry(text.text.to_string())
                .or_insert_with(|| text.text.encode_utf16().collect());
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
        if let Some(bitmap) = self.source_bitmaps.get(&key) {
            return Ok(bitmap.clone());
        }
        if self.source_bitmaps.len() >= 32 {
            self.source_bitmaps.clear();
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
        self.source_bitmaps.insert(key, bitmap.clone());
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
        let key = format!("{family}\\0{size}\\0{weight}\\0{align:?}");
        if let Some(format) = self.text_formats.get(&key) {
            return Ok(format.clone());
        }
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
        self.text_formats.insert(key, format.clone());
        Ok(format)
    }
}

/// WGC-Handler laufen auf ThreadPool-Threads (Send-Grenze). Der Zeiger wird
/// ausschliesslich innerhalb des MTA dereferenziert.
struct SendDirect3DDevice(IDirect3DDevice);

impl SendDirect3DDevice {
    fn recreate_pool(&self, pool: &Direct3D11CaptureFramePool, width: i32, height: i32) {
        let _ = pool.Recreate(
            &self.0,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            SizeInt32 {
                Width: width,
                Height: height,
            },
        );
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
                "Program and Preview window handles are required".into(),
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
                let mut media_context = MediaFoundationContext::start(&compositor.device).ok();
                let mut media_sources: HashMap<Uuid, (String, MediaVideoSource)> = HashMap::new();
                let mut captures: HashMap<Uuid, WindowCapture> = HashMap::new();
                let mut frames: HashMap<Uuid, D3d11CapturedFrame> = HashMap::new();
                let mut stale_windows: HashSet<Uuid> = HashSet::new();
                let mut image_textures: HashMap<Uuid, (String, ID3D11Texture2D)> = HashMap::new();
                let mut display_captures: HashMap<Uuid, DisplayCapture> = HashMap::new();
                let mut previous_visible: HashSet<Uuid> = HashSet::new();
                let mut last_source_sync = Instant::now() - Duration::from_secs(2);
                let mut deadline = Instant::now();
                while !thread_stop.load(Ordering::Acquire) {
                    let snapshot = project.read().clone();
                    let next_output = snapshot.output.clone();
                    if next_output.width != output.width || next_output.height != output.height {
                        let _ = events.send(EngineEvent::DeviceRecovery {
                            phase: DeviceRecoveryPhase::Started,
                            detail: None,
                        });
                        media_sources.clear();
                        drop(media_context.take());
                        captures.clear();
                        frames.clear();
                        stale_windows.clear();
                        display_captures.clear();
                        image_textures.clear();
                        previous_visible.clear();
                        match D3d11Compositor::create(
                            surfaces.program,
                            surfaces.preview,
                            next_output.width,
                            next_output.height,
                        ) {
                            Ok(recreated) => {
                                compositor = recreated;
                                media_context =
                                    MediaFoundationContext::start(&compositor.device).ok();
                                output = next_output;
                                last_source_sync = Instant::now() - Duration::from_secs(2);
                                let _ = events.send(EngineEvent::DeviceRecovery {
                                    phase: DeviceRecoveryPhase::Succeeded,
                                    detail: None,
                                });
                            }
                            Err(error) => {
                                let _ = events.send(EngineEvent::DeviceRecovery {
                                    phase: DeviceRecoveryPhase::Failed,
                                    detail: Some(error.to_string()),
                                });
                                break;
                            }
                        }
                    } else {
                        output = next_output;
                    }

                    if last_source_sync.elapsed() >= Duration::from_secs(1) {
                        synchronize_window_captures(
                            &snapshot,
                            surfaces,
                            &compositor.device,
                            &events,
                            &mut captures,
                            &mut frames,
                        );
                        synchronize_display_captures(
                            &snapshot,
                            &compositor.device,
                            &events,
                            &mut display_captures,
                        );
                        synchronize_images(
                            &snapshot,
                            &compositor.device,
                            &events,
                            &mut image_textures,
                        );
                        if let Some(context) = &media_context {
                            synchronize_media(
                                &snapshot,
                                context,
                                &events,
                                &media_audio,
                                &mut media_sources,
                            );
                        }
                        last_source_sync = Instant::now();
                    }

                    let active_scene = snapshot
                        .scenes
                        .iter()
                        .find(|scene| scene.id == snapshot.active_scene_id);
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
                    let mut failed_displays: Vec<Uuid> = Vec::new();
                    for (source_id, capture) in &mut display_captures {
                        match capture.acquire_latest(
                            &compositor.device.immediate_context,
                            &compositor.device.device,
                        ) {
                            Ok(true) => {
                                let _ = events.send(EngineEvent::SourceAvailable {
                                    source_id: *source_id,
                                });
                            }
                            Ok(false) => {}
                            Err(error) => {
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
                    // Versteckte Quellen halten alte Pool-Frames fest; nur sichtbare behalten.
                    frames.retain(|source_id, _| visible_ids.contains(source_id));
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
                            if *restart_on_show
                                && visible_ids.contains(id)
                                && !previous_visible.contains(id)
                            {
                                let _ = media.seek(0.0);
                            }
                            if let Some(position) = control.seek_seconds {
                                if let Err(error) = media.seek(position) {
                                    let _ = events.send(EngineEvent::UnsupportedMedia {
                                        source_id: *id,
                                        reason: error.to_string(),
                                    });
                                }
                                media_control.write().entry(*id).or_default().seek_seconds = None;
                            }
                            if !control.playing {
                                let _ = events.send(EngineEvent::MediaState {
                                    source_id: *id,
                                    state: crate::engine::MediaRuntimeState {
                                        playing: false,
                                        position_seconds: media.timestamp_100ns() as f64
                                            / 10_000_000.0,
                                        duration_seconds: None,
                                    },
                                });
                                continue;
                            }
                            if visible_ids.contains(id) || *continue_when_hidden {
                                match media.read_next() {
                                    Ok(false) if *looped => {
                                        let _ = media.seek(0.0);
                                    }
                                    Ok(_) => {
                                        let _ = events.send(EngineEvent::MediaState {
                                            source_id: *id,
                                            state: crate::engine::MediaRuntimeState {
                                                playing: true,
                                                position_seconds: media.timestamp_100ns() as f64
                                                    / 10_000_000.0,
                                                duration_seconds: None,
                                            },
                                        });
                                    }
                                    Err(error) => {
                                        let _ = events.send(EngineEvent::UnsupportedMedia {
                                            source_id: *id,
                                            reason: error.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    previous_visible = visible_ids;

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
                                    .and_then(|(_, media)| media.texture())
                                    .is_some();
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
                        let _ = events.send(EngineEvent::DeviceRecovery {
                            phase: DeviceRecoveryPhase::Failed,
                            detail: Some(error.to_string()),
                        });
                        break;
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

fn synchronize_media(
    project: &ProjectV1,
    context: &MediaFoundationContext,
    events: &mpsc::Sender<EngineEvent>,
    media_audio: &MediaAudioBus,
    sources: &mut HashMap<Uuid, (String, MediaVideoSource)>,
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
        let ring = media_audio
            .lock()
            .entry(source_id)
            .or_insert_with(|| Arc::new(Mutex::new(PcmRing::new(SAMPLE_RATE as usize * 2))))
            .clone();
        match context.open_video(Path::new(path), ring) {
            Ok(source) => {
                sources.insert(source_id, (path.to_string(), source));
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let _ = events.send(EngineEvent::UnsupportedMedia {
                    source_id,
                    reason: error.to_string(),
                });
            }
        }
    }
}

fn synchronize_images(
    project: &ProjectV1,
    device: &D3d11Device,
    events: &mpsc::Sender<EngineEvent>,
    textures: &mut HashMap<Uuid, (String, ID3D11Texture2D)>,
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
        match device.load_image(Path::new(path)) {
            Ok(texture) => {
                textures.insert(source_id, (path.to_string(), texture));
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let _ = events.send(EngineEvent::SourceUnavailable {
                    source_id,
                    reason: error.to_string(),
                });
            }
        }
    }
}

fn synchronize_display_captures(
    project: &ProjectV1,
    device: &D3d11Device,
    events: &mpsc::Sender<EngineEvent>,
    captures: &mut HashMap<Uuid, DisplayCapture>,
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
        match DisplayCapture::create(device, binding) {
            Ok(capture) => {
                captures.insert(source_id, capture);
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let _ = events.send(EngineEvent::SourceUnavailable {
                    source_id,
                    reason: error.to_string(),
                });
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
        let capture = crate::discovery::windows::resolve_window(binding, &excluded)
            .map_err(WindowsVideoError::SourceUnavailable)
            .and_then(|window| WindowCapture::start(device, source_id, window));
        match capture {
            Ok(capture) => {
                captures.insert(source_id, capture);
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(error) => {
                let _ = events.send(EngineEvent::SourceUnavailable {
                    source_id,
                    reason: error.to_string(),
                });
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
    pub source_id: Uuid,
    pub sequence: u64,
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
    pub fn start(
        device: &D3d11Device,
        source_id: Uuid,
        window: usize,
    ) -> Result<Self, WindowsVideoError> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|error| WindowsVideoError::CaptureApi(error.to_string()))?;
        let result = Self::start_initialized(device, source_id, window);
        if result.is_err() {
            unsafe { RoUninitialize() };
        }
        result
    }

    fn start_initialized(
        device: &D3d11Device,
        source_id: Uuid,
        window: usize,
    ) -> Result<Self, WindowsVideoError> {
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
        let sequence = Arc::new(AtomicU64::new(0));
        let callback_sequence = sequence.clone();
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
                    {
                        *last = (content.Width, content.Height);
                        callback_device.recreate_pool(pool, content.Width, content.Height);
                    }
                }
                let surface = frame.Surface()?;
                let access = surface.cast::<IDirect3DDxgiInterfaceAccess>()?;
                let texture = unsafe { access.GetInterface::<ID3D11Texture2D>()? };
                callback_latest.lock().replace(D3d11CapturedFrame {
                    source_id,
                    sequence: callback_sequence.fetch_add(1, Ordering::Relaxed) + 1,
                    texture,
                    frame,
                });
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
        let mut reset_token = 0;
        let mut manager = None;
        if let Err(error) = unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager) } {
            let _ = unsafe { MFShutdown() };
            return Err(WindowsVideoError::UnsupportedMedia(error.to_string()));
        }
        let manager = manager.ok_or_else(|| {
            WindowsVideoError::UnsupportedMedia("Media Foundation returned no DXGI manager".into())
        })?;
        unsafe { manager.ResetDevice(&device.device, reset_token) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        Ok(Self { manager })
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
        })
    }
}

impl Drop for MediaFoundationContext {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

pub struct MediaVideoSource {
    reader: IMFSourceReader,
    texture: Option<ID3D11Texture2D>,
    timestamp_100ns: i64,
    audio_ring: Arc<Mutex<PcmRing>>,
    audio_enabled: bool,
    video_enabled: bool,
}

impl MediaVideoSource {
    pub fn read_next(&mut self) -> Result<bool, WindowsVideoError> {
        let video = if self.video_enabled {
            self.read_video()?
        } else {
            false
        };
        let audio = if self.audio_enabled {
            self.read_audio()?
        } else {
            false
        };
        Ok(video || audio)
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
        self.timestamp_100ns = timestamp;
        Ok(true)
    }

    fn read_audio(&mut self) -> Result<bool, WindowsVideoError> {
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
            return Ok(false);
        };
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        let mut data = std::ptr::null_mut();
        let mut length = 0;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        if !data.is_null() && length >= 8 {
            let samples =
                unsafe { std::slice::from_raw_parts(data.cast::<f32>(), length as usize / 4) };
            let mut ring = self.audio_ring.lock();
            for frame in samples.chunks_exact(2) {
                ring.push([frame[0], frame[1]]);
            }
        }
        unsafe { buffer.Unlock() }
            .map_err(|error| WindowsVideoError::UnsupportedMedia(error.to_string()))?;
        Ok(true)
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
        Ok(())
    }

    pub fn texture(&self) -> Option<&ID3D11Texture2D> {
        self.texture.as_ref()
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
            format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            render_target: true,
            shader_resource: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwapChainDescriptor {
    pub buffer_count: u32,
    pub swap_effect: u32,
    pub allow_tearing: bool,
}

impl Default for SwapChainDescriptor {
    fn default() -> Self {
        Self {
            buffer_count: 2,
            swap_effect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            allow_tearing: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureKind {
    WindowGraphicsCapture,
    DesktopDuplication,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState {
    Offline,
    Starting,
    Live,
    Unavailable,
    Stopped,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum WindowsVideoError {
    #[error("source unavailable: {0}")]
    SourceUnavailable(String),
    #[error("protected content cannot be captured")]
    ProtectedContent,
    #[error("D3D11 device removed: HRESULT {0:#x}")]
    DeviceRemoved(i32),
    #[error("capture state transition is invalid")]
    InvalidTransition,
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

pub struct CaptureSession {
    pub source_id: Uuid,
    pub kind: CaptureKind,
    state: CaptureState,
    latest: LatestFrame,
}

impl CaptureSession {
    pub fn new(source_id: Uuid, kind: CaptureKind) -> Self {
        Self {
            source_id,
            kind,
            state: CaptureState::Offline,
            latest: LatestFrame::default(),
        }
    }

    pub fn start(&mut self) -> Result<(), WindowsVideoError> {
        if self.state != CaptureState::Offline && self.state != CaptureState::Unavailable {
            return Err(WindowsVideoError::InvalidTransition);
        }
        self.state = CaptureState::Starting;
        Ok(())
    }

    pub fn publish_texture(&mut self, frame: Arc<GpuFrame>) -> Result<(), WindowsVideoError> {
        if self.state != CaptureState::Starting && self.state != CaptureState::Live {
            return Err(WindowsVideoError::InvalidTransition);
        }
        self.latest.publish(frame);
        self.state = CaptureState::Live;
        Ok(())
    }

    pub fn take_latest(&self) -> Option<Arc<GpuFrame>> {
        self.latest.take()
    }

    pub fn unavailable(&mut self, reason: impl Into<String>) -> WindowsVideoError {
        let _ = self.latest.take();
        self.state = CaptureState::Unavailable;
        WindowsVideoError::SourceUnavailable(reason.into())
    }

    pub fn stop(&mut self) {
        let _ = self.latest.take();
        self.state = CaptureState::Stopped;
    }

    pub fn state(&self) -> CaptureState {
        self.state
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryStep {
    StopCaptureAndMedia,
    ReleaseResources,
    CreateDevice,
    CreateDeviceManager,
    CreateFramePools,
    CreateDecoders,
    CreateSwapchains,
    RebindScene,
}

pub const RECOVERY_ORDER: [RecoveryStep; 8] = [
    RecoveryStep::StopCaptureAndMedia,
    RecoveryStep::ReleaseResources,
    RecoveryStep::CreateDevice,
    RecoveryStep::CreateDeviceManager,
    RecoveryStep::CreateFramePools,
    RecoveryStep::CreateDecoders,
    RecoveryStep::CreateSwapchains,
    RecoveryStep::RebindScene,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_source_drops_queued_texture() {
        let mut session = CaptureSession::new(Uuid::new_v4(), CaptureKind::WindowGraphicsCapture);
        session.start().unwrap();
        session
            .publish_texture(Arc::new(GpuFrame {
                source_id: session.source_id,
                sequence: 1,
                timestamp_ns: 0,
                native_texture: 1,
            }))
            .unwrap();
        session.unavailable("minimized");
        assert!(session.take_latest().is_none());
        assert_eq!(session.state(), CaptureState::Unavailable);
    }
}
