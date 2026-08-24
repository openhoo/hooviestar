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
        PipeWirePortalLink,
    },
    linux_media::{LinuxMedia, MediaCommand, MediaNotice, MediaVideoFrame},
    text_raster::{TextCache, TextKey, parse_color as parse_text_color},
};
use crate::{
    audio::MediaAudioBus,
    engine::{DeviceRecoveryPhase, EngineEvent, NativeSurfaceKind, NativeSurfaces},
    project::{ProjectV1, Source, Transform},
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
    fence: vk::Fence,
    descriptor_set: vk::DescriptorSet,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
}

struct ImportedFrame {
    image: vk::Image,
    memories: Vec<vk::DeviceMemory>,
    view: vk::ImageView,
    sampler: Option<vk::Sampler>,
    conversion: Option<vk::SamplerYcbcrConversion>,
}

struct CachedExternalFrame {
    sequence: u64,
    texture: ImportedFrame,
}

struct CachedStaticTexture {
    key: String,
    texture: ImportedFrame,
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
    scene: SceneTarget,
    scene_sampler: vk::Sampler,
    item_sampler: vk::Sampler,
    descriptor_pool: vk::DescriptorPool,
    sampler_layout: vk::DescriptorSetLayout,
    item_descriptors: Vec<vk::DescriptorSet>,
    placeholder: ImportedFrame,
    static_textures: HashMap<Uuid, CachedStaticTexture>,
    image_cache: ImageCache,
    media_textures: HashMap<Uuid, CachedExternalFrame>,
    text_cache: TextCache,
    static_failures: HashMap<Uuid, String>,
    portal_textures: HashMap<Uuid, CachedExternalFrame>,
    item_pipeline: vk::Pipeline,
    item_pipeline_layout: vk::PipelineLayout,
    program: SwapchainTarget,
    preview: SwapchainTarget,
    output_width: u32,
    output_height: u32,
}

