// Exact GPU port of the Model 1 CPU rasterizer in model1_video.rs.
//
// The CPU side walks the reference integer scanline filler (fill_quad/fill_slope):
// vertices are integer pixels, edge x positions are tracked in 16.16 fixed
// point with a truncating slope division, and each row draws the inclusive
// span between the two active edges. This shader evaluates the same fixed
// point math in closed form per pixel: the span walk is a pure accumulation
// `x = x_top + (py - y_top) * slope`, so any row can be computed directly
// once the active edge of each polygon side is known.
//
// One thread per native pixel walks its 16x16 tile's quad list in reverse
// draw order (the CPU sorts far-to-near and overwrites, so the first covered
// quad here is the nearest one) and stops at the first hit -- the same final
// color as the CPU's painter loop, pixel for pixel.

struct Quad {
    xs: vec4<i32>,
    ys: vec4<i32>,
    viewport: vec4<i32>, // inclusive x1, x2, y1, y2
    color: u32,
    moire: u32,
    pad: vec2<u32>,
};
struct Quads { data: array<Quad> };
struct Pixels { data: array<u32> };
struct Words { data: array<u32> };
struct Params {
    count: u32, width: u32, height: u32,
    in_stride: u32, out_stride: u32, out_scale: u32, ss: u32, pad: u32,
    bin_stride: u32, bg_width: u32,
};

@group(0) @binding(0) var<storage, read> quads: Quads;
@group(0) @binding(1) var<storage, read_write> pixels: Pixels;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> bins: Words;
@group(0) @binding(4) var<storage, read> background: Words;

const FRAC: u32 = 16u;

// The reference draw_line (Bresenham) coverage, in closed form. The reference keeps
// one error value and tests it twice per step (`doubled >= dy` advances x,
// `doubled <= dx` advances y, both on the same pre-update error). Solving the
// recurrences gives the stepped coordinate of the k-th plotted point:
//   x-dominant: y(k) = y0 + sy * floor((2*k*ady + dx) / (2*dx)), k = 0..dx
//   y-dominant: x(m) = x0 + sx * floor((2*m*dx + ady) / (2*ady)), m = 0..ady
/// 50/50 average of two 0xAARRGGBB pixels: the perceptual equivalent of the
/// hardware's checkerboard stipple when smooth shadows are enabled
/// (params.pad bit 0).
fn mix50(a: u32, b: u32) -> u32 {
    let al = (((a >> 24u) & 255u) + ((b >> 24u) & 255u)) / 2u;
    let r = (((a >> 16u) & 255u) + ((b >> 16u) & 255u)) / 2u;
    let g = (((a >> 8u) & 255u) + ((b >> 8u) & 255u)) / 2u;
    let bl = ((a & 255u) + (b & 255u)) / 2u;
    return (al << 24u) | (r << 16u) | (g << 8u) | bl;
}

fn line_covers(x0: i32, y0: i32, x1: i32, y1: i32, px: i32, py: i32) -> bool {    let dx = abs(x1 - x0);
    let ady = abs(y1 - y0);
    let sx = sign(x1 - x0);
    let sy = sign(y1 - y0);
    if (dx >= ady) {
        if (dx == 0) { return px == x0 && py == y0; }
        let k = (px - x0) * sx;
        if (k < 0 || k > dx) { return false; }
        return py == y0 + sy * ((2 * k * ady + dx) / (2 * dx));
    }
    let m = (py - y0) * sy;
    if (m < 0 || m > ady) { return false; }
    return px == x0 + sx * ((2 * m * dx + ady) / (2 * ady));
}

