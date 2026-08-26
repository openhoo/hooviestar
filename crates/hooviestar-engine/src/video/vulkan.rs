//! Vulkan compositor for Linux portal capture.
//!
//! PipeWire delivers a bounded frame descriptor.  DMA-BUF fds are imported as
//! external Vulkan images; the source buffer token is returned to PipeWire only
//! after the submission fence has completed.  One offscreen scene is then
//! sampled by the Program and Preview swapchains.

use ash::{Device, Entry, Instance, khr, vk};
use parking_lot::RwLock;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
    XlibDisplayHandle, XlibWindowHandle,
};
use std::ptr::NonNull;
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    io::Cursor,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

use super::{
    MediaControlBus,
    image_cache::ImageCache,
    linux::{
        CaptureHandle, CapturedFrame, DRM_FORMAT_MOD_INVALID, FrameMemory, FrameMessage,
        PORTAL_LOST_REASON, PipeWirePortalLink,
    },
    linux_media::{LinuxMedia, MediaCommand, MediaNotice, MediaVideoFrame},
    text_raster::{TextAlignKey, TextCache, TextKey, parse_color as parse_text_color},
};
use crate::{
    audio::MediaAudioBus,
    engine::{DeviceRecoveryPhase, EngineEvent, NativeSurfaceKind, NativeSurfaces},
    project::{OutputConfig, ProjectV1, Scene, Source, Transform},
};

const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258;
const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;
const DRM_FORMAT_NV12: u32 = 0x3231_564e;

const ITEM_VERT: &[u8] = include_bytes!("shaders/item.vert.spv");
const ITEM_FRAG: &[u8] = include_bytes!("shaders/item.frag.spv");
const COMPOSITE_VERT: &[u8] = include_bytes!("shaders/composite.vert.spv");
const COMPOSITE_FRAG: &[u8] = include_bytes!("shaders/composite.frag.spv");
const MAX_SCENE_ITEMS: usize = 128;
/// Maximum consecutive compositor creation failures before the render thread
/// gives up permanently (mirrors the Windows render loop).
const MAX_RECOVERY_FAILURES: u32 = 8;
/// Minimum interval between re-import attempts of a failing portal/media
/// DMA-BUF source.
const IMPORT_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Bounded self-heal cadence for latched media: an Unsupported verdict or a
/// failed open retries through a fresh open session after this cooldown
/// instead of staying dead forever (Windows-parity), without re-attempting
/// on every tick.
const MEDIA_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

#[repr(C)]
#[derive(Clone, Copy)]
struct ItemPush {
    center: [f32; 2],
    half_extent: [f32; 2],
    cos_sin: [f32; 2],
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
    output_size: [f32; 2],
    opacity: f32,
    mode: u32,
}

struct SceneTarget {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    render_pass: vk::RenderPass,
    extent: vk::Extent2D,
}

struct SwapchainTarget {
    surface: vk::SurfaceKHR,
    loader: khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_pass: vk::RenderPass,
    rendered: Vec<vk::Semaphore>,
    extent: vk::Extent2D,
    available: vk::Semaphore,
    descriptor_set: vk::DescriptorSet,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
}

struct ImportedFrame {
    image: vk::Image,
    memories: Vec<vk::DeviceMemory>,
    view: vk::ImageView,
}

struct CachedExternalFrame {
    sequence: u64,
    texture: ImportedFrame,
    /// NV12 media imports use DISJOINT storage: ownership barriers must
    /// address PLANE_0/PLANE_1 instead of COLOR.
    disjoint: bool,
    /// Imported images start in UNDEFINED; the first acquire transitions
    /// UNDEFINED -> SHADER_READ_ONLY_OPTIMAL, later ones assume the
    /// steady-state GENERAL layout the release barrier restores.
    acquired: bool,
}

/// One external image taking part in the per-frame queue-family ownership
/// transfer, with the subresource ranges its barriers must address.
struct ExternalImageState {
    image: vk::Image,
    first_acquire: bool,
    ranges: [vk::ImageSubresourceRange; 2],
    range_count: usize,
}

/// Cheap identity probe stored next to each uploaded static texture so the
/// per-frame hit check compares stored fields by reference instead of
/// rebuilding cache-key strings and copying RGBA bitmaps every frame.
enum StaticIdentity {
    Image {
        path: PathBuf,
        fingerprint: (u128, u64),
    },
    Text(TextKey),
}

struct CachedStaticTexture {
    identity: StaticIdentity,
    texture: ImportedFrame,
}

/// What [`VulkanCompositor::synchronize_static_textures`] prepared for one
/// visible static source this frame. The RGBA buffer is materialized only
/// when an upload is actually required.
enum PreparedStatic {
    /// The uploaded texture already matches the source; nothing to do.
    Unchanged,
    Image {
        path: PathBuf,
        fingerprint: (u128, u64),
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
    },
    Text {
        key: TextKey,
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
    },
}

/// Borrowed view of one visible text item. Lets the hit check and the
/// `TextCache` retain pass compare keys without cloning the text/family
/// strings per frame.
#[derive(Clone, Copy)]
struct DesiredTextKey<'a> {
    text: &'a str,
    family: &'a str,
    size_bits: u32,
    weight: u16,
    color: [u8; 4],
    background: [u8; 4],
    align: TextAlignKey,
    width: u32,
    height: u32,
}

impl DesiredTextKey<'_> {
    fn matches(&self, key: &TextKey) -> bool {
        key.text == self.text
            && key.family == self.family
            && key.size_bits == self.size_bits
            && key.weight == self.weight
            && key.color == self.color
            && key.background == self.background
            && key.align == self.align
            && key.width == self.width
            && key.height == self.height
    }
}

/// CPU-side static-content caches owned by the render thread rather than the
/// compositor: decoded images, rasterized text, and failure records survive
/// swapchain/device recreations.
#[derive(Default)]
struct StaticCaches {
    image_cache: ImageCache,
    text_cache: TextCache,
    static_failures: HashMap<Uuid, String>,
}

/// Grouped NV12 media sampling state: the shared YCbCr conversion, its
/// sampler, the dedicated descriptor layout, and the media item pipeline
/// with its layout and descriptor sets. Destroyed as one unit via
/// [`MediaYcbcrPipeline::destroy`] from `VulkanCompositor::Drop`; the
/// `PartialVulkan` staging error path mirrors the same member order.
struct MediaYcbcrPipeline {
    sampler: vk::Sampler,
    conversion: vk::SamplerYcbcrConversion,
    sampler_layout: vk::DescriptorSetLayout,
    item_descriptors: Vec<vk::DescriptorSet>,
    item_pipeline: vk::Pipeline,
    item_pipeline_layout: vk::PipelineLayout,
}

impl MediaYcbcrPipeline {
    /// Destroys the group (descriptor sets are freed with the descriptor
    /// pool, which is destroyed separately).
    fn destroy(&self, device: &Device) {
        unsafe {
            device.destroy_pipeline(self.item_pipeline, None);
            device.destroy_pipeline_layout(self.item_pipeline_layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_sampler_ycbcr_conversion(self.conversion, None);
            device.destroy_descriptor_set_layout(self.sampler_layout, None);
        }
    }
}

struct VulkanCompositor {
    // Keeps the dynamically loaded Vulkan loader alive for every dispatch table.
    _entry: Entry,
    instance: Instance,
    surface_loader: khr::surface::Instance,
    external_memory_fd: khr::external_memory_fd::Device,
    physical: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    queue_family: u32,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    /// Shared per-frame submission fence. Only one fence is ever
    /// waited/reset/submitted (the render loop paces on it); it lives on the
    /// compositor rather than per swapchain target.
    frame_fence: vk::Fence,
    scene: SceneTarget,
    scene_sampler: vk::Sampler,
    item_sampler: vk::Sampler,
    descriptor_pool: vk::DescriptorPool,
    sampler_layout: vk::DescriptorSetLayout,
    item_descriptors: Vec<vk::DescriptorSet>,
    placeholder: ImportedFrame,
    static_textures: HashMap<Uuid, CachedStaticTexture>,
    media_textures: HashMap<Uuid, CachedExternalFrame>,
    portal_textures: HashMap<Uuid, CachedExternalFrame>,
    item_pipeline: vk::Pipeline,
    item_pipeline_layout: vk::PipelineLayout,
    /// NV12 media sampling must be fixed at pipeline-creation time through an
    /// immutable sampler (Vulkan spec, Sampler YCbCr Conversion): these are the
    /// shared conversion, its sampler, and the dedicated descriptor layout,
    /// pipeline, and sets exposing it. Binding 0 of `sampler_layout` stays
    /// non-YCbCr so portal/static/placeholder views keep using it
    /// (VUID-VkWriteDescriptorSet-01948: view and immutable-sampler
    /// conversions must match).
    media_ycbcr: MediaYcbcrPipeline,
    program: SwapchainTarget,
    preview: SwapchainTarget,
    output_width: u32,
    output_height: u32,
}

/// Owned core handles produced by [`PartialVulkan::build_core`] and consumed
/// by [`PartialVulkan::build_scene_pipeline`]. The staging slots in
/// `PartialVulkan` keep independent clones for the error path.
struct VulkanCore {
    instance: Instance,
    surface_loader: khr::surface::Instance,
    physical: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    program_surface: vk::SurfaceKHR,
    preview_surface: vk::SurfaceKHR,
}

/// Staging state for [`VulkanCompositor::create`]. Every fallible step registers
/// its result here immediately, so a failure anywhere in the chain can destroy
/// exactly what was created instead of leaking a half-initialized compositor.
struct PartialVulkan {
    entry: Option<Entry>,
    instance: Option<Instance>,
    surface_loader: Option<khr::surface::Instance>,
    external_memory_fd: Option<khr::external_memory_fd::Device>,
    physical: vk::PhysicalDevice,
    device: Option<Device>,
    queue: vk::Queue,
    queue_family: u32,
    command_pool: Option<vk::CommandPool>,
    command_buffer: Option<vk::CommandBuffer>,
    frame_fence: Option<vk::Fence>,
    scene: Option<SceneTarget>,
    scene_sampler: Option<vk::Sampler>,
    item_sampler: Option<vk::Sampler>,
    descriptor_pool: Option<vk::DescriptorPool>,
    sampler_layout: Option<vk::DescriptorSetLayout>,
    item_descriptors: Vec<vk::DescriptorSet>,
    placeholder: Option<ImportedFrame>,
    item_pipeline: Option<vk::Pipeline>,
    item_pipeline_layout: Option<vk::PipelineLayout>,
    program: Option<SwapchainTarget>,
    media_sampler: Option<vk::Sampler>,
    media_conversion: Option<vk::SamplerYcbcrConversion>,
    media_sampler_layout: Option<vk::DescriptorSetLayout>,
    media_item_descriptors: Vec<vk::DescriptorSet>,
    media_item_pipeline: Option<vk::Pipeline>,
    media_item_pipeline_layout: Option<vk::PipelineLayout>,
    preview: Option<SwapchainTarget>,
    // Surfaces are tracked until their swapchain target adopts them.
    program_surface: Option<vk::SurfaceKHR>,
    preview_surface: Option<vk::SurfaceKHR>,
}

impl PartialVulkan {
    fn new() -> Self {
        Self {
            entry: None,
            instance: None,
            surface_loader: None,
            external_memory_fd: None,
            physical: vk::PhysicalDevice::null(),
            device: None,
            queue: vk::Queue::null(),
            queue_family: 0,
            command_pool: None,
            command_buffer: None,
            frame_fence: None,
            scene: None,
            scene_sampler: None,
            item_sampler: None,
            descriptor_pool: None,
            sampler_layout: None,
            item_descriptors: Vec::new(),
            placeholder: None,
            item_pipeline: None,
            item_pipeline_layout: None,
            media_sampler: None,
            media_conversion: None,
            media_sampler_layout: None,
            media_item_descriptors: Vec::new(),
            media_item_pipeline: None,
            media_item_pipeline_layout: None,
            program: None,
            preview: None,
            program_surface: None,
            preview_surface: None,
        }
    }

    fn build(&mut self, surfaces: NativeSurfaces, width: u32, height: u32) -> Result<(), String> {
        let (display, program_window, preview_window) = raw_handles(surfaces)?;
        let core = self.build_core(display, program_window, preview_window)?;
        self.build_scene_pipeline(&core, surfaces, width, height)?;
        Ok(())
    }

