use tgpulse_core::model1_video::{gpu_quad_bins, GpuQuad};
use wgpu::util::DeviceExt;

/// A zero-length storage buffer is invalid. Frames with no 3D at all produce
/// an empty quad list, and the pass still has to run to composite the tile
/// layers.
fn pad(bytes: &[u8]) -> &[u8] {
    // Big enough to satisfy the largest element the shader declares (one
    // Quad); a smaller stub trips the binding-size check.
    if bytes.is_empty() {
        &[0u8; 64]
    } else {
        bytes
    }
}

pub const NATIVE_W: usize = 496;
pub const NATIVE_H: usize = 384;

/// Rounds a width in pixels up to a 64-pixel (256-byte) row stride, which is
/// what `copy_buffer_to_texture` requires.
fn row_stride(width: usize) -> usize {
    width.div_ceil(64) * 64
}

fn buffer_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Model 1 GPU rasterizer: flat-shaded, z-sorted quads, reusing the Model 2
/// resolve pass (supersample average + foreground composite). Parity with the
/// CPU rasterizer at out_scale 1 / ss 1 is what `gpudiff1` checks.
pub struct Model1Compute {
    raster: wgpu::ComputePipeline,
    resolve: wgpu::ComputePipeline,
    raster_bgl: wgpu::BindGroupLayout,
    resolve_bgl: wgpu::BindGroupLayout,
    pub out_scale: u32,
    pub ss: u32,
}

impl Model1Compute {
    pub fn output_dims(&self, wide_w: u32) -> (u32, u32) {
        (wide_w * self.out_scale, (NATIVE_H as u32) * self.out_scale)
    }

    pub fn output_stride(&self, wide_w: u32) -> u32 {
        row_stride(wide_w as usize * self.out_scale as usize) as u32
    }

    pub fn new(device: &wgpu::Device, out_scale: u32, ss: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Model 1 exact raster compute"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_model1.wgsl").into()),
        });

        let raster_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("m1 raster bgl"),
            entries: &[
                buffer_entry(0, true),  // quads
                buffer_entry(1, false), // supersampled pixels
                uniform_entry(2),
                buffer_entry(3, true), // tile bins
                buffer_entry(4, true), // background tiles
            ],
        });
        let resolve_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("m1 resolve bgl"),
            entries: &[
                buffer_entry(0, true),  // supersampled pixels
                buffer_entry(1, false), // native output
                uniform_entry(2),
                buffer_entry(3, true), // foreground tiles
            ],
        });

        let raster_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("m1 raster layout"),
            bind_group_layouts: &[&raster_bgl],
            push_constant_ranges: &[],
        });
        let resolve_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("m1 resolve layout"),
            bind_group_layouts: &[&resolve_bgl],
            push_constant_ranges: &[],
        });
        let raster = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("m1 exact raster"),
            layout: Some(&raster_layout),
            module: &shader,
            entry_point: "main",
        });
        let resolve_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Model 1 resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_resolve_m1.wgsl").into()),
        });
        let resolve = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("m1 resolve"),
            layout: Some(&resolve_layout),
            module: &resolve_shader,
            entry_point: "resolve",
        });
        Self {
            raster,
            resolve,
            raster_bgl,
            resolve_bgl,
            out_scale: out_scale.max(1),
            ss: ss.max(1),
        }
    }

    /// Rasterizes one frame; the returned buffer holds the image at
    /// `output_dims`, `output_stride` pixels per row.
    // One parameter per resource the compute pass binds.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        wide_w: u32,
        quads: &[GpuQuad],
        background: &[u32],
        foreground: &[u32],
        smooth_shadows: bool,
        stretch_2d: bool,
    ) -> wgpu::Buffer {
        let out_scale = self.out_scale as usize;
        let ss = self.ss as usize;
        let wide_w = wide_w as usize;
        let out_w = wide_w * out_scale;
        let out_h = NATIVE_H * out_scale;
        let out_stride = row_stride(out_w);
        let in_stride = out_stride * ss;
        let in_h = out_h * ss;

        let storage = |label, bytes: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: pad(bytes),
                usage: wgpu::BufferUsages::STORAGE,
            })
        };

        let quad_buf = storage("m1 quads", bytemuck::cast_slice(quads));
        let bg = storage("m1 background tiles", bytemuck::cast_slice(background));
        let fg = storage("m1 foreground tiles", bytemuck::cast_slice(foreground));

        let (ranges, indices) = gpu_quad_bins(quads, wide_w);
        let mut bin_data = Vec::with_capacity(ranges.len() * 2 + indices.len());
        for r in &ranges {
            bin_data.extend(r);
        }
        bin_data.extend(indices);
        let bins = storage("m1 tile bins", bytemuck::cast_slice(&bin_data));

        let hires = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("m1 supersampled pixels"),
            size: (in_stride * in_h * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("m1 resolved pixels"),
            size: (out_stride * out_h * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let params = |label| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&[
                    quads.len() as u32,
                    wide_w as u32,
                    NATIVE_H as u32,
                    in_stride as u32,
                    out_stride as u32,
                    self.out_scale,
                    self.ss,
                    u32::from(smooth_shadows) | (u32::from(stretch_2d) << 1),
                    (wide_w as u32).div_ceil(16),
                    NATIVE_W as u32,
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        };
        let raster_params = params("m1 raster params");
        let resolve_params = params("m1 resolve params");

        let raster_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("m1 raster bg"),
            layout: &self.raster_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: quad_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: hires.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: raster_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: bins.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: bg.as_entire_binding(),
                },
            ],
        });
        let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("m1 resolve bg"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: hires.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: out.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: resolve_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: fg.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Model 1 coverage"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.raster);
            pass.set_bind_group(0, &raster_bg, &[]);
            pass.dispatch_workgroups(
                (out_w * ss).div_ceil(8) as u32,
                (out_h * ss).div_ceil(8) as u32,
                1,
            );
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Model 1 resolve"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve);
            pass.set_bind_group(0, &resolve_bg, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(8) as u32, out_h.div_ceil(8) as u32, 1);
        }
        out
    }
}
