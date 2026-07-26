// Exact GPU port of the CPU rasterizer in geometry.rs.
//
// Every function below mirrors its Rust counterpart, including the integer
// log2 table, the packed luma+coverage intermediate, the edge clamping rules
// and the micro-texture level. The point is not a lookalike: a pixel produced
// here should be the pixel the reference rasterizer produces, so the two can be
// diffed and the difference driven to zero.
//
// One thread per screen pixel walks its own 16x16 tile's triangle list in the
// hardware's front-to-back order and stops at the first covered, non-discarded
// polygon -- what the CPU's one-bit fill buffer does.

struct Triangle {
    a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, reciprocal_area: vec4<f32>,
    material: vec4<u32>, texture: vec4<u32>, viewport: vec4<i32>,
};
struct Triangles { data: array<Triangle> };
struct Pixels { data: array<u32> };
struct Words { data: array<u32> };
// `scale` is the supersampling factor: the raster pass runs on a grid this
// many times denser than the board's 496x384 and the resolve pass averages
// each scale x scale block back down. At scale 1 the two passes together are
// bit-identical to the CPU rasterizer, which is what `gpudiff` checks.
// width/height are the board's native size. `out_scale` is how many times the
// displayed image is larger than native (crispness); `ss` is the supersample
// per output pixel (antialiasing). The raster grid is native * out_scale * ss.
struct Params {
    count: u32, width: u32, height: u32,
    in_stride: u32, out_stride: u32, out_scale: u32, ss: u32, pad: u32,
    bin_stride: u32, bg_width: u32,
};

@group(0) @binding(0) var<storage, read> triangles: Triangles;
@group(0) @binding(1) var<storage, read_write> pixels: Pixels;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tex0: Words;
@group(0) @binding(4) var<storage, read> tex1: Words;
@group(0) @binding(5) var<storage, read> luma_ram: Words;
@group(0) @binding(6) var<storage, read> colors: Words;
@group(0) @binding(7) var<storage, read> bins: Words;
@group(0) @binding(8) var<storage, read> background: Words;

fn log2_table(i: u32) -> u32 {
    // `fast_log2`'s mantissa table from geometry.rs. A float log2 is not a
    // substitute: the mip level is `mml >> 7` and the trilinear blend factor is
    // `(mml & 127) << 1`, so the exact integer is what both sides must agree on.
    var t = array<u32, 128>(
        0u, 2u, 5u, 8u, 11u, 14u, 16u, 19u, 22u, 25u, 27u, 30u, 33u, 35u, 38u, 40u,
        43u, 46u, 48u, 51u, 53u, 56u, 58u, 61u, 63u, 65u, 68u, 70u, 73u, 75u, 77u, 80u,
        82u, 84u, 87u, 89u, 91u, 93u, 96u, 98u, 100u, 102u, 104u, 106u, 109u, 111u, 113u, 115u,
        117u, 119u, 121u, 123u, 125u, 127u, 129u, 132u, 134u, 136u, 138u, 140u, 141u, 143u, 145u, 147u,
        149u, 151u, 153u, 155u, 157u, 159u, 161u, 162u, 164u, 166u, 168u, 170u, 172u, 173u, 175u, 177u,
        179u, 181u, 182u, 184u, 186u, 188u, 189u, 191u, 193u, 194u, 196u, 198u, 200u, 201u, 203u, 205u,
        206u, 208u, 209u, 211u, 213u, 214u, 216u, 218u, 219u, 221u, 222u, 224u, 225u, 227u, 229u, 230u,
        232u, 233u, 235u, 236u, 238u, 239u, 241u, 242u, 244u, 245u, 247u, 248u, 250u, 251u, 253u, 254u,
    );
    return t[i];
}

fn fast_log2(value: f32) -> i32 {
    if (value < 0.0) { return 0; }
    let bits = bitcast<u32>(value) >> 16u;
    return ((i32(bits >> 7u) - 127) << 8) | i32(log2_table(bits & 127u));
}

fn edge(a: vec2<f32>, b: vec2<f32>, p: vec2<f32>) -> f32 {
    return (p.x - a.x) * (b.y - a.y) - (p.y - a.y) * (b.x - a.x);
}

fn luma_byte(index: u32) -> u32 {
    return (luma_ram.data[index >> 2u] >> ((index & 3u) * 8u)) & 255u;
}