impl VulkanCompositor {
    fn create(surfaces: NativeSurfaces, width: u32, height: u32) -> Result<Self, String> {
        let (display, program_window, preview_window) = raw_handles(surfaces)?;
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
        let surface_loader = khr::surface::Instance::new(&entry, &instance);
        let program_surface =
            unsafe { ash_window::create_surface(&entry, &instance, display, program_window, None) }
                .map_err(|error| format!("Program surface: {error}"))?;
        let preview_surface =
            unsafe { ash_window::create_surface(&entry, &instance, display, preview_window, None) }
                .map_err(|error| format!("Preview surface: {error}"))?;
        let (physical, queue_family) =
            choose_device(&instance, &surface_loader, program_surface, preview_surface)?;
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
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&extensions);
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(|error| format!("create Vulkan device: {error}"))?;
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let external_memory_fd = khr::external_memory_fd::Device::new(&instance, &device);
        let command_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .map_err(|error| format!("create command pool: {error}"))?;
        let command_buffer = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|error| format!("allocate command buffer: {error}"))?[0];
        let placeholder = upload_rgba_texture(
            &instance,
            &device,
            physical,
            queue,
            command_pool,
            1,
            1,
            &[0, 0, 0, 0],
        )?;

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
        let descriptor_pool = unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets((MAX_SCENE_ITEMS + 1) as u32)
                    .pool_sizes(&[vk::DescriptorPoolSize::default()
                        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count((MAX_SCENE_ITEMS + 1) as u32)]),
                None,
            )
        }
        .map_err(|error| format!("descriptor pool: {error}"))?;
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
        let item_descriptors = sets[1..].to_vec();
        let scene_sampler = create_sampler(&device)?;
        let item_sampler = create_sampler(&device)?;
        let scene_render_pass = create_render_pass(&device, vk::Format::R16G16B16A16_SFLOAT)?;
        let scene = create_scene_target(
            &instance,
            &device,
            physical,
            scene_render_pass,
            width,
            height,
        )?;
        update_descriptor(
            &device,
            scene_descriptor,
            scene.view,
            scene_sampler,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        let item_render_pass = scene_render_pass;
        let item_pipeline_layout = create_item_pipeline_layout(&device, sampler_layout)?;
        let item_pipeline = create_item_pipeline(&device, item_render_pass, item_pipeline_layout)?;
        let program = create_swapchain(
            &instance,
            &device,
            physical,
            &surface_loader,
            program_surface,
            surfaces.program_width.max(1),
            surfaces.program_height.max(1),
            sampler_layout,
            scene_descriptor,
        )?;
        let preview = create_swapchain(
            &instance,
            &device,
            physical,
            &surface_loader,
            preview_surface,
            surfaces.preview_width.max(1),
            surfaces.preview_height.max(1),
            sampler_layout,
            scene_descriptor,
        )?;
        Ok(Self {
            instance,
            surface_loader,
            _entry: entry,
            external_memory_fd,
            physical,
            device,
            queue,
            queue_family,
            command_pool,
            command_buffer,
            scene,
            scene_sampler,
            item_sampler,
            descriptor_pool,
            placeholder,
            static_textures: HashMap::new(),
            image_cache: ImageCache::default(),
            text_cache: TextCache::default(),
            static_failures: HashMap::new(),
            media_textures: HashMap::new(),
            sampler_layout,
            item_descriptors,
            portal_textures: HashMap::new(),
            item_pipeline,
            item_pipeline_layout,
            program,
            preview,
            output_width: width,
            output_height: height,
        })
    }

    fn synchronize_portal_textures(
        &mut self,
        frames: &HashMap<Uuid, CapturedFrame>,
    ) -> Result<(), RenderError> {
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
        }
        for (source_id, frame) in frames {
            if self
                .portal_textures
                .get(source_id)
                .is_some_and(|cached| cached.sequence == frame.sequence)
            {
                continue;
            }
            let texture = import_frame(
                &self.instance,
                &self.device,
                &self.external_memory_fd,
                self.physical,
                frame,
            )
            .map_err(|reason| RenderError::Import {
                source_id: Some(*source_id),
                reason,
            })?;
            if let Some(previous) = self.portal_textures.insert(
                *source_id,
                CachedExternalFrame {
                    sequence: frame.sequence,
                    texture,
                },
            ) {
                destroy_imported(&self.device, previous.texture);
            }
        }
        Ok(())
    }
    fn synchronize_static_textures(
        &mut self,
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
        let mut desired_text = HashSet::new();
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
                    self.image_cache
                        .get_or_decode(path)
                        .inspect(|decoded| {
                            desired_images.insert(decoded.path.clone());
                        })
                        .map(|decoded| {
                            let decoded = decoded.clone();
                            (
                                *id,
                                format!(
                                    "image:{}:{}:{}",
                                    decoded.path.display(),
                                    decoded.fingerprint.0,
                                    decoded.fingerprint.1
                                ),
                                decoded.width,
                                decoded.height,
                                decoded.rgba8,
                            )
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
                    let key = TextKey {
                        text: text.clone(),
                        family: font_family.clone(),
                        size_bits: font_size_px.to_bits(),
                        weight: *font_weight,
                        color: parse_text_color(color),
                        background: parse_text_color(background_color),
                        align: align.clone().into(),
                        width: item.transform.width.round().max(1.0) as u32,
                        height: item.transform.height.round().max(1.0) as u32,
                    };
                    desired_text.insert(key.clone());

                    let cache_key = format!("text:{key:?}");
                    self.text_cache.rasterize(key).map(|raster| {
                        let raster = raster.clone();
                        (*id, cache_key, raster.width, raster.height, raster.rgba8)
                    })
                }
                _ => continue,
            };
            let result = prepared.map_err(|error| error.to_string()).and_then(
                |(source_id, key, width, height, rgba8)| {
                    if self
                        .static_textures
                        .get(&source_id)
                        .is_some_and(|cached| cached.key == key)
                    {
                        return Ok((source_id, false));
                    }
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
                            .insert(source_id, CachedStaticTexture { key, texture })
                        {
                            destroy_imported(&self.device, previous.texture);
                        }
                        (source_id, true)
                    })
                },
            );
            match result {
                Ok((source_id, changed)) => {
                    let recovered = self.static_failures.remove(&source_id).is_some();
                    if changed || recovered {
                        let _ = events.send(EngineEvent::SourceAvailable { source_id });
                    }
                }
                Err(reason) => {
                    let source_id = item.source_id;
                    if self.static_failures.get(&source_id) != Some(&reason) {
                        self.static_failures.insert(source_id, reason.clone());
                        let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                    }
                    if let Some(previous) = self.static_textures.remove(&source_id) {
                        destroy_imported(&self.device, previous.texture);
                    }
                }
            }
        }
        self.text_cache.retain(|key| desired_text.contains(key));
        self.image_cache
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
            self.static_failures.remove(&source_id);
        }
    }
    fn synchronize_media_textures(
        &mut self,
        frames: &HashMap<Uuid, MediaVideoFrame>,
    ) -> Result<(), RenderError> {
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
        }
        for (source_id, frame) in frames {
            if self
                .media_textures
                .get(source_id)
                .is_some_and(|cached| cached.sequence == frame.sequence)
            {
                continue;
            }
            let texture = import_media_frame(
                &self.instance,
                &self.device,
                &self.external_memory_fd,
                self.physical,
                frame,
            )
            .map_err(|reason| RenderError::Import {
                source_id: Some(*source_id),
                reason,
            })?;
            if let Some(previous) = self.media_textures.insert(
                *source_id,
                CachedExternalFrame {
                    sequence: frame.sequence,
                    texture,
                },
            ) {
                destroy_imported(&self.device, previous.texture);
            }
        }
        Ok(())
    }

    fn remove_external_texture(&mut self, source_id: Uuid) {
        if let Some(cached) = self.portal_textures.remove(&source_id) {
            destroy_imported(&self.device, cached.texture);
        }
        if let Some(cached) = self.media_textures.remove(&source_id) {
            destroy_imported(&self.device, cached.texture);
        }
    }

    fn render(
        &mut self,
        project: &ProjectV1,
        frames: &mut HashMap<Uuid, CapturedFrame>,
        media_frames: &HashMap<Uuid, MediaVideoFrame>,
        events: &std::sync::mpsc::Sender<EngineEvent>,
    ) -> Result<HashSet<Uuid>, RenderError> {
        let scene = project
            .scenes
            .iter()
            .find(|scene| scene.id == project.active_scene_id)
            .ok_or_else(|| RenderError::Import {
                source_id: None,
                reason: "active scene is missing".into(),
            })?;
        let (program_index, preview_index);
        let mut used = HashSet::new();
        unsafe {
            self.device
                .wait_for_fences(&[self.program.fence], true, u64::MAX)
                .map_err(RenderError::Vk)?;
        }
        self.synchronize_portal_textures(frames)?;
        self.synchronize_media_textures(media_frames)?;
        self.synchronize_static_textures(project, events);
        let mut external_images = [vk::Image::null(); MAX_SCENE_ITEMS];
        let mut external_count = 0usize;
        for item in scene.items.iter().filter(|item| item.visible) {
            let cached = self
                .portal_textures
                .get(&item.source_id)
                .or_else(|| self.media_textures.get(&item.source_id));
            let Some(cached) = cached else {
                continue;
            };
            let image = cached.texture.image;
            if external_count < MAX_SCENE_ITEMS
                && !external_images[..external_count].contains(&image)
            {
                external_images[external_count] = image;
                external_count += 1;
            }
        }
        unsafe {
            self.device
                .reset_fences(&[self.program.fence])
                .map_err(RenderError::Vk)?;
            program_index = acquire(&self.device, &self.program)?;
            preview_index = acquire(&self.device, &self.preview)?;
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(RenderError::Vk)?;
            self.device
                .begin_command_buffer(self.command_buffer, &vk::CommandBufferBeginInfo::default())
                .map_err(RenderError::Vk)?;
            let acquire_barriers = std::array::from_fn::<_, MAX_SCENE_ITEMS, _>(|index| {
                vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .dst_queue_family_index(self.queue_family)
                    .image(external_images[index])
                    .subresource_range(color_range())
            });
            if external_count > 0 {
                self.device.cmd_pipeline_barrier(
                    self.command_buffer,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::BY_REGION,
                    &[],
                    &[],
                    &acquire_barriers[..external_count],
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
                let item_descriptor = self.item_descriptors[draw_index];
                let mode = if let Some(cached) = self.portal_textures.get(&item.source_id) {
                    update_descriptor(
                        &self.device,
                        item_descriptor,
                        cached.texture.view,
                        self.item_sampler,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    used.insert(item.source_id);
                    0
                } else if let Some(cached) = self.media_textures.get(&item.source_id) {
                    update_descriptor(
                        &self.device,
                        item_descriptor,
                        cached.texture.view,
                        cached.texture.sampler.unwrap_or(self.item_sampler),
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    0
                } else if let Some(cached) = self.static_textures.get(&item.source_id) {
                    update_descriptor(
                        &self.device,
                        item_descriptor,
                        cached.texture.view,
                        self.item_sampler,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    0
                } else {
                    update_descriptor(
                        &self.device,
                        item_descriptor,
                        self.placeholder.view,
                        self.item_sampler,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    1
                };
                self.device.cmd_bind_descriptor_sets(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.item_pipeline_layout,
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
                    self.item_pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytes,
                );
                self.device.cmd_draw(self.command_buffer, 4, 1, 0, 0);
            }
            self.device.cmd_end_render_pass(self.command_buffer);
            let release_barriers = std::array::from_fn::<_, MAX_SCENE_ITEMS, _>(|index| {
                vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_access_mask(vk::AccessFlags::SHADER_READ)
                    .src_queue_family_index(self.queue_family)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                    .image(external_images[index])
                    .subresource_range(color_range())
            });
            if external_count > 0 {
                self.device.cmd_pipeline_barrier(
                    self.command_buffer,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::BY_REGION,
                    &[],
                    &[],
                    &release_barriers[..external_count],
                );
            }
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
                    self.program.fence,
                )
                .map_err(RenderError::Vk)?;
        }
        present(&self.program, self.queue, program_index)?;
        present(&self.preview, self.queue, preview_index)?;
        unsafe {
            self.device
                .wait_for_fences(&[self.program.fence], true, u64::MAX)
                .map_err(RenderError::Vk)?;
        }
        let _ = events;
        Ok(used)
    }
}

impl Drop for VulkanCompositor {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
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
}

#[derive(Debug)]
enum RenderError {
    Vk(vk::Result),
    Import {
        source_id: Option<Uuid>,
        reason: String,
    },
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
                let mut compositor = Some(match VulkanCompositor::create(
                    surfaces,
                    initial.width,
                    initial.height,
                ) {
                    Ok(value) => {
                        let _ = ready_tx.send(Ok(()));
                        value
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.clone()));
                        let _ = events.send(EngineEvent::DeviceRecovery {
                            phase: DeviceRecoveryPhase::Failed,
                            detail: Some(error),
                        });
                        return;
                    }
                });
                let mut capture = match CaptureHandle::spawn() {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = events.send(EngineEvent::EngineError {
                            message: error.to_string(),
                        });
                        return;
                    }
                };
                let media = match LinuxMedia::start(events.clone()) {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = events.send(EngineEvent::EngineError { message: format!("GStreamer unavailable: {error}") });
                        return;
                    }
                };
                let mut media_sources: HashMap<Uuid, MediaRuntimeBinding> = HashMap::new();
                let mut media_frames: HashMap<Uuid, MediaVideoFrame> = HashMap::new();
                let mut frames: HashMap<Uuid, CapturedFrame> = HashMap::new();
                let mut nodes: HashMap<Uuid, u32> = HashMap::new();
                let mut available = HashSet::new();
                let mut last_frame: HashMap<Uuid, Instant> = HashMap::new();
                let mut generation = 0;
                let mut output = initial;
                let mut deadline = Instant::now();
                while !thread_stop.load(Ordering::Acquire) {
                    let snapshot = project.read().clone();
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
                                media.remove(*id, &media_audio);
                            }
                            media_sources.remove(id);
                            media_frames.remove(id);
                        }
                        if !media_sources.contains_key(id) {
                            let opened = match media.open(*id, path, *looped, &media_audio) {
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
                        let should_play =
                            control.playing && (visible || *continue_when_hidden);
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
                        if let Some(position) = control.seek_seconds {
                            media.command(*id, MediaCommand::Seek(position));
                            media_control
                                .write()
                                .entry(*id)
                                .or_default()
                                .seek_seconds = None;
                        }
                    }
                    for id in media_sources.keys().copied().collect::<Vec<_>>() {
                        if !wanted_media.contains(&id) {
                            if media_sources.get(&id).is_some_and(|runtime| runtime.opened) {
                                media.remove(id, &media_audio);
                            }
                            media_sources.remove(&id);
                            media_frames.remove(&id);
                        }
                    }
                    for notice in media.drain_notices() {
                        match notice {
                            MediaNotice::State { source_id, state } => {
                                let _ = events.send(EngineEvent::MediaState { source_id, state });
                            }
                            MediaNotice::Unsupported { source_id, reason } => {
                                media.remove(source_id, &media_audio);
                                if let Some(runtime) = media_sources.get_mut(&source_id) {
                                    runtime.opened = false;
                                    runtime.playing = false;
                                }
                                media_frames.remove(&source_id);
                                available.remove(&source_id);
                                let _ = events.send(EngineEvent::UnsupportedMedia {
                                    source_id,
                                    reason,
                                });
                            }
                            MediaNotice::Video(frame) => {
                                let source_id = frame.source_id;
                                media_frames.insert(source_id, frame);
                                if available.insert(source_id) {
                                    let _ = events.send(EngineEvent::SourceAvailable { source_id });
                                }
                            }
                        }
                    }
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
                        drop(compositor.take());
                        compositor = Some(match VulkanCompositor::create(
                            current_surfaces,
                            snapshot.output.width,
                            snapshot.output.height,
                        ) {
                            Ok(next) => next,
                            Err(error) => {
                                let _ = events.send(EngineEvent::DeviceRecovery {
                                    phase: DeviceRecoveryPhase::Failed,
                                    detail: Some(error),
                                });
                                break;
                            }
                        });
                        output = snapshot.output.clone();
                        let _ = events.send(EngineEvent::DeviceRecovery {
                            phase: DeviceRecoveryPhase::Succeeded,
                            detail: None,
                        });
                    }
                    let current_generation = portal.generation();
                    if current_generation != generation {
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
                            Err(error) => {
                                let _ = events.send(EngineEvent::EngineError {
                                    message: error.to_string(),
                                });
                                break;
                            }
                        };
                        available.clear();
                        generation = current_generation;
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
                        if nodes.get(source_id) != Some(node) {
                            if nodes.contains_key(source_id) {
                                capture.stop(*source_id);
                            }
                            let remote = portal.take_remote();
                            capture.start(*source_id, *node, remote);
                            nodes.insert(*source_id, *node);
                        }
                    }
                    for source_id in nodes.keys().copied().collect::<Vec<_>>() {
                        if !wanted.contains_key(&source_id) {
                            capture.stop(source_id);
                            nodes.remove(&source_id);
                            if let Some(frame) = frames.remove(&source_id) {
                                capture.return_buffer(source_id, frame.buffer_token);
                            }
                        }
                    }
                    while let Ok(message) = capture.try_recv() {
                        match message {
                            FrameMessage::Frame(frame) => {
                                let source_id = frame.source_id;
                                if let Some(old) = frames.insert(source_id, frame) {
                                    capture.return_buffer(source_id, old.buffer_token);
                                }
                                last_frame.insert(source_id, Instant::now());
                                if available.insert(source_id) {
                                    let _ = events.send(EngineEvent::SourceAvailable { source_id });
                                }
                            }
                            FrameMessage::SourceError { source_id, reason } => {
                                available.remove(&source_id);
                                let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
                            }
                        }
                    }
                    for (source_id, seen) in last_frame.clone() {
                        if seen.elapsed() > Duration::from_millis(750) && available.remove(&source_id) {
                            let _ = events.send(EngineEvent::SourceUnavailable {
                                source_id,
                                reason: "PipeWire-Quelle liefert keine Live-Frames".into(),
                            });
                        }
                    }
                    match compositor
                        .as_mut()
                        .expect("Vulkan compositor initialized")
                        .render(
                            &snapshot,
                            &mut frames,
                            &media_frames,
                            &events,
                        )
                    {
                        Ok(_) => {}
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
                            drop(compositor.take());
                            compositor = Some(match VulkanCompositor::create(
                                *surface_state.read(),
                                output.width,
                                output.height,
                            ) {
                                Ok(next) => next,
                                Err(recovery_error) => {
                                    let _ = events.send(EngineEvent::DeviceRecovery {
                                        phase: DeviceRecoveryPhase::Failed,
                                        detail: Some(recovery_error),
                                    });
                                    break;
                                }
                            });
                            let _ = events.send(EngineEvent::DeviceRecovery {
                                phase: DeviceRecoveryPhase::Succeeded,
                                detail: None,
                            });
                        }
                        Err(RenderError::Import { source_id, reason }) => {
                            let Some(source_id) = source_id else {
                                let _ = events.send(EngineEvent::EngineError { message: reason });
                                continue;
                            };
                            if let Some(compositor) = compositor.as_mut() {
                                compositor.remove_external_texture(source_id);
                            }
                            available.remove(&source_id);
                            if snapshot.sources.iter().any(
                                |source| matches!(source, Source::Media { id, .. } if *id == source_id),
                            ) {
                                media.remove(source_id, &media_audio);
                                if let Some(runtime) = media_sources.get_mut(&source_id) {
                                    runtime.opened = false;
                                    runtime.playing = false;
                                }
                                media_frames.remove(&source_id);
                                let _ = events.send(EngineEvent::UnsupportedMedia {
                                    source_id,
                                    reason,
                                });
                            } else {
                                if let Some(frame) = frames.remove(&source_id) {
                                    capture.return_buffer(source_id, frame.buffer_token);
                                }
                                let _ = events.send(EngineEvent::SourceUnavailable {
                                    source_id,
                                    reason,
                                });
                            }
                        }
                    }
                    let frame_time = Duration::from_secs_f64(1.0 / output.fps.max(1) as f64);
                    deadline += frame_time;
                    if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                        thread::sleep(wait.min(Duration::from_millis(20)));
                    } else {
                        deadline = Instant::now();
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
                Err(format!("Vulkan renderer startup timeout: {error}"))
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
    let devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|error| format!("enumerate Vulkan devices: {error}"))?;
    for physical in devices {
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
    Err("no Vulkan graphics queue can present both surfaces".into())
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
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let memory_type = find_memory_type(
        instance,
        physical,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )?;
    let memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )
    }
    .map_err(|error| format!("allocate scene image: {error}"))?;
    unsafe { device.bind_image_memory(image, memory, 0) }
        .map_err(|error| format!("bind scene image: {error}"))?;
    let view = create_view(device, image, vk::Format::R16G16B16A16_SFLOAT)?;
    let framebuffer = unsafe {
        device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(std::slice::from_ref(&view))
                .width(width)
                .height(height)
                .layers(1),
            None,
        )
    }
    .map_err(|error| format!("scene framebuffer: {error}"))?;
    Ok(SceneTarget {
        image,
        memory,
        view,
        framebuffer,
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
    let swapchain = unsafe {
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
    .map_err(|error| format!("create swapchain: {error}"))?;
    let images = unsafe { swapchain_loader.get_swapchain_images(swapchain) }
        .map_err(|error| format!("swapchain images: {error}"))?;
    let mut views = Vec::with_capacity(images.len());
    let mut framebuffers = Vec::with_capacity(images.len());
    for image in &images {
        let view = create_view(device, *image, format.format)?;
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
        views.push(view);
        framebuffers.push(framebuffer);
    }
    let sem_info = vk::SemaphoreCreateInfo::default();
    let available = unsafe { device.create_semaphore(&sem_info, None) }
        .map_err(|error| format!("available semaphore: {error}"))?;
    let rendered = (0..images.len())
        .map(|_| unsafe { device.create_semaphore(&sem_info, None) })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("rendered semaphore: {error}"))?;
    let fence = unsafe {
        device.create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        )
    }
    .map_err(|error| format!("swapchain fence: {error}"))?;
    let pipeline_layout = create_composite_pipeline_layout(device, layout)?;
    let pipeline = create_composite_pipeline(device, render_pass, pipeline_layout)?;
    Ok(SwapchainTarget {
        surface,
        loader: swapchain_loader,
        swapchain,
        views,
        framebuffers,
        render_pass,
        extent,
        available,
        rendered,
        fence,
        descriptor_set,
        pipeline,
        pipeline_layout,
    })
}