// The shared part of coverage: which fill call (if any) governs a native
// pixel, and with what parameters. The integer rasterizer (div == 1) and the
// supersampled path (div > 1) then evaluate the same edges -- the first with
// The reference exact fixed-point row rule, the second continuously.
struct SpanInfo {
    kind: u32, // 0 none, 1 flat row, 2 fill_slope, 3 bottom fill_line, 4 wireframe
    // kind 1: flat-row span; kind 4: the two wireframe points (p0, p1 packed).
    flat_l: i32,
    flat_r: i32,
    // kinds 2/3: governing segment pair (16.16 anchors and fixed-point slopes).
    xa_top: i32,
    ya_top: i32,
    slope_a: i32,
    xb_top: i32,
    yb_top: i32,
    slope_b: i32,
    swapped: u32,
    limy: i32,
};

fn quad_span_info(q: Quad, px: i32, py: i32) -> SpanInfo {
    var info = SpanInfo(0u, 0, 0, 0, 0, 0, 0, 0, 0, 0u, 0);
    if (px < q.viewport.x || px > q.viewport.y || py < q.viewport.z || py > q.viewport.w) {
        return info;
    }

    // Order-preserving dedup; A,A,B,B is the wireframe primitive.
    var dxs = array<i32, 4>();
    var dys = array<i32, 4>();
    var n = 0;
    for (var i = 0; i < 4; i++) {
        var dup = false;
        for (var j = 0; j < n; j++) {
            if (dxs[j] == q.xs[i] && dys[j] == q.ys[i]) { dup = true; }
        }
        if (!dup) {
            dxs[n] = q.xs[i];
            dys[n] = q.ys[i];
            n++;
        }
    }
    if (n < 2) { return info; }
    if (n == 2) {
        info.kind = 4u;
        info.flat_l = dxs[0];
        info.flat_r = dys[0];
        info.xa_top = dxs[1];
        info.ya_top = dys[1];
        return info;
    }

    var mn = 0;
    var mx = 0;
    for (var i = 1; i < 4; i++) {
        if (q.ys[i] < q.ys[mn]) { mn = i; }
        if (q.ys[i] > q.ys[mx]) { mx = i; }
    }
    let ymin = q.ys[mn];
    let ymax = q.ys[mx];

    if (ymin == ymax) {
        if (py != ymin) { return info; }
        var left = q.xs[0];
        var right = q.xs[0];
        for (var i = 1; i < 4; i++) {
            left = min(left, q.xs[i]);
            right = max(right, q.xs[i]);
        }
        info.kind = 1u;
        info.flat_l = left;
        info.flat_r = right;
        return info;
    }

    // fill_quad draws nothing when the whole quad lies at or above the
    // viewport's top edge (limit_y <= view.y1).
    if (ymax <= q.viewport.z) { return info; }

    // Chain lengths: segments from mn to mx walking mod-4 backward (-1) and
    // forward (+1).
    var ka = 0;
    var kb = 0;
    for (var k = 1; k <= 3; k++) {
        if (((mn - k + 8) & 3) == mx && ka == 0) { ka = k; }
        if (((mn + k + 8) & 3) == mx && kb == 0) { kb = k; }
    }

    // Simulate the CPU walk's event sequence. Each fill_slope call spans rows
    // [cury, next) with the two active chain segments and evaluates its side
    // swap once at the start row; the walk advances one or both chains at each
    // event. A per-pixel "active segment" lookup is not equivalent here: the
    // chains can be non-monotonic in y (sliver quads), and only the sequential
    // walk knows which segment pair actually drew a row.
    let limy = min(ymax, q.viewport.w);
    if (py < ymin || py > limy) { return info; }

    var cury = ymin;
    var ta = 0;
    var tb = 0;
    for (var e = 0; e < 12; e++) {
        // Same-y runs collapse onto the last chain vertex at the current row.
        while (ta + 1 <= ka && q.ys[(mn - (ta + 1) + 8) & 3] == cury) { ta++; }
        while (tb + 1 <= kb && q.ys[(mn + (tb + 1) + 8) & 3] == cury) { tb++; }
        if (ta >= ka || tb >= kb) { return info; }

        let a0 = (mn - ta + 8) & 3;
        let a1 = (mn - (ta + 1) + 8) & 3;
        let b0 = (mn + tb + 8) & 3;
        let b1 = (mn + (tb + 1) + 8) & 3;
        let n1 = q.ys[a1];
        let n2 = q.ys[b1];
        let next = min(n1, n2);

        let xa_top = q.xs[a0] << FRAC;
        let xb_top = q.xs[b0] << FRAC;
        let slope_a = (xa_top - (q.xs[a1] << FRAC)) / (q.ys[a0] - q.ys[a1]);
        let slope_b = (xb_top - (q.xs[b1] << FRAC)) / (q.ys[b0] - q.ys[b1]);

        if (py < next) {
            // The governing fill_slope call: swap test at its start row
            // (after the viewport-top clamp).
            let swap_row = max(cury, q.viewport.z);
            let xa0 = xa_top + (swap_row - q.ys[a0]) * slope_a;
            let xb0 = xb_top + (swap_row - q.ys[b0]) * slope_b;
            info.kind = 2u;
            info.xa_top = xa_top;
            info.ya_top = q.ys[a0];
            info.slope_a = slope_a;
            info.xb_top = xb_top;
            info.yb_top = q.ys[b0];
            info.slope_b = slope_b;
            info.swapped = select(0u, 1u, xa0 > xb0 || (xa0 == xb0 && slope_a > slope_b));
            info.limy = limy;
            return info;
        }

        if (next >= limy) {
            // Walk's end. The final fill_line at limit_y (drawn only when the
            // quad's own bottom is in view) uses the accumulated edge x values
            // *positionally* and never swaps: crossed chains draw nothing.
            if (py == limy && limy == ymax) {
                info.kind = 3u;
                info.xa_top = xa_top;
                info.ya_top = q.ys[a0];
                info.slope_a = slope_a;
                info.xb_top = xb_top;
                info.yb_top = q.ys[b0];
                info.slope_b = slope_b;
                info.limy = limy;
            }
            return info;
        }

        cury = next;
        if (n1 == next) { ta++; }
        if (n2 == next) { tb++; }
    }
    return info;
}

