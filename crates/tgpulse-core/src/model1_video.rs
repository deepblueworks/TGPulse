//! Sega Model 1 display-list uploads and the first CPU 3D rasterizer.
//!
//! This module is intentionally independent of the working Model 2 geometry
//! path. It uses the same command framing, object-chain decoder,
//! transform, lighting, four-plane frustum clip, painter sort and HUD ordering.

use crate::model1::Model1System;
use crate::tilemap::{SCREEN_H, SCREEN_W};

const TGP_RAM_BASE: u32 = 0x40000;
const TGP_RAM_WORDS: usize = 0x100000 - TGP_RAM_BASE as usize;
const POLY_RAM_WORDS: usize = 0x400000;
const MOIRE: u32 = 0x0100_0000;
const FRAC_SHIFT: i32 = 16;
const MAX_LIST_COMMANDS: usize = 20_000;

#[derive(Clone, Copy, Default)]
struct LightParam {
    diffuse: f32,
    ambient: f32,
    specular: f32,
    power: u8,
}

pub(crate) struct Model1VideoState {
    tgp_ram: Vec<u16>,
    poly_ram: Vec<u32>,
    lightparams: [LightParam; 256],
    /// Config's smooth_shadows: blend MOIRE quads 50/50 instead of stippling.
    pub(crate) smooth_shadows: bool,
}

