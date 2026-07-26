// Dear ImGui's vertex format, transformed into clip space by an orthographic
// scale/translate. The surface is sRGB, so vertex colours -- which ImGui
// authors in sRGB -- are converted to linear before blending.

struct Uniforms {
    scale: vec2<f32>,
    translate: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c <= vec3<f32>(0.04045);
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, cutoff);
}

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(position * u.scale + u.translate, 0.0, 1.0);
    out.uv = uv;
    out.color = vec4<f32>(srgb_to_linear(color.rgb), color.a);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color * textureSample(atlas, atlas_sampler, in.uv);
}