// Continuous edge position in native pixels at a fractional row (the 16.16
// fixed-point walk evaluated without its integer row snap).
fn edge_px(x_top: i32, y_top: i32, slope: i32, row: f32) -> f32 {
    return (f32(x_top) + (row - f32(y_top)) * f32(slope)) / 65536.0;
}

/// The reference exact integer fill rule, bit-identical to the CPU rasterizer.
fn quad_covers(q: Quad, px: i32, py: i32) -> bool {
    let info = quad_span_info(q, px, py);
    switch info.kind {
        case 1u: {
            return px >= info.flat_l && px <= info.flat_r;
        }
        case 2u: {
            var left = (info.xa_top + (py - info.ya_top) * info.slope_a) >> FRAC;
            var right = (info.xb_top + (py - info.yb_top) * info.slope_b) >> FRAC;
            if (info.swapped != 0u) {
                let t = left;
                left = right;
                right = t;
            }
            return px >= left && px <= right;
        }
        case 3u: {
            let left = (info.xa_top + (info.limy - info.ya_top) * info.slope_a) >> FRAC;
            let right = (info.xb_top + (info.limy - info.yb_top) * info.slope_b) >> FRAC;
            return px >= left && px <= right;
        }
        case 4u: {
            return line_covers(info.flat_l, info.flat_r, info.xa_top, info.ya_top, px, py);
        }
        default: {
            return false;
        }
    }
}

/// Supersampled wireframe coverage: distance from the sample position to the
/// ideal segment, within half a native pixel (the hairline's width). The
/// integer Bresenham is kept for div == 1 parity; this is the same line the
/// reference rasterizes, seen at sub-pixel resolution.
fn line_covers_aa(x0: f32, y0: f32, x1: f32, y1: f32, qx: f32, qy: f32) -> bool {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    var t = 0.0;
    if (len2 > 0.0) {
        t = clamp(((qx - x0) * dx + (qy - y0) * dy) / len2, 0.0, 1.0);
    }
    let ex = x0 + t * dx - qx;
    let ey = y0 + t * dy - qy;
    return ex * ex + ey * ey <= 0.25;
}

