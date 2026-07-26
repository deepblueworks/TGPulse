// Model 1 resolve pass: like gpu_resolve.wgsl, but averages the alpha channel
// along with RGB. The Model 1 tile layers use 0xFE as their opaque alpha (the
// CPU compositor preserves it), so forcing 0xFF here would make every
// tile-only pixel differ from the reference composite.

struct Words { data: array<u32> };
struct Pixels { data: array<u32> };
struct Params {
    count: u32, width: u32, height: u32,
    in_stride: u32, out_stride: u32, out_scale: u32, ss: u32, pad: u32,
    bin_stride: u32, bg_width: u32,
};

@group(0) @binding(0) var<storage, read> hires: Words;
@group(0) @binding(1) var<storage, read_write> resolved: Pixels;
@group(0) @binding(2) var<uniform> rparams: Params;
@group(0) @binding(3) var<storage, read> fg_tiles: Words;

@compute @workgroup_size(8, 8)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_w = rparams.width * rparams.out_scale;
    let out_h = rparams.height * rparams.out_scale;
    if (gid.x >= out_w || gid.y >= out_h) { return; }
    let out_index = gid.y * rparams.out_stride + gid.x;

    // Foreground is a native-resolution 2D layer: point-scale it (each board
    // pixel becomes an out_scale x out_scale block) rather than resample it.
    let native = vec2<u32>(gid.x / rparams.out_scale, gid.y / rparams.out_scale);
    var fg = 0u;
    if ((rparams.pad & 2u) != 0u) {
        let fg_x = min(native.x * rparams.bg_width / rparams.width, rparams.bg_width - 1u);
        fg = fg_tiles.data[native.y * rparams.bg_width + fg_x];
    } else {
        let margin = (rparams.width - rparams.bg_width) / 2u;
        if (native.x >= margin && native.x < margin + rparams.bg_width) {
            fg = fg_tiles.data[native.y * rparams.bg_width + native.x - margin];
        }
    }
    if ((fg >> 24u) != 0u) {
        resolved.data[out_index] = fg;
        return;
    }

    // Average the ss x ss dense samples that fall inside this output pixel,
    // alpha included (the CPU composite keeps the tile layer's 0xFE).
    let s = rparams.ss;
    var acc = vec4<u32>(0u, 0u, 0u, 0u);
    for (var sy = 0u; sy < s; sy++) {
        for (var sx = 0u; sx < s; sx++) {
            let p = hires.data[(gid.y * s + sy) * rparams.in_stride + gid.x * s + sx];
            acc += vec4<u32>((p >> 24u) & 255u, (p >> 16u) & 255u, (p >> 8u) & 255u, p & 255u);
        }
    }
    let n = s * s;
    resolved.data[out_index] = ((acc.x / n) << 24u) | ((acc.y / n) << 16u) | ((acc.z / n) << 8u) | (acc.w / n);
}