    fn build_core(
        &mut self,
        display: RawDisplayHandle,
        program_window: RawWindowHandle,
        preview_window: RawWindowHandle,
    ) -> Result<VulkanCore, String> {
        let entry = unsafe { Entry::load() }.map_err(|error| format!("load Vulkan: {error}"))?;
        let app_name = CString::new("Hooviestar").map_err(|error| error.to_string())?;
        let app = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(&app_name)
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::API_VERSION_1_2);
        let extensions = ash_window::enumerate_required_extensions(display)
            .map_err(|error| format!("surface extensions: {error}"))?;
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(extensions);
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|error| format!("create Vulkan instance: {error}"))?;
        self.entry = Some(entry.clone());
        self.instance = Some(instance.clone());
        let surface_loader = khr::surface::Instance::new(&entry, &instance);
        self.surface_loader = Some(surface_loader.clone());
        let program_surface =
            unsafe { ash_window::create_surface(&entry, &instance, display, program_window, None) }
                .map_err(|error| format!("Programm-Oberfläche: {error}"))?;
        self.program_surface = Some(program_surface);
        let preview_surface =
            unsafe { ash_window::create_surface(&entry, &instance, display, preview_window, None) }
                .map_err(|error| format!("Preview-Oberfläche: {error}"))?;
        self.preview_surface = Some(preview_surface);
        let (physical, queue_family) =
            choose_device(&instance, &surface_loader, program_surface, preview_surface)?;
        self.physical = physical;
        self.queue_family = queue_family;
        let priority = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priority)];
        let extensions = [
            khr::swapchain::NAME.as_ptr(),
            khr::external_memory::NAME.as_ptr(),
            khr::external_memory_fd::NAME.as_ptr(),
            ash::ext::external_memory_dma_buf::NAME.as_ptr(),
            ash::ext::image_drm_format_modifier::NAME.as_ptr(),
            ash::ext::queue_family_foreign::NAME.as_ptr(),
        ];
        // The media path creates SamplerYcbcrConversions (core since Vulkan
        // 1.1, and the instance requests API 1.2); without this feature flag
        // every NV12 media import fails at conversion creation. The feature
        // lives in its own chained struct, not in PhysicalDeviceFeatures.
        let mut ycbcr_features = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default()
            .sampler_ycbcr_conversion(true);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .push_next(&mut ycbcr_features)
            .enabled_extension_names(&extensions);
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(|error| format!("create Vulkan device: {error}"))?;
        self.device = Some(device.clone());
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        self.queue = queue;
        let external_memory_fd = khr::external_memory_fd::Device::new(&instance, &device);
        self.external_memory_fd = Some(external_memory_fd.clone());
        let command_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .map_err(|error| format!("create command pool: {error}"))?;
        self.command_pool = Some(command_pool);
        let command_buffer = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|error| format!("allocate command buffer: {error}"))?[0];
        self.command_buffer = Some(command_buffer);
        // One shared, initially-signaled frame fence: render() waits on it
        // before acquiring, so the first frame must not block.
        let frame_fence = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )
        }
        .map_err(|error| format!("frame fence: {error}"))?;
        self.frame_fence = Some(frame_fence);
        Ok(VulkanCore {
            instance,
            surface_loader,
            physical,
            device,
            queue,
            command_pool,
            program_surface,
            preview_surface,
        })
    }

    fn build_scene_pipeline(
        &mut self,
        core: &VulkanCore,
        surfaces: NativeSurfaces,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let instance = &core.instance;
        let device = &core.device;
        let physical = core.physical;
        let queue = core.queue;
        let command_pool = core.command_pool;
        let surface_loader = &core.surface_loader;
        let program_surface = core.program_surface;
        let preview_surface = core.preview_surface;
        let placeholder = upload_rgba_texture(
            instance,
            device,
            physical,
            queue,
            command_pool,
            1,
            1,
            &[0, 0, 0, 0],
        )?;
        self.placeholder = Some(placeholder);
        let sampler_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                ]),
                None,
            )
        }
        .map_err(|error| format!("descriptor layout: {error}"))?;
        self.sampler_layout = Some(sampler_layout);
        // Vulkan normatively requires YCbCr conversion to be fixed at
        // pipeline-creation time through a combined image sampler with an
        // immutable sampler in the descriptor-set layout. Create the shared
        // NV12 conversion + sampler once and expose them through a dedicated
        // media layout; `sampler_layout` binding 0 stays non-YCbCr for
        // portal/static/placeholder views (VUID-VkWriteDescriptorSet-01948).
        let media_conversion = unsafe {
            device.create_sampler_ycbcr_conversion(
                &vk::SamplerYcbcrConversionCreateInfo::default()
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .ycbcr_model(vk::SamplerYcbcrModelConversion::YCBCR_709)
                    .ycbcr_range(vk::SamplerYcbcrRange::ITU_NARROW)
                    .components(vk::ComponentMapping::default())
                    .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
                    .y_chroma_offset(vk::ChromaLocation::MIDPOINT)
                    .chroma_filter(vk::Filter::NEAREST),
                None,
            )
        }
        .map_err(|error| format!("create shared media YCbCr conversion: {error}"))?;
        self.media_conversion = Some(media_conversion);
        let mut media_conversion_info =
            vk::SamplerYcbcrConversionInfo::default().conversion(media_conversion);
        let media_sampler = unsafe {
            device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .max_lod(1.0)
                    .push_next(&mut media_conversion_info),
                None,
            )
        }
        .map_err(|error| format!("create shared media YCbCr sampler: {error}"))?;
        self.media_sampler = Some(media_sampler);
        let media_sampler_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                        .immutable_samplers(std::slice::from_ref(&media_sampler)),
                ]),
                None,
            )
        }
        .map_err(|error| format!("media descriptor layout: {error}"))?;
        self.media_sampler_layout = Some(media_sampler_layout);
        let descriptor_pool = unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets((2 * MAX_SCENE_ITEMS + 1) as u32)
                    .pool_sizes(&[vk::DescriptorPoolSize::default()
                        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count((2 * MAX_SCENE_ITEMS + 1) as u32)]),
                None,
            )
        }
        .map_err(|error| format!("descriptor pool: {error}"))?;
        self.descriptor_pool = Some(descriptor_pool);
        let descriptor_layouts = vec![sampler_layout; MAX_SCENE_ITEMS + 1];
        let sets = unsafe {
            device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(descriptor_pool)
                    .set_layouts(&descriptor_layouts),
            )
        }
        .map_err(|error| format!("descriptor sets: {error}"))?;
        let scene_descriptor = sets[0];
        self.item_descriptors = sets[1..].to_vec();
        let media_descriptor_layouts = vec![media_sampler_layout; MAX_SCENE_ITEMS];
        let media_sets = unsafe {
            device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(descriptor_pool)
                    .set_layouts(&media_descriptor_layouts),
            )
        }
        .map_err(|error| format!("media descriptor sets: {error}"))?;
        self.media_item_descriptors = media_sets;
        let scene_sampler = create_sampler(device)?;
        self.scene_sampler = Some(scene_sampler);
        let item_sampler = create_sampler(device)?;
        self.item_sampler = Some(item_sampler);
        let scene_render_pass = create_render_pass(device, vk::Format::R16G16B16A16_SFLOAT)?;
        let scene =
            create_scene_target(instance, device, physical, scene_render_pass, width, height)?;
        update_descriptor(
            device,
            scene_descriptor,
            scene.view,
            scene_sampler,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        self.scene = Some(scene);
        let item_pipeline_layout = create_item_pipeline_layout(device, sampler_layout)?;
        self.item_pipeline_layout = Some(item_pipeline_layout);
        let item_pipeline = create_item_pipeline(device, scene_render_pass, item_pipeline_layout)?;
        self.item_pipeline = Some(item_pipeline);
        let media_item_pipeline_layout = create_item_pipeline_layout(device, media_sampler_layout)?;
        self.media_item_pipeline_layout = Some(media_item_pipeline_layout);
        let media_item_pipeline =
            create_item_pipeline(device, scene_render_pass, media_item_pipeline_layout)?;
        self.media_item_pipeline = Some(media_item_pipeline);
        let program = create_swapchain(
            instance,
            device,
            physical,
            surface_loader,
            program_surface,
            surfaces.program_width.max(1),
            surfaces.program_height.max(1),
            sampler_layout,
            scene_descriptor,
        )?;
        self.program = Some(program);
        // The swapchain target adopted the surface; drop our tracking entry.
        self.program_surface = None;
        let preview = create_swapchain(
            instance,
            device,
            physical,
            surface_loader,
            preview_surface,
            surfaces.preview_width.max(1),
            surfaces.preview_height.max(1),
            sampler_layout,
            scene_descriptor,
        )?;
        self.preview = Some(preview);
        self.preview_surface = None;
        Ok(())
    }

    /// Moves the fully built state into a compositor. Only valid after
    /// [`PartialVulkan::build`] returned `Ok`.
    fn into_compositor(mut self, width: u32, height: u32) -> VulkanCompositor {
        VulkanCompositor {
            _entry: self.entry.take().expect("Vulkan entry built"),
            instance: self.instance.take().expect("instance built"),
            surface_loader: self.surface_loader.take().expect("surface loader built"),
            external_memory_fd: self
                .external_memory_fd
                .take()
                .expect("external memory fd built"),
            physical: self.physical,
            device: self.device.take().expect("device built"),
            queue: self.queue,
            queue_family: self.queue_family,
            command_pool: self.command_pool.take().expect("command pool built"),
            command_buffer: self.command_buffer.take().expect("command buffer built"),
            frame_fence: self.frame_fence.take().expect("frame fence built"),
            scene: self.scene.take().expect("scene target built"),
            scene_sampler: self.scene_sampler.take().expect("scene sampler built"),
            item_sampler: self.item_sampler.take().expect("item sampler built"),
            descriptor_pool: self.descriptor_pool.take().expect("descriptor pool built"),
            sampler_layout: self.sampler_layout.take().expect("sampler layout built"),
            item_descriptors: std::mem::take(&mut self.item_descriptors),
            placeholder: self.placeholder.take().expect("placeholder built"),
            static_textures: HashMap::new(),
            media_textures: HashMap::new(),
            portal_textures: HashMap::new(),
            item_pipeline: self.item_pipeline.take().expect("item pipeline built"),
            item_pipeline_layout: self
                .item_pipeline_layout
                .take()
                .expect("item pipeline layout built"),
            media_ycbcr: MediaYcbcrPipeline {
                sampler: self.media_sampler.take().expect("media sampler built"),
                conversion: self
                    .media_conversion
                    .take()
                    .expect("media YCbCr conversion built"),
                sampler_layout: self
                    .media_sampler_layout
                    .take()
                    .expect("media sampler layout built"),
                item_descriptors: std::mem::take(&mut self.media_item_descriptors),
                item_pipeline: self
                    .media_item_pipeline
                    .take()
                    .expect("media item pipeline built"),
                item_pipeline_layout: self
                    .media_item_pipeline_layout
                    .take()
                    .expect("media item pipeline layout built"),
            },
            program: self.program.take().expect("program swapchain built"),
            preview: self.preview.take().expect("preview swapchain built"),
            output_width: width,
            output_height: height,
        }
    }

    /// Destroys everything created so far, mirroring `Drop for VulkanCompositor`.
    /// Safe on partially built state: pieces that were never created are `None`.
    unsafe fn destroy_created(&mut self) {
        unsafe {
            if let Some(device) = self.device.take() {
                let _ = device.device_wait_idle();
                let surface_loader = self.surface_loader.clone();
                if let Some(surface_loader) = surface_loader.as_ref() {
                    let mut program = self.program.take();
                    let mut preview = self.preview.take();
                    if let Some(target) = program.as_mut() {
                        destroy_target(&device, surface_loader, target);
                    }
                    if let Some(target) = preview.as_mut() {
                        destroy_target(&device, surface_loader, target);
                    }
                }
                if let Some(placeholder) = self.placeholder.take() {
                    destroy_imported(&device, placeholder);
                }
                if let Some(pipeline) = self.item_pipeline.take() {
                    device.destroy_pipeline(pipeline, None);
                }
                if let Some(layout) = self.item_pipeline_layout.take() {
                    device.destroy_pipeline_layout(layout, None);
                }
                // Mirror of `MediaYcbcrPipeline::destroy`: the media group's
                // members, in the same order, on partially built state.
                if let Some(pipeline) = self.media_item_pipeline.take() {
                    device.destroy_pipeline(pipeline, None);
                }
                if let Some(layout) = self.media_item_pipeline_layout.take() {
                    device.destroy_pipeline_layout(layout, None);
                }
                if let Some(sampler) = self.media_sampler.take() {
                    device.destroy_sampler(sampler, None);
                }
                if let Some(conversion) = self.media_conversion.take() {
                    device.destroy_sampler_ycbcr_conversion(conversion, None);
                }
                if let Some(layout) = self.media_sampler_layout.take() {
                    device.destroy_descriptor_set_layout(layout, None);
                }
                if let Some(mut scene) = self.scene.take() {
                    destroy_scene(&device, &mut scene);
                }
                if let Some(sampler) = self.scene_sampler.take() {
                    device.destroy_sampler(sampler, None);
                }
                if let Some(sampler) = self.item_sampler.take() {
                    device.destroy_sampler(sampler, None);
                }
                if let Some(pool) = self.descriptor_pool.take() {
                    device.destroy_descriptor_pool(pool, None);
                }
                if let Some(layout) = self.sampler_layout.take() {
                    device.destroy_descriptor_set_layout(layout, None);
                }
                if let Some(pool) = self.command_pool.take() {
                    device.destroy_command_pool(pool, None);
                }
                if let Some(fence) = self.frame_fence.take() {
                    device.destroy_fence(fence, None);
                }
                device.destroy_device(None);
            }
            // Swapchain targets adopt their surfaces on success; any still
            // tracked surface was created but never adopted and needs an
            // explicit destroy here.
            if let Some(surface_loader) = self.surface_loader.take() {
                for surface in [self.program_surface.take(), self.preview_surface.take()]
                    .into_iter()
                    .flatten()
                {
                    surface_loader.destroy_surface(surface, None);
                }
            }
            if let Some(instance) = self.instance.take() {
                instance.destroy_instance(None);
            }
        }
    }
}

impl VulkanCompositor {
    fn create(surfaces: NativeSurfaces, width: u32, height: u32) -> Result<Self, String> {
        let mut partial = PartialVulkan::new();
        match partial.build(surfaces, width, height) {
            Ok(()) => Ok(partial.into_compositor(width, height)),
            Err(error) => {
                // Roll back every resource created before the failing step
                // instead of leaking the partially initialized compositor.
                unsafe { partial.destroy_created() };
                Err(error)
            }
        }
    }

