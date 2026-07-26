// Fullscreen triangle that blits the emulated framebuffer.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    // Oversized triangle covering the viewport; uv derived from clip space.
    var xy = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 3.0,  1.0),
    );
    var out: VsOut;
    let p = xy[idx];
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

@group(0) @binding(0) var fb_tex: texture_2d<f32>;
@group(0) @binding(1) var fb_smp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(fb_tex, fb_smp, in.uv);
}
