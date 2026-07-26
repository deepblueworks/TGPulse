//! Presents the emulated image.
//!
//! The tile layers are drawn on the CPU into a 496x384 buffer; the 3D layer is
//! rasterized by a compute pass. This uploads the former, dispatches the
//! latter, composites them and blits the result to the window -- and gives the
//! user interface a chance to draw over the top.

use super::gpu_compute::ExactCompute;
use super::gpu_model1::Model1Compute;
use crate::platform::UiPass;
use tgpulse_core::geometry::GpuRasterTriangle;
use tgpulse_core::model1_video::GpuQuad;
use tgpulse_core::tilemap::{SCREEN_H, SCREEN_W};
use winit::window::Window;

pub struct Model2Video {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pub size: winit::dpi::PhysicalSize<u32>,

    pipeline: wgpu::RenderPipeline,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    exact: ExactCompute,
    exact_enabled: bool,
    /// Model 1's own compute rasterizer (flat quads); mutually exclusive with
    /// `exact` (Model 2 textured triangles) -- a window shows one system.
    exact_m1: Option<Model1Compute>,
    smooth_shadows: bool,
    widescreen: bool,
    stretch_2d: bool,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Presentation texture size in pixels (the blit source aspect).
    tex_w: u32,
    tex_h: u32,
}

impl Model2Video {
    pub fn exact_compute_enabled(&self) -> bool {
        self.exact_enabled
    }

    /// The wgpu objects, for building a renderer that draws into the same
    /// surface -- the user interface, in practice.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }
    pub fn model1_compute_enabled(&self) -> bool {
        self.exact_m1.is_some()
    }

    /// The native render width for the 3D layer this frame. With widescreen
    /// off it is the board's 496; with it on, the width follows the window's
    /// aspect ratio over the 384 lines (ultrawide included), so the image
    /// always fills the window with a widened field of view instead of bars.
    pub fn native_width(&self) -> u32 {
        if !self.widescreen {
            return SCREEN_W as u32;
        }
        let win_w = self.config.width.max(1) as f32;
        let win_h = self.config.height.max(1) as f32;
        ((SCREEN_H as f32 * win_w / win_h).round() as u32).clamp(SCREEN_W as u32, 2048)
    }

    /// Recreates the presentation texture when the render width changed
    /// (window resize under widescreen).
    fn ensure_texture(&mut self, native_w: u32) {
        let (tw, th) = if let Some(m1) = &self.exact_m1 {
            m1.output_dims(native_w)
        } else {
            self.exact.output_dims(native_w)
        };
        if (tw, th) == (self.tex_w, self.tex_h) {
            return;
        }
        let (texture, bind_group) =
            Self::make_fb_texture(&self.device, &self.bgl, &self.sampler, tw, th);
        self.texture = texture;
        self.bind_group = bind_group;
        self.tex_w = tw;
        self.tex_h = th;
    }

    fn make_fb_texture(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Framebuffer"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fb bg"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        (texture, bind_group)
    }

    /// Model 1 window: the GPU rasterizer consumes the quad stream unless
    /// MODEL2_CPU_RENDER asks for the CPU reference rasterizer.
    pub async fn new_model1(
        window: &Window,
        ssaa: u32,
        smooth_shadows: bool,
        widescreen: bool,
        stretch_2d: bool,
    ) -> Self {
        let gpu = std::env::var("MODEL2_CPU_RENDER").is_err();
        Self::new_inner(
            window,
            ssaa,
            true,
            gpu,
            smooth_shadows,
            widescreen,
            stretch_2d,
        )
        .await
    }

    pub async fn new(
        window: &Window,
        ssaa: u32,
        force_blit: bool,
        smooth_shadows: bool,
        widescreen: bool,
        stretch_2d: bool,
    ) -> Self {
        Self::new_inner(
            window,
            ssaa,
            force_blit,
            false,
            smooth_shadows,
            widescreen,
            stretch_2d,
        )
        .await
    }