    /// Emits per-source portal import transitions onto the engine event
    /// channel: `SourceUnavailable` when a source first enters
    /// `import_failures`, `SourceAvailable` when a successful re-import
    /// removes the entry. Stale eviction removes entries silently. The
    /// failure map lives on the render thread so backoff streaks survive
    /// compositor recreations.
    fn synchronize_portal_textures(
        &mut self,
        frames: &HashMap<Uuid, CapturedFrame>,
        import_failures: &mut HashMap<Uuid, Instant>,
        events: &std::sync::mpsc::Sender<EngineEvent>,
    ) {
        let stale = self
            .portal_textures
            .keys()
            .copied()
            .filter(|source_id| !frames.contains_key(source_id))
            .collect::<Vec<_>>();
        for source_id in stale {
            if let Some(cached) = self.portal_textures.remove(&source_id) {
                destroy_imported(&self.device, cached.texture);
            }
            import_failures.remove(&source_id);
        }
        for (source_id, frame) in frames {
            if self
                .portal_textures
                .get(source_id)
                .is_some_and(|cached| cached.sequence == frame.sequence)
            {
                continue;
            }
            // A source whose last import failed is retried at a bounded
            // interval instead of every frame; until then it keeps rendering
            // through the placeholder path.
            if import_failures
                .get(source_id)
                .is_some_and(|last| last.elapsed() < IMPORT_RETRY_INTERVAL)
            {
                continue;
            }
            match import_frame(
                &self.instance,
                &self.device,
                &self.external_memory_fd,
                self.physical,
                frame,
            ) {
                Ok(texture) => {
                    if import_failures.remove(source_id).is_some() {
                        let _ = events.send(EngineEvent::SourceAvailable {
                            source_id: *source_id,
                        });
                    }
                    if let Some(previous) = self.portal_textures.insert(
                        *source_id,
                        CachedExternalFrame {
                            sequence: frame.sequence,
                            texture,
                            // Packed RGB portal DMA-BUF: single plane, COLOR aspect.
                            disjoint: false,
                            acquired: false,
                        },
                    ) {
                        destroy_imported(&self.device, previous.texture);
                    }
                }
                Err(_) => {
                    // A single failing import must never abort the frame
                    // before acquire/submit/present — that would freeze both
                    // swapchains while the capture keeps delivering. Evict
                    // the source so its items render through the placeholder
                    // descriptor and back off re-import attempts.
                    if import_failures.insert(*source_id, Instant::now()).is_none() {
                        let _ = events.send(EngineEvent::SourceUnavailable {
                            source_id: *source_id,
                            reason: "Vulkan-DMA-BUF-Import fehlgeschlagen".into(),
                        });
                    }
                    self.remove_external_texture(*source_id);
                }
            }
        }
    }
    fn synchronize_static_textures(
        &mut self,
        caches: &mut StaticCaches,
        project: &ProjectV1,
        events: &std::sync::mpsc::Sender<EngineEvent>,
    ) {
        let Some(scene) = project
            .scenes
            .iter()
            .find(|scene| scene.id == project.active_scene_id)
        else {
            return;
        };
        let mut desired = HashSet::new();
        let mut desired_text = Vec::new();
        let mut desired_images = HashSet::new();

        for item in scene.items.iter().filter(|item| item.visible) {
            let Some(source) = project
                .sources
                .iter()
                .find(|source| source.id() == item.source_id)
            else {
                continue;
            };
            let prepared = match source {
                Source::Image { id, path, .. } => {
                    desired.insert(*id);
                    caches
                        .image_cache
                        .get_or_decode(path)
                        .inspect(|decoded| {
                            desired_images.insert(decoded.path.clone());
                        })
                        .map(|decoded| {
                            // Cache-hit fast path: compare the stored
                            // identity (canonical path + file fingerprint)
                            // before building any key string or copying the
                            // decoded bitmap.
                            let unchanged = self.static_textures.get(id).is_some_and(|cached| {
                                matches!(
                                    &cached.identity,
                                    StaticIdentity::Image {
                                        path: identity_path,
                                        fingerprint: identity_fingerprint,
                                    } if identity_path == &decoded.path
                                        && *identity_fingerprint
                                            == decoded.fingerprint
                                )
                            });
                            if unchanged {
                                (*id, PreparedStatic::Unchanged)
                            } else {
                                (
                                    *id,
                                    PreparedStatic::Image {
                                        path: decoded.path.clone(),
                                        fingerprint: decoded.fingerprint,
                                        width: decoded.width,
                                        height: decoded.height,
                                        rgba8: decoded.rgba8.clone(),
                                    },
                                )
                            }
                        })
                }
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
                } => {
                    desired.insert(*id);
                    let wanted = DesiredTextKey {
                        text: text.as_str(),
                        family: font_family.as_str(),
                        size_bits: font_size_px.to_bits(),
                        weight: *font_weight,
                        color: parse_text_color(color),
                        background: parse_text_color(background_color),
                        align: align.clone().into(),
                        width: item.transform.width.round().max(1.0) as u32,
                        height: item.transform.height.round().max(1.0) as u32,
                    };
                    desired_text.push(wanted);
                    // Cache-hit fast path: scalar fields first, string
                    // contents by borrow. No TextKey construction, no
                    // rasterization lookup, no bitmap copy.
                    let unchanged = self.static_textures.get(id).is_some_and(|cached| {
                        matches!(
                            &cached.identity,
                            StaticIdentity::Text(key) if wanted.matches(key)
                        )
                    });
                    if unchanged {
                        Ok((*id, PreparedStatic::Unchanged))
                    } else {
                        let key = TextKey {
                            text: text.clone(),
                            family: font_family.clone(),
                            size_bits: wanted.size_bits,
                            weight: wanted.weight,
                            color: wanted.color,
                            background: wanted.background,
                            align: wanted.align,
                            width: wanted.width,
                            height: wanted.height,
                        };
                        caches.text_cache.rasterize(key.clone()).map(|raster| {
                            (
                                *id,
                                PreparedStatic::Text {
                                    key,
                                    width: raster.width,
                                    height: raster.height,
                                    rgba8: raster.rgba8.clone(),
                                },
                            )
                        })
                    }
                }
                _ => continue,
            };
            let result =
                prepared
                    .map_err(|error| error.to_string())
                    .and_then(|(source_id, prepared)| {
                        let (identity, width, height, rgba8) = match prepared {
                            PreparedStatic::Unchanged => return Ok((source_id, false)),
                            PreparedStatic::Image {
                                path,
                                fingerprint,
                                width,
                                height,
                                rgba8,
                            } => (
                                StaticIdentity::Image { path, fingerprint },
                                width,
                                height,
                                rgba8,
                            ),
                            PreparedStatic::Text {
                                key,
                                width,
                                height,
                                rgba8,
                            } => (StaticIdentity::Text(key), width, height, rgba8),
                        };
                        upload_rgba_texture(
                            &self.instance,
                            &self.device,
                            self.physical,
                            self.queue,
                            self.command_pool,
                            width,
                            height,
                            &rgba8,
                        )
                        .map(|texture| {
                            if let Some(previous) = self
                                .static_textures
                                .insert(source_id, CachedStaticTexture { identity, texture })
                            {
                                destroy_imported(&self.device, previous.texture);
                            }
                            (source_id, true)
                        })
                    });
            match result {
                Ok((source_id, changed)) => {
                    let recovered = caches.static_failures.remove(&source_id).is_some();
                    if changed || recovered {
                        let _ = events.send(EngineEvent::SourceAvailable { source_id });
                    }
                }
                Err(reason) => {
                    let source_id = item.source_id;
                    if caches.static_failures.get(&source_id) != Some(&reason) {
                        caches.static_failures.insert(source_id, reason.clone());
                        let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                    }
                    if let Some(previous) = self.static_textures.remove(&source_id) {
                        destroy_imported(&self.device, previous.texture);
                    }
                }
            }
        }
        caches
            .text_cache
            .retain(|key| desired_text.iter().any(|wanted| wanted.matches(key)));
        caches
            .image_cache
            .retain(|path| desired_images.contains(path));

        let stale = self
            .static_textures
            .keys()
            .copied()
            .filter(|source_id| !desired.contains(source_id))
            .collect::<Vec<_>>();
        for source_id in stale {
            if let Some(cached) = self.static_textures.remove(&source_id) {
                destroy_imported(&self.device, cached.texture);
            }
            caches.static_failures.remove(&source_id);
        }
    }
    /// See [`VulkanCompositor::synchronize_portal_textures`] for the
    /// transition-emission contract and the render-thread-owned failure map.
    fn synchronize_media_textures(
        &mut self,
        frames: &HashMap<Uuid, MediaVideoFrame>,
        import_failures: &mut HashMap<Uuid, Instant>,
        events: &std::sync::mpsc::Sender<EngineEvent>,
    ) {
        let stale = self
            .media_textures
            .keys()
            .copied()
            .filter(|source_id| !frames.contains_key(source_id))
            .collect::<Vec<_>>();
        for source_id in stale {
            if let Some(cached) = self.media_textures.remove(&source_id) {
                destroy_imported(&self.device, cached.texture);
            }
            import_failures.remove(&source_id);
        }
        for (source_id, frame) in frames {
            if self
                .media_textures
                .get(source_id)
                .is_some_and(|cached| cached.sequence == frame.sequence)
            {
                continue;
            }
            // Bounded retry after a failed import (see
            // synchronize_portal_textures).
            if import_failures
                .get(source_id)
                .is_some_and(|last| last.elapsed() < IMPORT_RETRY_INTERVAL)
            {
                continue;
            }
            match import_media_frame(
                &self.instance,
                &self.device,
                &self.external_memory_fd,
                self.physical,
                self.media_ycbcr.conversion,
                frame,
            ) {
                Ok(texture) => {
                    if import_failures.remove(source_id).is_some() {
                        let _ = events.send(EngineEvent::SourceAvailable {
                            source_id: *source_id,
                        });
                    }
                    if let Some(previous) = self.media_textures.insert(
                        *source_id,
                        CachedExternalFrame {
                            sequence: frame.sequence,
                            texture,
                            // import_media_frame only accepts NV12, created DISJOINT.
                            disjoint: true,
                            acquired: false,
                        },
                    ) {
                        destroy_imported(&self.device, previous.texture);
                    }
                }
                Err(_) => {
                    // Never abort the frame on a per-source import failure;
                    // evict and render the placeholder instead (see
                    // synchronize_portal_textures).
                    if import_failures.insert(*source_id, Instant::now()).is_none() {
                        let _ = events.send(EngineEvent::SourceUnavailable {
                            source_id: *source_id,
                            reason: "Vulkan-DMA-BUF-Import fehlgeschlagen".into(),
                        });
                    }
                    self.remove_external_texture(*source_id);
                }
            }
        }
    }

    fn remove_external_texture(&mut self, source_id: Uuid) {
        if let Some(cached) = self.portal_textures.remove(&source_id) {
            destroy_imported(&self.device, cached.texture);
        }
        if let Some(cached) = self.media_textures.remove(&source_id) {
            destroy_imported(&self.device, cached.texture);
        }
    }

    /// Rebuilds the scene render target when the project output size changed.
    /// Device, pipelines, immutable-sampler architecture, descriptor pool, and
    /// every texture cache stay untouched; only the color attachment rotates
    /// and the shared scene descriptor is repointed at its new view. The
    /// replacement is created before the old attachments are freed, so a
    /// failure leaves the current target fully functional.
    fn recreate_scene_target(&mut self, width: u32, height: u32) -> Result<(), String> {
        if self.output_width == width && self.output_height == height {
            return Ok(());
        }
        let next = create_scene_target(
            &self.instance,
            &self.device,
            self.physical,
            self.scene.render_pass,
            width,
            height,
        )?;
        update_descriptor(
            &self.device,
            self.program.descriptor_set,
            next.view,
            self.scene_sampler,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        destroy_scene_attachments(&self.device, &mut self.scene);
        self.scene = next;
        self.output_width = width;
        self.output_height = height;
        Ok(())
    }

    /// Rebuilds the program/preview swapchain targets in place. Pass
    /// `force = true` for swapchain-scoped errors (OUT_OF_DATE/SUBOPTIMAL)
    /// where the extent may be unchanged but the swapchain is stale;
    /// otherwise only mismatched extents are recreated. Device, pipelines,
    /// descriptor pool, and every texture cache survive.
    fn recreate_swapchains(
        &mut self,
        program: (u32, u32),
        preview: (u32, u32),
        force: bool,
    ) -> Result<(), String> {
        let program_changed = force
            || self.program.extent.width != program.0
            || self.program.extent.height != program.1;
        let preview_changed = force
            || self.preview.extent.width != preview.0
            || self.preview.extent.height != preview.1;
        if !program_changed && !preview_changed {
            return Ok(());
        }
        // Mirror Drop: drain the presentation queue before retiring
        // swapchain resources.
        unsafe {
            let _ = self.device.device_wait_idle();
        }
        if program_changed {
            recreate_swapchain(
                &self.instance,
                &self.device,
                self.physical,
                &self.surface_loader,
                self.sampler_layout,
                &mut self.program,
                program.0,
                program.1,
            )?;
        }
        if preview_changed {
            recreate_swapchain(
                &self.instance,
                &self.device,
                self.physical,
                &self.surface_loader,
                self.sampler_layout,
                &mut self.preview,
                preview.0,
                preview.1,
            )?;
        }
        Ok(())
    }

    fn render(
        &mut self,
        caches: &mut StaticCaches,
        project: &ProjectV1,
        frames: &mut HashMap<Uuid, CapturedFrame>,
        media_frames: &HashMap<Uuid, MediaVideoFrame>,
        import_failures: &mut HashMap<Uuid, Instant>,
        events: &std::sync::mpsc::Sender<EngineEvent>,
    ) -> Result<(), RenderError> {
        let scene = project
            .scenes
            .iter()
            .find(|scene| scene.id == project.active_scene_id)
            .ok_or_else(|| RenderError::Import {
                reason: "active scene is missing".into(),
            })?;
        let (program_index, preview_index);
        unsafe {
            self.device
                .wait_for_fences(&[self.frame_fence], true, u64::MAX)
                .map_err(RenderError::Vk)?;
        }
        self.synchronize_portal_textures(frames, import_failures, events);
        self.synchronize_media_textures(media_frames, import_failures, events);
        self.synchronize_static_textures(caches, project, events);
        let (external_images, external_count) = self.collect_external_images(scene);
        unsafe {
            // Acquire both swapchain images before resetting the fence: a
            // timeout skip then leaves the fence signaled and no semaphores
            // pending, so the next frame stays in sync.
            program_index = acquire(&self.program)?;
            preview_index = match acquire(&self.preview) {
                Ok(index) => index,
                Err(RenderError::AcquireTimeout) => {
                    return self.handle_acquire_timeout(program_index);
                }
                Err(other) => return Err(other),
            };
            self.device
                .reset_fences(&[self.frame_fence])
                .map_err(RenderError::Vk)?;
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(RenderError::Vk)?;
            self.device
                .begin_command_buffer(self.command_buffer, &vk::CommandBufferBeginInfo::default())
                .map_err(RenderError::Vk)?;
            self.record_item_pass(scene, project, &external_images, external_count);
            self.record_composite_passes(program_index, preview_index);
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(RenderError::Vk)?;
            let waits = [self.program.available, self.preview.available];
            let stages = [
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            ];
            let signals = [
                self.program.rendered[program_index as usize],
                self.preview.rendered[preview_index as usize],
            ];
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(&waits)
                        .wait_dst_stage_mask(&stages)
                        .command_buffers(&[self.command_buffer])
                        .signal_semaphores(&signals)],
                    self.frame_fence,
                )
                .map_err(RenderError::Vk)?;
            // The first-acquire barriers live only in this command
            // buffer; commit the flags only after the submit succeeded so
            // an acquire timeout or earlier failure makes the next frame
            // re-issue the UNDEFINED -> SHADER_READ_ONLY transition.
            for external in &external_images[..external_count] {
                if !external.first_acquire {
                    continue;
                }
                for cached in self
                    .portal_textures
                    .values_mut()
                    .chain(self.media_textures.values_mut())
                {
                    if cached.texture.image == external.image {
                        cached.acquired = true;
                    }
                }
            }
        }
        present(&self.program, self.queue, program_index)?;
        present(&self.preview, self.queue, preview_index)?;
        unsafe {
            self.device
                .wait_for_fences(&[self.frame_fence], true, u64::MAX)
                .map_err(RenderError::Vk)?;
        }
        Ok(())
    }
    /// Pure-move extraction of the external-image ownership-range collection
    /// from `render`.
    fn collect_external_images(
        &mut self,
        scene: &Scene,
    ) -> ([ExternalImageState; MAX_SCENE_ITEMS], usize) {
        let mut external_images: [ExternalImageState; MAX_SCENE_ITEMS] =
            std::array::from_fn(|_| ExternalImageState {
                image: vk::Image::null(),
                first_acquire: false,
                ranges: [vk::ImageSubresourceRange::default(); 2],
                range_count: 0,
            });
        let mut external_count = 0usize;
        for item in scene.items.iter().filter(|item| item.visible) {
            let cached = if let Some(cached) = self.portal_textures.get_mut(&item.source_id) {
                Some(cached)
            } else {
                self.media_textures.get_mut(&item.source_id)
            };
            let Some(cached) = cached else {
                continue;
            };
            let image = cached.texture.image;
            if external_count < MAX_SCENE_ITEMS
                && !external_images[..external_count]
                    .iter()
                    .any(|external| external.image == image)
            {
                // DISJOINT multi-planar images reject COLOR-aspect barriers;
                // each plane needs its own ownership transfer.
                let (ranges, range_count) = if cached.disjoint {
                    (
                        [
                            plane_range(vk::ImageAspectFlags::PLANE_0),
                            plane_range(vk::ImageAspectFlags::PLANE_1),
                        ],
                        2,
                    )
                } else {
                    ([color_range(), vk::ImageSubresourceRange::default()], 1)
                };
                external_images[external_count] = ExternalImageState {
                    image,
                    first_acquire: !cached.acquired,
                    ranges,
                    range_count,
                };
                external_count += 1;
            }
        }
        (external_images, external_count)
    }

    /// Pure-move extraction of the preview `AcquireTimeout` arm from
    /// `render`: the program image was acquired but will not be rendered
    /// this frame; consume its acquire semaphore with an empty submit and
    /// present the untouched image so the next acquire stays synchronized.
    fn handle_acquire_timeout(&mut self, program_index: u32) -> Result<(), RenderError> {
        unsafe {
            self.device
                .reset_fences(&[self.frame_fence])
                .map_err(RenderError::Vk)?;
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(std::slice::from_ref(&self.program.available))
                        .wait_dst_stage_mask(&[vk::PipelineStageFlags::TOP_OF_PIPE])
                        .signal_semaphores(&[self.program.rendered[program_index as usize]])],
                    self.frame_fence,
                )
                .map_err(RenderError::Vk)?;
        }
        present(&self.program, self.queue, program_index)?;
        Ok(())
    }

    /// Pure-move extraction of the scene-pass recording from `render`:
    /// ownership acquire barriers, the scene render pass with all visible
    /// items, and the ownership release barriers. Runs between
    /// `begin_command_buffer` and the composite passes.
    fn record_item_pass(
        &self,
        scene: &Scene,
        project: &ProjectV1,
        external_images: &[ExternalImageState; MAX_SCENE_ITEMS],
        external_count: usize,
    ) {
        unsafe {
            let mut acquire_barriers = [vk::ImageMemoryBarrier::default(); MAX_SCENE_ITEMS * 2];
            let mut acquire_count = 0usize;
            for external in &external_images[..external_count] {
                let old_layout = if external.first_acquire {
                    vk::ImageLayout::UNDEFINED
                } else {
                    vk::ImageLayout::GENERAL
                };
                for range in &external.ranges[..external.range_count] {
                    acquire_barriers[acquire_count] = vk::ImageMemoryBarrier::default()
                        .old_layout(old_layout)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                        .dst_queue_family_index(self.queue_family)
                        .image(external.image)
                        .subresource_range(*range);
                    acquire_count += 1;
                }
            }
            if acquire_count > 0 {
                self.device.cmd_pipeline_barrier(
                    self.command_buffer,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::BY_REGION,
                    &[],
                    &[],
                    &acquire_barriers[..acquire_count],
                );
            }
            let scene_clear = [parse_color(&project.output.background)];
            self.device.cmd_begin_render_pass(
                self.command_buffer,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.scene.render_pass)
                    .framebuffer(self.scene.framebuffer)
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: self.scene.extent,
                    })
                    .clear_values(&[vk::ClearValue {
                        color: vk::ClearColorValue {
                            float32: scene_clear[0],
                        },
                    }]),
                vk::SubpassContents::INLINE,
            );
            self.device.cmd_set_viewport(
                self.command_buffer,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: self.output_width as f32,
                    height: self.output_height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(
                self.command_buffer,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: self.scene.extent,
                }],
            );
            let mut media_bound = false;
            self.device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.item_pipeline,
            );
            for (draw_index, item) in scene
                .items
                .iter()
                .filter(|item| item.visible)
                .take(MAX_SCENE_ITEMS)
                .enumerate()
            {
                let mut mode = 0u32;
                let mut media_draw = false;
                if let Some(cached) = self.portal_textures.get(&item.source_id) {
                    update_descriptor(
                        &self.device,
                        self.item_descriptors[draw_index],
                        cached.texture.view,
                        self.item_sampler,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                } else if let Some(cached) = self.media_textures.get(&item.source_id) {
                    // The view carries the shared SamplerYcbcrConversionInfo;
                    // the write's sampler field is ignored because the
                    // binding's immutable sampler fixes the conversion at
                    // pipeline-creation time.
                    update_descriptor(
                        &self.device,
                        self.media_ycbcr.item_descriptors[draw_index],
                        cached.texture.view,
                        vk::Sampler::null(),
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    media_draw = true;
                } else if let Some(cached) = self.static_textures.get(&item.source_id) {
                    update_descriptor(
                        &self.device,
                        self.item_descriptors[draw_index],
                        cached.texture.view,
                        self.item_sampler,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                } else {
                    update_descriptor(
                        &self.device,
                        self.item_descriptors[draw_index],
                        self.placeholder.view,
                        self.item_sampler,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    mode = 1;
                }
                if media_draw != media_bound {
                    self.device.cmd_bind_pipeline(
                        self.command_buffer,
                        vk::PipelineBindPoint::GRAPHICS,
                        if media_draw {
                            self.media_ycbcr.item_pipeline
                        } else {
                            self.item_pipeline
                        },
                    );
                    media_bound = media_draw;
                }
                let (item_descriptor, draw_pipeline_layout) = if media_draw {
                    (
                        self.media_ycbcr.item_descriptors[draw_index],
                        self.media_ycbcr.item_pipeline_layout,
                    )
                } else {
                    (self.item_descriptors[draw_index], self.item_pipeline_layout)
                };
                self.device.cmd_bind_descriptor_sets(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    draw_pipeline_layout,
                    0,
                    &[item_descriptor],
                    &[],
                );
                let push = item_push(item.transform, self.output_width, self.output_height, mode);
                let bytes = std::slice::from_raw_parts(
                    (&push as *const ItemPush).cast::<u8>(),
                    std::mem::size_of::<ItemPush>(),
                );
                self.device.cmd_push_constants(
                    self.command_buffer,
                    draw_pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytes,
                );
                self.device.cmd_draw(self.command_buffer, 4, 1, 0, 0);
            }
            self.device.cmd_end_render_pass(self.command_buffer);
            let mut release_barriers = [vk::ImageMemoryBarrier::default(); MAX_SCENE_ITEMS * 2];
            let mut release_count = 0usize;
            for external in &external_images[..external_count] {
                for range in &external.ranges[..external.range_count] {
                    release_barriers[release_count] = vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_access_mask(vk::AccessFlags::SHADER_READ)
                        .src_queue_family_index(self.queue_family)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                        .image(external.image)
                        .subresource_range(*range);
                    release_count += 1;
                }
            }
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::BY_REGION,
                &[],
                &[],
                &release_barriers[..release_count],
            );
        }
    }

    /// Pure-move extraction of the composite pair from `render`: blit the
    /// finished scene target into both presentation targets.
    fn record_composite_passes(&self, program_index: u32, preview_index: u32) {
        composite_target(
            &self.device,
            self.command_buffer,
            &self.program,
            program_index,
            self.scene.extent,
        );
        composite_target(
            &self.device,
            self.command_buffer,
            &self.preview,
            preview_index,
            self.scene.extent,
        );
    }
}

