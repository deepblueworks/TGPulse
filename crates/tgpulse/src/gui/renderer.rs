//! Draws Dear ImGui's vertex stream with wgpu.
//!
//! ImGui hands over one vertex buffer, one index buffer and a list of draw
//! commands per frame, each command naming a scissor rectangle and a texture.
//! Only the font atlas is ever bound here, so this is a single pipeline with a
//! single bind group and a scissor change between commands.
//!
//! It is written out rather than taken from a crate because the published wgpu
//! backends track a different wgpu release than the emulator's compute
//! rasterizer does, and the rasterizer is not worth porting for a menu.

use imgui::{DrawCmd, DrawData, DrawVert};

use crate::platform::UiPass;

/// Screen-space transform, in the push-constant-free form: a uniform buffer
/// holding the orthographic scale and translation ImGui's projection needs.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    scale: [f32; 2],
    translate: [f32; 2],
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    vertex_capacity: u64,
    index_capacity: u64,
    /// Draw data captured for the frame currently being submitted.
    frame: Option<CapturedFrame>,
}

/// ImGui's per-frame output, flattened so it outlives the `Ui` borrow and can
/// be handed to the render pass later in the frame.
struct CapturedFrame {
    vertices: Vec<DrawVert>,
    indices: Vec<u16>,
    /// (index offset, index count, vertex offset, clip rect) per command.
    commands: Vec<(u32, u32, i32, [f32; 4])>,
    display_pos: [f32; 2],
    display_size: [f32; 2],
    framebuffer_scale: [f32; 2],
}

const INITIAL_VERTICES: u64 = 8 * 1024;
const INITIAL_INDICES: u64 = 24 * 1024;
const VERTEX_SIZE: u64 = std::mem::size_of::<DrawVert>() as u64;

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_atlas: &imgui::FontAtlasTexture<'_>,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("imgui-font"),
            size: wgpu::Extent3d {
                width: font_atlas.width,
                height: font_atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            font_atlas.data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(font_atlas.width * 4),
                rows_per_image: Some(font_atlas.height),
            },
            wgpu::Extent3d {
                width: font_atlas.width,
                height: font_atlas.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("imgui-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("imgui-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("imgui-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("imgui-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("imgui-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("imgui.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("imgui-layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("imgui-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_SIZE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Unorm8x4,
                            offset: 16,
                            shader_location: 2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        Self {
            pipeline,
            uniform_buffer,
            bind_group,
            vertices: empty_buffer(
                device,
                "imgui-vertices",
                wgpu::BufferUsages::VERTEX,
                INITIAL_VERTICES * VERTEX_SIZE,
            ),
            indices: empty_buffer(
                device,
                "imgui-indices",
                wgpu::BufferUsages::INDEX,
                INITIAL_INDICES * 2,
            ),
            vertex_capacity: INITIAL_VERTICES,
            index_capacity: INITIAL_INDICES,
            frame: None,
        }
    }

    /// Flattens a frame's draw data. Called while the ImGui context is still
    /// borrowed; the drawing itself happens later, inside the render pass.
    pub fn capture(&mut self, draw_data: &DrawData) {
        let mut vertices = Vec::new();
        let mut indices: Vec<u16> = Vec::new();
        let mut commands = Vec::new();

        for list in draw_data.draw_lists() {
            let vertex_offset = vertices.len() as i32;
            let index_offset = indices.len() as u32;
            vertices.extend_from_slice(list.vtx_buffer());
            indices.extend_from_slice(list.idx_buffer());
            for cmd in list.commands() {
                if let DrawCmd::Elements { count, cmd_params } = cmd {
                    commands.push((
                        index_offset + cmd_params.idx_offset as u32,
                        count as u32,
                        vertex_offset + cmd_params.vtx_offset as i32,
                        cmd_params.clip_rect,
                    ));
                }
            }
        }

        // A buffer write must be a whole number of 4-byte words, and indices
        // are 16-bit: an odd count needs one more. The draws address explicit
        // ranges, so the extra index is never referenced.
        if !indices.len().is_multiple_of(2) {
            indices.push(0);
        }

        self.frame = Some(CapturedFrame {
            vertices,
            indices,
            commands,
            display_pos: draw_data.display_pos,
            display_size: draw_data.display_size,
            framebuffer_scale: draw_data.framebuffer_scale,
        });
    }
}

impl UiPass for Renderer {
    fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    ) {
        let Some(frame) = self.frame.take() else {
            return;
        };
        if frame.commands.is_empty() || frame.vertices.is_empty() {
            return;
        }

        // Buffers only ever grow: menus settle at a stable size within a few
        // frames, so reallocating is a startup cost rather than a per-frame one.
        if frame.vertices.len() as u64 > self.vertex_capacity {
            self.vertex_capacity = (frame.vertices.len() as u64).next_power_of_two();
            self.vertices = empty_buffer(
                device,
                "imgui-vertices",
                wgpu::BufferUsages::VERTEX,
                self.vertex_capacity * VERTEX_SIZE,
            );
        }
        if frame.indices.len() as u64 > self.index_capacity {
            self.index_capacity = (frame.indices.len() as u64).next_power_of_two().max(2);
            self.indices = empty_buffer(
                device,
                "imgui-indices",
                wgpu::BufferUsages::INDEX,
                self.index_capacity * 2,
            );
        }
        // `DrawVert` is `#[repr(C)]` with the exact layout the vertex buffer
        // declares, but it does not implement `Pod`, so the reinterpretation is
        // spelled out here rather than copying every vertex into a mirror type.
        let vertex_bytes = unsafe {
            std::slice::from_raw_parts(
                frame.vertices.as_ptr() as *const u8,
                std::mem::size_of_val(frame.vertices.as_slice()),
            )
        };
        queue.write_buffer(&self.vertices, 0, vertex_bytes);
        queue.write_buffer(&self.indices, 0, bytemuck::cast_slice(&frame.indices));
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                scale: [
                    2.0 / frame.display_size[0].max(1.0),
                    -2.0 / frame.display_size[1].max(1.0),
                ],
                translate: [
                    -1.0 - frame.display_pos[0] * 2.0 / frame.display_size[0].max(1.0),
                    1.0 + frame.display_pos[1] * 2.0 / frame.display_size[1].max(1.0),
                ],
            }),
        );

        let (fb_w, fb_h) = (
            frame.display_size[0] * frame.framebuffer_scale[0],
            frame.display_size[1] * frame.framebuffer_scale[1],
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("imgui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);

        for (index_offset, count, vertex_offset, clip) in frame.commands {
            // Clip rects arrive in ImGui's coordinates; the scissor wants
            // framebuffer pixels, clamped to the target or wgpu rejects it.
            let x0 = ((clip[0] - frame.display_pos[0]) * frame.framebuffer_scale[0]).max(0.0);
            let y0 = ((clip[1] - frame.display_pos[1]) * frame.framebuffer_scale[1]).max(0.0);
            let x1 = ((clip[2] - frame.display_pos[0]) * frame.framebuffer_scale[0]).min(fb_w);
            let y1 = ((clip[3] - frame.display_pos[1]) * frame.framebuffer_scale[1]).min(fb_h);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            pass.set_scissor_rect(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
            pass.draw_indexed(index_offset..index_offset + count, vertex_offset, 0..1);
        }
    }
}

fn empty_buffer(
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    size: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
