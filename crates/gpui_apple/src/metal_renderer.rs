use crate::metal_atlas::MetalAtlas;
use anyhow::{Context as _, Result};
use block::ConcreteBlock;
use cocoa::{
    base::{NO, YES},
    foundation::{NSSize, NSUInteger},
    quartzcore::AutoresizingMask,
};
use gpui::{
    AtlasTextureId, BackdropBlur, Background, Bounds, ContentMask, DevicePixels, DrawOrder,
    PaintSurface, Path, Point, PrimitiveBatch, ScaledPixels, Scene, Size, point, size,
};
#[cfg(any(test, feature = "bench-support", feature = "test-support"))]
use image::RgbaImage;

use core_foundation::base::TCFType;
use core_video::{
    metal_texture::CVMetalTextureGetTexture, metal_texture_cache::CVMetalTextureCache,
    pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use foreign_types::{ForeignType, ForeignTypeRef};
use metal::{
    CAMetalLayer, CommandQueue, MTLGPUFamily, MTLPixelFormat, MTLResourceOptions, NSRange,
};
use objc::{self, class, msg_send, sel, sel_impl};
use parking_lot::Mutex;

#[link(name = "MetalPerformanceShaders", kind = "framework")]
unsafe extern "C" {}

use std::{
    cell::Cell, ffi::c_void, mem, mem::MaybeUninit, ops::Range, ptr, slice, sync::Arc,
    time::{Duration, Instant},
};

// Exported to metal
pub(crate) type PointF = gpui::Point<f32>;

#[cfg(not(feature = "runtime_shaders"))]
const SHADERS_METALLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders.metallib"));
#[cfg(feature = "runtime_shaders")]
const SHADERS_SOURCE_FILE: &str = include_str!(concat!(env!("OUT_DIR"), "/stitched_shaders.metal"));
// Use 4x MSAA, all devices support it.
// https://developer.apple.com/documentation/metal/mtldevice/1433355-supportstexturesamplecount
const PATH_SAMPLE_COUNT: u32 = 4;
/// Metal requires the offset a buffer is bound at to be 256-byte aligned.
const INSTANCE_BUFFER_ALIGNMENT: usize = 256;
const MAX_INSTANCE_BUFFER_SIZE: usize = 256 * 1024 * 1024;
const DEFAULT_INSTANCE_BUFFER_SIZE: usize = 2 * 1024 * 1024;
const INSTANCE_BUFFER_SHRINK_AFTER_FRAMES: u32 = 120;
const SCRATCH_RELEASE_AFTER_IDLE_FRAMES: u32 = 30;
const PATH_TEXTURE_IDLE_GRACE: Duration = Duration::from_secs(5);
const MAX_BACKDROP_TEXTURE_PAIRS: usize = 4;
const MAX_BACKDROP_TEXTURE_BYTES: u64 = 32 * 1024 * 1024;
const BACKDROP_TEXTURE_SIZE_QUANTUM: u64 = 64;
const MAX_BACKDROP_KERNELS: usize = 4;
const MPS_IMAGE_EDGE_MODE_CLAMP: u64 = 1;

pub type Context = Arc<Mutex<InstanceBufferPool>>;
pub type Renderer = MetalRenderer;

pub unsafe fn new_renderer(
    context: self::Context,
    _native_window: *mut c_void,
    _native_view: *mut c_void,
    _bounds: gpui::Size<f32>,
    transparent: bool,
) -> Renderer {
    MetalRenderer::new(context, transparent)
}

pub struct InstanceBufferPool {
    buffer_size: usize,
    buffers: Vec<metal::Buffer>,
    low_usage_frames: u32,
}

impl Default for InstanceBufferPool {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_INSTANCE_BUFFER_SIZE,
            buffers: Vec::new(),
            low_usage_frames: 0,
        }
    }
}

pub(crate) struct InstanceBuffer {
    metal_buffer: metal::Buffer,
    size: usize,
}

impl InstanceBufferPool {
    pub(crate) fn reset(&mut self, buffer_size: usize) {
        self.buffer_size = buffer_size;
        self.buffers.clear();
        self.low_usage_frames = 0;
    }

    pub(crate) fn note_usage(&mut self, used_bytes: usize) {
        if self.buffer_size > DEFAULT_INSTANCE_BUFFER_SIZE && used_bytes <= self.buffer_size / 2 {
            self.low_usage_frames += 1;
            if self.low_usage_frames >= INSTANCE_BUFFER_SHRINK_AFTER_FRAMES {
                self.reset((self.buffer_size / 2).max(DEFAULT_INSTANCE_BUFFER_SIZE));
            }
        } else {
            self.low_usage_frames = 0;
        }
    }

    pub(crate) fn acquire(
        &mut self,
        device: &metal::Device,
        unified_memory: bool,
    ) -> InstanceBuffer {
        let buffer = self.buffers.pop().unwrap_or_else(|| {
            let options = if unified_memory {
                MTLResourceOptions::StorageModeShared
                    // Buffers are write only which can benefit from the combined cache
                    // https://developer.apple.com/documentation/metal/mtlresourceoptions/cpucachemodewritecombined
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            };

            device.new_buffer(self.buffer_size as u64, options)
        });
        InstanceBuffer {
            metal_buffer: buffer,
            size: self.buffer_size,
        }
    }

    pub(crate) fn release(&mut self, buffer: InstanceBuffer) {
        if buffer.size == self.buffer_size {
            self.buffers.push(buffer.metal_buffer)
        }
    }
}

pub struct MetalRenderer {
    device: metal::Device,
    layer: Option<metal::MetalLayer>,
    is_apple_gpu: bool,
    is_unified_memory: bool,
    presents_with_transaction: bool,
    /// For headless rendering, tracks whether output should be opaque
    opaque: bool,
    command_queue: CommandQueue,
    paths_rasterization_pipeline_state: metal::RenderPipelineState,
    path_sprites_pipeline_state: metal::RenderPipelineState,
    shadows_pipeline_state: metal::RenderPipelineState,
    backdrop_blur_pipeline_state: metal::RenderPipelineState,
    backdrop_textures: Vec<BackdropTextures>,
    backdrop_kernels: Vec<(f32, *mut objc::runtime::Object)>,
    blur_free_frames: u32,
    path_free_frames: u32,
    last_path_frame: Option<Instant>,
    gpu_stats_last_logged: Option<Instant>,
    quads_pipeline_state: metal::RenderPipelineState,
    underlines_pipeline_state: metal::RenderPipelineState,
    monochrome_sprites_pipeline_state: metal::RenderPipelineState,
    polychrome_sprites_pipeline_state: metal::RenderPipelineState,
    surfaces_pipeline_state: metal::RenderPipelineState,
    unit_vertices: metal::Buffer,
    #[allow(clippy::arc_with_non_send_sync)]
    instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    sprite_atlas: Arc<MetalAtlas>,
    core_video_texture_cache: core_video::metal_texture_cache::CVMetalTextureCache,
    path_intermediate_texture: Option<metal::Texture>,
    path_intermediate_msaa_texture: Option<metal::Texture>,
    path_sample_count: u32,
    /// Offscreen render target reused across `render_scene` calls when
    /// rendering headlessly without reading pixels back.
    #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
    headless_render_target: Option<metal::Texture>,
}

struct BackdropTextures {
    used_this_frame: bool,
    scratch: metal::Texture,
    blurred: metal::Texture,
}

impl BackdropTextures {
    fn bytes(&self) -> u64 {
        self.scratch.width() * self.scratch.height() * 8
    }
}

impl Drop for MetalRenderer {
    fn drop(&mut self) {
        self.release_backdrop_resources();
    }
}

#[repr(C)]
pub struct PathRasterizationVertex {
    pub xy_position: Point<ScaledPixels>,
    pub st_position: Point<f32>,
    pub color: Background,
    pub bounds: Bounds<ScaledPixels>,
}

impl MetalRenderer {
    /// Creates a new MetalRenderer with a CAMetalLayer for window-based rendering.
    pub fn new(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>, transparent: bool) -> Self {
        let device = Self::create_device();

        let layer = metal::MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        // Support direct-to-display rendering if the window is not transparent
        // https://developer.apple.com/documentation/metal/managing-your-game-window-for-metal-in-macos
        layer.set_opaque(!transparent);
        layer.set_maximum_drawable_count(2);
        // Allow texture reading for visual tests (captures screenshots without ScreenCaptureKit)
        #[cfg(any(test, feature = "test-support"))]
        layer.set_framebuffer_only(false);
        unsafe {
            let _: () = msg_send![&*layer, setAllowsNextDrawableTimeout: NO];
            let _: () = msg_send![&*layer, setNeedsDisplayOnBoundsChange: YES];
            let _: () = msg_send![
                &*layer,
                setAutoresizingMask: AutoresizingMask::WIDTH_SIZABLE
                    | AutoresizingMask::HEIGHT_SIZABLE
            ];
        }

        Self::new_internal(device, Some(layer), !transparent, instance_buffer_pool)
    }