impl Drop for VulkanCompositor {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.media_ycbcr.destroy(&self.device);
            for (_, cached) in self.portal_textures.drain() {
                destroy_imported(&self.device, cached.texture);
            }
            for (_, cached) in self.media_textures.drain() {
                destroy_imported(&self.device, cached.texture);
            }
            destroy_target(&self.device, &self.surface_loader, &mut self.program);
            destroy_target(&self.device, &self.surface_loader, &mut self.preview);
            for (_, cached) in self.static_textures.drain() {
                destroy_imported(&self.device, cached.texture);
            }
            self.device.destroy_image_view(self.placeholder.view, None);
            self.device.destroy_image(self.placeholder.image, None);
            for memory in self.placeholder.memories.drain(..) {
                self.device.free_memory(memory, None);
            }
            self.device.destroy_pipeline(self.item_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.item_pipeline_layout, None);
            destroy_scene(&self.device, &mut self.scene);
            self.device.destroy_sampler(self.scene_sampler, None);
            self.device.destroy_sampler(self.item_sampler, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.sampler_layout, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_fence(self.frame_fence, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
struct MediaRuntimeBinding {
    path: String,
    looped: bool,
    visible: bool,
    playing: bool,
    opened: bool,
    retry_after: Option<Instant>,
    control_epoch: u64,
}

#[derive(Debug)]
enum RenderError {
    Vk(vk::Result),
    Import {
        reason: String,
    },
    /// Swapchain acquire timed out; the frame is skipped and the render loop
    /// re-checks its stop flag before retrying.
    AcquireTimeout,
}

pub struct RenderRuntime {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl RenderRuntime {
    pub fn start(
        surfaces: NativeSurfaces,
        project: Arc<RwLock<ProjectV1>>,
        events: std::sync::mpsc::Sender<EngineEvent>,
        portal: Arc<PipeWirePortalLink>,
        media_audio: MediaAudioBus,
        media_control: MediaControlBus,
        surface_state: Arc<RwLock<NativeSurfaces>>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("vulkan-render".into())
            .spawn(move || {
                let initial = project.read().output.clone();
                let Some((mut compositor, mut capture, media)) =
                    start_render_services(surfaces, &initial, &events, &ready_tx)
                else {
                    return;
                };
                let mut state = RenderLoopState {
                    media_sources: HashMap::new(),
                    media_frames: HashMap::new(),
                    frames: HashMap::new(),
                    nodes: HashMap::new(),
                    available: HashSet::new(),
                    import_failures: HashMap::new(),
                    capture_started: HashMap::new(),
                    restart_backoff: HashMap::new(),
                    portal_lost: HashSet::new(),
                    evict_notified: HashMap::new(),
                    recovery_failures: 0,
                    generation: 0,
                    output: initial,
                    static_caches: StaticCaches::default(),
                    deadline: Instant::now(),
                };
                let RenderLoopState {
                    media_sources,
                    media_frames,
                    frames,
                    nodes,
                    available,
                    import_failures,
                    capture_started,
                    restart_backoff,
                    portal_lost,
                    evict_notified,
                    recovery_failures,
                    generation,
                    output,
                    static_caches,
                    deadline,
                } = &mut state;
                while !thread_stop.load(Ordering::Acquire) {
                    let snapshot = project.read().clone();
                    if compositor.is_none() {
                        // Bounded recovery: retry the failed creation instead
                        // of exiting the thread (mirrors the Windows loop).
                        match recover_compositor(
                            &mut compositor,
                            *surface_state.read(),
                            snapshot.output.width,
                            snapshot.output.height,
                            recovery_failures,
                            &events,
                        ) {
                            RecoveryOutcome::Recovered => {
                                *output = snapshot.output.clone();
                            }
                            RecoveryOutcome::RetryAfterBackoff => {
                                sleep_backoff(&thread_stop, *recovery_failures);
                            }
                            RecoveryOutcome::GiveUp => break,
                        }
                        continue;
                    }
                    reconcile_media_state(
                        &snapshot,
                        &media,
                        &media_audio,
                        &media_control,
                        &events,
                        media_sources,
                        media_frames,
                        available,
                        import_failures,
                    );
                    let current_surfaces = *surface_state.read();
                    let active_compositor =
                        compositor.as_ref().expect("Vulkan compositor initialized");
                    if snapshot.output.width != output.width
                        || snapshot.output.height != output.height
                        || active_compositor.program.extent.width
                            != current_surfaces.program_width.max(1)
                        || active_compositor.program.extent.height
                            != current_surfaces.program_height.max(1)
                        || active_compositor.preview.extent.width
                            != current_surfaces.preview_width.max(1)
                        || active_compositor.preview.extent.height
                            != current_surfaces.preview_height.max(1)
                    {
                        let _ = events.send(EngineEvent::DeviceRecovery {
                            phase: DeviceRecoveryPhase::Started,
                            detail: None,
                        });
                        // Selective recreation: only surface-sized state
                        // rotates. Device, pipelines, and the static caches
                        // survive, so images are not re-decoded and text is
                        // not re-rasterized while a window edge is dragged.
                        let recreated = recreate_surface_sized_state(
                            compositor
                                .as_mut()
                                .expect("Vulkan compositor initialized"),
                            &current_surfaces,
                            (snapshot.output.width, snapshot.output.height),
                        );
                        match recreated {
                            Ok(()) => {
                                *recovery_failures = 0;
                                *output = snapshot.output.clone();
                                let _ = events.send(EngineEvent::DeviceRecovery {
                                    phase: DeviceRecoveryPhase::Succeeded,
                                    detail: None,
                                });
                            }
                            Err(error) => {
                                // Selective recreation failed; fall back to
                                // the full device rebuild with the same
                                // bounded-retry accounting as a failed create.
                                drop(compositor.take());
                                *recovery_failures += 1;
                                let _ = events.send(EngineEvent::DeviceRecovery {
                                    phase: DeviceRecoveryPhase::Failed,
                                    detail: Some(error.clone()),
                                });
                                if *recovery_failures >= MAX_RECOVERY_FAILURES {
                                    let _ = events.send(EngineEvent::EngineError {
                                        message: format!(
                                        "Vulkan-Renderer nach {MAX_RECOVERY_FAILURES} Wiederherstellungsversuchen aufgegeben: {error}"
                                        ),
                                    });
                                    break;
                                }
                                // The compositor stays None; the loop head
                                // retries the creation after the backoff.
                                sleep_backoff(&thread_stop, *recovery_failures);
                                continue;
                            }
                        }
                    }
                    let current_generation = portal.generation();
                    if current_generation != *generation {
                        for source_id in nodes.keys().copied().collect::<Vec<_>>() {
                            capture.stop(source_id);
                        }
                        nodes.clear();
                        for (source_id, frame) in frames.drain() {
                            capture.return_buffer(source_id, frame.buffer_token);
                        }
                        capture.shutdown();
                        capture = match CaptureHandle::spawn() {
                            Ok(value) => value,
                            Err(mut error) => {
                                // Bounded respawn retry: a transient spawn
                                // failure must not kill the render thread;
                                // give up only past MAX_RECOVERY_FAILURES
                                // (mirrors the compositor accounting).
                                let respawned = loop {
                                    *recovery_failures += 1;
                                    let _ = events.send(EngineEvent::DeviceRecovery {
                                        phase: DeviceRecoveryPhase::Failed,
                                        detail: Some(error.to_string()),
                                    });
                                    if *recovery_failures >= MAX_RECOVERY_FAILURES {
                                        let _ = events.send(EngineEvent::EngineError {
                                            message: format!(
                                                "Vulkan-Renderer nach {MAX_RECOVERY_FAILURES} Wiederherstellungsversuchen aufgegeben: {error}"
                                            ),
                                        });
                                        break None;
                                    }
                                    sleep_backoff(&thread_stop, *recovery_failures);
                                    match CaptureHandle::spawn() {
                                        Ok(value) => break Some(value),
                                        Err(retry_error) => error = retry_error,
                                    }
                                };
                                match respawned {
                                    Some(value) => value,
                                    None => break,
                                }
                            }
                        };
                        available.clear();
                        capture_started.clear();
                        evict_notified.clear();
                        restart_backoff.clear();
                        portal_lost.clear();
                        *generation = current_generation;
                    }
                    let selected = portal.streams();
                    let marker = portal.binding_marker();
                    let selected_nodes: HashSet<u32> =
                        selected.iter().map(|stream| stream.pipewire_node_id).collect();
                    let wanted: HashMap<Uuid, u32> = snapshot
                        .sources
                        .iter()
                        .filter_map(|source| match source {
                            Source::Window { id, binding, .. } => marker
                                .as_ref()
                                .is_some_and(|marker| &binding.process_path == marker)
                                .then(|| binding.window_title.parse::<u32>().ok())
                                .flatten()
                                .filter(|node| selected_nodes.contains(node))
                                .map(|node| (*id, node)),
                            Source::Display { id, binding, .. } => marker
                                .as_ref()
                                .is_some_and(|marker| &binding.adapter_luid == marker)
                                .then_some(binding.output_id)
                                .filter(|node| selected_nodes.contains(node))
                                .map(|node| (*id, node)),
                            _ => None,
                        })
                        .collect();
                    for (source_id, node) in &wanted {
                        if portal_lost.contains(source_id) {
                            continue;
                        }
                        if nodes.get(source_id) != Some(node) {
                            if nodes.contains_key(source_id) {
                                capture.stop(*source_id);
                            } else if restart_backoff
                                .get(source_id)
                                .is_some_and(|last| last.elapsed() < IMPORT_RETRY_INTERVAL)
                            {
                                // Fehlgeschlagener Start: Backoff abwarten,
                                // kein Restart-Sturm über die Capture-Thread-
                                // Verbindung.
                                continue;
                            }
                            let remote = portal.take_remote();
                            capture.start(*source_id, *node, remote);
                            restart_backoff.insert(*source_id, Instant::now());
                            nodes.insert(*source_id, *node);
                            capture_started.insert(*source_id, Instant::now());
                        }
                    }
                    for source_id in nodes.keys().copied().collect::<Vec<_>>() {
                        if !wanted.contains_key(&source_id) {
                            capture.stop(source_id);
                            nodes.remove(&source_id);
                            capture_started.remove(&source_id);
                            evict_notified.remove(&source_id);
                            restart_backoff.remove(&source_id);
                            if let Some(frame) = frames.remove(&source_id) {
                                capture.return_buffer(source_id, frame.buffer_token);
                            }
                        }
                    }
                    drain_capture_messages(
                        &mut capture,
                        frames,
                        capture_started,
                        available,
                        nodes,
                        import_failures,
                        evict_notified,
                        restart_backoff,
                        portal_lost,
                        &events,
                    );
                    // Portal delivery is damage-driven: after a capture delivered its
                    // first frame, silence just means idle content and the last frame
                    // keeps being rendered. Only captures that never delivered
                    // anything fall offline after the grace window; genuine stream
                    // failures arrive as SourceError events.
                    let expired: Vec<Uuid> = capture_started
                        .iter()
                        .filter(|(_, started)| started.elapsed() > Duration::from_millis(750))
                        .map(|(source_id, _)| *source_id)
                        .collect();
                    for source_id in expired {
                        capture_started.remove(&source_id);
                        available.remove(&source_id);
                        // Windows-parity restart: evict the silent capture so
                        // the wanted-loop re-issues capture.start next tick
                        // instead of leaving a frozen frame forever.
                        nodes.remove(&source_id);
                        if let Some(frame) = frames.remove(&source_id) {
                            capture.return_buffer(source_id, frame.buffer_token);
                        }
                        capture.stop(source_id);
                        let reason = "PipeWire-Quelle liefert keine Live-Frames";
                        if evict_notified.get(&source_id).map(String::as_str) != Some(reason) {
                            evict_notified.insert(source_id, reason.to_string());
                            let _ = events.send(EngineEvent::SourceUnavailable {
                                source_id,
                                reason: reason.into(),
                            });
                        }
                    }
                    match compositor
                        .as_mut()
                        .expect("Vulkan compositor initialized")
                        .render(
                            static_caches,
                            &snapshot,
                            frames,
                            media_frames,
                            import_failures,
                            &events,
                        )
                    {
                        Ok(()) => {}
                        Err(RenderError::AcquireTimeout) => {}
                        Err(RenderError::Vk(error)) => {
                            let detail = if matches!(
                                error,
                                vk::Result::ERROR_OUT_OF_DATE_KHR
                                    | vk::Result::SUBOPTIMAL_KHR
                            ) {
                                "Vulkan-Swapchain wird neu erstellt".to_string()
                            } else {
                                format!("Vulkan-Gerät wird nach {error:?} neu erstellt")
                            };
                            let _ = events.send(EngineEvent::DeviceRecovery {
                                phase: DeviceRecoveryPhase::Started,
                                detail: Some(detail),
                            });
                            let swapchain_scoped = matches!(
                                error,
                                vk::Result::ERROR_OUT_OF_DATE_KHR
                                    | vk::Result::SUBOPTIMAL_KHR
                            );
                            let mut recovered = false;
                            if swapchain_scoped {
                                // Swapchain-scoped error: recreate only the
                                // presentation targets. Device, pipelines, and
                                // texture caches stay alive.
                                let current_surfaces = *surface_state.read();
                                let active = compositor
                                    .as_mut()
                                    .expect("Vulkan compositor initialized");
                                recovered = match active.recreate_swapchains(
                                    (
                                        current_surfaces.program_width.max(1),
                                        current_surfaces.program_height.max(1),
                                    ),
                                    (
                                        current_surfaces.preview_width.max(1),
                                        current_surfaces.preview_height.max(1),
                                    ),
                                    true,
                                ) {
                                    Ok(()) => {
                                        *recovery_failures = 0;
                                        let _ = events.send(EngineEvent::DeviceRecovery {
                                            phase: DeviceRecoveryPhase::Succeeded,
                                            detail: None,
                                        });
                                        true
                                    }
                                    Err(_) => false,
                                };
                            }
                            if !recovered {
                                drop(compositor.take());
                                match VulkanCompositor::create(
                                    *surface_state.read(),
                                    output.width,
                                    output.height,
                                ) {
                                    Ok(next) => {
                                        compositor = Some(next);
                                        *recovery_failures = 0;
                                        let _ = events.send(EngineEvent::DeviceRecovery {
                                            phase: DeviceRecoveryPhase::Succeeded,
                                            detail: None,
                                        });
                                    }
                                    Err(recovery_error) => {
                                        *recovery_failures += 1;
                                        let _ = events.send(EngineEvent::DeviceRecovery {
                                            phase: DeviceRecoveryPhase::Failed,
                                            detail: Some(recovery_error.clone()),
                                        });
                                        if *recovery_failures >= MAX_RECOVERY_FAILURES {
                                            let _ = events.send(EngineEvent::EngineError {
                                                message: format!(
                                                    "Vulkan-Renderer nach {MAX_RECOVERY_FAILURES} Wiederherstellungsversuchen aufgegeben: {recovery_error}"
                                                ),
                                            });
                                            break;
                                        }
                                        // The compositor stays None; the loop head
                                        // retries the creation after the backoff.
                                        sleep_backoff(&thread_stop, *recovery_failures);
                                    }
                                }
                            }
                        }
                        Err(RenderError::Import { reason }) => {
                            let _ = events.send(EngineEvent::EngineError { message: reason });
                            continue;
                        }
                    }
                    let frame_time = Duration::from_secs_f64(1.0 / output.fps.max(1) as f64);
                    *deadline += frame_time;
                    if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                        thread::sleep(wait.min(Duration::from_millis(20)));
                    } else {
                        *deadline = Instant::now();
                    }
                }
                if let Some(compositor) = compositor.as_mut() {
                    for source_id in media_frames.keys().copied().collect::<Vec<_>>() {
                        compositor.remove_external_texture(source_id);
                    }
                }
                media_frames.clear();
                media.shutdown(&media_audio);
                if let Some(compositor) = compositor.as_mut() {
                    for source_id in frames.keys().copied().collect::<Vec<_>>() {
                        compositor.remove_external_texture(source_id);
                    }
                }
                for (_, frame) in frames.drain() {
                    capture.return_buffer(frame.source_id, frame.buffer_token);
                }
                capture.shutdown();
            })
            .map_err(|error| error.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Mutex::new(Some(thread)),
            }),
            Ok(Err(error)) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(format!(
                    "Zeitüberschreitung beim Vulkan-Renderer-Start: {error}"
                ))
            }
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().expect("renderer mutex poisoned").take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RenderRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Pure-move extraction of the capture `FrameMessage` drain loop body from
/// the render thread closure.
#[allow(clippy::too_many_arguments)]
fn drain_capture_messages(
    capture: &mut CaptureHandle,
    frames: &mut HashMap<Uuid, CapturedFrame>,
    capture_started: &mut HashMap<Uuid, Instant>,
    available: &mut HashSet<Uuid>,
    nodes: &mut HashMap<Uuid, u32>,
    import_failures: &HashMap<Uuid, Instant>,
    evict_notified: &mut HashMap<Uuid, String>,
    restart_backoff: &mut HashMap<Uuid, Instant>,
    portal_lost: &mut HashSet<Uuid>,
    events: &std::sync::mpsc::Sender<EngineEvent>,
) {
    while let Ok(message) = capture.try_recv() {
        match message {
            FrameMessage::Frame(frame) => {
                let source_id = frame.source_id;
                if let Some(old) = frames.insert(source_id, frame) {
                    capture.return_buffer(source_id, old.buffer_token);
                }
                capture_started.remove(&source_id);
                // Frames arrived again: re-arm eviction reporting.
                evict_notified.remove(&source_id);
                restart_backoff.remove(&source_id);
                if available.insert(source_id) && !import_failures.contains_key(&source_id) {
                    let _ = events.send(EngineEvent::SourceAvailable { source_id });
                }
            }
            FrameMessage::SourceError { source_id, reason } => {
                available.remove(&source_id);
                capture_started.remove(&source_id);
                // Windows-parity restart: tear the errored stream down so the
                // wanted-loop re-issues capture.start next tick instead of
                // keeping a frozen last frame forever.
                nodes.remove(&source_id);
                if let Some(frame) = frames.remove(&source_id) {
                    capture.return_buffer(source_id, frame.buffer_token);
                }
                capture.stop(source_id);
                // Portal-Tod terminal behandeln: Grund merken und weitere
                // Startversuche bis zum Generationswechsel aussetzen.
                if reason == PORTAL_LOST_REASON {
                    portal_lost.insert(source_id);
                }
                if evict_notified.get(&source_id).map(String::as_str) != Some(reason.as_str()) {
                    evict_notified.insert(source_id, reason.clone());
                    let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                }
            }
        }
    }
}

/// Pure-move extraction of the selective-recreation sequence from the render
/// thread closure's output-resize block: scene target first, then both
/// swapchain targets. Recovery accounting (including the give-up `break`)
/// stays at the call site because it crosses the loop boundary.
fn recreate_surface_sized_state(
    compositor: &mut VulkanCompositor,
    surfaces: &NativeSurfaces,
    output: (u32, u32),
) -> Result<(), String> {
    compositor
        .recreate_scene_target(output.0, output.1)
        .and_then(|()| {
            compositor.recreate_swapchains(
                (
                    surfaces.program_width.max(1),
                    surfaces.program_height.max(1),
                ),
                (
                    surfaces.preview_width.max(1),
                    surfaces.preview_height.max(1),
                ),
                false,
            )
        })
}

/// Bookkeeping locals of the render-thread main loop. Pure code motion:
/// the previous loose closure locals, grouped so the loop owns one `mut`
/// value whose fields are destructured as `&mut` bindings.
struct RenderLoopState {
    media_sources: HashMap<Uuid, MediaRuntimeBinding>,
    media_frames: HashMap<Uuid, MediaVideoFrame>,
    frames: HashMap<Uuid, CapturedFrame>,
    nodes: HashMap<Uuid, u32>,
    available: HashSet<Uuid>,
    // Import-failure streaks live on the render thread (not on
    // the compositor) so backoff state survives device/surface
    // recreations.
    import_failures: HashMap<Uuid, Instant>,
    capture_started: HashMap<Uuid, Instant>,
    // Restart-Rückfallebene für frische Starts: Eine Quelle,
    // deren capture.start fehlschlägt, wird erst nach
    // IMPORT_RETRY_INTERVAL erneut versucht, statt die
    // Capture-Verbindung jede Schleifenrunde neu aufzureißen.
    restart_backoff: HashMap<Uuid, Instant>,
    // Terminaler Portal-Verlust pro Quelle: Solange gesetzt,
    // startet die Wanted-Schleife diese Quelle nicht erneut;
    // erst ein Generationswechsel (neue Auswahl, neues fd)
    // räumt die Sperre weg.
    portal_lost: HashSet<Uuid>,
    // Eviction/failure-report dedup: identical consecutive
    // reasons are reported once until frames recover
    // (mirrors windows.rs should_report_failure philosophy).
    evict_notified: HashMap<Uuid, String>,
    recovery_failures: u32,
    generation: u64,
    // (mirror windows.rs output resize)
    output: OutputConfig,
    static_caches: StaticCaches,
    deadline: Instant,
}

/// Pure-move extraction of the render thread's three sequential startup
/// steps (compositor, capture, media). On failure the ready channel and
/// the event log are fed exactly as before and `None` is returned so the
/// caller bails out of the thread closure.
fn start_render_services(
    surfaces: NativeSurfaces,
    initial: &OutputConfig,
    events: &std::sync::mpsc::Sender<EngineEvent>,
    ready_tx: &std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Option<(Option<VulkanCompositor>, CaptureHandle, LinuxMedia)> {
    let compositor = Some(
        match VulkanCompositor::create(surfaces, initial.width, initial.height) {
            Ok(value) => value,
            Err(error) => {
                let _ = ready_tx.send(Err(error.clone()));
                let _ = events.send(EngineEvent::DeviceRecovery {
                    phase: DeviceRecoveryPhase::Failed,
                    detail: Some(error),
                });
                return None;
            }
        },
    );
    let capture = match CaptureHandle::spawn() {
        Ok(value) => value,
        Err(error) => {
            // The ready channel must resolve even when startup
            // fails, or `start` blocks until its timeout while
            // the thread dies silently.
            let _ = ready_tx.send(Err(error.to_string()));
            let _ = events.send(EngineEvent::EngineError {
                message: error.to_string(),
            });
            return None;
        }
    };
    let media = match LinuxMedia::start(events.clone()) {
        Ok(value) => value,
        Err(error) => {
            let _ = ready_tx.send(Err(format!("GStreamer nicht verfügbar: {error}")));
            let _ = events.send(EngineEvent::EngineError {
                message: format!("GStreamer nicht verfügbar: {error}"),
            });
            return None;
        }
    };
    // Every startup step succeeded; unblock the waiting caller.
    let _ = ready_tx.send(Ok(()));
    Some((compositor, capture, media))
}

/// Outcome of [`recover_compositor`]: sleep_backoff and loop control
/// (`break`/`continue`) stay at the call site because they cross the loop
/// boundary (precedent: `recreate_surface_sized_state`).
enum RecoveryOutcome {
    Recovered,
    RetryAfterBackoff,
    GiveUp,
}

/// Pure-move extraction of the compositor-recovery block from the render
/// thread loop head. Pure accounting lives here; the backoff sleep and the
/// give-up break stay with the caller.
fn recover_compositor(
    compositor: &mut Option<VulkanCompositor>,
    surfaces: NativeSurfaces,
    width: u32,
    height: u32,
    recovery_failures: &mut u32,
    events: &std::sync::mpsc::Sender<EngineEvent>,
) -> RecoveryOutcome {
    match VulkanCompositor::create(surfaces, width, height) {
        Ok(next) => {
            *compositor = Some(next);
            *recovery_failures = 0;
            let _ = events.send(EngineEvent::DeviceRecovery {
                phase: DeviceRecoveryPhase::Succeeded,
                detail: None,
            });
            RecoveryOutcome::Recovered
        }
        Err(error) => {
            *recovery_failures += 1;
            let _ = events.send(EngineEvent::DeviceRecovery {
                phase: DeviceRecoveryPhase::Failed,
                detail: Some(error.clone()),
            });
            if *recovery_failures >= MAX_RECOVERY_FAILURES {
                let _ = events.send(EngineEvent::EngineError {
                    message: format!(
                        "Vulkan-Renderer nach {MAX_RECOVERY_FAILURES} Wiederherstellungsversuchen aufgegeben: {error}"
                    ),
                });
                RecoveryOutcome::GiveUp
            } else {
                RecoveryOutcome::RetryAfterBackoff
            }
        }
    }
}

/// Pure-move extraction of the media reconcile and notice-drain loop body
/// from the render thread closure.
#[allow(clippy::too_many_arguments)]
fn reconcile_media_state(
    snapshot: &ProjectV1,
    media: &LinuxMedia,
    media_audio: &MediaAudioBus,
    media_control: &MediaControlBus,
    events: &std::sync::mpsc::Sender<EngineEvent>,
    media_sources: &mut HashMap<Uuid, MediaRuntimeBinding>,
    media_frames: &mut HashMap<Uuid, MediaVideoFrame>,
    available: &mut HashSet<Uuid>,
    import_failures: &HashMap<Uuid, Instant>,
) {
    let wanted_media: HashSet<Uuid> = snapshot
        .sources
        .iter()
        .filter_map(|source| match source {
            Source::Media { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    for source in &snapshot.sources {
        let Source::Media {
            id,
            path,
            looped,
            continue_when_hidden,
            restart_on_show,
            ..
        } = source
        else {
            continue;
        };
        let visible = snapshot
            .scenes
            .iter()
            .find(|scene| scene.id == snapshot.active_scene_id)
            .is_some_and(|scene| {
                scene
                    .items
                    .iter()
                    .any(|item| item.source_id == *id && item.visible)
            });
        let path_changed = media_sources
            .get(id)
            .is_some_and(|runtime| runtime.path != *path);
        if path_changed {
            if media_sources.get(id).is_some_and(|runtime| runtime.opened) {
                media.remove(*id, media_audio);
            }
            media_sources.remove(id);
            media_frames.remove(id);
        }
        // Bounded self-heal: once the cooldown elapsed, drop
        // the failed binding so the open below runs a fresh
        // session (fresh ReopenBudget + latch) instead of the
        // binding staying dead forever. Path changes keep
        // their immediate-tick behavior above.
        let retry_due = media_sources.get(id).is_some_and(|runtime| {
            !runtime.opened
                && runtime
                    .retry_after
                    .is_some_and(|deadline| Instant::now() >= deadline)
        });
        if retry_due {
            // The Unsupported arm already tore the session
            // down (opened == false); only bookkeeping remains.
            media_sources.remove(id);
            media_frames.remove(id);
        }
        if !media_sources.contains_key(id) {
            let opened = match media.open(*id, path, *looped, media_audio) {
                Ok(()) => true,
                Err(reason) => {
                    let _ = events.send(EngineEvent::UnsupportedMedia {
                        source_id: *id,
                        reason,
                    });
                    false
                }
            };
            media_sources.insert(
                *id,
                MediaRuntimeBinding {
                    path: path.clone(),
                    looped: *looped,
                    visible,
                    playing: true,
                    opened,
                    retry_after: if opened {
                        None
                    } else {
                        Some(Instant::now() + MEDIA_RETRY_COOLDOWN)
                    },
                    control_epoch: 0,
                },
            );
        }
        let runtime = media_sources.get_mut(id).expect("media runtime inserted");
        if !runtime.opened {
            continue;
        }
        if runtime.looped != *looped {
            media.command(*id, MediaCommand::SetLoop(*looped));
            runtime.looped = *looped;
        }
        if visible && !runtime.visible && *restart_on_show {
            media.command(*id, MediaCommand::Seek(0.0));
        }
        let control = media_control.read().get(id).copied().unwrap_or_default();
        runtime.control_epoch = control.epoch;
        let should_play = control.playing && (visible || *continue_when_hidden);
        if runtime.playing != should_play {
            media.command(
                *id,
                if should_play {
                    MediaCommand::Play
                } else {
                    MediaCommand::Pause
                },
            );
            runtime.playing = should_play;
        }
        runtime.visible = visible;
        // Atomar entnehmen: ein zwischen Snapshot und
        // Loeschen ankommender Seek darf nicht verloren
        // gehen (Windows-Paritaet: take unter einem Lock).
        let seek = media_control
            .write()
            .entry(*id)
            .or_default()
            .seek_seconds
            .take();
        if let Some(position) = seek {
            media.command(*id, MediaCommand::Seek(position));
        }
    }
    for id in media_sources.keys().copied().collect::<Vec<_>>() {
        if !wanted_media.contains(&id) {
            if media_sources.get(&id).is_some_and(|runtime| runtime.opened) {
                media.remove(id, media_audio);
            }
            media_sources.remove(&id);
            media_frames.remove(&id);
        }
    }
    for notice in media.drain_notices() {
        match notice {
            MediaNotice::State { source_id, state } => {
                if let Some(runtime) = media_sources.get_mut(&source_id) {
                    runtime.playing = state.playing;
                    // Windows-Paritaet: das vom Worker selbst
                    // ausgeloeste Pausieren am Dateiende muss
                    // den Bus zurueckschreiben, sonst bleibt
                    // playing=true haengen und Play erzeugt
                    // keine Kante mehr. Der Epoch-Vergleich
                    // schuetzt ein gleichzeitiges Nutzer-Play.
                    if !state.playing {
                        let mut bus = media_control.write();
                        let entry = bus.entry(source_id).or_default();
                        if entry.epoch == runtime.control_epoch {
                            entry.playing = false;
                        }
                    }
                }
                let _ = events.send(EngineEvent::MediaState { source_id, state });
            }
            MediaNotice::Unsupported { source_id, reason } => {
                media.remove(source_id, media_audio);
                if let Some(runtime) = media_sources.get_mut(&source_id) {
                    runtime.opened = false;
                    runtime.playing = false;
                    runtime.retry_after = Some(Instant::now() + MEDIA_RETRY_COOLDOWN);
                }
                media_frames.remove(&source_id);
                available.remove(&source_id);
                let _ = events.send(EngineEvent::UnsupportedMedia { source_id, reason });
            }
            MediaNotice::SeekFailed { source_id, reason } => {
                // Windows-Paritaet: ein fehlgeschlagener Seek
                // meldet den Grund als UnsupportedMedia-Event,
                // ohne die Sitzung zu verwerfen. Ein separater
                // MediaState-Dedup existiert auf diesem Pfad
                // nicht; die Pipeline dedupliziert workerseitig
                // und ein erfolgreicher Seek ändert die Position
                // ohnehin, sodass das nächste State-Event fließt.
                let _ = events.send(EngineEvent::UnsupportedMedia { source_id, reason });
            }
            MediaNotice::Video(frame) => {
                let source_id = frame.source_id;
                media_frames.insert(source_id, frame);
                if available.insert(source_id) && !import_failures.contains_key(&source_id) {
                    let _ = events.send(EngineEvent::SourceAvailable { source_id });
                }
            }
        }
    }
}

fn raw_handles(
    surfaces: NativeSurfaces,
) -> Result<(RawDisplayHandle, RawWindowHandle, RawWindowHandle), String> {
    let display = NonNull::new(surfaces.display as *mut std::ffi::c_void)
        .ok_or_else(|| "native display handle is null".to_string())?;
    match surfaces.kind {
        NativeSurfaceKind::Xlib => {
            let display = RawDisplayHandle::Xlib(XlibDisplayHandle::new(Some(display), 0));
            let program = RawWindowHandle::Xlib(XlibWindowHandle::new(surfaces.program as u64));
            let preview = RawWindowHandle::Xlib(XlibWindowHandle::new(surfaces.preview as u64));
            Ok((display, program, preview))
        }
        NativeSurfaceKind::Wayland => {
            let display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display));
            let program = NonNull::new(surfaces.program as *mut std::ffi::c_void)
                .ok_or_else(|| "Program Wayland surface is null".to_string())?;
            let preview = NonNull::new(surfaces.preview as *mut std::ffi::c_void)
                .ok_or_else(|| "Preview Wayland surface is null".to_string())?;
            Ok((
                display,
                RawWindowHandle::Wayland(WaylandWindowHandle::new(program)),
                RawWindowHandle::Wayland(WaylandWindowHandle::new(preview)),
            ))
        }
        _ => Err("unsupported Linux native surface type".into()),
    }
}

fn choose_device(
    instance: &Instance,
    surface_loader: &khr::surface::Instance,
    program: vk::SurfaceKHR,
    preview: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32), String> {
    // Extensions unconditionally enabled on the logical device below. Requesting
    // an unsupported name makes vkCreateDevice fail with
    // VK_ERROR_EXTENSION_NOT_PRESENT, so devices lacking any of them are
    // disqualified before queue selection instead. The same applies to the
    // samplerYcbcrConversion feature the media path requires.
    const REQUIRED_EXTENSIONS: [&std::ffi::CStr; 6] = [
        khr::swapchain::NAME,
        khr::external_memory::NAME,
        khr::external_memory_fd::NAME,
        ash::ext::external_memory_dma_buf::NAME,
        ash::ext::image_drm_format_modifier::NAME,
        ash::ext::queue_family_foreign::NAME,
    ];
    let devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|error| format!("enumerate Vulkan devices: {error}"))?;
    for physical in devices {
        let supported = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(|error| format!("query device extensions: {error}"))?;
        if !REQUIRED_EXTENSIONS.iter().all(|name| {
            supported
                .iter()
                .any(|property| property.extension_name_as_c_str() == Ok(*name))
        }) {
            continue;
        }
        // Enabling an unsupported feature fails vkCreateDevice, so disqualify
        // such devices here as well. sampler_ycbcr_conversion is promoted
        // from an extension and lives in its own chained query struct.
        let mut ycbcr_features = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default();
        let mut features2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut ycbcr_features);
        unsafe {
            instance.get_physical_device_features2(physical, &mut features2);
        }
        if ycbcr_features.sampler_ycbcr_conversion != vk::TRUE {
            continue;
        }
        let properties = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        for (index, family) in properties.iter().enumerate() {
            if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                continue;
            }
            let program_ok = unsafe {
                surface_loader.get_physical_device_surface_support(physical, index as u32, program)
            }
            .unwrap_or(false);
            let preview_ok = unsafe {
                surface_loader.get_physical_device_surface_support(physical, index as u32, preview)
            }
            .unwrap_or(false);
            if program_ok && preview_ok {
                return Ok((physical, index as u32));
            }
        }
    }
    Err(
        "no Vulkan device supports the required extensions and features and can present both surfaces"
            .into(),
    )
}

fn create_sampler(device: &Device) -> Result<vk::Sampler, String> {
    unsafe {
        device.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(1.0),
            None,
        )
    }
    .map_err(|error| format!("create sampler: {error}"))
}

fn create_render_pass(device: &Device, format: vk::Format) -> Result<vk::RenderPass, String> {
    let attachment = vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(if format == vk::Format::R16G16B16A16_SFLOAT {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::PRESENT_SRC_KHR
        });
    let reference = vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(std::slice::from_ref(&reference));
    let dependency = vk::SubpassDependency::default()
        .src_subpass(0)
        .dst_subpass(vk::SUBPASS_EXTERNAL)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .dependency_flags(vk::DependencyFlags::BY_REGION);
    unsafe {
        device.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(std::slice::from_ref(&attachment))
                .subpasses(std::slice::from_ref(&subpass))
                .dependencies(std::slice::from_ref(&dependency)),
            None,
        )
    }
    .map_err(|error| format!("create render pass: {error}"))
}