fn get_texel(bx: i32, by: i32, x: i32, y: i32, sheet1: bool) -> u32 {
    var x2 = bx + x;
    var y2 = by + y;
    if (x2 >= 1024) { x2 -= 1024; y2 = y2 ^ 1024; }
    let off = u32((y2 / 2) * 512 + x2 / 2);
    var t = select(tex0.data[off >> 1u], tex1.data[off >> 1u], sheet1);
    if ((off & 1u) != 0u) { t >>= 16u; }
    if ((y & 1) == 0) { t >>= 8u; }
    if ((x & 1) == 0) { t >>= 4u; }
    return t & 15u;
}

/// Interpolates luma (bits 0-7) and coverage (bits 16-23) in one operation.
fn lerp_packed(x: u32, y: u32, f: u32) -> u32 {
    return (x + (((y - x) * f) >> 8u)) & 0x00ff00ffu;
}

/// One mip level, or the 128x128 micro-texture at level -1.
fn fetch(level: i32, h0: u32, h2: u32, u: f32, v: f32, translucent: bool) -> u32 {
    let width = i32(32u << (h0 & 7u));
    let height = i32(32u << ((h0 >> 3u) & 7u));

    var tw: i32; var th: i32; var bx: i32; var by: i32; var sheet1: bool;
    var uf: i32; var vf: i32;
    let primary = (h2 & 0x1000u) != 0u;
    if (level == -1) {
        let shift = 1i << ((h0 >> 10u) & 3u);
        tw = 128; th = 128;
        bx = i32((h2 >> 13u) & 1u) * 128;
        by = i32((h2 >> 14u) & 3u) * 128;
        sheet1 = !primary;
        uf = i32(u * 32.0) * shift;
        vf = i32(v * 32.0) * shift;
    } else {
        tw = width >> u32(level);
        th = height >> u32(level);
        bx = i32((u32(32 * i32(h2 & 0x3fu)) - 2048u) >> u32(level)) & 2047;
        by = i32((u32(32 * i32((h2 >> 6u) & 0x1fu)) - 1024u) >> u32(level)) & 1023;
        sheet1 = primary != ((level & 1) != 0);
        uf = i32(u * 32.0) >> u32(level);
        vf = i32(v * 32.0) >> u32(level);
    }

    if ((h0 & 0x100u) != 0u && (uf & (tw * 256)) != 0) { uf = ~uf; }
    if ((h0 & 0x200u) != 0u && (vf & (th * 256)) != 0) { vf = ~vf; }
    uf -= 0x80;
    vf -= 0x80;
    var fu = u32(uf & 255);
    var fv = u32(vf & 255);
    var x0 = (uf >> 8u) & (tw - 1);
    var x1 = (x0 + 1) & (tw - 1);
    var y0 = (vf >> 8u) & (th - 1);
    var y1 = (y0 + 1) & (th - 1);

    // Non-wrapping textures must not blend their last row/column against the
    // opposite edge.
    let wrap_x = (h0 & 0x40u) != 0u && (h0 & 0x100u) == 0u;
    let wrap_y = (h0 & 0x80u) != 0u && (h0 & 0x200u) == 0u;
    if (!wrap_x && x1 == 0) {
        if (fu >= 0x80u) { x0 = 0; x1 = 1; fu = 0u; }
        else { x1 = x0; x0 = max(x0 - 1, 0i); fu = 0x100u; }
    }
    if (!wrap_y && y1 == 0) {
        if (fv >= 0x80u) { y0 = 0; y1 = 1; fv = 0u; }
        else { y1 = y0; y0 = max(y0 - 1, 0i); fv = 0x100u; }
    }

    var a = get_texel(bx, by, x0, y0, sheet1) << 4u;
    var b = get_texel(bx, by, x1, y0, sheet1) << 4u;
    var c = get_texel(bx, by, x0, y1, sheet1) << 4u;
    var d = get_texel(bx, by, x1, y1, sheet1) << 4u;
    if (translucent) {
        // Coverage rides in bit 23 so it survives both filter stages. Testing
        // the raw 4-bit texel after interpolation instead makes fences, foliage
        // and car windows either vanish or turn opaque.
        if (a != 0xf0u) { a |= 0x00800000u; }
        if (b != 0xf0u) { b |= 0x00800000u; }
        if (c != 0xf0u) { c |= 0x00800000u; }
        if (d != 0xf0u) { d |= 0x00800000u; }
        if (a == 0xf0u) { a = b & 0xffu; }
        if (b == 0xf0u) { b = a & 0xffu; }
        if (c == 0xf0u) { c = d & 0xffu; }
        if (d == 0xf0u) { d = c & 0xffu; }
    }
    var ab = lerp_packed(a, b, fu);
    var cd = lerp_packed(c, d, fu);
    if (translucent) {
        if (ab == 0xf0u) { ab = cd & 0xffu; }
        if (cd == 0xf0u) { cd = ab & 0xffu; }
    }
    return lerp_packed(ab, cd, fv);
}