    /// Creates a new headless MetalRenderer for offscreen rendering without a window.
    ///
    /// This renderer can render scenes to images without requiring a CAMetalLayer,
    /// window, or AppKit. Use `render_scene_to_image()` to render scenes.
    #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
    pub fn new_headless(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>) -> Self {
        let device = Self::create_device();
        Self::new_internal(device, None, true, instance_buffer_pool)
    }

    fn create_device() -> metal::Device {
        // Prefer low‐power integrated GPUs on Intel Mac. On Apple
        // Silicon, there is only ever one GPU, so this is equivalent to
        // `metal::Device::system_default()`.
        if let Some(d) = metal::Device::all()
            .into_iter()
            .min_by_key(|d| (d.is_removable(), !d.is_low_power()))
        {
            d
        } else {
            // For some reason `all()` can return an empty list, see https://github.com/zed-industries/zed/issues/37689
            // In that case, we fall back to the system default device.
            log::error!(
                "Unable to enumerate Metal devices; attempting to use system default device"
            );
            metal::Device::system_default().unwrap_or_else(|| {
                log::error!("unable to access a compatible graphics device");
                std::process::exit(1);
            })
        }
    }

    fn new_internal(
        device: metal::Device,
        layer: Option<metal::MetalLayer>,
        opaque: bool,
        instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    ) -> Self {
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(&SHADERS_SOURCE_FILE, &metal::CompileOptions::new())
            .expect("error building metal library");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(SHADERS_METALLIB)
            .expect("error building metal library");

        fn to_float2_bits(point: PointF) -> u64 {
            let mut output = point.y.to_bits() as u64;
            output <<= 32;
            output |= point.x.to_bits() as u64;
            output
        }

        // Shared memory can be used only if CPU and GPU share the same memory space.
        // https://developer.apple.com/documentation/metal/setting-resource-storage-modes
        let is_unified_memory = device.has_unified_memory();
        // Apple GPU families support memoryless textures, which can significantly reduce
        // memory usage by keeping render targets in on-chip tile memory instead of
        // allocating backing store in system memory.
        // https://developer.apple.com/documentation/metal/mtlgpufamily
        let is_apple_gpu = device.supports_family(MTLGPUFamily::Apple1);

        let unit_vertices = [
            to_float2_bits(point(0., 0.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(1., 1.)),
        ];
        let unit_vertices = device.new_buffer_with_data(
            unit_vertices.as_ptr() as *const c_void,
            mem::size_of_val(&unit_vertices) as u64,
            if is_unified_memory {
                MTLResourceOptions::StorageModeShared
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            },
        );

        let paths_rasterization_pipeline_state = build_path_rasterization_pipeline_state(
            &device,
            &library,
            "paths_rasterization",
            "path_rasterization_vertex",
            "path_rasterization_fragment",
            MTLPixelFormat::BGRA8Unorm,
            PATH_SAMPLE_COUNT,
        );
        let path_sprites_pipeline_state = build_path_sprite_pipeline_state(
            &device,
            &library,
            "path_sprites",
            "path_sprite_vertex",
            "path_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let shadows_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "shadows",
            "shadow_vertex",
            "shadow_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let backdrop_blur_pipeline_state = build_pipeline_state_no_blend(
            &device,
            &library,
            "backdrop_blur",
            "backdrop_blur_vertex",
            "backdrop_blur_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let quads_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "quads",
            "quad_vertex",
            "quad_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let underlines_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "underlines",
            "underline_vertex",
            "underline_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let monochrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "monochrome_sprites",
            "monochrome_sprite_vertex",
            "monochrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let polychrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "polychrome_sprites",
            "polychrome_sprite_vertex",
            "polychrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let surfaces_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "surfaces",
            "surface_vertex",
            "surface_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );

        let command_queue = device.new_command_queue();
        let sprite_atlas = Arc::new(MetalAtlas::new(device.clone(), is_apple_gpu));
        let core_video_texture_cache =
            CVMetalTextureCache::new(None, device.clone(), None).unwrap();

        Self {
            device,
            layer,
            presents_with_transaction: false,
            is_apple_gpu,
            is_unified_memory,
            opaque,
            command_queue,
            paths_rasterization_pipeline_state,
            path_sprites_pipeline_state,
            shadows_pipeline_state,
            backdrop_blur_pipeline_state,
            backdrop_textures: Vec::new(),
            backdrop_kernels: Vec::new(),
            blur_free_frames: 0,
            path_free_frames: 0,
            last_path_frame: None,
            gpu_stats_last_logged: None,
            quads_pipeline_state,
            underlines_pipeline_state,
            monochrome_sprites_pipeline_state,
            polychrome_sprites_pipeline_state,
            surfaces_pipeline_state,
            unit_vertices,
            instance_buffer_pool,
            sprite_atlas,
            core_video_texture_cache,
            path_intermediate_texture: None,
            path_intermediate_msaa_texture: None,
            path_sample_count: PATH_SAMPLE_COUNT,
            #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
            headless_render_target: None,
        }
    }

    pub fn layer(&self) -> Option<&metal::MetalLayerRef> {
        self.layer.as_ref().map(|l| l.as_ref())
    }

    pub fn layer_ptr(&self) -> *mut CAMetalLayer {
        self.layer
            .as_ref()
            .map(|l| l.as_ptr())
            .unwrap_or(ptr::null_mut())
    }

    pub fn sprite_atlas(&self) -> &Arc<MetalAtlas> {
        &self.sprite_atlas
    }

    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.presents_with_transaction = presents_with_transaction;
        if let Some(layer) = &self.layer {
            layer.set_presents_with_transaction(presents_with_transaction);
        }
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        if let Some(layer) = &self.layer {
            let ns_size = NSSize {
                width: size.width.0 as f64,
                height: size.height.0 as f64,
            };
            unsafe {
                let _: () = msg_send![
                    layer.as_ref(),
                    setDrawableSize: ns_size
                ];
            }
        }
        self.path_intermediate_texture = None;
        self.path_intermediate_msaa_texture = None;
    }

    fn ensure_path_intermediates(&mut self, size: Size<DevicePixels>) {
        // We are uncertain when this happens, but sometimes size can be 0 here. Most likely before
        // the layout pass on window creation. Zero-sized texture creation causes SIGABRT.
        // https://github.com/zed-industries/zed/issues/36229
        if size.width.0 <= 0 || size.height.0 <= 0 {
            self.path_intermediate_texture = None;
            self.path_intermediate_msaa_texture = None;
            return;
        }
        let is_current_size = self
            .path_intermediate_texture
            .as_ref()
            .is_some_and(|texture| {
                texture.width() == size.width.0 as u64 && texture.height() == size.height.0 as u64
            });
        if is_current_size {
            return;
        }

        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        self.path_intermediate_texture = Some(self.device.new_texture(&texture_descriptor));

        if self.path_sample_count > 1 {
            // https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus
            // Rendering MSAA textures are done in a single pass, so we can use memory-less storage on Apple Silicon
            let storage_mode = if self.is_apple_gpu {
                metal::MTLStorageMode::Memoryless
            } else {
                metal::MTLStorageMode::Private
            };

            let msaa_descriptor = texture_descriptor;
            msaa_descriptor.set_texture_type(metal::MTLTextureType::D2Multisample);
            msaa_descriptor.set_storage_mode(storage_mode);
            msaa_descriptor.set_sample_count(self.path_sample_count as _);
            self.path_intermediate_msaa_texture = Some(self.device.new_texture(&msaa_descriptor));
        } else {
            self.path_intermediate_msaa_texture = None;
        }
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        self.opaque = !transparent;
        if let Some(layer) = &self.layer {
            layer.set_opaque(!transparent);
        }
    }

    pub fn destroy(&self) {
        // nothing to do
    }

    pub fn draw(&mut self, scene: &Scene) {
        // Display-link callbacks don't run inside an AppKit autorelease pool drain.
        objc::rc::autoreleasepool(|| self.draw_frame(scene));
    }

    fn draw_frame(&mut self, scene: &Scene) {
        let layer = match &self.layer {
            Some(l) => l.clone(),
            None => {
                log::error!(
                    "draw() called on headless renderer - use render_scene_to_image() instead"
                );
                return;
            }
        };
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        allow_reading_drawables_for_blur(&layer, scene);
        let drawable = if let Some(drawable) = layer.next_drawable() {
            drawable
        } else {
            log::error!(
                "failed to retrieve next drawable, drawable size: {:?}",
                viewport_size
            );
            return;
        };

        let command_buffer = match self.render_frame(scene, drawable.texture(), viewport_size) {
            Ok(command_buffer) => command_buffer,
            Err(error) => {
                log::error!("failed to render: {error:#}");
                return;
            }
        };

        if self.presents_with_transaction {
            command_buffer.commit();
            command_buffer.wait_until_scheduled();
            drawable.present();
        } else {
            command_buffer.present_drawable(drawable);
            command_buffer.commit();
        }
    }

    fn render_frame(
        &mut self,
        scene: &Scene,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        let mut writer = InstanceBufferWriter::new(
            &self.device,
            &self.instance_buffer_pool,
            self.is_unified_memory,
        );
        let instance_bindings = write_instances(scene, &mut writer).with_context(|| {
            format!(
                "scene too large: {} paths, {} shadows, {} quads, {} underlines, {} mono, {} poly, {} surfaces, {} backdrop blurs",
                scene.paths.len(),
                scene.shadows.len(),
                scene.quads.len(),
                scene.underlines.len(),
                scene.monochrome_sprites.len(),
                scene.polychrome_sprites.len(),
                scene.surfaces.len(),
                scene.backdrop_blurs.len(),
            )
        })?;
        let command_buffer = self.draw_primitives_to_texture(
            scene,
            &instance_bindings,
            &mut writer,
            texture,
            viewport_size,
        )?;

        self.instance_buffer_pool
            .lock()
            .note_usage(writer.bytes_written());
        self.maybe_log_gpu_stats();

        let instance_buffer_pool = self.instance_buffer_pool.clone();
        let instance_buffer = Cell::new(Some(writer.finish()));
        let block = ConcreteBlock::new(move |_| {
            if let Some(instance_buffer) = instance_buffer.take() {
                instance_buffer_pool.lock().release(instance_buffer);
            }
        });
        let block = block.copy();
        command_buffer.add_completed_handler(&block);

        Ok(command_buffer)
    }

    /// Renders the scene to a texture and returns the pixel data as an RGBA image.
    /// This does not present the frame to screen - useful for visual testing
    /// where we want to capture what would be rendered without displaying it.
    ///
    /// Note: This requires a layer-backed renderer. For headless rendering,
    /// use `render_scene_to_image()` instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_to_image(&mut self, scene: &Scene) -> Result<RgbaImage> {
        let layer = self
            .layer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("render_to_image requires a layer-backed renderer"))?;
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        allow_reading_drawables_for_blur(&layer, scene);
        let drawable = layer
            .next_drawable()
            .ok_or_else(|| anyhow::anyhow!("Failed to get drawable for render_to_image"))?;