fn create_scene_target(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    render_pass: vk::RenderPass,
    width: u32,
    height: u32,
) -> Result<SceneTarget, String> {
    let extent = vk::Extent2D { width, height };
    let image = unsafe {
        device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R16G16B16A16_SFLOAT)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED),
            None,
        )
    }
    .map_err(|error| format!("create scene image: {error}"))?;
    // Staging rollback: every dependent resource registers itself the moment
    // it exists, so a failure below releases exactly what was created instead
    // of leaking a partial attachment on the hot resize path (PartialVulkan
    // discipline).
    let mut memory = None;
    let mut view = None;
    let mut framebuffer = None;
    let staged = (|| -> Result<(), String> {
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory_type = find_memory_type(
            instance,
            physical,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let allocated = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )
        }
        .map_err(|error| format!("allocate scene image: {error}"))?;
        memory = Some(allocated);
        unsafe { device.bind_image_memory(image, allocated, 0) }
            .map_err(|error| format!("bind scene image: {error}"))?;
        let attached = create_view(
            device,
            image,
            vk::Format::R16G16B16A16_SFLOAT,
            vk::ComponentMapping::default(),
        )?;
        view = Some(attached);
        let target = unsafe {
            device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(std::slice::from_ref(&attached))
                    .width(width)
                    .height(height)
                    .layers(1),
                None,
            )
        }
        .map_err(|error| format!("scene framebuffer: {error}"))?;
        framebuffer = Some(target);
        Ok(())
    })();
    if let Err(error) = staged {
        // Reverse creation order; `render_pass` belongs to the caller.
        unsafe {
            if let Some(framebuffer) = framebuffer {
                device.destroy_framebuffer(framebuffer, None);
            }
            if let Some(view) = view {
                device.destroy_image_view(view, None);
            }
            device.destroy_image(image, None);
            if let Some(memory) = memory {
                device.free_memory(memory, None);
            }
        }
        return Err(error);
    }
    Ok(SceneTarget {
        image,
        memory: memory.expect("staged scene memory"),
        view: view.expect("staged scene view"),
        framebuffer: framebuffer.expect("staged scene framebuffer"),
        render_pass,
        extent,
    })
}