impl Model1VideoState {
    pub(crate) fn new() -> Self {
        Self {
            tgp_ram: vec![0; TGP_RAM_WORDS],
            poly_ram: vec![0; POLY_RAM_WORDS],
            lightparams: [LightParam::default(); 256],
            smooth_shadows: false,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Point {
    x: f32,
    y: f32,
    z: f32,
    sx: i32,
    sy: i32,
}

#[derive(Clone, Copy)]
struct Quad {
    points: [Point; 4],
    z: f32,
    color: u32,
    moire: bool,
}

#[derive(Clone)]
struct View {
    xc: i32,
    yc: i32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    zoomx: f32,
    zoomy: f32,
    viewx: f32,
    viewy: f32,
    a_bottom: f32,
    a_top: f32,
    a_left: f32,
    a_right: f32,
    translation: [f32; 12],
    light: [f32; 3],
    lightparams: [LightParam; 256],
    spec_enable: bool,
    smooth_shadows: bool,
    /// Widescreen: pixels to shift projected x right, centering the scene in
    /// the widened buffer. Zero otherwise (projection is then bit-identical
    /// to the hardware path).
    ws_shift: f32,
    /// Viewport expansion factor (1.0 = hardware). recompute_frustum applies
    /// it to the base x1/x2, so the frustum stays anchored at xc even when
    /// later display-list commands recompute it.
    ws_expand: f32,
    /// The pixel-space viewport window the fill clamps to (base window
    /// expanded and shifted for widescreen; equal to x1/x2 otherwise).
    px1: i32,
    px2: i32,
}

impl View {
    fn new(lightparams: [LightParam; 256]) -> Self {
        let mut view = Self {
            xc: 0,
            yc: 0,
            x1: 0,
            y1: 0,
            x2: SCREEN_W as i32 - 1,
            y2: SCREEN_H as i32 - 1,
            zoomx: 1.0,
            zoomy: 1.0,
            viewx: 0.0,
            viewy: 0.0,
            a_bottom: 0.0,
            a_top: 0.0,
            a_left: 0.0,
            a_right: 0.0,
            translation: [0.0; 12],
            light: [0.0; 3],
            lightparams,
            spec_enable: false,
            smooth_shadows: false,
            ws_shift: 0.0,
            ws_expand: 1.0,
            px1: 0,
            px2: SCREEN_W as i32 - 1,
        };
        view.translation[0] = 1.0;
        view.translation[4] = 1.0;
        view.translation[8] = 1.0;
        view.recompute_frustum();
        view
    }

    fn recompute_frustum(&mut self) {
        if self.zoomx != 0.0 {
            // The frustum is view-space: it widens around xc, never with the
            // screen-space pixel shift. Idempotent under later zoom/translate
            // commands, which recompute it from the base window.
            let ex1 = self.xc as f32 - (self.xc - self.x1) as f32 * self.ws_expand;
            let ex2 = self.xc as f32 + (self.x2 - self.xc) as f32 * self.ws_expand;
            self.a_left = (ex1 - self.xc as f32 - self.viewx) / self.zoomx;
            self.a_right = (ex2 - self.xc as f32 - self.viewx) / self.zoomx;
        }
        if self.zoomy != 0.0 {
            self.a_bottom = (-self.y1 as f32 + self.yc as f32 - self.viewy) / self.zoomy;
            self.a_top = (-self.y2 as f32 + self.yc as f32 - self.viewy) / self.zoomy;
        }
    }

    fn set_viewport(&mut self, xc: i32, yc: i32, x1: i32, x2: i32, y1: i32, y2: i32) {
        self.xc = xc;
        self.yc = yc;
        self.x1 = x1;
        self.x2 = x2;
        self.y1 = y1;
        self.y2 = y2;
        self.px1 = x1;
        self.px2 = x2;
        self.recompute_frustum();
    }

    fn transform_point(&self, point: &mut Point) {
        let q = *point;
        let xx = self.translation[0] * q.x
            + self.translation[3] * q.y
            + self.translation[6] * q.z
            + self.translation[9];
        point.y = self.translation[1] * q.x
            + self.translation[4] * q.y
            + self.translation[7] * q.z
            + self.translation[10];
        point.z = self.translation[2] * q.x
            + self.translation[5] * q.y
            + self.translation[8] * q.z
            + self.translation[11];
        point.x = xx;
    }

    fn transform_vector(&self, vector: [f32; 3]) -> [f32; 3] {
        [
            self.translation[0] * vector[0]
                + self.translation[3] * vector[1]
                + self.translation[6] * vector[2],
            self.translation[1] * vector[0]
                + self.translation[4] * vector[1]
                + self.translation[7] * vector[2],
            self.translation[2] * vector[0]
                + self.translation[5] * vector[1]
                + self.translation[8] * vector[2],
        ]
    }

    fn project_point(&self, point: &mut Point) {
        if point.z == 0.0 {
            point.sx = 0;
            point.sy = 0;
            return;
        }
        let xx = point.x / point.z;
        let yy = point.y / point.z;
        point.sx = (self.xc as f32 + xx * self.zoomx + self.viewx + self.ws_shift) as i32;
        point.sy = (self.yc as f32 - (yy * self.zoomy + self.viewy)) as i32;
    }
}

#[derive(Default)]
pub struct Model1RenderStats {
    pub objects: usize,
    pub source_quads: usize,
    pub clipped_quads: usize,
    pub pixels: usize,
}

/// One z-sorted, clipped quad for the GPU compute rasterizer
/// (`hw/gpu_model1.wgsl`). The CPU rasterizer's fill rules are reproduced
/// there exactly, so the stream carries the same integer screen coordinates
/// the CPU's fixed-point span walker consumes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuQuad {
    /// Integer screen x of the four clipped vertices, in polygon order.
    pub xs: [i32; 4],
    /// Integer screen y of the four clipped vertices.
    pub ys: [i32; 4],
    /// Inclusive viewport the fill clamps to: x1, x2, y1, y2.
    pub viewport: [i32; 4],
    /// Final lit/translated colour, 0xAARRGGBB.
    pub color: u32,
    /// Checkerboard stipple flag.
    pub moire: u32,
    pub pad: [u32; 2],
}

impl GpuQuad {
    fn new(view: &View, quad: &Quad) -> Self {
        let mut out = Self {
            viewport: [view.px1, view.px2, view.y1, view.y2],
            color: quad.color & !MOIRE,
            moire: u32::from(quad.moire),
            ..Self::default()
        };
        for (index, point) in quad.points.iter().enumerate() {
            out.xs[index] = point.sx;
            out.ys[index] = point.sy;
        }
        out
    }
}

/// 16x16 screen-tile bins for the compute rasterizer, in draw order (so a
/// reversed per-bin walk visits quads front-to-back exactly as the CPU's
/// painter sort draws them back-to-front).
pub fn gpu_quad_bins(quads: &[GpuQuad], width: usize) -> (Vec<[u32; 2]>, Vec<u32>) {
    let bw = width.div_ceil(16);
    const BH: usize = 24;
    let xclamp = (width - 1) as i32;
    let mut bins = vec![Vec::<u32>::new(); bw * BH];
    for (i, q) in quads.iter().enumerate() {
        let xmin =
            q.xs.iter()
                .min()
                .copied()
                .unwrap_or(0)
                .max(q.viewport[0])
                .clamp(0, xclamp) as usize
                / 16;
        let xmax =
            q.xs.iter()
                .max()
                .copied()
                .unwrap_or(0)
                .min(q.viewport[1])
                .clamp(0, xclamp) as usize
                / 16;
        let ymin =
            q.ys.iter()
                .min()
                .copied()
                .unwrap_or(0)
                .max(q.viewport[2])
                .clamp(0, SCREEN_H as i32 - 1) as usize
                / 16;
        let ymax =
            q.ys.iter()
                .max()
                .copied()
                .unwrap_or(0)
                .min(q.viewport[3])
                .clamp(0, SCREEN_H as i32 - 1) as usize
                / 16;
        if xmin > xmax || ymin > ymax {
            continue;
        }
        for y in ymin..=ymax {
            for x in xmin..=xmax {
                bins[y * bw + x].push(i as u32);
            }
        }
    }
    let mut ranges = Vec::with_capacity(bw * BH);
    let mut indices = Vec::new();
    for bin in bins {
        let start = indices.len() as u32;
        indices.extend(bin);
        ranges.push([start, indices.len() as u32 - start]);
    }
    (ranges, indices)
}

/// Buffer select for rendering: the latched bit 6 of listctl[0]. The latch
/// (bit 3 -> bit 6 when bit 2 is clear) happens once per frame at vblank (see
/// Model1System::trigger_vblank), so this bit is stable across the frame even
/// though the game rewrites bit 3 mid-frame.
fn selected_list(control: u16) -> usize {
    usize::from(control & 0x40 != 0)
}

fn word(list: &[u8], address: usize) -> u32 {
    let byte = (address & 0x7fff) * 2;
    u16::from_le_bytes([
        list.get(byte).copied().unwrap_or(0),
        list.get(byte + 1).copied().unwrap_or(0),
    ]) as u32
}

fn readi(list: &[u8], address: usize) -> u32 {
    word(list, address) | (word(list, address + 1) << 16)
}

fn readi16(list: &[u8], address: usize) -> i16 {
    word(list, address) as u16 as i16
}

fn readf(list: &[u8], address: usize) -> f32 {
    f32::from_bits(readi(list, address))
}

fn u16_at(memory: &[u8], index: usize) -> u16 {
    let byte = index * 2;
    u16::from_le_bytes([
        memory.get(byte).copied().unwrap_or(0),
        memory.get(byte + 1).copied().unwrap_or(0),
    ])
}

fn skip_direct(list: &[u8], mut offset: usize) -> usize {
    offset += 18;
    for _ in 0..100_000 {
        let primitive_type = readi(list, offset + 2) & 3;
        if primitive_type == 0 {
            return offset + 4;
        }
        offset += if primitive_type == 2 { 12 } else { 20 };
    }
    offset
}

fn apply_color_upload(state: &mut Model1VideoState, list: &[u8], offset: usize) -> usize {
    let address = readi(list, offset + 2);
    let length = readi(list, offset + 4) as usize + 1;
    if length > 0x8000 {
        return offset;
    }

    if let Some(base) = address.checked_sub(TGP_RAM_BASE) {
        for index in 0..length {
            if let Some(destination) = state.tgp_ram.get_mut(base as usize + index) {
                *destination = readi16(list, offset + 6 + index * 2) as u16;
            }
        }
    }

    offset + 6 + length * 2
}

fn apply_polygon_upload(state: &mut Model1VideoState, list: &[u8], offset: usize) -> usize {
    let address = readi(list, offset + 2);
    let length = readi(list, offset + 4) as usize;
    if length > 0x8000 {
        return offset;
    }

    if let Some(base) = address.checked_sub(0x800000) {
        for index in 0..length {
            if let Some(destination) = state.poly_ram.get_mut(base as usize + index) {
                *destination = readi(list, offset + 6 + index * 2);
            }
        }
    }

    offset + 6 + length * 2
}

fn apply_light_upload(
    state: &mut Model1VideoState,
    mut view: Option<&mut View>,
    list: &[u8],
    offset: usize,
) -> usize {
    let address = readi(list, offset + 2) as usize;
    let length = readi(list, offset + 4) as usize;
    if length > 0x8000 {
        return offset;
    }

    for index in 0..length {
        let value = readi(list, offset + 6 + index * 2);
        let parameter = LightParam {
            diffuse: (value & 0xff) as f32 / 255.0,
            ambient: ((value >> 8) & 0xff) as f32 / 255.0,
            specular: ((value >> 16) & 0xff) as f32 / 255.0,
            power: (value >> 24) as u8,
        };

        if let Some(destination) = state.lightparams.get_mut(address + index) {
            *destination = parameter;
        }
        if let Some(current_view) = view.as_deref_mut() {
            if let Some(destination) = current_view.lightparams.get_mut(address + index) {
                *destination = parameter;
            }
        }
    }

    offset + 6 + length * 2
}

/// The reference vblank-side `tgp_scan`: retain renderer upload RAM even when no frame
/// was rasterized. This is essential because VR uploads its complete colour RAM
/// during startup, then later object lists only reference those persistent words.
pub(crate) fn scan_uploads(system: &mut Model1System) {
    if system.listctl[1] & 0x1f != 0x1f {
        return;
    }

    let selected = selected_list(system.listctl[0]);
    let list = system.display_list[selected].clone();
    let mut offset = 0usize;

    for _ in 0..MAX_LIST_COMMANDS {
        match readi(&list, offset) {
            0 => offset += 2,
            1 | 0x41 => offset += 8,
            2 => offset = skip_direct(&list, offset),
            3 => offset += 16,
            4 => {
                let next = apply_color_upload(&mut system.video, &list, offset);
                if next == offset {
                    break;
                }
                offset = next;
            }
            5 => {
                let next = apply_polygon_upload(&mut system.video, &list, offset);
                if next == offset {
                    break;
                }
                offset = next;
            }
            6 => {
                let next = apply_light_upload(&mut system.video, None, &list, offset);
                if next == offset {
                    break;
                }
                offset = next;
            }
            7 | 8 => offset += 4,
            9 => offset += 6,
            0x0a => offset += 8,
            0x0b => offset += 26,
            0x0c => offset += 6,
            0x0f => break,
            _ => break,
        }
    }
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length > 0.0 && length.is_finite() {
        [vector[0] / length, vector[1] / length, vector[2] / length]
    } else {
        [0.0; 3]
    }
}

fn determinant(p1: Point, p2: Point, p3: Point) -> f32 {
    let x1 = p2.x - p1.x;
    let y1 = p2.y - p1.y;
    let z1 = p2.z - p1.z;
    let x2 = p3.x - p1.x;
    let y2 = p3.y - p1.y;
    let z2 = p3.z - p1.z;

    p1.x * (y1 * z2 - y2 * z1) + p1.y * (z1 * x2 - z2 * x1) + p1.z * (x1 * y2 - x2 * y1)
}

fn compute_specular(
    enabled: bool,
    parameter: LightParam,
    normal: [f32; 3],
    light: [f32; 3],
    diffuse: f32,
) -> f32 {
    if !enabled || parameter.power == 0 || parameter.specular <= 0.0 {
        return 0.0;
    }

    let mut value = 2.0 * diffuse * normal[2] - light[2];
    if value <= 0.0 {
        return 0.0;
    }
    if parameter.power >= 2 {
        value *= value;
    }
    if parameter.power >= 4 {
        value *= value;
    }
    if parameter.power >= 7 {
        value *= value;
    }
    (value * parameter.specular).min(1.0)
}

fn expand5(value: u32) -> u32 {
    (value << 3) | (value >> 2)
}

fn object_color(system: &Model1System, view: &View, texture_address: u32, normal: [f32; 3]) -> u32 {
    let texture_index = match texture_address.checked_sub(TGP_RAM_BASE) {
        Some(index) => index as usize,
        None => return 0xff00_0000,
    };
    let texture_data = system
        .video
        .tgp_ram
        .get(texture_index)
        .copied()
        .unwrap_or(0);

    let palette_word = u16_at(
        &system.palette_ram,
        0x1000 | (texture_data as usize & 0x3ff),
    );
    let mut r = (palette_word & 0x1f) as usize;
    let mut g = ((palette_word >> 5) & 0x1f) as usize;
    let mut b = ((palette_word >> 10) & 0x1f) as usize;

    if ((texture_data >> 10) & 3) == 1 && system.frame_num & 1 != 0 {
        let old_b = b;
        b = r;
        r = g;
        g = old_b;
    }

    let diffuse = normal[0] * view.light[0] + normal[1] * view.light[1] + normal[2] * view.light[2];
    let light_mode = 0usize;
    let parameter = view.lightparams[light_mode];
    let specular = compute_specular(view.spec_enable, parameter, normal, view.light, diffuse);
    let level = parameter.ambient + parameter.diffuse * diffuse.max(0.0) + specular;
    let mut luminosity = (255.0 * level.min(1.0)) as i32;
    luminosity >>= 2;
    luminosity = luminosity.clamp(0, 0x3f);
    if texture_data & 0x400 != 0 {
        luminosity = 0x3f;
    }

    let lum = luminosity as usize;
    r = ((u16_at(&system.colorxlat_ram, (r << 8) | lum) >> 3) & 0x1f) as usize;
    g = ((u16_at(&system.colorxlat_ram, (g << 8) | lum | 0x2000) >> 3) & 0x1f) as usize;
    b = ((u16_at(&system.colorxlat_ram, (b << 8) | lum | 0x4000) >> 3) & 0x1f) as usize;

    0xff00_0000 | (expand5(r as u32) << 16) | (expand5(g as u32) << 8) | expand5(b as u32)
}

fn clipped(view: &View, plane: usize, point: Point) -> bool {
    match plane {
        0 => point.y > point.z * view.a_bottom,
        1 => point.y < point.z * view.a_top,
        2 => point.x < point.z * view.a_left,
        _ => point.x > point.z * view.a_right,
    }
}

fn intersection(view: &View, plane: usize, p1: Point, p2: Point) -> Point {
    let (a, p1_axis, p2_axis) = match plane {
        0 | 1 => (
            if plane == 0 {
                view.a_bottom
            } else {
                view.a_top
            },
            p1.y,
            p2.y,
        ),
        2 | 3 => (
            if plane == 2 {
                view.a_left
            } else {
                view.a_right
            },
            p1.x,
            p2.x,
        ),
        _ => unreachable!(),
    };

    let denominator = (p2.z - p1.z) * a - (p2_axis - p1_axis);
    if denominator.abs() < f32::EPSILON {
        return p1;
    }

    let t = (p2.z * a - p2_axis) / denominator;
    let mut point = Point {
        x: p1.x * t + p2.x * (1.0 - t),
        y: p1.y * t + p2.y * (1.0 - t),
        z: p1.z * t + p2.z * (1.0 - t),
        ..Point::default()
    };
    view.project_point(&mut point);
    point
}

fn clip_quad_recursive(view: &View, level: usize, quad: Quad, output: &mut Vec<Quad>) {
    if level == 4 {
        output.push(quad);
        return;
    }

    let outside = [
        clipped(view, level, quad.points[0]),
        clipped(view, level, quad.points[1]),
        clipped(view, level, quad.points[2]),
        clipped(view, level, quad.points[3]),
    ];

    if outside.iter().all(|value| !*value) {
        clip_quad_recursive(view, level + 1, quad, output);
        return;
    }
    if outside.iter().all(|value| *value) {
        return;
    }

    let start = (0..4)
        .find(|&index| outside[index] && !outside[(index + 3) & 3])
        .unwrap_or(0);

    let points = [
        quad.points[start],
        quad.points[(start + 1) & 3],
        quad.points[(start + 2) & 3],
        quad.points[(start + 3) & 3],
    ];
    let rotated = [
        outside[start],
        outside[(start + 1) & 3],
        outside[(start + 2) & 3],
        outside[(start + 3) & 3],
    ];

    let recurse = |new_points: [Point; 4], output: &mut Vec<Quad>| {
        clip_quad_recursive(
            view,
            level + 1,
            Quad {
                points: new_points,
                ..quad
            },
            output,
        );
    };

    if rotated[1] {
        if rotated[2] {
            let first = intersection(view, level, points[2], points[3]);
            let second = intersection(view, level, points[3], points[0]);
            recurse([first, points[3], second, second], output);
        } else {
            let first = intersection(view, level, points[1], points[2]);
            let second = intersection(view, level, points[3], points[0]);
            recurse([first, points[2], points[3], second], output);
        }
    } else if rotated[2] {
        let first = intersection(view, level, points[0], points[1]);
        let second = intersection(view, level, points[1], points[2]);
        recurse([first, points[1], second, second], output);

        let third = intersection(view, level, points[2], points[3]);
        let fourth = intersection(view, level, points[3], points[0]);
        recurse([third, points[3], fourth, fourth], output);
    } else {
        let first = intersection(view, level, points[0], points[1]);
        let second = intersection(view, level, points[3], points[0]);
        recurse([first, points[1], points[2], points[3]], output);
        recurse([points[3], second, first, first], output);
    }
}

// The display-list object header is this wide; naming its fields as a struct
// would restate the same list.
#[allow(clippy::too_many_arguments)]
fn push_object(
    system: &Model1System,
    view: &View,
    texture_address: u32,
    polygon_address: u32,
    mut size: u32,
    old_z: &mut f32,
    output: &mut Vec<Quad>,
    stats: &mut Model1RenderStats,
) {
    if texture_address == u32::MAX || size >= 0x1000000 {
        return;
    }

    let source = if polygon_address & 0x800000 != 0 {
        &system.video.poly_ram
    } else {
        &system.polygons
    };
    let mut cursor = (polygon_address & 0x7fffff) as usize;
    if cursor.checked_add(6).is_none() || cursor + 6 > source.len() {
        return;
    }

    stats.objects += 1;
    if size == 0 {
        size = u32::MAX;
    }

    let mut old_p0 = Point {
        x: f32::from_bits(source[cursor]),
        y: f32::from_bits(source[cursor + 1]),
        z: f32::from_bits(source[cursor + 2]),
        ..Point::default()
    };
    let mut old_p1 = Point {
        x: f32::from_bits(source[cursor + 3]),
        y: f32::from_bits(source[cursor + 4]),
        z: f32::from_bits(source[cursor + 5]),
        ..Point::default()
    };
    view.transform_point(&mut old_p0);
    view.transform_point(&mut old_p1);
    if old_p0.z > 0.0 {
        view.project_point(&mut old_p0);
    }
    if old_p1.z > 0.0 {
        view.project_point(&mut old_p1);
    }

    cursor += 6;
    let mut texture = texture_address;

    for _ in 0..size.min(100_000) {
        if cursor.checked_add(10).is_none() || cursor + 10 > source.len() {
            break;
        }

        let flags = source[cursor];
        let primitive_type = flags & 3;
        if primitive_type == 0 {
            break;
        }
        if flags & 0x1000 != 0 {
            texture = texture.wrapping_add(1);
        }

        let mut normal = [
            f32::from_bits(source[cursor + 1]),
            f32::from_bits(source[cursor + 2]),
            f32::from_bits(source[cursor + 3]),
        ];
        let mut p0 = Point {
            x: f32::from_bits(source[cursor + 4]),
            y: f32::from_bits(source[cursor + 5]),
            z: f32::from_bits(source[cursor + 6]),
            ..Point::default()
        };
        let mut p1 = Point {
            x: f32::from_bits(source[cursor + 7]),
            y: f32::from_bits(source[cursor + 8]),
            z: f32::from_bits(source[cursor + 9]),
            ..Point::default()
        };
        if primitive_type == 2 {
            p1 = p0;
        }

        let link = (flags >> 8) & 3;
        normal = normalize(view.transform_vector(normal));
        view.transform_point(&mut p0);
        view.transform_point(&mut p1);
        if p0.z > 0.0 {
            view.project_point(&mut p0);
        }
        if p1.z > 0.0 {
            view.project_point(&mut p1);
        }

        if link != 0 && (flags & 0x4000 != 0 || determinant(old_p1, old_p0, p0) <= 0.0) {
            let z = match (flags >> 10) & 3 {
                0 => *old_z,
                1 => {
                    *old_z = old_p1.z.min(old_p0.z).min(p0.z).min(p1.z);
                    *old_z
                }
                2 => {
                    *old_z = old_p1.z.max(old_p0.z).max(p0.z).max(p1.z);
                    *old_z
                }
                _ => 0.0,
            };

            let light_mode =
                (((flags >> 17) & 15) | if flags & 0x0040_0000 != 0 { 0x80 } else { 0 }) as usize;
            let mut color_view = view.clone();
            if light_mode < color_view.lightparams.len() {
                color_view.lightparams[0] = color_view.lightparams[light_mode];
            }

            let quad = Quad {
                points: [old_p1, old_p0, p0, p1],
                z,
                color: object_color(system, &color_view, texture, normal),
                moire: flags & 0x2000 != 0,
            };
            stats.source_quads += 1;
            clip_quad_recursive(view, 0, quad, output);
        }

        match link {
            0 | 2 => {
                old_p0 = p0;
                old_p1 = p1;
            }
            1 => old_p1 = p0,
            3 => old_p0 = p1,
            _ => {}
        }

        cursor += 10;
    }
}

/// 50/50 average of two 0xAARRGGBB pixels: the perceptual equivalent of the
/// hardware's checkerboard stipple when smooth_shadows is on.
fn mix50(a: u32, b: u32) -> u32 {
    let blend = |s: u32| (((a >> s) & 0xff) + ((b >> s) & 0xff)) / 2;
    (blend(24) << 24) | (blend(16) << 16) | (blend(8) << 8) | blend(0)
}

fn write_pixel(
    framebuffer: &mut [u32],
    view: &View,
    x: i32,
    y: i32,
    color: u32,
    moire: bool,
    stats: &mut Model1RenderStats,
) {
    if x < 0 || y < 0 || x >= SCREEN_W as i32 || y >= SCREEN_H as i32 {
        return;
    }

    let index = y as usize * SCREEN_W + x as usize;
    let color = color & !MOIRE;
    if moire && view.smooth_shadows {
        framebuffer[index] = mix50(framebuffer[index], color);
    } else {
        if moire && ((x ^ y) & 1) != 0 {
            return;
        }
        framebuffer[index] = color;
    }
    stats.pixels += 1;
}

// Endpoints, colour, depth and the two layers it writes.
#[allow(clippy::too_many_arguments)]
fn draw_line(
    framebuffer: &mut [u32],
    view: &View,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    quad: Quad,
    stats: &mut Model1RenderStats,
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;

    loop {
        if x0 >= view.px1 && x0 <= view.px2 && y0 >= view.y1 && y0 <= view.y2 {
            write_pixel(framebuffer, view, x0, y0, quad.color, quad.moire, stats);
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let doubled = error * 2;
        if doubled >= dy {
            error += dy;
            x0 += sx;
        }
        if doubled <= dx {
            error += dx;
            y0 += sy;
        }
    }
}

fn draw_span(
    framebuffer: &mut [u32],
    view: &View,
    mut x1: i32,
    x2: i32,
    y: i32,
    quad: Quad,
    stats: &mut Model1RenderStats,
) {
    while x1 <= x2 {
        write_pixel(framebuffer, view, x1, y, quad.color, quad.moire, stats);
        x1 += 1;
    }
}

fn fill_line(
    framebuffer: &mut [u32],
    view: &View,
    quad: Quad,
    y: i32,
    x1: i32,
    x2: i32,
    stats: &mut Model1RenderStats,
) {
    if y > view.y2 || y < view.y1 {
        return;
    }

    let mut left = x1 >> FRAC_SHIFT;
    let mut right = x2 >> FRAC_SHIFT;

    // This is the reference intersection test. When a span lies wholly outside one
    // side, the subsequent clamping leaves left > right and draws nothing.
    if left <= view.px2 || right >= view.px1 {
        if left < view.px1 {
            left = view.px1;
        }
        if right > view.px2 {
            right = view.px2;
        }
        draw_span(framebuffer, view, left, right, y, quad, stats);
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_slope(
    framebuffer: &mut [u32],
    view: &View,
    quad: Quad,
    mut x1: i32,
    mut x2: i32,
    mut slope1: i32,
    mut slope2: i32,
    mut y1: i32,
    mut y2: i32,
    stats: &mut Model1RenderStats,
) -> (i32, i32) {
    if y1 > view.y2 {
        return (x1, x2);
    }

    if y2 <= view.y1 {
        let delta = i64::from(y2 - y1);
        return (
            (i64::from(x1) + delta * i64::from(slope1)) as i32,
            (i64::from(x2) + delta * i64::from(slope2)) as i32,
        );
    }

    if y2 > view.y2 {
        y2 = view.y2 + 1;
    }

    if y1 < view.y1 {
        let delta = i64::from(view.y1 - y1);
        x1 = (i64::from(x1) + delta * i64::from(slope1)) as i32;
        x2 = (i64::from(x2) + delta * i64::from(slope2)) as i32;
        y1 = view.y1;
    }

    let mut swapped = false;
    if x1 > x2 || (x1 == x2 && slope1 > slope2) {
        std::mem::swap(&mut x1, &mut x2);
        std::mem::swap(&mut slope1, &mut slope2);
        swapped = true;
    }

    while y1 < y2 {
        let mut left = x1 >> FRAC_SHIFT;
        let mut right = x2 >> FRAC_SHIFT;

        if left <= view.px2 || right >= view.px1 {
            if left < view.px1 {
                left = view.px1;
            }
            if right > view.px2 {
                right = view.px2;
            }
            draw_span(framebuffer, view, left, right, y1, quad, stats);
        }

        x1 = x1.wrapping_add(slope1);
        x2 = x2.wrapping_add(slope2);
        y1 += 1;
    }

    // The reference swaps the nx1/nx2 output pointers along with the values, so each
    // side of the caller's walk keeps its own edge identity across the swap.
    if swapped {
        (x2, x1)
    } else {
        (x1, x2)
    }
}

fn slope(x1: i32, x2: i32, y1: i32, y2: i32) -> Option<i32> {
    let denominator = y1 - y2;
    if denominator == 0 {
        None
    } else {
        Some((i64::from(x1 - x2) / i64::from(denominator)) as i32)
    }
}

fn fill_quad(framebuffer: &mut [u32], view: &View, quad: Quad, stats: &mut Model1RenderStats) {
    let mut distinct = Vec::<(i32, i32)>::new();
    for point in quad.points {
        let position = (point.sx, point.sy);
        if !distinct.contains(&position) {
            distinct.push(position);
        }
    }

    // The reference treats A,A,B,B as a wireframe primitive.
    if distinct.len() == 2 {
        draw_line(
            framebuffer,
            view,
            distinct[0].0,
            distinct[0].1,
            distinct[1].0,
            distinct[1].1,
            quad,
            stats,
        );
        return;
    }
    if distinct.len() < 3 {
        return;
    }

    let mut xs = [0i32; 8];
    let mut ys = [0i32; 8];
    for index in 0..4 {
        let x = quad.points[index].sx.saturating_mul(1i32 << FRAC_SHIFT);
        let y = quad.points[index].sy;
        xs[index] = x;
        xs[index + 4] = x;
        ys[index] = y;
        ys[index + 4] = y;
    }

    let mut minimum = 0usize;
    let mut maximum = 0usize;
    for index in 1..4 {
        if ys[index] < ys[minimum] {
            minimum = index;
        }
        if ys[index] > ys[maximum] {
            maximum = index;
        }
    }

    let mut current_y = ys[minimum];
    let mut limit_y = ys[maximum];

    if current_y == limit_y {
        let mut left = xs[0];
        let mut right = xs[0];
        for &x in &xs[1..4] {
            left = left.min(x);
            right = right.max(x);
        }
        fill_line(framebuffer, view, quad, current_y, left, right, stats);
        return;
    }

    if current_y > view.y2 || limit_y <= view.y1 {
        return;
    }
    if limit_y > view.y2 {
        limit_y = view.y2;
    }

    let mut side1 = minimum + 4;
    let mut side2 = minimum;
    let mut x1;
    let mut x2;
    let mut slope1;
    let mut slope2;

    'startup: loop {
        while side1 > 0 && ys[side1 - 1] == current_y {
            side1 -= 1;
        }
        while side2 + 1 < ys.len() && ys[side2 + 1] == current_y {
            side2 += 1;
        }

        if side1 == 0 || side2 + 1 >= ys.len() {
            return;
        }

        x1 = xs[side1];
        x2 = xs[side2];

        let Some(value1) = slope(x1, xs[side1 - 1], current_y, ys[side1 - 1]) else {
            return;
        };
        let Some(value2) = slope(x2, xs[side2 + 1], current_y, ys[side2 + 1]) else {
            return;
        };

        slope1 = value1;
        slope2 = value2;

        loop {
            let next1 = ys[side1 - 1];
            let next2 = ys[side2 + 1];

            if next1 == next2 {
                (x1, x2) = fill_slope(
                    framebuffer,
                    view,
                    quad,
                    x1,
                    x2,
                    slope1,
                    slope2,
                    current_y,
                    next1,
                    stats,
                );
                current_y = next1;

                if current_y >= limit_y {
                    break 'startup;
                }

                side1 -= 1;
                side2 += 1;
                continue 'startup;
            }

            if next1 < next2 {
                (x1, x2) = fill_slope(
                    framebuffer,
                    view,
                    quad,
                    x1,
                    x2,
                    slope1,
                    slope2,
                    current_y,
                    next1,
                    stats,
                );
                current_y = next1;

                if current_y >= limit_y {
                    break 'startup;
                }

                side1 -= 1;
                while side1 > 0 && ys[side1 - 1] == current_y {
                    side1 -= 1;
                }
                if side1 == 0 {
                    return;
                }

                x1 = xs[side1];
                let Some(value) = slope(x1, xs[side1 - 1], current_y, ys[side1 - 1]) else {
                    return;
                };
                slope1 = value;
            } else {
                (x1, x2) = fill_slope(
                    framebuffer,
                    view,
                    quad,
                    x1,
                    x2,
                    slope1,
                    slope2,
                    current_y,
                    next2,
                    stats,
                );
                current_y = next2;

                if current_y >= limit_y {
                    break 'startup;
                }

                side2 += 1;
                while side2 + 1 < ys.len() && ys[side2 + 1] == current_y {
                    side2 += 1;
                }
                if side2 + 1 >= ys.len() {
                    return;
                }

                x2 = xs[side2];
                let Some(value) = slope(x2, xs[side2 + 1], current_y, ys[side2 + 1]) else {
                    return;
                };
                slope2 = value;
            }
        }
    }

    if current_y == limit_y {
        fill_line(framebuffer, view, quad, current_y, x1, x2, stats);
    }
}

/// Where the display-list walk sends its z-sorted quads: the CPU rasterizer's
/// framebuffer, or a stream for the GPU compute rasterizer.
enum RasterSink<'a> {
    Framebuffer(&'a mut [u32]),
    Quads(&'a mut Vec<GpuQuad>),
}

fn draw_queued(
    sink: &mut RasterSink,
    view: &View,
    queue: &mut Vec<Quad>,
    stats: &mut Model1RenderStats,
) {
    queue.sort_by(|left, right| right.z.total_cmp(&left.z));
    stats.clipped_quads += queue.len();
    for quad in queue.drain(..) {
        match sink {
            RasterSink::Framebuffer(framebuffer) => fill_quad(framebuffer, view, quad, stats),
            RasterSink::Quads(out) => out.push(GpuQuad::new(view, &quad)),
        }
    }
}

/// Renders command 0x01 objects between the background and category-1 HUD
/// tilemaps. Direct polygons and command 0x41 are deliberately left for later
/// milestones; Virtua Racing's measured 3000-frame workload uses neither.
pub fn render_below_hud(system: &mut Model1System, framebuffer: &mut [u32]) -> Model1RenderStats {
    let mut stats = Model1RenderStats::default();
    if framebuffer.len() < SCREEN_W * SCREEN_H {
        return stats;
    }
    walk_render_list(
        system,
        &mut RasterSink::Framebuffer(framebuffer),
        &mut stats,
        496.0,
    );
    stats
}

/// The GPU path: same walk, same sort, but the quads go to a stream the
/// compute rasterizer consumes. Colors, transforms and clipping stay here on
/// the CPU; only pixel coverage moves to the GPU.
pub fn gpu_quads(system: &mut Model1System) -> Vec<GpuQuad> {
    gpu_quads_ws(system, 496.0)
}

/// `wide_w` is the render width in native pixels: above 496, each viewport's
/// horizontal extent widens around its center (more field of view on the
/// sides, not a stretch) and the projection shifts to re-center.
pub fn gpu_quads_ws(system: &mut Model1System, wide_w: f32) -> Vec<GpuQuad> {
    let mut stats = Model1RenderStats::default();
    let mut quads = Vec::new();
    walk_render_list(
        system,
        &mut RasterSink::Quads(&mut quads),
        &mut stats,
        wide_w,
    );
    quads
}

fn walk_render_list(
    system: &mut Model1System,
    sink: &mut RasterSink,
    stats: &mut Model1RenderStats,
    wide_w: f32,
) {
    if system.listctl[1] & 0x1f != 0x1f {
        return;
    }

    let selected = selected_list(system.listctl[0]);
    let list = system.display_list[selected].clone();
    let mut view = View::new(system.video.lightparams);
    view.smooth_shadows = system.video.smooth_shadows;
    if wide_w > 496.0 {
        view.ws_shift = (wide_w - 496.0) / 2.0;
    }
    let ws_expand = wide_w / 496.0;
    let mut queue = Vec::<Quad>::new();
    let mut old_z = 0.0f32;
    let mut offset = 0usize;

    for _ in 0..MAX_LIST_COMMANDS {
        match readi(&list, offset) {
            0 => offset += 2,
            1 => {
                let texture = readi(&list, offset + 2);
                let polygon = readi(&list, offset + 4);
                let size = readi(&list, offset + 6);
                push_object(
                    system, &view, texture, polygon, size, &mut old_z, &mut queue, stats,
                );
                offset += 8;
            }
            0x41 => offset += 8,
            2 => offset = skip_direct(&list, offset),
            3 => {
                draw_queued(sink, &view, &mut queue, stats);

                let xc = readi16(&list, offset + 4) as i32;
                let yc = 383 - (readi16(&list, offset + 6) as i32 - 39);
                let x1 = readi16(&list, offset + 8) as i32;
                let y2 = 383 - (readi16(&list, offset + 10) as i32 - 39);
                let x2 = readi16(&list, offset + 12) as i32;
                let y1 = 383 - (readi16(&list, offset + 14) as i32 - 39);
                view.set_viewport(xc, yc, x1, x2, y1, y2);
                if ws_expand > 1.0 {
                    view.ws_expand = ws_expand;
                    // The pixel window (span clamp + GpuQuad.viewport) shifts
                    // right with the projection; the frustum is recomputed
                    // from the base window + ws_expand wherever needed.
                    let fcx = xc as f32;
                    view.px1 = (fcx - (fcx - x1 as f32) * ws_expand + view.ws_shift) as i32;
                    view.px2 = (fcx + (x2 as f32 - fcx) * ws_expand + view.ws_shift) as i32;
                    view.recompute_frustum();
                }
                offset += 16;
            }
            4 => {
                let next = apply_color_upload(&mut system.video, &list, offset);
                if next == offset {
                    break;
                }
                offset = next;
            }
            5 => {
                let next = apply_polygon_upload(&mut system.video, &list, offset);
                if next == offset {
                    break;
                }
                offset = next;
            }
            6 => {
                let next = apply_light_upload(&mut system.video, Some(&mut view), &list, offset);
                if next == offset {
                    break;
                }
                offset = next;
            }
            7 => {
                view.spec_enable = readi(&list, offset + 2) & 1 != 0;
                offset += 4;
            }
            8 => offset += 4,
            9 => {
                view.zoomx = readf(&list, offset + 2) * 4.0;
                view.zoomy = readf(&list, offset + 4) * 4.0;
                view.recompute_frustum();
                offset += 6;
            }
            0x0a => {
                view.light = normalize([
                    readf(&list, offset + 2),
                    readf(&list, offset + 4),
                    readf(&list, offset + 6),
                ]);
                offset += 8;
            }
            0x0b => {
                for index in 0..12 {
                    view.translation[index] = readf(&list, offset + 2 + index * 2);
                }
                offset += 26;
            }
            0x0c => {
                view.viewx = readf(&list, offset + 2);
                view.viewy = readf(&list, offset + 4);
                view.recompute_frustum();
                offset += 6;
            }
            0x0f => break,
            _ => break,
        }
    }

    draw_queued(sink, &view, &mut queue, stats);
}