/// Luma in x; y is 0 when the texel is discarded, otherwise its coverage
/// (0x80 = fully covered). With smooth on, a translucent texture keeps its
/// interpolated coverage as a blend weight instead of being thresholded.
fn sample_texel(h0: u32, h2: u32, u: f32, v: f32, z: f32, texlod: i32, smooth_sh: bool) -> vec2<u32> {
    let width = i32(32u << (h0 & 7u));
    let height = i32(32u << ((h0 >> 3u) & 7u));
    // floor(log2(min(w,h))) - 1, as the reference derives from leading_zeros.
    let max_level = i32(31u - countLeadingZeros(u32(min(width, height)))) - 1;
    let mml = -texlod + fast_log2(z);
    let level = clamp(mml >> 7u, 0i, max_level);
    let translucent = (h0 & 0x2000u) != 0u;

    var t = fetch(level, h0, h2, u, v, translucent);
    if (mml > 0 && level < max_level) {
        t = lerp_packed(t, fetch(level + 1, h0, h2, u, v, translucent), u32((mml & 127i) * 2));
    } else if ((h0 & 0x1000u) != 0u && mml < 0) {
        let min_lod = (h0 >> 10u) & 3u;
        let f = u32(min((-mml) >> min_lod, 127i));
        t = lerp_packed(t, fetch(-1, h0, h2, u, v, translucent), f);
    }
    if (translucent) {
        let cov = (t >> 16u) & 0xffu;
        if (smooth_sh) {
            if (cov == 0u) { return vec2<u32>(0u, 0u); }
            return vec2<u32>(t & 0xffu, cov);
        }
        if (t < 0x00400000u) { return vec2<u32>(0u, 0u); }
    }
    return vec2<u32>(t & 0xffu, 0x80u);
}