#[allow(clippy::too_many_arguments)]
fn create_swapchain(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    surface_loader: &khr::surface::Instance,
    surface: vk::SurfaceKHR,
    width: u32,
    height: u32,
    layout: vk::DescriptorSetLayout,
    descriptor_set: vk::DescriptorSet,
) -> Result<SwapchainTarget, String> {
    let capabilities =
        unsafe { surface_loader.get_physical_device_surface_capabilities(physical, surface) }
            .map_err(|error| format!("surface capabilities: {error}"))?;
    let formats = unsafe { surface_loader.get_physical_device_surface_formats(physical, surface) }
        .map_err(|error| format!("surface formats: {error}"))?;
    let format = formats
        .iter()
        .find(|format| format.format == vk::Format::B8G8R8A8_UNORM)
        .or_else(|| formats.first())
        .ok_or_else(|| "surface has no Vulkan formats".to_string())?;
    let extent = if capabilities.current_extent.width != u32::MAX {
        capabilities.current_extent
    } else {
        vk::Extent2D {
            width: width.clamp(
                capabilities.min_image_extent.width,
                capabilities.max_image_extent.width,
            ),
            height: height.clamp(
                capabilities.min_image_extent.height,
                capabilities.max_image_extent.height,
            ),
        }
    };
    let count = capabilities.min_image_count.saturating_add(1).clamp(
        capabilities.min_image_count,
        if capabilities.max_image_count == 0 {
            u32::MAX
        } else {
            capabilities.max_image_count
        },
    );
    let render_pass = create_render_pass(device, format.format)?;
    let swapchain_loader = khr::swapchain::Device::new(instance, device);
    // Staging rollback: every dependent resource registers itself the moment
    // it exists, so a failure anywhere in the chain destroys exactly what was
    // created before propagating (PartialVulkan discipline). Surface and
    // descriptor_set stay with the caller.
    let mut swapchain = None;
    let mut views: Vec<vk::ImageView> = Vec::new();
    let mut framebuffers: Vec<vk::Framebuffer> = Vec::new();
    let mut available = None;
    let mut rendered: Vec<vk::Semaphore> = Vec::new();
    let mut pipeline_layout = None;
    let mut pipeline = None;
    let staged = (|| -> Result<(), String> {
        swapchain = Some(
            unsafe {
                swapchain_loader.create_swapchain(
                    &vk::SwapchainCreateInfoKHR::default()
                        .surface(surface)
                        .min_image_count(count)
                        .image_format(format.format)
                        .image_color_space(format.color_space)
                        .image_extent(extent)
                        .image_array_layers(1)
                        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                        .pre_transform(capabilities.current_transform)
                        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                        .present_mode(vk::PresentModeKHR::FIFO)
                        .clipped(true),
                    None,
                )
            }
            .map_err(|error| format!("create swapchain: {error}"))?,
        );
        let images =
            unsafe { swapchain_loader.get_swapchain_images(swapchain.expect("staged swapchain")) }
                .map_err(|error| format!("swapchain images: {error}"))?;
        for image in &images {
            let view = create_view(
                device,
                *image,
                format.format,
                vk::ComponentMapping::default(),
            )?;
            views.push(view);
            let framebuffer = unsafe {
                device.create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(render_pass)
                        .attachments(std::slice::from_ref(&view))
                        .width(extent.width)
                        .height(extent.height)
                        .layers(1),
                    None,
                )
            }
            .map_err(|error| format!("swapchain framebuffer: {error}"))?;
            framebuffers.push(framebuffer);
        }
        let sem_info = vk::SemaphoreCreateInfo::default();
        available = Some(
            unsafe { device.create_semaphore(&sem_info, None) }
                .map_err(|error| format!("available semaphore: {error}"))?,
        );
        for _ in 0..images.len() {
            let semaphore = unsafe { device.create_semaphore(&sem_info, None) }
                .map_err(|error| format!("rendered semaphore: {error}"))?;
            rendered.push(semaphore);
        }
        pipeline_layout = Some(create_composite_pipeline_layout(device, layout)?);
        pipeline = Some(create_composite_pipeline(
            device,
            render_pass,
            pipeline_layout.expect("staged pipeline layout"),
        )?);
        Ok(())
    })();
    if let Err(error) = staged {
        // Reverse creation order; the surface is NOT destroyed — a
        // replacement target adopts it (see recreate_swapchain).
        unsafe {
            if let Some(pipeline) = pipeline {
                device.destroy_pipeline(pipeline, None);
            }
            if let Some(pipeline_layout) = pipeline_layout {
                device.destroy_pipeline_layout(pipeline_layout, None);
            }
            for semaphore in rendered.drain(..) {
                device.destroy_semaphore(semaphore, None);
            }
            if let Some(available) = available {
                device.destroy_semaphore(available, None);
            }
            for framebuffer in framebuffers.drain(..) {
                device.destroy_framebuffer(framebuffer, None);
            }
            for view in views.drain(..) {
                device.destroy_image_view(view, None);
            }
            if let Some(swapchain) = swapchain {
                swapchain_loader.destroy_swapchain(swapchain, None);
            }
            device.destroy_render_pass(render_pass, None);
        }
        return Err(error);
    }
    Ok(SwapchainTarget {
        surface,
        loader: swapchain_loader,
        swapchain: swapchain.expect("staged swapchain"),
        views,
        framebuffers,
        render_pass,
        extent,
        available: available.expect("staged available semaphore"),
        rendered,
        descriptor_set,
        pipeline: pipeline.expect("staged composite pipeline"),
        pipeline_layout: pipeline_layout.expect("staged pipeline layout"),
    })
}