        let command_buffer = self.render_frame(scene, drawable.texture(), viewport_size)?;

        // Commit and wait for completion without presenting
        command_buffer.commit();
        command_buffer.wait_until_completed();

        read_texture_to_image(drawable.texture())
    }

    /// Renders a scene to an image without requiring a window or CAMetalLayer.
    ///
    /// This is the primary method for headless rendering. It creates an offscreen
    /// texture, renders the scene to it, and returns the pixel data as an RGBA image.
    #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
    pub fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<RgbaImage> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene_to_image: {:?}", size);
        }

        // Create an offscreen texture as render target
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Managed);
        let target_texture = self.device.new_texture(&texture_descriptor);

        let command_buffer = self.render_frame(scene, &target_texture, size)?;

        // On discrete GPUs (non-unified memory), Managed textures require an
        // explicit blit synchronize before the CPU can read back the rendered
        // data. Without this, get_bytes returns stale zeros.
        if !self.is_unified_memory {
            let blit = command_buffer.new_blit_command_encoder();
            blit.synchronize_resource(&target_texture);
            blit.end_encoding();
        }

        // Commit and wait for completion
        command_buffer.commit();
        command_buffer.wait_until_completed();

        read_texture_to_image(&target_texture)
    }

    /// Renders a scene to a reused offscreen texture without reading pixels
    /// back or blocking on GPU completion.
    ///
    /// This mirrors the CPU cost of presenting a frame to a window (scene
    /// encoding, instance buffer writes, command submission) and is used by
    /// headless benchmark rendering, where the produced pixels are never
    /// inspected.
    #[cfg(any(test, feature = "bench-support", feature = "test-support"))]
    pub fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene: {:?}", size);
        }

        let needs_new_target = self.headless_render_target.as_ref().is_none_or(|texture| {
            texture.width() != size.width.0 as u64 || texture.height() != size.height.0 as u64
        });
        if needs_new_target {
            let texture_descriptor = metal::TextureDescriptor::new();
            texture_descriptor.set_width(size.width.0 as u64);
            texture_descriptor.set_height(size.height.0 as u64);
            texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            texture_descriptor.set_usage(
                metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead,
            );
            texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            self.headless_render_target = Some(self.device.new_texture(&texture_descriptor));
        }
        let target_texture = self
            .headless_render_target
            .clone()
            .expect("just ensured the render target exists");

        let command_buffer = self.render_frame(scene, &target_texture, size)?;

        // Commit without waiting, mirroring presentation to a real window where
        // the CPU doesn't block on the GPU.
        command_buffer.commit();
        Ok(())
    }

    fn draw_primitives_to_texture(
        &mut self,
        scene: &Scene,
        instance_bindings: &InstanceBindings,
        writer: &mut InstanceBufferWriter,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        let command_queue = self.command_queue.clone();
        let command_buffer = command_queue.new_command_buffer();
        let alpha = if self.opaque { 1. } else { 0. };

        self.release_unused_scratch_textures(scene, viewport_size);

        let mut command_encoder = new_command_encoder_for_texture(
            command_buffer,
            texture,
            viewport_size,
            Some(metal::MTLClearColor::new(0., 0., 0., alpha)),
        );

        let mut pending_blurs = scene.backdrop_blurs.iter().enumerate().peekable();
        for batch in scene.batches() {
            let batch_order = batch_first_order(scene, &batch);
            while let Some((blur_index, blur)) = pending_blurs.next_if(|(_, blur)| {
                batch_order.is_some_and(|batch_order| blur.order <= batch_order)
            }) {
                command_encoder = self.draw_backdrop_blur(
                    blur,
                    blur_index,
                    instance_bindings,
                    texture,
                    viewport_size,
                    command_buffer,
                    command_encoder,
                );
            }
            match batch {
                PrimitiveBatch::Shadows(range) => {
                    self.draw_shadows(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::Quads(range) => {
                    self.draw_quads(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::Paths(range) => {
                    let paths = &scene.paths[range];
                    command_encoder.end_encoding();

                    let did_draw = self.draw_paths_to_intermediate(
                        paths,
                        writer,
                        viewport_size,
                        command_buffer,
                    )?;

                    command_encoder = new_command_encoder_for_texture(
                        command_buffer,
                        texture,
                        viewport_size,
                        None,
                    );

                    if did_draw {
                        if let Err(error) = self.draw_paths_from_intermediate(
                            paths,
                            writer,
                            viewport_size,
                            command_encoder,
                        ) {
                            command_encoder.end_encoding();
                            return Err(error);
                        }
                    }
                }
                PrimitiveBatch::Underlines(range) => {
                    self.draw_underlines(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::MonochromeSprites { texture_id, range } => self
                    .draw_monochrome_sprites(
                        texture_id,
                        range,
                        instance_bindings,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::PolychromeSprites { texture_id, range } => self
                    .draw_polychrome_sprites(
                        texture_id,
                        range,
                        instance_bindings,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::Surfaces(range) => self.draw_surfaces(
                    &scene.surfaces[range.clone()],
                    range.start,
                    instance_bindings,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::SubpixelSprites { .. } => unreachable!(),
            }
        }

        command_encoder.end_encoding();

        Ok(command_buffer.to_owned())
    }

    fn release_unused_scratch_textures(
        &mut self,
        scene: &Scene,
        viewport_size: Size<DevicePixels>,
    ) {
        for textures in &mut self.backdrop_textures {
            textures.used_this_frame = false;
        }
        if scene.backdrop_blurs.is_empty() {
            self.blur_free_frames = self.blur_free_frames.saturating_add(1);
            if self.blur_free_frames >= SCRATCH_RELEASE_AFTER_IDLE_FRAMES {
                self.release_backdrop_resources();
            }
        } else {
            self.blur_free_frames = 0;
        }
        if scene.paths.is_empty() {
            self.path_free_frames = self.path_free_frames.saturating_add(1);
            if self.path_free_frames >= SCRATCH_RELEASE_AFTER_IDLE_FRAMES {
                self.path_intermediate_texture = None;
                self.path_intermediate_msaa_texture = None;
            }
        } else {
            self.path_free_frames = 0;
            self.last_path_frame = Some(Instant::now());
            self.ensure_path_intermediates(viewport_size);
        }
    }

    /// Returns how long to wait before trimming again to free the path textures.
    pub fn trim_idle_resources(&mut self) -> Option<Duration> {
        self.backdrop_textures
            .retain(|textures| textures.used_this_frame);
        if self.backdrop_textures.is_empty() {
            self.release_backdrop_resources();
        }
        let holds_path_textures = self.path_intermediate_texture.is_some()
            || self.path_intermediate_msaa_texture.is_some();
        if self.path_free_frames == 0 || !holds_path_textures {
            return None;
        }
        let since_paths_drawn = self.last_path_frame.map(|drawn_at| drawn_at.elapsed());
        match since_paths_drawn {
            Some(elapsed) if elapsed < PATH_TEXTURE_IDLE_GRACE => {
                Some(PATH_TEXTURE_IDLE_GRACE - elapsed)
            }
            _ => {
                self.path_intermediate_texture = None;
                self.path_intermediate_msaa_texture = None;
                None
            }
        }
    }

    fn release_backdrop_resources(&mut self) {
        self.backdrop_textures.clear();
        for (_, kernel) in self.backdrop_kernels.drain(..) {
            unsafe {
                let _: () = msg_send![kernel, release];
            }
        }
    }

    fn maybe_log_gpu_stats(&mut self) {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| {
            std::env::var_os("GPUI_GPU_STATS")
                .is_some_and(|value| value != "0" && !value.is_empty())
        });
        if !enabled {
            return;
        }
        let now = Instant::now();
        if self
            .gpu_stats_last_logged
            .is_some_and(|last| now.duration_since(last).as_secs() < 10)
        {
            return;
        }
        self.gpu_stats_last_logged = Some(now);

        const MEGABYTE: f64 = 1024.0 * 1024.0;
        let backdrop_bytes: u64 = self
            .backdrop_textures
            .iter()
            .map(BackdropTextures::bytes)
            .sum();
        let path_texture_bytes = self
            .path_intermediate_texture
            .as_ref()
            .map_or(0, |texture| texture.width() * texture.height() * 4);
        // The MSAA texture is memoryless on Apple GPUs.
        let path_bytes = if self.path_intermediate_msaa_texture.is_some() && !self.is_apple_gpu {
            path_texture_bytes * (1 + self.path_sample_count as u64)
        } else {
            path_texture_bytes
        };
        let (atlas_textures, atlas_bytes) = self.sprite_atlas.texture_stats();
        let (pool_buffers, pool_buffer_size) = {
            let pool = self.instance_buffer_pool.lock();
            (pool.buffers.len(), pool.buffer_size)
        };
        log::info!(
            "gpu stats: device: {:.1} MB; atlas: {} textures, {:.1} MB; instance pool: {} x {:.1} MB; backdrop scratch: {:.1} MB; path intermediate: {:.1} MB",
            self.device.current_allocated_size() as f64 / MEGABYTE,
            atlas_textures,
            atlas_bytes as f64 / MEGABYTE,
            pool_buffers,
            pool_buffer_size as f64 / MEGABYTE,
            backdrop_bytes as f64 / MEGABYTE,
            path_bytes as f64 / MEGABYTE,
        );
    }

    fn draw_backdrop_blur<'a>(
        &mut self,
        blur: &BackdropBlur,
        blur_index: usize,
        instance_bindings: &InstanceBindings,
        texture: &'a metal::TextureRef,
        viewport_size: Size<DevicePixels>,
        command_buffer: &'a metal::CommandBufferRef,
        command_encoder: &'a metal::RenderCommandEncoderRef,
    ) -> &'a metal::RenderCommandEncoderRef {
        // CAMetalLayer can still vend a pooled framebuffer-only drawable after the flag is cleared.
        if texture.framebuffer_only() {
            return command_encoder;
        }
        let sigma = blur.blur_radius.0.max(1.0);
        // The gaussian reaches about 3 sigma, so only that padded region needs a snapshot.
        let padding = (sigma * 3.0).ceil() + 2.0;
        let visible = blur.bounds.intersect(&blur.content_mask.bounds);
        let drawable_width = texture.width() as i64;
        let drawable_height = texture.height() as i64;
        let left = ((visible.origin.x.0 - padding).floor() as i64).max(0);
        let top = ((visible.origin.y.0 - padding).floor() as i64).max(0);
        let right = ((visible.origin.x.0 + visible.size.width.0 + padding).ceil() as i64)
            .min(drawable_width);
        let bottom = ((visible.origin.y.0 + visible.size.height.0 + padding).ceil() as i64)
            .min(drawable_height);
        if right <= left || bottom <= top {
            return command_encoder;
        }
        let Some(kernel) = self.ensure_gaussian_kernel(sigma) else {
            return command_encoder;
        };

        command_encoder.end_encoding();
        let (scratch, blurred) = self.ensure_backdrop_scratch(
            (right - left) as u64,
            (bottom - top) as u64,
            drawable_width as u64,
            drawable_height as u64,
            texture.pixel_format(),
        );
        // Copy a full scratch-sized window so the clamped blur edges never sample stale texels.
        let copy_x = left.min(drawable_width - scratch.width() as i64).max(0) as u64;
        let copy_y = top.min(drawable_height - scratch.height() as i64).max(0) as u64;
        let blit = command_buffer.new_blit_command_encoder();
        blit.copy_from_texture(
            texture,
            0,
            0,
            metal::MTLOrigin {
                x: copy_x,
                y: copy_y,
                z: 0,
            },
            metal::MTLSize {
                width: scratch.width(),
                height: scratch.height(),
                depth: 1,
            },
            &scratch,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        blit.end_encoding();
        unsafe {
            let _: () = msg_send![
                kernel,
                encodeToCommandBuffer: command_buffer.as_ptr() as *mut objc::runtime::Object
                sourceTexture: scratch.as_ptr() as *mut objc::runtime::Object
                destinationTexture: blurred.as_ptr() as *mut objc::runtime::Object
            ];
        }

        let command_encoder =
            new_command_encoder_for_texture(command_buffer, texture, viewport_size, None);
        let source_rect = [
            copy_x as f32,
            copy_y as f32,
            scratch.width() as f32,
            scratch.height() as f32,
        ];
        command_encoder.set_render_pipeline_state(&self.backdrop_blur_pipeline_state);
        command_encoder.set_vertex_buffer(
            BackdropBlurInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            BackdropBlurInputIndex::Blurs as u64,
            Some(&instance_bindings.backdrop_blurs.buffer),
            instance_bindings.backdrop_blurs.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            BackdropBlurInputIndex::Blurs as u64,
            Some(&instance_bindings.backdrop_blurs.buffer),
            instance_bindings.backdrop_blurs.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            BackdropBlurInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_bytes(
            BackdropBlurInputIndex::SourceRect as u64,
            mem::size_of_val(&source_rect) as u64,
            source_rect.as_ptr() as *const _,
        );
        command_encoder
            .set_fragment_texture(BackdropBlurInputIndex::SourceTexture as u64, Some(&blurred));
        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            1,
            blur_index as u64,
        );
        command_encoder
    }

    fn ensure_backdrop_scratch(
        &mut self,
        needed_width: u64,
        needed_height: u64,
        drawable_width: u64,
        drawable_height: u64,
        format: MTLPixelFormat,
    ) -> (metal::Texture, metal::Texture) {
        let width = (needed_width.div_ceil(BACKDROP_TEXTURE_SIZE_QUANTUM)
            * BACKDROP_TEXTURE_SIZE_QUANTUM)
            .min(drawable_width);
        let height = (needed_height.div_ceil(BACKDROP_TEXTURE_SIZE_QUANTUM)
            * BACKDROP_TEXTURE_SIZE_QUANTUM)
            .min(drawable_height);
        self.backdrop_textures.retain(|textures| {
            textures.scratch.width() <= drawable_width
                && textures.scratch.height() <= drawable_height
                && textures.scratch.pixel_format() == format
        });
        let cached = self
            .backdrop_textures
            .iter()
            .position(|textures| {
                textures.scratch.width() == width && textures.scratch.height() == height
            })
            .map(|index| self.backdrop_textures.remove(index));

        let needed_bytes = width * height * 8;
        let budget = (drawable_width * drawable_height * 16)
            .min(MAX_BACKDROP_TEXTURE_BYTES)
            .max(needed_bytes);
        while !self.backdrop_textures.is_empty()
            && (self.backdrop_textures.len() >= MAX_BACKDROP_TEXTURE_PAIRS
                || self
                    .backdrop_textures
                    .iter()
                    .map(BackdropTextures::bytes)
                    .sum::<u64>()
                    + needed_bytes
                    > budget)
        {
            self.backdrop_textures.remove(0);
        }

        let mut textures = cached.unwrap_or_else(|| {
            let descriptor = metal::TextureDescriptor::new();
            descriptor.set_texture_type(metal::MTLTextureType::D2);
            descriptor.set_pixel_format(format);
            descriptor.set_width(width);
            descriptor.set_height(height);
            descriptor.set_usage(metal::MTLTextureUsage::ShaderRead);
            descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            let scratch = self.device.new_texture(&descriptor);
            descriptor.set_usage(
                metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
            );
            let blurred = self.device.new_texture(&descriptor);
            BackdropTextures {
                used_this_frame: true,
                scratch,
                blurred,
            }
        });
        textures.used_this_frame = true;
        let result = (textures.scratch.clone(), textures.blurred.clone());
        self.backdrop_textures.push(textures);
        result
    }

    fn ensure_gaussian_kernel(&mut self, sigma: f32) -> Option<*mut objc::runtime::Object> {
        if let Some(index) = self
            .backdrop_kernels
            .iter()
            .position(|(cached_sigma, _)| (*cached_sigma - sigma).abs() < 0.01)
        {
            let entry = self.backdrop_kernels.remove(index);
            self.backdrop_kernels.push(entry);
            return Some(entry.1);
        }
        let kernel: *mut objc::runtime::Object = unsafe {
            let allocated: *mut objc::runtime::Object =
                msg_send![class!(MPSImageGaussianBlur), alloc];
            msg_send![
                allocated,
                initWithDevice: self.device.as_ptr() as *mut objc::runtime::Object
                sigma: sigma
            ]
        };
        if kernel.is_null() {
            log::error!("failed to create a gaussian blur kernel with sigma {sigma}");
            return None;
        }
        // The default zero edge mode bleeds transparent black in from the window border.
        unsafe {
            let _: () = msg_send![kernel, setEdgeMode: MPS_IMAGE_EDGE_MODE_CLAMP];
        }
        if self.backdrop_kernels.len() >= MAX_BACKDROP_KERNELS {
            let (_, evicted) = self.backdrop_kernels.remove(0);
            unsafe {
                let _: () = msg_send![evicted, release];
            }
        }
        self.backdrop_kernels.push((sigma, kernel));
        Some(kernel)
    }

    fn draw_paths_to_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        writer: &mut InstanceBufferWriter,
        viewport_size: Size<DevicePixels>,
        command_buffer: &metal::CommandBufferRef,
    ) -> Result<bool> {
        if paths.is_empty() {
            return Ok(false);
        }
        let intermediate_texture = self
            .path_intermediate_texture
            .as_ref()
            .context("missing path intermediate texture")?;

        let mut vertices = Vec::new();
        for path in paths {
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds: path.bounds.intersect(&path.content_mask.bounds),
            }));
        }
        let vertex_instance_bindings = writer.write(&vertices)?;

        let render_pass_descriptor = metal::RenderPassDescriptor::new();
        let color_attachment = render_pass_descriptor
            .color_attachments()
            .object_at(0)
            .unwrap();
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(metal::MTLClearColor::new(0., 0., 0., 0.));

        if let Some(msaa_texture) = &self.path_intermediate_msaa_texture {
            color_attachment.set_texture(Some(msaa_texture));
            color_attachment.set_resolve_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::MultisampleResolve);
        } else {
            color_attachment.set_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);
        }

        let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
        command_encoder.set_render_pipeline_state(&self.paths_rasterization_pipeline_state);
        command_encoder.set_vertex_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&vertex_instance_bindings.buffer),
            vertex_instance_bindings.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            PathRasterizationInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&vertex_instance_bindings.buffer),
            vertex_instance_bindings.offset as u64,
        );
        command_encoder.draw_primitives(
            metal::MTLPrimitiveType::Triangle,
            0,
            vertices.len() as u64,
        );

        command_encoder.end_encoding();
        Ok(true)
    }

    fn draw_shadows(
        &self,
        shadows: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if shadows.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.shadows_pipeline_state);
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_bindings.shadows.buffer),
            instance_bindings.shadows.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_bindings.shadows.buffer),
            instance_bindings.shadows.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            ShadowInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            shadows.len() as u64,
            shadows.start as u64,
        );
    }

    fn draw_quads(
        &self,
        quads: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if quads.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.quads_pipeline_state);
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_bindings.quads.buffer),
            instance_bindings.quads.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_bindings.quads.buffer),
            instance_bindings.quads.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            QuadInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            quads.len() as u64,
            quads.start as u64,
        );
    }

    fn draw_paths_from_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        writer: &mut InstanceBufferWriter,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> Result<()> {
        let Some(first_path) = paths.first() else {
            return Ok(());
        };
        let intermediate_texture = self
            .path_intermediate_texture
            .as_ref()
            .context("missing path intermediate texture")?;

        command_encoder.set_render_pipeline_state(&self.path_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.set_fragment_texture(
            SpriteInputIndex::AtlasTexture as u64,
            Some(intermediate_texture),
        );

        // When copying paths from the intermediate texture to the drawable,
        // each pixel must only be copied once, in case of transparent paths.
        //
        // If all paths have the same draw order, then their bounds are all
        // disjoint, so we can copy each path's bounds individually. If this
        // batch combines different draw orders, we perform a single copy
        // for a minimal spanning rect.
        let sprites;
        if paths.last().unwrap().order == first_path.order {
            sprites = paths
                .iter()
                .map(|path| PathSprite {
                    bounds: path.clipped_bounds(),
                })
                .collect();
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            sprites = vec![PathSprite { bounds }];
        }

        let sprite_instance_bindings = writer.write(&sprites)?;
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&sprite_instance_bindings.buffer),
            sprite_instance_bindings.offset as u64,
        );

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
        );
        Ok(())
    }

    fn draw_underlines(
        &self,
        underlines: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if underlines.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.underlines_pipeline_state);
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_bindings.underlines.buffer),
            instance_bindings.underlines.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_bindings.underlines.buffer),
            instance_bindings.underlines.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            UnderlineInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            underlines.len() as u64,
            underlines.start as u64,
        );
    }

    fn draw_monochrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if sprites.is_empty() {
            return;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.monochrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.monochrome_sprites.buffer),
            instance_bindings.monochrome_sprites.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.monochrome_sprites.buffer),
            instance_bindings.monochrome_sprites.offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
            sprites.start as u64,
        );
    }

    fn draw_polychrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if sprites.is_empty() {
            return;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.polychrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.polychrome_sprites.buffer),
            instance_bindings.polychrome_sprites.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.polychrome_sprites.buffer),
            instance_bindings.polychrome_sprites.offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
            sprites.start as u64,
        );
    }

    fn draw_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        first_surface: usize,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if surfaces.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.surfaces_pipeline_state);
        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Surfaces as u64,
            Some(&instance_bindings.surfaces.buffer),
            instance_bindings.surfaces.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SurfaceInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        for (index, surface) in surfaces.iter().enumerate() {
            let texture_size = size(
                DevicePixels::from(surface.image_buffer.get_width() as i32),
                DevicePixels::from(surface.image_buffer.get_height() as i32),
            );

            assert_eq!(
                surface.image_buffer.get_pixel_format(),
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            );

            let y_texture = self
                .core_video_texture_cache
                .create_texture_from_image(
                    surface.image_buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::R8Unorm,
                    surface.image_buffer.get_width_of_plane(0),
                    surface.image_buffer.get_height_of_plane(0),
                    0,
                )
                .unwrap();
            let cb_cr_texture = self
                .core_video_texture_cache
                .create_texture_from_image(
                    surface.image_buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::RG8Unorm,
                    surface.image_buffer.get_width_of_plane(1),
                    surface.image_buffer.get_height_of_plane(1),
                    1,
                )
                .unwrap();

            command_encoder.set_vertex_bytes(
                SurfaceInputIndex::TextureSize as u64,
                mem::size_of_val(&texture_size) as u64,
                &texture_size as *const Size<DevicePixels> as *const _,
            );
            // let y_texture = y_texture.get_texture().unwrap().
            command_encoder.set_fragment_texture(SurfaceInputIndex::YTexture as u64, unsafe {
                let texture = CVMetalTextureGetTexture(y_texture.as_concrete_TypeRef());
                Some(metal::TextureRef::from_ptr(texture as *mut _))
            });
            command_encoder.set_fragment_texture(SurfaceInputIndex::CbCrTexture as u64, unsafe {
                let texture = CVMetalTextureGetTexture(cb_cr_texture.as_concrete_TypeRef());
                Some(metal::TextureRef::from_ptr(texture as *mut _))
            });

            command_encoder.draw_primitives_instanced_base_instance(
                metal::MTLPrimitiveType::Triangle,
                0,
                6,
                1,
                (first_surface + index) as u64,
            );
        }
    }
}

