use tgpulse_core::geometry::{gpu_triangle_bins, GpuRasterTriangle};
use wgpu::util::DeviceExt;

/// A zero-length storage buffer is invalid. Frames with no 3D at all -- the
/// boot settings page lives entirely in the foreground tile layer -- have an
/// empty triangle and colour list, and the pass still has to run to composite
/// that layer.
fn pad(bytes: &[u8]) -> &[u8] {
    // Big enough to satisfy the largest element the shader declares (one
    // Triangle); a smaller stub trips the binding-size check.
    if bytes.is_empty() {
        &[0u8; 112]
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

pub struct ExactCompute {
    raster: wgpu::ComputePipeline,
    resolve: wgpu::ComputePipeline,
    raster_bgl: wgpu::BindGroupLayout,
    resolve_bgl: wgpu::BindGroupLayout,
    /// How many times larger than native the displayed image is. This is what
    /// makes 3D look crisp rather than blurry: the old path averaged everything
    /// down to native 496x384 before display, capping detail there.
    pub out_scale: u32,
    /// Supersamples per output pixel. This is the antialiasing. `out_scale == 1
    /// && ss == 1` reproduces the CPU rasterizer exactly, which is what
    /// `gpudiff` checks.
    pub ss: u32,
}

impl ExactCompute {
    /// Displayed image size in pixels for a given native render width.
    pub fn output_dims(&self, wide_w: u32) -> (u32, u32) {
        (wide_w * self.out_scale, (NATIVE_H as u32) * self.out_scale)
    }

    /// Row stride of the returned buffer, in pixels, for that width.
    pub fn output_stride(&self, wide_w: u32) -> u32 {
        row_stride(wide_w as usize * self.out_scale as usize) as u32
    }

    pub fn new(device: &wgpu::Device, out_scale: u32, ss: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Model 2 exact raster compute"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_compute.wgsl").into()),
        });

        // Two bind groups rather than one: together the passes want more
        // storage buffers than a single stage is guaranteed to allow.
        let raster_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raster bgl"),
            entries: &[
                buffer_entry(0, true),  // triangles
                buffer_entry(1, false), // supersampled pixels
                uniform_entry(2),
                buffer_entry(3, true), // texture sheet 0
                buffer_entry(4, true), // texture sheet 1
                buffer_entry(5, true), // luma RAM
                buffer_entry(6, true), // material colours
                buffer_entry(7, true), // tile bins
                buffer_entry(8, true), // background tiles
            ],
        });
        let resolve_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("resolve bgl"),
            entries: &[
                buffer_entry(0, true),  // supersampled pixels
                buffer_entry(1, false), // native output
                uniform_entry(2),
                buffer_entry(3, true), // foreground tiles
            ],
        });

        let raster_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("raster layout"),
            bind_group_layouts: &[&raster_bgl],
            push_constant_ranges: &[],
        });
        let resolve_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("resolve layout"),
            bind_group_layouts: &[&resolve_bgl],
            push_constant_ranges: &[],
        });
        let raster = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("exact raster"),
            layout: Some(&raster_layout),
            module: &shader,
            entry_point: "main",
        });
        let resolve_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Model 2 resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_resolve.wgsl").into()),
        });
        let resolve = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("resolve"),
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

    /// Rasterizes one frame; the returned buffer holds the native-resolution
    /// image, `STRIDE` pixels per row.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        wide_w: u32,
        triangles: &[GpuRasterTriangle],
        background: &[u32],
        texture0: &[u32],
        texture1: &[u32],
        luma: &[u8],
        colors: &[u32],
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
        // The dense grid is the output grid supersampled by ss; keeping its
        // stride at `out_stride * ss` lets the resolve index it as
        // `(y * ss + sy) * in_stride + x * ss + sx`.
        let in_stride = out_stride * ss;
        let in_h = out_h * ss;

        let storage = |label, bytes: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: pad(bytes),
                usage: wgpu::BufferUsages::STORAGE,
            })
        };

        let tri = storage("triangles", bytemuck::cast_slice(triangles));
        let t0 = storage("texture 0", bytemuck::cast_slice(texture0));
        let t1 = storage("texture 1", bytemuck::cast_slice(texture1));
        let lu = storage("luma", luma);
        let co = storage("colours", bytemuck::cast_slice(colors));
        let bg = storage("background tiles", bytemuck::cast_slice(background));
        let fg = storage("foreground tiles", bytemuck::cast_slice(foreground));

        let (ranges, indices) = gpu_triangle_bins(triangles, wide_w);
        let mut bin_data = Vec::with_capacity(ranges.len() * 2 + indices.len());
        for r in &ranges {
            bin_data.extend(r);
        }
        bin_data.extend(indices);
        let bins = storage("tile bins", bytemuck::cast_slice(&bin_data));

        let hires = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("supersampled pixels"),
            size: (in_stride * in_h * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resolved pixels"),
            size: (out_stride * out_h * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        // Both passes share the one Params struct (8 u32); only the strides they
        // act on differ, and each reads the pair it needs.
        let params = |bytes_label| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(bytes_label),
                contents: bytemuck::cast_slice(&[
                    triangles.len() as u32,
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
        let raster_params = params("raster params");
        let resolve_params = params("resolve params");

        let raster_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raster bg"),
            layout: &self.raster_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: tri.as_entire_binding(),
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
                    resource: t0.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: t1.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: lu.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: co.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: bins.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: bg.as_entire_binding(),
                },
            ],
        });
        let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resolve bg"),
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
                label: Some("Model 2 coverage"),
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
                label: Some("Model 2 resolve"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve);
            pass.set_bind_group(0, &resolve_bg, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(8) as u32, out_h.div_ceil(8) as u32, 1);
        }
        out
    }
}