/// Rebuilds one swapchain target in place for a resize or a swapchain-scoped
/// error. The replacement is created on the same surface BEFORE the old state
/// is retired, so a failure leaves the previous target fully functional
/// (staging rollback). The surface handle, the shared scene descriptor set,
/// and the device/pipeline lifetime all survive.
#[allow(clippy::too_many_arguments)]
fn recreate_swapchain(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    surface_loader: &khr::surface::Instance,
    layout: vk::DescriptorSetLayout,
    target: &mut SwapchainTarget,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let next = create_swapchain(
        instance,
        device,
        physical,
        surface_loader,
        target.surface,
        width,
        height,
        layout,
        target.descriptor_set,
    )?;
    // Success only: retire the old swapchain state. The surface is NOT
    // destroyed — the replacement target adopted the same handle.
    destroy_swapchain_state(device, target);
    *target = next;
    Ok(())
}

/// Identity mapping except alpha, which reads as a constant 1.0. X-format
/// DRM buffers leave the alpha byte undefined; without this swizzle the item
/// shader premultiplies by that garbage and X-captured windows render with
/// random per-pixel transparency.
fn opaque_alpha_components() -> vk::ComponentMapping {
    vk::ComponentMapping {
        r: vk::ComponentSwizzle::R,
        g: vk::ComponentSwizzle::G,
        b: vk::ComponentSwizzle::B,
        a: vk::ComponentSwizzle::ONE,
    }
}

fn create_view(
    device: &Device,
    image: vk::Image,
    format: vk::Format,
    components: vk::ComponentMapping,
) -> Result<vk::ImageView, String> {
    unsafe {
        device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .components(components)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
                        .layer_count(1),
                ),
            None,
        )
    }
    .map_err(|error| format!("create image view: {error}"))
}

fn create_item_pipeline_layout(
    device: &Device,
    sampler_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, String> {
    unsafe {
        device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(std::slice::from_ref(&sampler_layout))
                .push_constant_ranges(std::slice::from_ref(
                    &vk::PushConstantRange::default()
                        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                        .offset(0)
                        .size(std::mem::size_of::<ItemPush>() as u32),
                )),
            None,
        )
    }
    .map_err(|error| format!("item pipeline layout: {error}"))
}

fn create_composite_pipeline_layout(
    device: &Device,
    sampler_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, String> {
    unsafe {
        device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(std::slice::from_ref(&sampler_layout)),
            None,
        )
    }
    .map_err(|error| format!("composite pipeline layout: {error}"))
}

#[allow(clippy::too_many_arguments)]
fn create_graphics_pipeline(
    device: &Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vert_bytes: &[u8],
    frag_bytes: &[u8],
    topology: vk::PrimitiveTopology,
    blend_enable: bool,
    label: &str,
) -> Result<vk::Pipeline, String> {
    let vert = create_shader(device, vert_bytes)?;
    let frag = create_shader(device, frag_bytes)?;
    let entry = CString::new("main").unwrap();
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert)
            .name(&entry),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag)
            .name(&entry),
    ];
    // With blending disabled the factor/op fields are ignored by Vulkan, so
    // the disabled arm keeps the previous default state byte-for-byte.
    let blend = if blend_enable {
        vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)
    } else {
        vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
    };
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let pipeline = unsafe {
        device
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                &[vk::GraphicsPipelineCreateInfo::default()
                    .stages(&stages)
                    .vertex_input_state(&vk::PipelineVertexInputStateCreateInfo::default())
                    .input_assembly_state(
                        &vk::PipelineInputAssemblyStateCreateInfo::default().topology(topology),
                    )
                    .viewport_state(
                        &vk::PipelineViewportStateCreateInfo::default()
                            .viewport_count(1)
                            .scissor_count(1),
                    )
                    .rasterization_state(
                        &vk::PipelineRasterizationStateCreateInfo::default()
                            .polygon_mode(vk::PolygonMode::FILL)
                            .cull_mode(vk::CullModeFlags::NONE)
                            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
                            .line_width(1.0),
                    )
                    .multisample_state(
                        &vk::PipelineMultisampleStateCreateInfo::default()
                            .rasterization_samples(vk::SampleCountFlags::TYPE_1),
                    )
                    .color_blend_state(
                        &vk::PipelineColorBlendStateCreateInfo::default()
                            .attachments(std::slice::from_ref(&blend)),
                    )
                    .dynamic_state(&dynamic)
                    .layout(layout)
                    .render_pass(render_pass)
                    .subpass(0)],
                None,
            )
            .map_err(|(_, error)| error)
    }
    .map(|pipelines| pipelines[0])
    .map_err(|error| format!("{label}: {error}"));
    unsafe {
        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
    }
    pipeline
}

fn create_item_pipeline(
    device: &Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, String> {
    create_graphics_pipeline(
        device,
        render_pass,
        layout,
        ITEM_VERT,
        ITEM_FRAG,
        vk::PrimitiveTopology::TRIANGLE_STRIP,
        true,
        "item pipeline",
    )
}

fn create_composite_pipeline(
    device: &Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, String> {
    create_graphics_pipeline(
        device,
        render_pass,
        layout,
        COMPOSITE_VERT,
        COMPOSITE_FRAG,
        vk::PrimitiveTopology::TRIANGLE_LIST,
        false,
        "composite pipeline",
    )
}

fn create_shader(device: &Device, bytes: &[u8]) -> Result<vk::ShaderModule, String> {
    let code = ash::util::read_spv(&mut Cursor::new(bytes))
        .map_err(|error| format!("read embedded SPIR-V: {error}"))?;
    unsafe { device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None) }
        .map_err(|error| format!("create shader module: {error}"))
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}

/// Barrier range for one plane of a DISJOINT multi-planar (NV12) image;
/// COLOR is invalid for such images.
fn plane_range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(aspect)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}

fn update_descriptor(
    device: &Device,
    set: vk::DescriptorSet,
    view: vk::ImageView,
    sampler: vk::Sampler,
    layout: vk::ImageLayout,
) {
    let image = vk::DescriptorImageInfo::default()
        .image_view(view)
        .sampler(sampler)
        .image_layout(layout);
    unsafe {
        device.update_descriptor_sets(
            &[vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&image))],
            &[],
        );
    }
}

fn create_buffer_resource(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    size: vk::DeviceSize,
    usage: vk::BufferUsageFlags,
    memory_flags: vk::MemoryPropertyFlags,
) -> Result<(vk::Buffer, vk::DeviceMemory), String> {
    let buffer = unsafe {
        device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )
    }
    .map_err(|error| format!("create upload buffer: {error}"))?;
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let memory_type = match find_memory_type(
        instance,
        physical,
        requirements.memory_type_bits,
        memory_flags,
    ) {
        Ok(index) => index,
        Err(error) => {
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(error);
        }
    };
    let memory = match unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )
    } {
        Ok(memory) => memory,
        Err(error) => {
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(format!("allocate upload buffer: {error}"));
        }
    };
    if let Err(error) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
        unsafe {
            device.free_memory(memory, None);
            device.destroy_buffer(buffer, None);
        }
        return Err(format!("bind upload buffer: {error}"));
    }
    Ok((buffer, memory))
}

fn create_sampled_image_resource(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    width: u32,
    height: u32,
) -> Result<(vk::Image, vk::DeviceMemory), String> {
    let image = unsafe {
        device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )
    }
    .map_err(|error| format!("create sampled image: {error}"))?;
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let memory_type = match find_memory_type(
        instance,
        physical,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) {
        Ok(index) => index,
        Err(error) => {
            unsafe { device.destroy_image(image, None) };
            return Err(error);
        }
    };
    let memory = match unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )
    } {
        Ok(memory) => memory,
        Err(error) => {
            unsafe { device.destroy_image(image, None) };
            return Err(format!("allocate sampled image: {error}"));
        }
    };
    if let Err(error) = unsafe { device.bind_image_memory(image, memory, 0) } {
        unsafe {
            device.free_memory(memory, None);
            device.destroy_image(image, None);
        }
        return Err(format!("bind sampled image: {error}"));
    }
    Ok((image, memory))
}

#[allow(clippy::too_many_arguments)]
fn upload_rgba_texture(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<ImportedFrame, String> {
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "static texture dimensions overflow".to_string())?;
    if width == 0 || height == 0 || rgba8.len() != expected {
        return Err("static texture has invalid dimensions or byte length".into());
    }
    let size = expected as vk::DeviceSize;
    let (staging_buffer, staging_memory) = create_buffer_resource(
        instance,
        device,
        physical,
        size,
        vk::BufferUsageFlags::TRANSFER_SRC,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    )?;
    let mapped = unsafe { device.map_memory(staging_memory, 0, size, vk::MemoryMapFlags::empty()) };
    let mapped = match mapped {
        Ok(mapped) => mapped,
        Err(error) => {
            unsafe {
                device.destroy_buffer(staging_buffer, None);
                device.free_memory(staging_memory, None);
            }
            return Err(format!("map static texture upload: {error}"));
        }
    };
    unsafe {
        std::ptr::copy_nonoverlapping(rgba8.as_ptr(), mapped.cast::<u8>(), expected);
        device.unmap_memory(staging_memory);
    }
    let (image, memory) =
        match create_sampled_image_resource(instance, device, physical, width, height) {
            Ok(resource) => resource,
            Err(error) => {
                unsafe {
                    device.destroy_buffer(staging_buffer, None);
                    device.free_memory(staging_memory, None);
                }
                return Err(error);
            }
        };
    let mut command_buffer = vk::CommandBuffer::null();
    let mut fence = vk::Fence::null();
    let operation = (|| -> Result<(), String> {
        command_buffer = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|error| format!("allocate static upload command: {error}"))?[0];
        fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|error| format!("create static upload fence: {error}"))?;
        unsafe {
            device
                .begin_command_buffer(
                    command_buffer,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|error| format!("begin static texture upload: {error}"))?;
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(color_range())],
            );
            device.cmd_copy_buffer_to_image(
                command_buffer,
                staging_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .mip_level(0)
                            .base_array_layer(0)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width,

                        height,
                        depth: 1,
                    })],
            );
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(color_range())],
            );
            device
                .end_command_buffer(command_buffer)
                .map_err(|error| format!("end static texture upload: {error}"))?;
            device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default()
                        .command_buffers(std::slice::from_ref(&command_buffer))],
                    fence,
                )
                .map_err(|error| format!("submit static texture upload: {error}"))?;
            device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|error| format!("wait static texture upload: {error}"))?;
        }
        Ok(())
    })();
    unsafe {
        if fence != vk::Fence::null() {
            device.destroy_fence(fence, None);
        }
        if command_buffer != vk::CommandBuffer::null() {
            device.free_command_buffers(command_pool, &[command_buffer]);
        }
        device.destroy_buffer(staging_buffer, None);
        device.free_memory(staging_memory, None);
    }
    if let Err(error) = operation {
        unsafe {
            device.destroy_image(image, None);
            device.free_memory(memory, None);
        }
        return Err(error);
    }
    let view = match create_view(
        device,
        image,
        vk::Format::R8G8B8A8_UNORM,
        vk::ComponentMapping::default(),
    ) {
        Ok(view) => view,
        Err(error) => {
            unsafe {
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            return Err(error);
        }
    };
    Ok(ImportedFrame {
        image,
        memories: vec![memory],
        view,
    })
}