/// Supersampled coverage: the same edges evaluated continuously at the
/// sample's fractional position (this is what anti-aliases the 3D layer).
/// The swap/event structure stays on the integer walk, so coverage can never
/// diverge from the reference by more than sub-pixel edge gradation.
fn quad_covers_aa(q: Quad, qx: f32, qy: f32) -> bool {
    let px = i32(floor(qx));
    let py = i32(floor(qy));
    let info = quad_span_info(q, px, py);
    switch info.kind {
        case 1u: {
            return f32(info.flat_l) <= qx && qx < f32(info.flat_r + 1);
        }
        case 2u: {
            var left = edge_px(info.xa_top, info.ya_top, info.slope_a, qy);
            var right = edge_px(info.xb_top, info.yb_top, info.slope_b, qy);
            if (info.swapped != 0u) {
                let t = left;
                left = right;
                right = t;
            }
            return left <= qx && qx <= right;
        }
        case 3u: {
            let left = edge_px(info.xa_top, info.ya_top, info.slope_a, f32(info.limy));
            let right = edge_px(info.xb_top, info.yb_top, info.slope_b, f32(info.limy));
            return left <= qx && qx <= right;
        }
        case 4u: {
            // Wireframe hairlines: the continuous capsule test (the native
            // Bresenham still applies on the div == 1 path, quad_covers).
            return line_covers_aa(f32(info.flat_l), f32(info.flat_r), f32(info.xa_top), f32(info.ya_top), qx, qy);
        }
        default: {
            return false;
        }
    }
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let div = params.out_scale * params.ss;
    let hi_w = params.width * div;
    let hi_h = params.height * div;
    if (gid.x >= hi_w || gid.y >= hi_h) { return; }

    let native = vec2<u32>(gid.x / div, gid.y / div);
    let px = i32(native.x);
    let py = i32(native.y);

    // The tile layers are always bg_width wide. Widescreen: stretched across
    // the grid when params.pad bit 1 is set, else centered with black sides.
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

    // Reverse draw order: the CPU sorts far-to-near and overwrites, so the
    // first covered quad walking backwards is the visible (nearest) one.
    // div == 1 is the reference's own integer rule (gpudiff1 checks it);
    // above that, coverage is sampled continuously for anti-aliasing.
    let qpos = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / f32(div);
    let smooth_sh = (params.pad & 1u) != 0u;
    var blend_from = 0u;
    var have_blend = false;
    for (var jj = 0u; jj < count; jj++) {
        let j = count - 1u - jj;
        let q = quads.data[bins.data[params.bin_stride * 48u + start + j]];
        var covered = false;
        if (div == 1u) {
            covered = quad_covers(q, px, py);
        } else {
            covered = quad_covers_aa(q, qpos.x, qpos.y);
        }
        if (!covered) { continue; }
        if (q.moire != 0u) {
            if (smooth_sh) {
                // Smooth shadows: this quad blends 50/50 with whatever is
                // behind it -- remember it and keep walking back.
                if (!have_blend) {
                    have_blend = true;
                    blend_from = q.color;
                    continue;
                }
            } else if (((native.x ^ native.y) & 1u) != 0u) {
                // Moire stipple drops odd checkerboard pixels (write_pixel).
                continue;
            }
        }
        var outc = q.color;
        if (have_blend) {
            outc = mix50(outc, blend_from);
        }
        pixels.data[gid.y * params.in_stride + gid.x] = outc;
        return;
    }
    if (have_blend) {
        // Nothing covered behind the shadow quad: blend with the tile layer.
        let behind = bg_color;
        pixels.data[gid.y * params.in_stride + gid.x] = mix50(blend_from, behind);
    }
}