// Blur copies from the drawable, which a framebuffer-only drawable forbids.
fn allow_reading_drawables_for_blur(layer: &metal::MetalLayerRef, scene: &Scene) {
    if !scene.backdrop_blurs.is_empty() && layer.framebuffer_only() {
        layer.set_framebuffer_only(false);
    }
}

fn new_command_encoder_for_texture<'a>(
    command_buffer: &'a metal::CommandBufferRef,
    texture: &'a metal::TextureRef,
    viewport_size: Size<DevicePixels>,
    clear_color: Option<metal::MTLClearColor>,
) -> &'a metal::RenderCommandEncoderRef {
    let render_pass_descriptor = metal::RenderPassDescriptor::new();
    let color_attachment = render_pass_descriptor
        .color_attachments()
        .object_at(0)
        .unwrap();
    color_attachment.set_texture(Some(texture));
    color_attachment.set_store_action(metal::MTLStoreAction::Store);
    if let Some(clear_color) = clear_color {
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(clear_color);
    } else {
        color_attachment.set_load_action(metal::MTLLoadAction::Load);
    }

    let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
    command_encoder.set_viewport(metal::MTLViewport {
        originX: 0.0,
        originY: 0.0,
        width: i32::from(viewport_size.width) as f64,
        height: i32::from(viewport_size.height) as f64,
        znear: 0.0,
        zfar: 1.0,
    });
    command_encoder
}