/// Blends b over a with weight n/128: the perceptual equivalent of the
/// hardware's dithered transparency (checkerboard stipple is n = 64) when
/// smooth shadows are enabled (params.pad bit 0).
fn mixn(a: u32, b: u32, n: u32) -> u32 {
    let inv = 128u - n;
    let al = (((a >> 24u) & 255u) * inv + ((b >> 24u) & 255u) * n) >> 7u;
    let r = (((a >> 16u) & 255u) * inv + ((b >> 16u) & 255u) * n) >> 7u;
    let g = (((a >> 8u) & 255u) * inv + ((b >> 8u) & 255u) * n) >> 7u;
    let bl = ((a & 255u) * inv + (b & 255u) * n) >> 7u;
    return (al << 24u) | (r << 16u) | (g << 8u) | bl;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let div = params.out_scale * params.ss;
    let hi_w = params.width * div;
    let hi_h = params.height * div;
    if (gid.x >= hi_w || gid.y >= hi_h) { return; }

    // Sample position in the board's own pixel grid. With out_scale*ss == 1 this
    // is the reference's `(x + 0.5, y + 0.5)`; above that, the samples inside one
    // board pixel sit on its sub-pixel centres.
    let native = vec2<u32>(gid.x / div, gid.y / div);
    let q = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / f32(div);

    // Uncovered samples show the tile layers behind 3D. They are constant across
    // a board pixel, so both the resolve's average and the point-scaling return
    // them exactly -- only polygon edges gain intermediate values.
    // The tile layers are always bg_width wide. Widescreen: stretched across
    // the grid when params.pad bit 1 is set, else
    // centered at native width with black sides.
    var bg_color = 0xff000000u;
    var bg_x = 0u;
    var bg_show = false;
    if ((params.pad & 2u) != 0u) {
        bg_x = min(native.x * params.bg_width / params.width, params.bg_width - 1u);
        bg_show = true;
    } else {
        let margin = (params.width - params.bg_width) / 2u;
        if (native.x >= margin && native.x < margin + params.bg_width) {
            bg_x = native.x - margin;
            bg_show = true;
        }
    }
    if (bg_show) {
        bg_color = background.data[native.y * params.bg_width + bg_x];
    }
    pixels.data[gid.y * params.in_stride + gid.x] = bg_color;

    let bin = (native.y >> 4u) * params.bin_stride + (native.x >> 4u);
    let start = bins.data[bin * 2u];
    let count = bins.data[bin * 2u + 1u];

    let smooth_sh = (params.pad & 1u) != 0u;
    var blend_from = 0u;
    var blend_num = 64u;
    var have_blend = false;
    for (var j = 0u; j < count; j++) {
        let t = triangles.data[bins.data[params.bin_stride * 48u + start + j]];
        if (i32(native.x) < t.viewport.x || i32(native.x) > t.viewport.y ||
            i32(native.y) < t.viewport.z || i32(native.y) > t.viewport.w) { continue; }

        let e0 = edge(t.a.xy, t.b.xy, q);
        let e1 = edge(t.b.xy, t.c.xy, q);
        let e2 = edge(t.c.xy, t.a.xy, q);
        let inside = (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) ||
                     (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0);
        if (!inside) { continue; }

        var packed = t.material.x;
        if ((t.texture.z & 1u) != 0u) {
            let wa = e1 / t.reciprocal_area.w;
            let wb = e2 / t.reciprocal_area.w;
            let wc = e0 / t.reciprocal_area.w;
            let iz = wa * t.reciprocal_area.x + wb * t.reciprocal_area.y + wc * t.reciprocal_area.z;
            if (iz != iz || abs(iz) > 3.402823e38 || iz == 0.0) { continue; }
            // Same association as the reference: (w * uv) * rz, summed in a/b/c
            // order, then divided.
            let u = (wa * t.a.z * t.reciprocal_area.x + wb * t.b.z * t.reciprocal_area.y
                     + wc * t.c.z * t.reciprocal_area.z) / iz;
            let v = (wa * t.a.w * t.reciprocal_area.x + wb * t.b.w * t.reciprocal_area.y
                     + wc * t.c.w * t.reciprocal_area.z) / iz;
            let s = sample_texel(t.material.y, t.texture.x, u, v, 1.0 / iz,
                                 bitcast<i32>(t.texture.w), smooth_sh);
            if (s.y == 0u) { continue; }
            let li = ((t.material.z & 255u) << 7u) + (s.x >> 1u);
            let lum = min((luma_byte(li) * t.texture.y) / 256u, 63u);
            packed = colors.data[t.material.w * 64u + lum];

            // Smooth transparency: a translucent texture's partial coverage
            // (car windows, puddles) becomes a blend weight instead of the
            // hardware's keep/discard threshold, which reads as a mosaic.
            if (smooth_sh && (t.material.y & 0x2000u) != 0u && s.y < 0x80u) {
                if (!have_blend) {
                    have_blend = true;
                    blend_from = packed;
                    blend_num = s.y;
                    continue;
                }
            }
        }

        if ((t.texture.z & 2u) != 0u) {
            if (smooth_sh) {
                // Smooth shadows: this stippled triangle blends 50/50 with
                // whatever is behind it -- remember it and keep walking back.
                if (!have_blend) {
                    have_blend = true;
                    blend_from = packed;
                    blend_num = 64u;
                    continue;
                }
            } else if (((native.x ^ native.y) & 1u) == 0u) {
                // Checkerboard stipple. It is a board-pixel pattern, so it
                // keys off the native coordinate rather than the supersample
                // grid; keying off the dense grid would make it a finer,
                // different pattern.
                continue;
            }
        }
        if (have_blend) {
            packed = mixn(packed, blend_from, blend_num);
        }
        pixels.data[gid.y * params.in_stride + gid.x] = packed;
        return;
    }
    if (have_blend) {
        // Nothing covered behind the stippled triangle: blend with the tiles.
        let behind = bg_color;
        pixels.data[gid.y * params.in_stride + gid.x] = mixn(behind, blend_from, blend_num);
    }
}