fn create_view(
    device: &Device,
    image: vk::Image,
    format: vk::Format,
) -> Result<vk::ImageView, String> {
    unsafe {
        device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
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

fn create_item_pipeline(
    device: &Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, String> {
    let vert = create_shader(device, ITEM_VERT)?;
    let frag = create_shader(device, ITEM_FRAG)?;
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
    let blend = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA);
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
                        &vk::PipelineInputAssemblyStateCreateInfo::default()
                            .topology(vk::PrimitiveTopology::TRIANGLE_STRIP),
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
    .map_err(|error| format!("item pipeline: {error}"));
    unsafe {
        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
    }
    pipeline
}

fn create_composite_pipeline(
    device: &Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, String> {
    let vert = create_shader(device, COMPOSITE_VERT)?;
    let frag = create_shader(device, COMPOSITE_FRAG)?;
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
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let color_write = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA);
    let pipeline = unsafe {
        device
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                &[vk::GraphicsPipelineCreateInfo::default()
                    .stages(&stages)
                    .vertex_input_state(&vk::PipelineVertexInputStateCreateInfo::default())
                    .input_assembly_state(
                        &vk::PipelineInputAssemblyStateCreateInfo::default()
                            .topology(vk::PrimitiveTopology::TRIANGLE_LIST),
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
                            .attachments(std::slice::from_ref(&color_write)),
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
    .map_err(|error| format!("composite pipeline: {error}"));
    unsafe {
        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
    }
    pipeline
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
    let view = match create_view(device, image, vk::Format::R8G8B8A8_UNORM) {
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
        sampler: None,
        conversion: None,
    })
}

fn import_media_frame(
    instance: &Instance,
    device: &Device,
    external_fd: &khr::external_memory_fd::Device,
    physical: vk::PhysicalDevice,
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
    let mut conversion = vk::SamplerYcbcrConversion::null();
    let mut view = vk::ImageView::null();
    let mut sampler = vk::Sampler::null();
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
            let memory_type = find_memory_type(
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
        conversion = unsafe {
            device.create_sampler_ycbcr_conversion(
                &vk::SamplerYcbcrConversionCreateInfo::default()
                    .format(format)
                    .ycbcr_model(vk::SamplerYcbcrModelConversion::YCBCR_709)
                    .ycbcr_range(vk::SamplerYcbcrRange::ITU_NARROW)
                    .components(vk::ComponentMapping::default())
                    .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
                    .y_chroma_offset(vk::ChromaLocation::MIDPOINT)
                    .chroma_filter(vk::Filter::NEAREST),
                None,
            )
        }
        .map_err(|error| format!("create media YCbCr conversion: {error}"))?;
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
        let mut sampler_conversion =
            vk::SamplerYcbcrConversionInfo::default().conversion(conversion);
        sampler = unsafe {
            device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .max_lod(1.0)
                    .push_next(&mut sampler_conversion),
                None,
            )
        }
        .map_err(|error| format!("create media YCbCr sampler: {error}"))?;
        Ok(())
    })();
    if let Err(error) = operation {
        unsafe {
            if sampler != vk::Sampler::null() {
                device.destroy_sampler(sampler, None);
            }
            if view != vk::ImageView::null() {
                device.destroy_image_view(view, None);
            }
            if conversion != vk::SamplerYcbcrConversion::null() {
                device.destroy_sampler_ycbcr_conversion(conversion, None);
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
        sampler: Some(sampler),
        conversion: Some(conversion),
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
        let memory_type = find_memory_type(
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
        create_view(device, image, format)
    })();
    match result {
        Ok(view) => Ok(ImportedFrame {
            image,
            memories: vec![memory],
            view,
            sampler: None,
            conversion: None,
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
        .or_else(|| (0..properties.memory_type_count).find(|index| (bits & (1 << index)) != 0))
        .ok_or_else(|| "no compatible Vulkan memory type".into())
}

fn item_push(transform: Transform, width: u32, height: u32, mode: u32) -> ItemPush {
    let radians = transform.rotation_degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    let crop_left = transform.crop_left.max(0.0).min(transform.width);
    let crop_right = transform
        .crop_right
        .max(0.0)
        .min(transform.width - crop_left);
    let crop_top = transform.crop_top.max(0.0).min(transform.height);
    let crop_bottom = transform
        .crop_bottom
        .max(0.0)
        .min(transform.height - crop_top);
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

fn acquire(_device: &Device, target: &SwapchainTarget) -> Result<u32, RenderError> {
    unsafe {
        target
            .loader
            .acquire_next_image(
                target.swapchain,
                u64::MAX,
                target.available,
                vk::Fence::null(),
            )
            .map(|(index, _)| index)
            .map_err(RenderError::Vk)
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
        if let Some(sampler) = imported.sampler {
            device.destroy_sampler(sampler, None);
        }
        if let Some(conversion) = imported.conversion {
            device.destroy_sampler_ycbcr_conversion(conversion, None);
        }
        device.destroy_image_view(imported.view, None);
        device.destroy_image(imported.image, None);
        for memory in imported.memories {
            device.free_memory(memory, None);
        }
    }
}

fn parse_color(value: &str) -> [f32; 4] {
    const FALLBACK: [f32; 4] = [0.02, 0.02, 0.03, 1.0];
    let Some(hex) = value.strip_prefix('#') else {
        return FALLBACK;
    };
    let bytes = hex.as_bytes();
    if bytes.len() != 6 {
        return FALLBACK;
    }
    let nibble = |byte: u8| -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    };
    let channel = |index: usize| -> Option<f32> {
        Some((nibble(bytes[index])? * 16 + nibble(bytes[index + 1])?) as f32 / 255.0)
    };
    match (channel(0), channel(2), channel(4)) {
        (Some(r), Some(g), Some(b)) => [r, g, b, 1.0],
        _ => FALLBACK,
    }
}

fn destroy_scene(device: &Device, scene: &mut SceneTarget) {
    unsafe {
        device.destroy_framebuffer(scene.framebuffer, None);
        device.destroy_image_view(scene.view, None);
        device.destroy_image(scene.image, None);
        device.free_memory(scene.memory, None);
        device.destroy_render_pass(scene.render_pass, None);
    }
}
fn destroy_target(
    device: &Device,
    surface_loader: &khr::surface::Instance,
    target: &mut SwapchainTarget,
) {
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
        device.destroy_fence(target.fence, None);
        device.destroy_semaphore(target.available, None);
        for semaphore in target.rendered.drain(..) {
            device.destroy_semaphore(semaphore, None);
        }
        target.loader.destroy_swapchain(target.swapchain, None);
        surface_loader.destroy_surface(target.surface, None);
    }
}