#[cfg(any(test, feature = "bench-support", feature = "test-support"))]
fn read_texture_to_image(texture: &metal::TextureRef) -> Result<RgbaImage> {
    let width = texture.width() as u32;
    let height = texture.height() as u32;
    let bytes_per_row = width as usize * 4;
    let mut pixels = vec![0u8; height as usize * bytes_per_row];

    let region = metal::MTLRegion {
        origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
        size: metal::MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        },
    };
    texture.get_bytes(
        pixels.as_mut_ptr() as *mut std::ffi::c_void,
        bytes_per_row as u64,
        region,
        0,
    );

    // Convert BGRA to RGBA (swap B and R channels)
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    RgbaImage::from_raw(width, height, pixels).context("failed to create RgbaImage from pixel data")
}

fn build_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::SourceAlpha);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    // Additive destination alpha turns antialiased edges opaque over a transparent window.
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_pipeline_state_no_blend(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor
        .color_attachments()
        .object_at(0)
        .expect("render pipeline descriptor has a color attachment");
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(false);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn batch_first_order(scene: &Scene, batch: &PrimitiveBatch) -> Option<DrawOrder> {
    match batch {
        PrimitiveBatch::Shadows(range) => scene
            .shadows
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::Quads(range) => scene
            .quads
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::Paths(range) => scene
            .paths
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::Underlines(range) => scene
            .underlines
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::MonochromeSprites { range, .. } => scene
            .monochrome_sprites
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::SubpixelSprites { range, .. } => scene
            .subpixel_sprites
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::PolychromeSprites { range, .. } => scene
            .polychrome_sprites
            .get(range.start)
            .map(|primitive| primitive.order),
        PrimitiveBatch::Surfaces(range) => scene
            .surfaces
            .get(range.start)
            .map(|primitive| primitive.order),
    }
}

fn build_path_sprite_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_rasterization_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
    path_sample_count: u32,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    if path_sample_count > 1 {
        descriptor.set_raster_sample_count(path_sample_count as _);
        descriptor.set_alpha_to_coverage_enabled(false);
    }
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

#[derive(Clone)]
struct InstanceBinding {
    buffer: metal::Buffer,
    offset: usize,
}

struct InstanceBindings {
    quads: InstanceBinding,
    shadows: InstanceBinding,
    underlines: InstanceBinding,
    monochrome_sprites: InstanceBinding,
    polychrome_sprites: InstanceBinding,
    surfaces: InstanceBinding,
    backdrop_blurs: InstanceBinding,
}

fn write_instances(scene: &Scene, writer: &mut InstanceBufferWriter) -> Result<InstanceBindings> {
    Ok(InstanceBindings {
        quads: writer.write(&scene.quads)?,
        shadows: writer.write(&scene.shadows)?,
        underlines: writer.write(&scene.underlines)?,
        monochrome_sprites: writer.write(&scene.monochrome_sprites)?,
        polychrome_sprites: writer.write(&scene.polychrome_sprites)?,
        surfaces: writer.write_iter(scene.surfaces.iter().map(|surface| SurfaceBounds {
            bounds: surface.bounds,
            content_mask: surface.content_mask,
        }))?,
        backdrop_blurs: writer.write(&scene.backdrop_blurs)?,
    })
}

struct InstanceBufferWriter {
    device: metal::Device,
    pool: Arc<Mutex<InstanceBufferPool>>,
    unified_memory: bool,
    filled: Vec<(InstanceBuffer, usize)>,
    current: InstanceBuffer,
    offset: usize,
}

impl InstanceBufferWriter {
    fn new(
        device: &metal::Device,
        pool: &Arc<Mutex<InstanceBufferPool>>,
        unified_memory: bool,
    ) -> Self {
        let current = pool.lock().acquire(device, unified_memory);
        Self {
            device: device.clone(),
            pool: pool.clone(),
            unified_memory,
            filled: Vec::new(),
            current,
            offset: 0,
        }
    }

    fn allocate<T>(&mut self, count: usize) -> Result<(InstanceBinding, &mut [MaybeUninit<T>])> {
        let size = mem::size_of::<T>() * count;
        let mut offset = self.offset.next_multiple_of(INSTANCE_BUFFER_ALIGNMENT);
        if offset + size > self.current.size {
            self.grow(size)?;
            offset = 0;
        }
        self.offset = offset + size;

        let binding = InstanceBinding {
            buffer: self.current.metal_buffer.clone(),
            offset,
        };
        // Safety: the reservation lies within a buffer this frame owns
        // exclusively, and never overlaps one handed out earlier.
        let values = unsafe {
            let start = (self.current.metal_buffer.contents() as *mut u8).add(offset);
            slice::from_raw_parts_mut(start.cast::<MaybeUninit<T>>(), count)
        };
        Ok((binding, values))
    }

    fn write<T>(&mut self, values: &[T]) -> Result<InstanceBinding> {
        let (binding, destination) = self.allocate::<T>(values.len())?;
        unsafe {
            ptr::copy_nonoverlapping(
                values.as_ptr(),
                destination.as_mut_ptr().cast::<T>(),
                values.len(),
            );
        }
        Ok(binding)
    }

    fn write_iter<T>(
        &mut self,
        values: impl ExactSizeIterator<Item = T>,
    ) -> Result<InstanceBinding> {
        let (binding, destination) = self.allocate::<T>(values.len())?;
        for (slot, value) in destination.iter_mut().zip(values) {
            slot.write(value);
        }
        Ok(binding)
    }

    fn grow(&mut self, required: usize) -> Result<()> {
        let mut pool = self.pool.lock();
        let buffer_size = (pool.buffer_size * 2)
            .max(required.next_power_of_two())
            .min(MAX_INSTANCE_BUFFER_SIZE);
        anyhow::ensure!(
            buffer_size >= required,
            "instance buffer needs {required} bytes, above the maximum of {MAX_INSTANCE_BUFFER_SIZE}"
        );
        anyhow::ensure!(
            buffer_size > self.current.size,
            "frame instance data exceeds the {MAX_INSTANCE_BUFFER_SIZE}-byte maximum"
        );
        if buffer_size != pool.buffer_size {
            log::info!("increased instance buffer size to {buffer_size}");
            pool.reset(buffer_size);
        }
        let buffer = pool.acquire(&self.device, self.unified_memory);
        drop(pool);

        let filled = mem::replace(&mut self.current, buffer);
        self.filled.push((filled, self.offset));
        self.offset = 0;
        Ok(())
    }

    fn bytes_written(&self) -> usize {
        self.filled
            .iter()
            .map(|(_, written)| written)
            .sum::<usize>()
            + self.offset
    }

    fn finish(self) -> InstanceBuffer {
        let Self {
            unified_memory,
            filled,
            current,
            offset,
            ..
        } = self;

        if !unified_memory {
            for (buffer, written) in &filled {
                if *written == 0 {
                    continue;
                }
                buffer.metal_buffer.did_modify_range(NSRange {
                    location: 0,
                    length: *written as NSUInteger,
                });
            }
            if offset > 0 {
                current.metal_buffer.did_modify_range(NSRange {
                    location: 0,
                    length: offset as NSUInteger,
                });
            }
        }

        // Metal retains encoded resources until the command buffer completes.
        // Only the final, largest buffer is worth keeping in the pool.
        drop(filled);
        current
    }
}

#[repr(C)]
enum ShadowInputIndex {
    Vertices = 0,
    Shadows = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum BackdropBlurInputIndex {
    Vertices = 0,
    Blurs = 1,
    ViewportSize = 2,
    SourceTexture = 3,
    SourceRect = 4,
}

#[repr(C)]
enum QuadInputIndex {
    Vertices = 0,
    Quads = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum UnderlineInputIndex {
    Vertices = 0,
    Underlines = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum SpriteInputIndex {
    Vertices = 0,
    Sprites = 1,
    ViewportSize = 2,
    AtlasTextureSize = 3,
    AtlasTexture = 4,
}

#[repr(C)]
enum SurfaceInputIndex {
    Vertices = 0,
    Surfaces = 1,
    ViewportSize = 2,
    TextureSize = 3,
    YTexture = 4,
    CbCrTexture = 5,
}

#[repr(C)]
enum PathRasterizationInputIndex {
    Vertices = 0,
    ViewportSize = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PathSprite {
    pub bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SurfaceBounds {
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
}

#[cfg(any(test, feature = "bench-support", feature = "test-support"))]
pub struct MetalHeadlessRenderer {
    renderer: MetalRenderer,
}

#[cfg(any(test, feature = "bench-support", feature = "test-support"))]
impl MetalHeadlessRenderer {
    pub fn new() -> Self {
        let instance_buffer_pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let renderer = MetalRenderer::new_headless(instance_buffer_pool);
        Self { renderer }
    }
}

#[cfg(any(test, feature = "bench-support", feature = "test-support"))]
impl gpui::PlatformHeadlessRenderer for MetalHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        self.renderer.render_scene_to_image(scene, size)
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        self.renderer.render_scene(scene, size)
    }

    fn sprite_atlas(&self) -> Arc<dyn gpui::PlatformAtlas> {
        self.renderer.sprite_atlas().clone()
    }
}

#[cfg(test)]
mod backdrop_blur_tests {
    use super::*;
    use gpui::{Corners, Edges, Hsla, Quad, Shadow, solid_background, transparent_black};
    use image::RgbaImage;

    const VIEW_WIDTH: i32 = 640;
    const VIEW_HEIGHT: i32 = 480;

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    fn viewport() -> Bounds<ScaledPixels> {
        bounds(0., 0., VIEW_WIDTH as f32, VIEW_HEIGHT as f32)
    }

    fn viewport_size() -> Size<DevicePixels> {
        size(DevicePixels(VIEW_WIDTH), DevicePixels(VIEW_HEIGHT))
    }

    fn new_renderer() -> MetalRenderer {
        MetalRenderer::new_headless(Arc::new(Mutex::new(InstanceBufferPool::default())))
    }

    fn push_quad(scene: &mut Scene, quad_bounds: Bounds<ScaledPixels>, lightness: f32) {
        scene.insert_primitive(Quad {
            order: 0,
            border_style: Default::default(),
            bounds: quad_bounds,
            content_mask: ContentMask { bounds: viewport() },
            background: solid_background(Hsla {
                h: 0.,
                s: 0.,
                l: lightness,
                a: 1.,
            }),
            border_color: transparent_black(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            fade: gpui::EdgeFadeParams::default(),
        });
    }

    // A gaussian blur of a linear ramp reproduces the ramp, so any snapshot misalignment shows up.
    fn push_gradient(scene: &mut Scene, horizontal: bool) {
        const STEP: f32 = 2.0;
        let extent = if horizontal { VIEW_WIDTH } else { VIEW_HEIGHT } as f32;
        let steps = (extent / STEP) as i32;
        for index in 0..steps {
            let progress = index as f32 / steps as f32;
            let step_bounds = if horizontal {
                bounds(index as f32 * STEP, 0., STEP, VIEW_HEIGHT as f32)
            } else {
                bounds(0., index as f32 * STEP, VIEW_WIDTH as f32, STEP)
            };
            push_quad(scene, step_bounds, 0.15 + 0.7 * progress);
        }
    }

    // Mirrors Window::paint_backdrop_blur, including its invisible splitter shadow.
    fn push_blur(scene: &mut Scene, blur_bounds: Bounds<ScaledPixels>, radius: f32) {
        scene.push_layer(blur_bounds);
        scene.insert_primitive(Shadow {
            order: 0,
            blur_radius: ScaledPixels(0.),
            bounds: blur_bounds,
            corner_radii: Corners::default(),
            content_mask: ContentMask { bounds: viewport() },
            color: transparent_black(),
            element_bounds: blur_bounds,
            element_corner_radii: Corners::default(),
            inset: 0,
            pad: 0,
        });
        scene.insert_backdrop_blur(BackdropBlur {
            order: 0,
            blur_radius: ScaledPixels(radius),
            bounds: blur_bounds,
            content_mask: ContentMask { bounds: viewport() },
            corner_radii: Corners::default(),
        });
        scene.pop_layer();
    }

    fn render(scene: &mut Scene) -> RgbaImage {
        scene.finish();
        new_renderer()
            .render_scene_to_image(scene, viewport_size())
            .expect("render_scene_to_image failed")
    }

    fn max_abs_diff(
        expected: &RgbaImage,
        actual: &RgbaImage,
        region: (Range<u32>, Range<u32>),
    ) -> u32 {
        let (columns, rows) = region;
        let mut max = 0u32;
        for y in rows {
            for x in columns.clone() {
                let expected_pixel = expected.get_pixel(x, y);
                let actual_pixel = actual.get_pixel(x, y);
                for channel in 0..3 {
                    max = max.max(
                        (expected_pixel[channel] as i32 - actual_pixel[channel] as i32)
                            .unsigned_abs(),
                    );
                }
            }
        }
        max
    }

    #[test]
    fn monochrome_fade_varies_within_a_glyph_and_moves_continuously() {
        use gpui::{
            EdgeFadeParams, MonochromeSprite, PlatformAtlas, RenderSvgParams, TransformationMatrix,
        };
        use std::borrow::Cow;
        assert!(
            metal::Device::system_default().is_some(),
            "requires Metal GPU"
        );
        let mut renderer = new_renderer();
        let tile_size = size(DevicePixels(32), DevicePixels(16));
        let tile = renderer
            .sprite_atlas()
            .get_or_insert_with(
                &RenderSvgParams {
                    path: "fade-test-mask".into(),
                    size: tile_size,
                }
                .into(),
                &mut || Ok(Some((tile_size, Cow::Owned(vec![255; 32 * 16])))),
            )
            .unwrap()
            .unwrap();
        let mut paint = |edge: f32, band: f32| {
            let mut scene = Scene::default();
            push_quad(&mut scene, viewport(), 0.0);
            // A solid mask isolates the opacity ramp from glyph shape and antialiasing.
            scene.insert_primitive(MonochromeSprite {
                order: 0,
                pad: 0,
                bounds: bounds(20., 20., 32., 16.),
                content_mask: ContentMask { bounds: viewport() },
                color: Hsla {
                    h: 0.,
                    s: 0.,
                    l: 1.,
                    a: 1.,
                },
                tile,
                transformation: TransformationMatrix::unit(),
                fade: EdgeFadeParams {
                    right_x: edge,
                    band_right: band,
                    ..Default::default()
                },
            });
            scene.finish();
            renderer
                .render_scene_to_image(&scene, viewport_size())
                .unwrap()
        };
        let plain = paint(52., 0.);
        assert_eq!(plain.get_pixel(40, 28)[0], 255);
        let faded = paint(52., 20.);
        for x in 33..51 {
            let expected = (((52. - (x as f32 + 0.5)) / 20.).powi(2) * 255.).round() as i32;
            assert!(
                (faded.get_pixel(x, 28)[0] as i32 - expected).abs() <= 2,
                "x={x}"
            );
        }
        let shifted = paint(52.5, 20.);
        for x in 33..51 {
            let delta = shifted.get_pixel(x, 28)[0] as i32 - faded.get_pixel(x, 28)[0] as i32;
            assert!(
                (0..=14).contains(&delta),
                "discontinuous fade at x={x}: {delta}"
            );
        }
    }

    #[test]
    fn image_alpha_mask_matches_reference_and_reuses_texture_across_frames() {
        use gpui::{
            ImageAlphaMaskParams, PlatformAtlas, PolychromeSprite, RenderImage, RenderImageParams,
        };
        use std::borrow::Cow;
        assert!(
            metal::Device::system_default().is_some(),
            "requires Metal GPU"
        );
        let mut renderer = new_renderer();
        // Partial alpha also covers source-alpha and color preservation.
        let source = RenderImage::new([image::Frame::new(RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([79, 151, 233, 180]),
        ))]);
        let key = RenderImageParams {
            image_id: source.id,
            frame_index: 0,
        }
        .into();
        let tile = renderer
            .sprite_atlas()
            .get_or_insert_with(&key, &mut || {
                Ok(Some((
                    source.size(0),
                    Cow::Borrowed(source.as_bytes(0).unwrap()),
                )))
            })
            .unwrap()
            .unwrap();
        let mut paint = |alpha_mask| {
            let mut scene = Scene::default();
            push_quad(&mut scene, viewport(), 0.0);
            scene.insert_primitive(PolychromeSprite {
                order: 0,
                pad: 0,
                grayscale: false.into(),
                opacity: 0.75,
                bounds: viewport(),
                content_mask: ContentMask { bounds: viewport() },
                corner_radii: Default::default(),
                fade: Default::default(),
                alpha_mask,
                tile,
            });
            scene.finish();
            assert_eq!(scene.polychrome_sprites.len(), 1);
            let cached = renderer
                .sprite_atlas()
                .get_or_insert_with(&key, &mut || panic!("mask geometry reuploaded the image"))
                .unwrap()
                .unwrap();
            assert_eq!(cached, tile);
            renderer
                .render_scene_to_image(&scene, viewport_size())
                .unwrap()
        };
        let baseline = paint(ImageAlphaMaskParams::default());
        for (left, top, width, radius, feather) in [
            (100.0, 360.0, 440.0, 26.0, 220.0),
            (212.25, 352.5, 260.0, 26.0, 220.0),
            (40.5, 300.25, 300.0, 13.0, 120.0),
            (0.0, 420.0, 600.0, 52.0, 240.0),
        ] {
            let mask = ImageAlphaMaskParams {
                bounds: bounds(left, top, width, 124.0),
                radius,
                feather,
                clearance: 8.0,
                bottom_y: 480.0,
                bottom_feather: 96.8,
                pad: 0.0,
            };
            let actual = paint(mask);
            let smooth = |value: f32| {
                let clamped = value.clamp(0.0, 1.0);
                clamped * clamped * (3.0 - 2.0 * clamped)
            };
            for y in 0..VIEW_HEIGHT as u32 {
                for x in 0..VIEW_WIDTH as u32 {
                    let qx = (x as f32 + 0.5 - left - width * 0.5).abs() - width * 0.5 + radius;
                    let qy = (y as f32 + 0.5 - top - 62.0).abs() - 62.0 + radius;
                    let distance = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
                    let alpha =
                        smooth((distance - 8.0) / feather).min(smooth((479.5 - y as f32) / 96.8));
                    for channel in 0..3 {
                        let expected = baseline.get_pixel(x, y)[channel] as f32 * alpha;
                        assert!(
                            (actual.get_pixel(x, y)[channel] as f32 - expected).abs() <= 2.0,
                            "GPU/reference mismatch at {x},{y} channel {channel}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            paint(ImageAlphaMaskParams::default()),
            baseline,
            "disabling mask must restore ordinary image exactly"
        );
    }

    #[test]
    fn blur_preserves_linear_gradient() {
        if metal::Device::system_default().is_none() {
            return;
        }
        let mut base = Scene::default();
        push_gradient(&mut base, true);
        let expected = render(&mut base);

        let mut scene = Scene::default();
        push_gradient(&mut scene, true);
        push_blur(&mut scene, bounds(200., 150., 200., 120.), 20.);
        let actual = render(&mut scene);

        assert_eq!(
            max_abs_diff(&expected, &actual, (0..VIEW_WIDTH as u32, 0..140)),
            0
        );
        let diff = max_abs_diff(&expected, &actual, (208..392, 158..262));
        assert!(diff <= 3, "gradient shifted under blur: max diff {diff}");
    }

    #[test]
    fn blur_actually_blurs_sharp_edges() {
        if metal::Device::system_default().is_none() {
            return;
        }
        let mut scene = Scene::default();
        push_quad(&mut scene, bounds(0., 0., 320., VIEW_HEIGHT as f32), 0.);
        push_quad(&mut scene, bounds(320., 0., 320., VIEW_HEIGHT as f32), 1.);
        push_blur(&mut scene, bounds(260., 180., 120., 120.), 16.);
        let image = render(&mut scene);

        let pixel = image.get_pixel(320, 240);
        assert!(
            (80..=176).contains(&pixel[0]),
            "expected smeared edge, got {:?}",
            pixel
        );
    }

    #[test]
    fn blur_flush_against_window_edge_has_no_vignette() {
        if metal::Device::system_default().is_none() {
            return;
        }
        let mut base = Scene::default();
        push_gradient(&mut base, false);
        let expected = render(&mut base);

        let mut scene = Scene::default();
        push_gradient(&mut scene, false);
        push_blur(&mut scene, bounds(0., 160., 160., 160.), 20.);
        let actual = render(&mut scene);

        let diff = max_abs_diff(&expected, &actual, (0..152, 168..312));
        assert!(
            diff <= 3,
            "vignette or shift at window edge: max diff {diff}"
        );
    }

    #[test]
    fn two_blurs_of_different_sizes_in_one_frame() {
        if metal::Device::system_default().is_none() {
            return;
        }
        let mut base = Scene::default();
        push_gradient(&mut base, true);
        let expected = render(&mut base);

        let mut scene = Scene::default();
        push_gradient(&mut scene, true);
        push_blur(&mut scene, bounds(40., 40., 120., 100.), 12.);
        push_blur(&mut scene, bounds(300., 200., 260., 200.), 24.);
        let actual = render(&mut scene);

        let first_diff = max_abs_diff(&expected, &actual, (48..152, 48..132));
        let second_diff = max_abs_diff(&expected, &actual, (308..552, 208..392));
        assert!(
            first_diff <= 3 && second_diff <= 3,
            "blur regions shifted: {first_diff} / {second_diff}"
        );
    }

    #[test]
    fn alternating_blurs_reuse_resources_and_preserve_pixels() {
        if metal::Device::system_default().is_none() {
            return;
        }
        objc::rc::autoreleasepool(|| {
            let mut renderer = new_renderer();
            let mut scene = Scene::default();
            push_gradient(&mut scene, true);
            push_blur(&mut scene, bounds(20., 370., 580., 80.), 16.);
            push_blur(&mut scene, bounds(80., 20., 180., 360.), 44.);
            push_quad(&mut scene, bounds(100., 100., 1., 1.), 1.);
            scene.finish();
            let first = renderer
                .render_scene_to_image(&scene, viewport_size())
                .unwrap();
            let scratch_textures = |renderer: &MetalRenderer| {
                renderer
                    .backdrop_textures
                    .iter()
                    .map(|textures| textures.scratch.as_ptr())
                    .collect::<Vec<_>>()
            };
            let textures = scratch_textures(&renderer);
            let kernels = renderer.backdrop_kernels.clone();
            assert_eq!(textures.len(), 2);
            assert_eq!(kernels.len(), 2);
            for _ in 0..4 {
                let warm = renderer
                    .render_scene_to_image(&scene, viewport_size())
                    .unwrap();
                assert_eq!(first, warm, "cache reuse must preserve every pixel");
                assert_eq!(textures, scratch_textures(&renderer));
                assert_eq!(kernels, renderer.backdrop_kernels);
            }
            let mut composer = Scene::default();
            push_gradient(&mut composer, true);
            push_blur(&mut composer, bounds(20., 370., 580., 80.), 16.);
            composer.finish();
            renderer
                .render_scene_to_image(&composer, viewport_size())
                .unwrap();
            renderer.trim_idle_resources();
            assert_eq!(renderer.backdrop_textures.len(), 1);
            assert_eq!(renderer.backdrop_textures[0].scratch.as_ptr(), textures[0]);

            let mut plain = Scene::default();
            push_gradient(&mut plain, true);
            plain.finish();
            for _ in 0..SCRATCH_RELEASE_AFTER_IDLE_FRAMES {
                renderer
                    .render_scene_to_image(&plain, viewport_size())
                    .unwrap();
            }
            assert!(renderer.backdrop_textures.is_empty());
            assert!(renderer.backdrop_kernels.is_empty());
        });
    }

    #[test]
    fn idle_trim_frees_path_textures_once_grace_passes() {
        if metal::Device::system_default().is_none() {
            return;
        }
        let mut renderer = new_renderer();
        renderer.ensure_path_intermediates(viewport_size());
        renderer.path_free_frames = 1;
        renderer.last_path_frame = Some(Instant::now());
        let delay = renderer.trim_idle_resources().unwrap();
        assert!(delay <= PATH_TEXTURE_IDLE_GRACE);
        assert!(renderer.path_intermediate_texture.is_some());

        renderer.last_path_frame = Instant::now().checked_sub(PATH_TEXTURE_IDLE_GRACE);
        assert_eq!(renderer.trim_idle_resources(), None);
        assert!(renderer.path_intermediate_texture.is_none());
        assert!(renderer.path_intermediate_msaa_texture.is_none());
        assert_eq!(renderer.trim_idle_resources(), None);
    }

    #[test]
    fn backdrop_cache_is_bounded_across_sizes_and_window_shrink() {
        if metal::Device::system_default().is_none() {
            return;
        }
        let mut renderer = new_renderer();
        for side in (256..=3072).step_by(256) {
            renderer.ensure_backdrop_scratch(side, side, 4096, 4096, MTLPixelFormat::BGRA8Unorm);
            assert!(renderer.backdrop_textures.len() <= MAX_BACKDROP_TEXTURE_PAIRS);
            let bytes: u64 = renderer
                .backdrop_textures
                .iter()
                .map(BackdropTextures::bytes)
                .sum();
            assert!(bytes <= MAX_BACKDROP_TEXTURE_BYTES.max(side * side * 8));
            renderer.ensure_gaussian_kernel(side as f32 / 64.).unwrap();
            assert!(renderer.backdrop_kernels.len() <= MAX_BACKDROP_KERNELS);
        }
        renderer.ensure_backdrop_scratch(120, 100, 640, 480, MTLPixelFormat::BGRA8Unorm);
        assert!(
            renderer
                .backdrop_textures
                .iter()
                .all(|textures| textures.scratch.width() <= 640 && textures.scratch.height() <= 480)
        );
    }
}