    async fn new_inner(
        window: &Window,
        ssaa: u32,
        force_blit: bool,
        model1_gpu: bool,
        smooth_shadows: bool,
        widescreen: bool,
        stretch_2d: bool,
    ) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        // SAFETY: the surface is only used while the window is alive.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(window).unwrap())
        }
        .unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No compatible GPU adapter found");

        let info = adapter.get_info();
        log::info!(
            target: "video",
            "{:?} on {} ({})",
            info.backend, info.name, info.driver
        );

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .unwrap();

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // Displayed 3D is rendered at this multiple of native; the tile layers
        // are point-scaled to match, so the whole image is presented above
        // native resolution and looks crisp rather than blurred down to 496.
        const OUT_SCALE: u32 = 2;
        let exact = ExactCompute::new(&device, OUT_SCALE, ssaa);
        let exact_m1 = if model1_gpu {
            Some(Model1Compute::new(&device, OUT_SCALE, ssaa))
        } else {
            None
        };
        // Model 2 uses the GPU compute rasterizer unless MODEL2_CPU_RENDER asks
        // otherwise; Model 1 sets `exact_m1` instead (or blits the CPU image).
        let exact_enabled = !force_blit && std::env::var("MODEL2_CPU_RENDER").is_err();

        // The presentation texture holds whatever the active renderer produces:
        // the compute output at OUT_SCALE x native, or the CPU fallback at
        // native. Sizing it to the renderer avoids blitting a small image
        // stretched across a larger one.
        let (tex_w, tex_h) = if exact_enabled {
            exact.output_dims(super::gpu_compute::NATIVE_W as u32)
        } else if let Some(m1) = &exact_m1 {
            m1.output_dims(super::gpu_model1::NATIVE_W as u32)
        } else {
            (SCREEN_W as u32, SCREEN_H as u32)
        };
        let (tex_w, tex_h) = (tex_w.max(1), tex_h.max(1));
        // Keep the legacy/reference framebuffer pixel-sharp. Anti-aliasing is
        // performed by the high-resolution GPU geometry target; filtering the
        // 496x384 composite here only blurs HUD glyphs and texture detail.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fb bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let (texture, bind_group) = Self::make_fb_texture(&device, &bgl, &sampler, tex_w, tex_h);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Blit pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        Self {
            surface,
            device,
            queue,
            config,
            size,
            pipeline,
            texture,
            bind_group,
            exact,
            exact_enabled,
            exact_m1,
            smooth_shadows,
            widescreen,
            stretch_2d,
            bgl,
            sampler,
            tex_w,
            tex_h,
        }
    }

    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.size = size;
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Uploads `fb` (496x384, 0xAARRGGBB) and presents it.
    // One parameter per layer the frame is composited from.
    #[allow(clippy::too_many_arguments)]
    pub fn present(
        &mut self,
        ui: Option<&mut dyn UiPass>,
        fb: &[u32],
        gpu_colors: &[u32],
        texture0: &[u32],
        texture1: &[u32],
        luma: &[u8],
        exact_triangles: &[GpuRasterTriangle],
        foreground: &[u32],
    ) {
        // The CPU fallback delivers its whole image in `fb`; the GPU renderer
        // writes the presentation texture itself from the compute pass below,
        // so uploading `fb` there would just be overwritten (and is a different
        // size). Only the fallback needs this.
        if !self.exact_enabled {
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(fb),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(SCREEN_W as u32 * 4),
                    rows_per_image: Some(SCREEN_H as u32),
                },
                wgpu::Extent3d {
                    width: SCREEN_W as u32,
                    height: SCREEN_H as u32,
                    depth_or_array_layers: 1,
                },
            );
        }

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(_) => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        // Run the pass whenever the GPU renderer is on, *including* frames with
        // no 3D at all: the same pass composites the foreground tile layer, and
        // screens like the boot settings page live entirely in that layer over
        // an empty background. Gating on triangle count blanked them.
        if self.exact_enabled {
            let wide_w = self.native_width();
            self.ensure_texture(wide_w);
            let computed = self.exact.dispatch(
                &self.device,
                &mut enc,
                wide_w,
                exact_triangles,
                fb,
                texture0,
                texture1,
                luma,
                gpu_colors,
                foreground,
                self.smooth_shadows,
                self.stretch_2d,
            );
            let (w, h) = self.exact.output_dims(wide_w);
            enc.copy_buffer_to_texture(
                wgpu::ImageCopyBuffer {
                    buffer: &computed,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(self.exact.output_stride(wide_w) * 4),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::ImageCopyTexture {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.blit(&mut enc, &view);
        if let Some(ui) = ui {
            ui.draw(&self.device, &self.queue, &mut enc, &view);
        }
        self.queue.submit(Some(enc.finish()));
        frame.present();
    }

    /// Model 1 present. With the GPU rasterizer active, `fb` is only the 2D
    /// background layer: the compute pass draws the 3D quads over it and the
    /// resolve pass lays the foreground HUD on top. Otherwise `fb` is the
    /// complete CPU-composited frame and is just uploaded.
    pub fn present_model1(
        &mut self,
        ui: Option<&mut dyn UiPass>,
        fb: &[u32],
        quads: &[GpuQuad],
        foreground: &[u32],
    ) {
        if self.exact_m1.is_none() {
            self.upload_frame(fb);
        }

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(_) => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        let mut wide_w = 0;
        if self.exact_m1.is_some() {
            wide_w = self.native_width();
            self.ensure_texture(wide_w);
        }
        if let Some(m1) = &self.exact_m1 {
            let computed = m1.dispatch(
                &self.device,
                &mut enc,
                wide_w,
                quads,
                fb,
                foreground,
                self.smooth_shadows,
                self.stretch_2d,
            );
            let (w, h) = m1.output_dims(wide_w);
            enc.copy_buffer_to_texture(
                wgpu::ImageCopyBuffer {
                    buffer: &computed,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(m1.output_stride(wide_w) * 4),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::ImageCopyTexture {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.blit(&mut enc, &view);
        if let Some(ui) = ui {
            ui.draw(&self.device, &self.queue, &mut enc, &view);
        }
        self.queue.submit(Some(enc.finish()));
        frame.present();
    }

    fn upload_frame(&self, fb: &[u32]) {
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(fb),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(SCREEN_W as u32 * 4),
                rows_per_image: Some(SCREEN_H as u32),
            },
            wgpu::Extent3d {
                width: SCREEN_W as u32,
                height: SCREEN_H as u32,
                depth_or_array_layers: 1,
            },
        );
    }

    /// The letterboxed viewport the emulated image occupies inside the window,
    /// in physical pixels: `(x, y, width, height)`. The margins around it are
    /// the black bars. The front end maps the mouse through this for the
    /// lightgun so the aim tracks 1:1 even pillarboxed on an ultrawide.
    pub fn view_rect(&self) -> (f32, f32, f32, f32) {
        let (win_w, win_h) = (self.config.width as f32, self.config.height as f32);
        let scale = (win_w / self.tex_w as f32).min(win_h / self.tex_h as f32);
        let vp_w = (self.tex_w as f32 * scale).max(1.0);
        let vp_h = (self.tex_h as f32 * scale).max(1.0);
        let vp_x = ((win_w - vp_w) / 2.0).max(0.0);
        let vp_y = ((win_h - vp_h) / 2.0).max(0.0);
        (vp_x, vp_y, vp_w, vp_h)
    }

    fn blit(&self, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        // Fit the emulated image into the window preserving its aspect ratio,
        // letterboxed/pillarboxed, instead of stretching it across the whole
        // surface (which is what maximizing or fullscreening did before).
        let (vp_x, vp_y, vp_w, vp_h) = self.view_rect();
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Blit pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rp.set_viewport(vp_x, vp_y, vp_w, vp_h, 0.0, 1.0);
        rp.set_pipeline(&self.pipeline);
        rp.set_bind_group(0, &self.bind_group, &[]);
        rp.draw(0..3, 0..1);
    }
}