fn import_media_frame(
    instance: &Instance,
    device: &Device,
    external_fd: &khr::external_memory_fd::Device,
    physical: vk::PhysicalDevice,
    conversion: vk::SamplerYcbcrConversion,
    frame: &MediaVideoFrame,
) -> Result<ImportedFrame, String> {
    if frame.drm_format != DRM_FORMAT_NV12 {
        return Err(format!(
            "unsupported media DMA-BUF DRM format: {:#x}",
            frame.drm_format
        ));
    }
    if frame.modifier == DRM_FORMAT_MOD_INVALID {
        return Err("media DMA-BUF modifier is invalid".into());
    }
    let planes = frame.dma_buf_planes()?;
    if planes.len() != 2 {
        return Err(format!(
            "NV12 DMA-BUF requires two planes, received {}",
            planes.len()
        ));
    }
    let layouts = planes
        .iter()
        .enumerate()
        .map(|(index, plane)| vk::SubresourceLayout {
            offset: u64::from(plane.offset),
            size: u64::from(plane.stride)
                * u64::from(if index == 0 {
                    frame.height
                } else {
                    frame.height.div_ceil(2)
                }),
            row_pitch: u64::from(plane.stride),
            array_pitch: 0,
            depth_pitch: 0,
        })
        .collect::<Vec<_>>();
    let format = vk::Format::G8_B8R8_2PLANE_420_UNORM;
    let mut external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(frame.modifier)
        .plane_layouts(&layouts);
    let image = unsafe {
        device.create_image(
            &vk::ImageCreateInfo::default()
                .flags(vk::ImageCreateFlags::DISJOINT)
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width: frame.width,
                    height: frame.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external)
                .push_next(&mut modifier_info),
            None,
        )
    }
    .map_err(|error| format!("create NV12 DMA-BUF image: {error}"))?;
    let mut memories = Vec::with_capacity(2);
    let mut view = vk::ImageView::null();
    let operation = (|| -> Result<(), String> {
        for (index, plane) in planes.into_iter().enumerate() {
            let aspect = if index == 0 {
                vk::ImageAspectFlags::PLANE_0
            } else {
                vk::ImageAspectFlags::PLANE_1
            };
            let mut plane_requirements =
                vk::ImagePlaneMemoryRequirementsInfo::default().plane_aspect(aspect);
            let requirements_info = vk::ImageMemoryRequirementsInfo2::default()
                .image(image)
                .push_next(&mut plane_requirements);
            let mut requirements = vk::MemoryRequirements2::default();
            unsafe {
                device.get_image_memory_requirements2(&requirements_info, &mut requirements);
            }
            let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
            unsafe {
                external_fd
                    .get_memory_fd_properties(
                        vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                        plane.fd.as_raw_fd(),
                        &mut fd_properties,
                    )
                    .map_err(|error| format!("media DMA-BUF memory properties: {error}"))?;
            }
            let memory_type = find_memory_type_any(
                instance,
                physical,
                requirements.memory_requirements.memory_type_bits & fd_properties.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )?;
            let raw_fd = plane.fd.into_raw_fd();
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(raw_fd);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let allocation = unsafe {
                device.allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirements.memory_requirements.size)
                        .memory_type_index(memory_type)
                        .push_next(&mut import)
                        .push_next(&mut dedicated),
                    None,
                )
            };
            let memory = match allocation {
                Ok(memory) => memory,
                Err(error) => {
                    unsafe { drop(std::os::fd::OwnedFd::from_raw_fd(raw_fd)) };
                    return Err(format!("import media DMA-BUF plane: {error}"));
                }
            };
            memories.push(memory);
            let mut plane_bind = vk::BindImagePlaneMemoryInfo::default().plane_aspect(aspect);
            let bind = vk::BindImageMemoryInfo::default()
                .image(image)
                .memory(memory)
                .memory_offset(0)
                .push_next(&mut plane_bind);
            unsafe { device.bind_image_memory2(std::slice::from_ref(&bind)) }
                .map_err(|error| format!("bind media DMA-BUF plane: {error}"))?;
        }
        let mut conversion_info = vk::SamplerYcbcrConversionInfo::default().conversion(conversion);
        view = unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range())
                    .push_next(&mut conversion_info),
                None,
            )
        }
        .map_err(|error| format!("create media YCbCr image view: {error}"))?;
        Ok(())
    })();
    if let Err(error) = operation {
        unsafe {
            if view != vk::ImageView::null() {
                device.destroy_image_view(view, None);
            }
            for memory in memories {
                device.free_memory(memory, None);
            }
            device.destroy_image(image, None);
        }
        return Err(error);
    }
    Ok(ImportedFrame {
        image,
        memories,
        view,
    })
}
fn import_frame(
    instance: &Instance,
    device: &Device,
    external_fd: &khr::external_memory_fd::Device,
    physical: vk::PhysicalDevice,
    frame: &CapturedFrame,
) -> Result<ImportedFrame, String> {
    let FrameMemory::DmaBuf { planes } = &frame.memory;
    let format =
        drm_to_vk(frame.drm_format).ok_or_else(|| "unsupported DMA-BUF DRM format".to_string())?;
    // X-format DRM buffers leave the alpha byte undefined; swizzling the
    // view's alpha to a constant 1 keeps sampled opacity well-defined.
    let components =
        if frame.drm_format == DRM_FORMAT_XRGB8888 || frame.drm_format == DRM_FORMAT_XBGR8888 {
            opaque_alpha_components()
        } else {
            vk::ComponentMapping::default()
        };
    if frame.modifier == DRM_FORMAT_MOD_INVALID {
        return Err("DMA-BUF modifier was not fixated by PipeWire".into());
    }
    if planes.len() != 1 {
        return Err("packed RGB DMA-BUF must expose exactly one image plane".into());
    }
    let modifier = frame.modifier;
    let plane = &planes[0];
    let image_layout = vk::SubresourceLayout {
        offset: u64::from(plane.offset),
        size: u64::from(plane.stride) * u64::from(frame.height),
        row_pitch: u64::from(plane.stride),
        array_pitch: 0,
        depth_pitch: 0,
    };
    let mut external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(modifier)
        .plane_layouts(std::slice::from_ref(&image_layout));
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: frame.width,
            height: frame.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external)
        .push_next(&mut modifier_info);
    let image = unsafe { device.create_image(&image_info, None) }
        .map_err(|error| format!("create imported DMA-BUF image: {error}"))?;
    let mut memory = vk::DeviceMemory::null();
    let result = (|| -> Result<vk::ImageView, String> {
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
        unsafe {
            external_fd
                .get_memory_fd_properties(
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    plane.fd.as_raw_fd(),
                    &mut fd_properties,
                )
                .map_err(|error| format!("DMA-BUF memory properties: {error}"))?;
        }
        let memory_type = find_memory_type_any(
            instance,
            physical,
            requirements.memory_type_bits & fd_properties.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let raw_fd = plane
            .fd
            .try_clone()
            .map_err(|error| format!("duplicate DMA-BUF fd: {error}"))?
            .into_raw_fd();
        let mut import = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(raw_fd);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let allocation = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type)
                    .push_next(&mut import)
                    .push_next(&mut dedicated),
                None,
            )
        };
        memory = match allocation {
            Ok(memory) => memory,
            Err(error) => {
                // Vulkan takes fd ownership only after successful import.
                unsafe {
                    drop(std::os::fd::OwnedFd::from_raw_fd(raw_fd));
                }
                return Err(format!("import DMA-BUF memory: {error}"));
            }
        };
        unsafe { device.bind_image_memory(image, memory, 0) }
            .map_err(|error| format!("bind imported DMA-BUF: {error}"))?;
        create_view(device, image, format, components)
    })();
    match result {
        Ok(view) => Ok(ImportedFrame {
            image,
            memories: vec![memory],
            view,
        }),
        Err(error) => {
            unsafe {
                if memory != vk::DeviceMemory::null() {
                    device.free_memory(memory, None);
                }
                device.destroy_image(image, None);
            }
            Err(error)
        }
    }
}

fn drm_to_vk(format: u32) -> Option<vk::Format> {
    match format {
        DRM_FORMAT_XRGB8888 => Some(vk::Format::B8G8R8A8_UNORM),
        DRM_FORMAT_ARGB8888 => Some(vk::Format::B8G8R8A8_UNORM),
        DRM_FORMAT_XBGR8888 => Some(vk::Format::R8G8B8A8_UNORM),
        DRM_FORMAT_ABGR8888 => Some(vk::Format::R8G8B8A8_UNORM),
        _ => None,
    }
}

fn find_memory_type(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    bits: u32,
    required: vk::MemoryPropertyFlags,
) -> Result<u32, String> {
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    (0..properties.memory_type_count)
        .find(|index| {
            (bits & (1 << index)) != 0
                && properties.memory_types[*index as usize]
                    .property_flags
                    .contains(required)
        })
        .ok_or_else(|| {
            format!(
                "no Vulkan memory type with bits {bits:#b} satisfying {:#x}",
                required.as_raw()
            )
        })
}

/// Lenient variant for IMPORTED memory (dma-buf fds): the export-constraint
/// intersection with the fd properties can leave types whose property flags
/// miss the requested mask even though the kernel buffer works as-is, so any
/// type within `bits` is acceptable as a fallback.
fn find_memory_type_any(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    bits: u32,
    required: vk::MemoryPropertyFlags,
) -> Result<u32, String> {
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    (0..properties.memory_type_count)
        .find(|index| {
            (bits & (1 << index)) != 0
                && properties.memory_types[*index as usize]
                    .property_flags
                    .contains(required)
        })
        .or_else(|| (0..properties.memory_type_count).find(|index| (bits & (1 << index)) != 0))
        .ok_or_else(|| "no compatible Vulkan memory type".into())
}

fn item_push(transform: Transform, width: u32, height: u32, mode: u32) -> ItemPush {
    let radians = transform.rotation_degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    let crop_left = transform.crop_left.clamp(0.0, transform.width);
    let crop_right = transform.crop_right.clamp(0.0, transform.width - crop_left);
    let crop_top = transform.crop_top.clamp(0.0, transform.height);
    let crop_bottom = transform
        .crop_bottom
        .clamp(0.0, transform.height - crop_top);
    let inner_width = (transform.width - crop_left - crop_right).max(1.0);
    let inner_height = (transform.height - crop_top - crop_bottom).max(1.0);
    ItemPush {
        center: [
            transform.x + transform.width * 0.5,
            transform.y + transform.height * 0.5,
        ],
        half_extent: [transform.width * 0.5, transform.height * 0.5],
        cos_sin: [cos, sin],
        uv_scale: [
            inner_width / transform.width,
            inner_height / transform.height,
        ],
        uv_offset: [crop_left / transform.width, crop_top / transform.height],
        output_size: [width as f32, height as f32],
        opacity: transform.opacity,
        mode,
    }
}

fn composite_target(
    device: &Device,
    command_buffer: vk::CommandBuffer,
    target: &SwapchainTarget,
    image_index: u32,
    scene_extent: vk::Extent2D,
) {
    unsafe {
        device.cmd_begin_render_pass(
            command_buffer,
            &vk::RenderPassBeginInfo::default()
                .render_pass(target.render_pass)
                .framebuffer(target.framebuffers[image_index as usize])
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: target.extent,
                })
                .clear_values(&[vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [0.0, 0.0, 0.0, 1.0],
                    },
                }]),
            vk::SubpassContents::INLINE,
        );
        let scale = (target.extent.width as f32 / scene_extent.width as f32)
            .min(target.extent.height as f32 / scene_extent.height as f32);
        let width = (scene_extent.width as f32 * scale).max(1.0);
        let height = (scene_extent.height as f32 * scale).max(1.0);
        let x = (target.extent.width as f32 - width) * 0.5;
        let y = (target.extent.height as f32 - height) * 0.5;
        device.cmd_set_viewport(
            command_buffer,
            0,
            &[vk::Viewport {
                x,
                y,
                width,
                height,
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        );
        device.cmd_set_scissor(
            command_buffer,
            0,
            &[vk::Rect2D {
                offset: vk::Offset2D {
                    x: x.round() as i32,
                    y: y.round() as i32,
                },
                extent: vk::Extent2D {
                    width: width.round() as u32,
                    height: height.round() as u32,
                },
            }],
        );
        device.cmd_bind_pipeline(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            target.pipeline,
        );
        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            target.pipeline_layout,
            0,
            &[target.descriptor_set],
            &[],
        );
        device.cmd_draw(command_buffer, 3, 1, 0, 0);
        device.cmd_end_render_pass(command_buffer);
    }
}

/// Interruptible backoff between consecutive compositor creation failures
/// (mirrors the Windows render loop): 200 ms per failure, capped at 2 s.
fn sleep_backoff(stop: &AtomicBool, failures: u32) {
    let backoff = Duration::from_millis(200)
        .saturating_mul(failures)
        .min(Duration::from_secs(2));
    let deadline = Instant::now() + backoff;
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
    }
}

fn acquire(target: &SwapchainTarget) -> Result<u32, RenderError> {
    unsafe {
        target
            .loader
            .acquire_next_image(
                target.swapchain,
                // Bounded wait: a hidden or unmapped window can stall the
                // presentation engine indefinitely. Timing out lets the render
                // loop skip the frame and re-check its stop flag so shutdown's
                // join always terminates.
                100_000_000,
                target.available,
                vk::Fence::null(),
            )
            .map(|(index, _)| index)
            .map_err(|error| match error {
                vk::Result::TIMEOUT => RenderError::AcquireTimeout,
                other => RenderError::Vk(other),
            })
    }
}

fn present(target: &SwapchainTarget, queue: vk::Queue, index: u32) -> Result<(), RenderError> {
    unsafe {
        target
            .loader
            .queue_present(
                queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(std::slice::from_ref(&target.rendered[index as usize]))
                    .swapchains(std::slice::from_ref(&target.swapchain))
                    .image_indices(std::slice::from_ref(&index)),
            )
            .map(|_| ())
            .map_err(RenderError::Vk)
    }
}

fn destroy_imported(device: &Device, imported: ImportedFrame) {
    unsafe {
        device.destroy_image_view(imported.view, None);
        device.destroy_image(imported.image, None);
        for memory in imported.memories {
            device.free_memory(memory, None);
        }
    }
}

fn parse_color(value: &str) -> [f32; 4] {
    const FALLBACK: [f32; 4] = [0.02, 0.02, 0.03, 1.0];
    // Dunkler Fallback bleibt modulspezifisch: Ein Wert ohne '#'-Präfix
    // erreicht den geteilten text_raster-Parser gar nicht erst.
    if value.strip_prefix('#').is_none() {
        return FALLBACK;
    }
    // Der geteilte Parser akzeptiert 6- und 8-stelliges Hex (8-stelliges
    // RGBA ist für den Szenen-Hintergrund bewusst eine Obermenge) und
    // fallbackt selbst auf Weiß bei ungültigen Ziffern.
    let [r, g, b, a] = parse_text_color(value);
    [
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        f32::from(a) / 255.0,
    ]
}

fn destroy_scene(device: &Device, scene: &mut SceneTarget) {
    unsafe {
        destroy_scene_attachments(device, scene);
        device.destroy_render_pass(scene.render_pass, None);
    }
}
/// Frees a scene target's image attachments but keeps `render_pass` alive:
/// the replacement target reuses the same pass, so pipelines stay untouched
/// across a selective scene recreation.
fn destroy_scene_attachments(device: &Device, scene: &mut SceneTarget) {
    unsafe {
        device.destroy_framebuffer(scene.framebuffer, None);
        device.destroy_image_view(scene.view, None);
        device.destroy_image(scene.image, None);
        device.free_memory(scene.memory, None);
    }
}
fn destroy_target(
    device: &Device,
    surface_loader: &khr::surface::Instance,
    target: &mut SwapchainTarget,
) {
    destroy_swapchain_state(device, target);
    unsafe { surface_loader.destroy_surface(target.surface, None) };
}
/// Frees every swapchain-owned resource except the surface, which a
/// replacement target adopts (see `recreate_swapchain`).
fn destroy_swapchain_state(device: &Device, target: &mut SwapchainTarget) {
    unsafe {
        for framebuffer in target.framebuffers.drain(..) {
            device.destroy_framebuffer(framebuffer, None);
        }
        for view in target.views.drain(..) {
            device.destroy_image_view(view, None);
        }
        device.destroy_pipeline(target.pipeline, None);
        device.destroy_pipeline_layout(target.pipeline_layout, None);
        device.destroy_render_pass(target.render_pass, None);
        device.destroy_semaphore(target.available, None);
        for semaphore in target.rendered.drain(..) {
            device.destroy_semaphore(semaphore, None);
        }
        target.loader.destroy_swapchain(target.swapchain, None);
    }
}
