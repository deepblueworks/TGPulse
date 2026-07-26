//! HLE front end for the Model 2 geometrizer display list.
//!
//! The real geometry DSP is not emulated by the reference either: its command stream is
//! decoded behaviorally. Keep parsing bounded because the list lives in a
//! circular 128KB buffer controlled by the game.

use crate::system::Model2System;

#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub u: f32,
    pub v: f32,
}

#[inline]
fn cross(a: Vertex3, b: Vertex3, c: Vertex3) -> Vertex3 {
    let p = Vertex3 {
        x: b.x - a.x,
        y: b.y - a.y,
        z: b.z - a.z,
        ..Default::default()
    };
    let q = Vertex3 {
        x: c.x - a.x,
        y: c.y - a.y,
        z: c.z - a.z,
        ..Default::default()
    };
    Vertex3 {
        x: p.y * q.z - p.z * q.y,
        y: p.z * q.x - p.x * q.z,
        z: p.x * q.y - p.y * q.x,
        ..Default::default()
    }
}

#[inline]
fn normalized(mut v: Vertex3) -> Vertex3 {
    let n = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    if n != 0.0 {
        v.x /= n;
        v.y /= n;
        v.z /= n;
    }
    v
}

#[derive(Clone, Debug, Default)]
pub struct SolidPolygon {
    pub v: Vec<Vertex3>,
    pub z: f32,
    pub z_key: u16,
    pub sequence: u32,
    pub color_base: u16,
    pub luma: u8,
    pub texlod: i32,
    pub textured: bool,
    pub texheader: [u16; 4],
    pub viewport: [i16; 4],
    pub center: [i16; 2],
    pub window: u8,
}

/// Triangle stream consumed by the WGPU rasterizer. Geometry/HLE still
/// decodes the Model 2 display list, but pixel coverage is no longer tied to
/// the CPU renderer. Screen coordinates are kept in native-board units; the
/// GPU vertex shader maps them to whichever HD render target is active.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuVertex {
    /// Native screen x/y, camera-space z, reciprocal z.
    pub screen_z: [f32; 4],
    /// Texture coordinates and polygon-constant luma/LOD.
    pub uv_luma_lod: [f32; 4],
    /// Final untextured colour, texture header words 0/1, draw-order key.
    pub material: [u32; 4],
    /// Texture header words 2/3 and flags.
    pub texture: [u32; 4],
    /// Inclusive native-pixel viewport: xmin, xmax, ymin, ymax.
    pub viewport: [f32; 4],
}

/// One clipped fan triangle for the exact compute rasterizer. Values are laid
/// out as vec4s so the Rust and WGSL storage-buffer layouts are identical.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuRasterTriangle {
    /// screen x/y, u/z, v/z
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
    /// reciprocal z for a/b/c, signed triangle area
    pub reciprocal_area: [f32; 4],
    /// color, h0, h1, priority
    pub material: [u32; 4],
    /// h2, h3, flags, texlod
    pub texture: [u32; 4],
    /// inclusive xmin, xmax, ymin, ymax
    pub viewport: [i32; 4],
}

#[derive(Clone, Copy, Debug, Default)]
struct TextureParameter {
    diffuse: f32,
    ambient: f32,
    specular_control: u8,
    specular_scale: f32,
}

pub struct GeometryEngine {
    mode: u32,
    matrix: [f32; 12],
    focus: [f32; 2],
    light: Vertex3,
    lod: f32,
    coef: [f32; 32],
    texparam: [TextureParameter; 32],
    polygon_ram0: Vec<u32>,
    polygon_ram1: Vec<u32>,
    texture_ram: Vec<u16>,
    log_ram: Vec<u8>,
    viewport: [i16; 4],
    centers: [[i16; 2]; 4],
    cur_window: u8,
    z_adjust: u32,
    /// The reference keeps this in a `u8`, so the register write truncates: a value
    /// like 0x1ff lands as 0xff, which is the "z-clip disabled" encoding.
    /// Holding the full 32-bit word instead leaves the clip armed with a
    /// bogus threshold and culls most of a large object's polygons.
    master_z_clip: u8,
    polygon_z: f32,
    pub polygons: Vec<SolidPolygon>,
    pub unsupported_modes: u32,
}

impl Default for GeometryEngine {
    fn default() -> Self {
        Self {
            mode: 0,
            matrix: [0.0; 12],
            focus: [0.0; 2],
            light: Vertex3::default(),
            lod: 0.0,
            coef: [0.0; 32],
            texparam: [TextureParameter::default(); 32],
            polygon_ram0: vec![0; 0x8000],
            polygon_ram1: vec![0; 0x8000],
            texture_ram: vec![0; 0x10000],
            log_ram: vec![0; 0x8000],
            viewport: [0; 4],
            centers: [[0; 2]; 4],
            cur_window: 0,
            z_adjust: 0,
            master_z_clip: 0,
            polygon_z: 1.0e10,
            polygons: Vec::new(),
            unsupported_modes: 0,
        }
    }
}

impl GeometryEngine {
    pub fn mode(&self) -> u32 {
        self.mode
    }
    /// Translation column of the last matrix the display list loaded, i.e. the
    /// camera/world transform in effect at the end of the frame. Useful as a
    /// cheap "is anything actually moving" probe.
    pub fn last_translate(&self) -> [f32; 3] {
        [self.matrix[9], self.matrix[10], self.matrix[11]]
    }
    fn transform_point_raw(&self, p: Vertex3) -> Vertex3 {
        Vertex3 {
            x: p.x * self.matrix[0] + p.y * self.matrix[3] + p.z * self.matrix[6] + self.matrix[9],
            y: p.x * self.matrix[1] + p.y * self.matrix[4] + p.z * self.matrix[7] + self.matrix[10],
            z: p.x * self.matrix[2] + p.y * self.matrix[5] + p.z * self.matrix[8] + self.matrix[11],
            u: p.u,
            v: p.v,
        }
    }
    fn apply_focus(&self, mut p: Vertex3) -> Vertex3 {
        p.x *= self.focus[0];
        p.y *= self.focus[1];
        p
    }
    /// The geometrizer-to-rasterizer bus carries the upper 24 bits of each
    /// IEEE-754 word (`f2u(value) >> 8`). Reconstructing that word before
    /// clipping is observable for vertices very close to the eye plane.
    #[inline]
    fn raster_coord(mut p: Vertex3) -> Vertex3 {
        p.x = f32::from_bits(p.x.to_bits() & !0xff);
        p.y = f32::from_bits(p.y.to_bits() & !0xff);
        p.z = f32::from_bits(p.z.to_bits() & !0xff);
        p
    }
    fn transform_vector(&self, p: Vertex3) -> Vertex3 {
        Vertex3 {
            x: p.x * self.matrix[0] + p.y * self.matrix[3] + p.z * self.matrix[6],
            y: p.x * self.matrix[1] + p.y * self.matrix[4] + p.z * self.matrix[7],
            z: p.x * self.matrix[2] + p.y * self.matrix[5] + p.z * self.matrix[8],
            u: p.u,
            v: p.v,
        }
    }
    fn v(src: &[u32], p: &mut usize) -> Option<Vertex3> {
        let v = Vertex3 {
            x: f32::from_bits(*src.get(*p)?),
            y: f32::from_bits(*src.get(*p + 1)?),
            z: f32::from_bits(*src.get(*p + 2)?),
            u: 0.0,
            v: 0.0,
        };
        *p += 3;
        Some(v)
    }
    fn tex_word(&self, rom: &[u16], address: u32) -> u16 {
        if address & 0x800000 != 0 {
            self.texture_ram[(address as usize) & 0xffff]
        } else {
            rom[(address as usize) & (rom.len() - 1)]
        }
    }

    // Every parameter is one of the object command's own fields; bundling them
    // into a struct would only rename the same list.
    #[allow(clippy::too_many_arguments)]
    fn parse_object(
        &mut self,
        src: &[u32],
        mut p: usize,
        count: u32,
        mut tpa: u32,
        mut tha: u32,
        center_sel: usize,
        texture_rom: &[u16],
    ) {
        let Some(mut p0_raw) = Self::v(src, &mut p).map(|v| self.transform_point_raw(v)) else {
            return;
        };
        let Some(mut p1_raw) = Self::v(src, &mut p).map(|v| self.transform_point_raw(v)) else {
            return;
        };
        let mut p0 = Self::raster_coord(self.apply_focus(p0_raw));
        let mut p1 = Self::raster_coord(self.apply_focus(p1_raw));
        let normals_present = self.mode & 2 == 0;
        let count = if count == 0 { 0xfffff } else { count };
        for _ in 0..count {
            let Some(&attr) = src.get(p) else { break };
            p += 1;
            // Both parse variants consume three words here: the modes with
            // normals read one, the modes without skip the same space and
            // derive the normal from the face's cross product instead.
            let Some(stored_normal) = Self::v(src, &mut p) else {
                break;
            };
            if attr & 3 == 0 {
                break;
            }
            let Some(p2_raw) = Self::v(src, &mut p).map(|v| self.transform_point_raw(v)) else {
                break;
            };
            let normal = if normals_present {
                self.transform_vector(stored_normal)
            } else {
                normalized(cross(p0_raw, p1_raw, p2_raw))
            };
            let dotl = normal.x * self.light.x + normal.y * self.light.y + normal.z * self.light.z;
            let dotp = normal.x * p2_raw.x + normal.y * p2_raw.y + normal.z * p2_raw.z;
            let p2 = Self::raster_coord(self.apply_focus(p2_raw));
            let tp = self.texparam[((attr >> 18) & 0x1f) as usize];
            let lum = if dotl * dotp < 0.0 { 0.0 } else { dotl.abs() };
            let mut specular = 0.0;
            if self.mode & 1 != 0 && tp.specular_control != 0 {
                specular = ((2.0 * dotl) * normal.z - self.light.z).max(0.0);
                if tp.specular_control >> 1 != 0 {
                    specular *= specular
                }
                if tp.specular_control >> 2 != 0 {
                    specular *= specular
                }
                if (tp.specular_control.wrapping_add(1)) >> 3 != 0 {
                    specular *= specular
                }
                specular *= tp.specular_scale;
            }
            let luma = (lum * tp.diffuse + tp.ambient + specular).clamp(0.0, 255.0) as u8;
            let distance = self.coef[((attr >> 27) & 31) as usize] * dotp.abs() * self.lod;
            let lodword = distance.to_bits() >> 8;
            let texlod = (((lodword >> 8) & 0x7f80) as i32 - 0x3f80)
                + self.log_ram[(lodword as usize) & 0x7fff] as i32;
            let face = dotp < 0.0;
            let mut verts = vec![p1, p0, p2];
            let (p3_raw, p3) = if attr & 1 != 0 {
                let Some(q_raw) = Self::v(src, &mut p).map(|v| self.transform_point_raw(v)) else {
                    break;
                };
                let q = Self::raster_coord(self.apply_focus(q_raw));
                verts.push(q);
                (q_raw, q)
            } else {
                if p + 3 > src.len() {
                    break;
                }
                p += 3;
                (p2_raw, p2)
            };

            let th = [
                self.tex_word(texture_rom, tha),
                self.tex_word(texture_rom, tha + 1),
                self.tex_word(texture_rom, tha + 2),
                self.tex_word(texture_rom, tha + 3),
            ];
            for (i, v) in verts.iter_mut().enumerate() {
                v.v = self.tex_word(texture_rom, tpa + i as u32 * 2) as f32;
                v.u = self.tex_word(texture_rom, tpa + i as u32 * 2 + 1) as f32;
            }
            let color_base = (th[3] >> 6) & 0x3ff;
            let min_z = verts.iter().map(|v| v.z).fold(f32::INFINITY, f32::min);
            let max_z = verts.iter().map(|v| v.z).fold(f32::NEG_INFINITY, f32::max);
            let cull = self.check_culling(attr, face, min_z, max_z);
            let z = match (attr >> 10) & 3 {
                0 => self.polygon_z,
                1 => min_z,
                2 => max_z,
                _ => 1.0e10,
            };
            self.polygon_z = z;
            if !cull
                && verts
                    .iter()
                    .all(|v| v.x.is_finite() && v.y.is_finite() && v.z.is_finite())
            {
                let sequence = self.polygons.len() as u32;
                self.polygons.push(SolidPolygon {
                    v: verts,
                    z,
                    z_key: float_to_zval(z, self.z_adjust),
                    sequence,
                    color_base,
                    luma,
                    texlod,
                    textured: th[0] & 0x4000 != 0,
                    texheader: th,
                    viewport: self.viewport,
                    center: self.centers[center_sel & 3],
                    window: self.cur_window,
                });
            }
            tpa = tpa.wrapping_add(if attr & 1 != 0 { 8 } else { 6 });
            let mut tho = ((attr >> 12) & 0x1f) as i32;
            if tho & 0x10 != 0 {
                tho -= 0x20;
            }
            tha = tha.wrapping_add_signed(tho * 4);
            match (attr >> 8) & 3 {
                0 | 2 => {
                    p0 = p2;
                    p1 = p3;
                    p0_raw = p2_raw;
                    p1_raw = p3_raw;
                }
                1 => {
                    p1 = p2;
                    p1_raw = p2_raw;
                }
                3 => {
                    p0 = p3;
                    p0_raw = p3_raw;
                }
                _ => {}
            }
        }
        let _ = (tpa, self.lod, self.coef[0], self.log_ram[0]);
    }

    /// Command 02/12 bypasses the geometrizer math and feeds the hardware
    /// rasterizer's polygon stream directly. Coordinates/luma/distance lose
    /// their low eight bits on that bus, exactly as in `geo_direct_data`.
    fn parse_direct(
        &mut self,
        src: &[u32],
        mut p: usize,
        mut tpa: u32,
        mut tha: u32,
        center_sel: usize,
        texture_rom: &[u16],
    ) -> usize {
        let read_point = |src: &[u32], p: &mut usize| -> Option<Vertex3> {
            let q = Vertex3 {
                x: f32::from_bits(*src.get(*p)? & !0xff),
                y: f32::from_bits(*src.get(*p + 1)? & !0xff),
                z: f32::from_bits(*src.get(*p + 2)? & !0xff),
                ..Default::default()
            };
            *p += 3;
            Some(q)
        };
        let Some(mut p0) = read_point(src, &mut p) else {
            return src.len();
        };
        let Some(mut p1) = read_point(src, &mut p) else {
            return src.len();
        };
        loop {
            let Some(&attr) = src.get(p) else {
                return src.len();
            };
            p += 1;
            if attr & 3 == 0 {
                break;
            }
            let Some(&luma_word) = src.get(p) else {
                return src.len();
            };
            let Some(&distance_word) = src.get(p + 1) else {
                return src.len();
            };
            p += 2;
            let Some(p2) = read_point(src, &mut p) else {
                return src.len();
            };
            let mut verts = vec![p1, p0, p2];
            let p3 = if attr & 1 != 0 {
                let Some(q) = read_point(src, &mut p) else {
                    return src.len();
                };
                verts.push(q);
                q
            } else {
                p2
            };

            let th = [
                self.tex_word(texture_rom, tha),
                self.tex_word(texture_rom, tha + 1),
                self.tex_word(texture_rom, tha + 2),
                self.tex_word(texture_rom, tha + 3),
            ];
            for (i, v) in verts.iter_mut().enumerate() {
                v.v = self.tex_word(texture_rom, tpa + i as u32 * 2) as f32;
                v.u = self.tex_word(texture_rom, tpa + i as u32 * 2 + 1) as f32;
            }
            let min_z = verts.iter().map(|v| v.z).fold(f32::INFINITY, f32::min);
            let max_z = verts.iter().map(|v| v.z).fold(f32::NEG_INFINITY, f32::max);
            let face = luma_word & 0x8000_0000 != 0;
            let cull = self.check_culling(attr, face, min_z, max_z);
            let z = match (attr >> 10) & 3 {
                0 => self.polygon_z,
                1 => min_z,
                2 => max_z,
                _ => 1.0e10,
            };
            self.polygon_z = z;
            if !cull
                && verts
                    .iter()
                    .all(|v| v.x.is_finite() && v.y.is_finite() && v.z.is_finite())
            {
                let d = distance_word >> 8;
                let texlod = (((d >> 8) & 0x7f80) as i32 - 0x3f80)
                    + self.log_ram[(d as usize) & 0x7fff] as i32;
                self.polygons.push(SolidPolygon {
                    v: verts,
                    z,
                    z_key: float_to_zval(z, self.z_adjust),
                    sequence: self.polygons.len() as u32,
                    color_base: (th[3] >> 6) & 0x3ff,
                    luma: ((luma_word >> 23) & 0xff) as u8,
                    texlod,
                    textured: th[0] & 0x4000 != 0,
                    texheader: th,
                    viewport: self.viewport,
                    center: self.centers[center_sel & 3],
                    window: self.cur_window,
                });
            }
            tpa = tpa.wrapping_add(if attr & 1 != 0 { 8 } else { 6 });
            let mut tho = ((attr >> 12) & 0x1f) as i32;
            if tho & 0x10 != 0 {
                tho |= -16;
            }
            tha = tha.wrapping_add_signed(tho * 4);
            match (attr >> 8) & 3 {
                0 | 2 => {
                    p0 = p2;
                    p1 = p3;
                }
                1 => p1 = p2,
                3 => p0 = p3,
                _ => {}
            }
        }
        p
    }

    pub fn parse(
        &mut self,
        buffer: &[u32],
        polygon_rom: &[u32],
        texture_rom: &[u16],
        read_address: u32,
    ) {
        self.polygons.clear();
        self.cur_window = 0;
        self.polygon_z = 1.0e10;
        let mut p = ((read_address & 0x1ffff) >> 2) as usize;
        log::trace!(target: "geo", "list read_address={read_address:08X} word={p:05X}");
        for _ in 0..0x8000 {
            let Some(&opcode) = buffer.get(p) else { break };
            p += 1;
            if opcode & 0x80000000 != 0 {
                p = ((opcode & 0x1ffff) >> 2) as usize;
                continue;
            }
            let cmd = (opcode >> 23) & 0x1f;
            log::trace!(target: "geo", "cmd at={:05X} op={opcode:08X} cmd={cmd:02X}", p - 1);
            match cmd {
                0x01 | 0x11 => {
                    if p + 4 > buffer.len() {
                        break;
                    }
                    let tpa = buffer[p];
                    let tha = buffer[p + 1];
                    let oba = buffer[p + 2];
                    let n = buffer[p + 3];
                    p += 4;
                    log::trace!(
                        target: "geo",
                        "object at={:05X} tpa={tpa:08X} tha={tha:08X} oba={oba:08X} n={n:08X} mode={}",
                        p - 4,
                        self.mode
                    );
                    let center_sel = ((opcode >> 29) & 3) as usize;
                    // The three object sources are tested in this order: fast
                    // polygon RAM wins over the ROM, so an address carrying
                    // both bits is RAM. Testing the ROM bit first sends
                    // dynamically built models -- Virtua Fighter 2's hair, for
                    // one -- off to read unrelated ROM as geometry.
                    if oba & 0x01000000 != 0 || oba & 0x00800000 == 0 {
                        let ram = if oba & 0x01000000 != 0 {
                            &self.polygon_ram1
                        } else {
                            &self.polygon_ram0
                        };
                        let q = (oba as usize) & 0x7fff;
                        let owned = ram[q..].to_vec();
                        self.parse_object(&owned, 0, n, tpa, tha, center_sel, texture_rom)
                    } else {
                        let q = (oba as usize) & (polygon_rom.len() - 1);
                        self.parse_object(
                            &polygon_rom[q..],
                            0,
                            n,
                            tpa,
                            tha,
                            center_sel,
                            texture_rom,
                        )
                    }
                }
                0x02 | 0x12 => {
                    if p + 8 > buffer.len() {
                        break;
                    }
                    let tpa = buffer[p];
                    let tha = buffer[p + 1];
                    p = self.parse_direct(
                        buffer,
                        p + 2,
                        tpa,
                        tha,
                        ((opcode >> 29) & 3) as usize,
                        texture_rom,
                    );
                }
                0x03 | 0x13 => {
                    if p + 6 > buffer.len() {
                        break;
                    }
                    for i in 0..6 {
                        let w = buffer[p + i];
                        // This is geometrizer input (XXXXYYYY), before
                        // geo_window_data repacks it to the rasterizer's
                        // 00XXXYYY stream.
                        let sx = ((w >> 16) & 0xfff) as i16;
                        let sy = (w & 0xfff) as i16;
                        let cv = |x: i16| if x & 0x800 != 0 { x | !0xfff } else { x };
                        if i == 0 {
                            self.viewport[0] = cv(sx);
                            self.viewport[1] = cv(sy)
                        } else if i == 1 {
                            self.viewport[2] = cv(sx);
                            self.viewport[3] = cv(sy)
                        } else {
                            self.centers[i - 2] = [cv(sx), cv(sy)]
                        }
                    }
                    p += 6;
                    self.cur_window = self.cur_window.wrapping_add(1);
                }
                0x04 => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    let mut a = buffer[p];
                    let n = buffer[p + 1] as usize;
                    p += 2;
                    if p + n > buffer.len() {
                        break;
                    }
                    for &d in &buffer[p..p + n] {
                        if a & 0x800000 != 0 {
                            self.texture_ram[(a as usize) & 0xffff] = d as u16
                        } else {
                            self.log_ram[(a as usize) & 0x7fff] = d as u8
                        }
                        a = a.wrapping_add(1)
                    }
                    p += n;
                }
                0x05 | 0x15 => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    let a = buffer[p];
                    let n = buffer[p + 1] as usize;
                    p += 2;
                    if p + n > buffer.len() {
                        break;
                    }
                    let ram = if a & 0x01000000 != 0 {
                        &mut self.polygon_ram1
                    } else {
                        &mut self.polygon_ram0
                    };
                    let q = (a as usize) & 0x7fff;
                    let m = n.min(ram.len() - q);
                    ram[q..q + m].copy_from_slice(&buffer[p..p + m]);
                    p += n;
                }
                0x06 => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    // The command names a starting slot and a count; the slot
                    // wraps at 32, as the register file does.
                    let first = (buffer[p] >> 2) as usize;
                    let n = buffer[p + 1] as usize;
                    p += 2;
                    for i in first..first + n {
                        if p + 2 > buffer.len() {
                            break;
                        }
                        let v = buffer[p];
                        self.texparam[i & 31] = TextureParameter {
                            diffuse: (v & 255) as f32,
                            ambient: ((v >> 8) & 255) as f32,
                            specular_scale: ((v >> 16) & 255) as f32,
                            specular_control: (v >> 24) as u8,
                        };
                        self.coef[i & 31] = f32::from_bits(buffer[p + 1]);
                        p += 2;
                    }
                }
                0x07 | 0x17 => {
                    if p >= buffer.len() {
                        break;
                    }
                    self.mode = buffer[p];
                    p += 1;
                }
                0x08 | 0x18 | 0x16 => {
                    if p >= buffer.len() {
                        break;
                    }
                    if cmd == 0x16 {
                        self.lod = f32::from_bits(buffer[p])
                    } else if cmd == 0x08 || cmd == 0x18 {
                        self.z_adjust = buffer[p] & !0xff;
                    }
                    p += 1;
                }
                0x09 | 0x19 => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    self.focus = [f32::from_bits(buffer[p]), f32::from_bits(buffer[p + 1])];
                    p += 2;
                }
                0x0a | 0x1a => {
                    if p + 3 > buffer.len() {
                        break;
                    }
                    self.light = Vertex3 {
                        x: f32::from_bits(buffer[p]),
                        y: f32::from_bits(buffer[p + 1]),
                        z: f32::from_bits(buffer[p + 2]),
                        u: 0.0,
                        v: 0.0,
                    };
                    p += 3;
                }
                0x0b | 0x1b => {
                    if p + 12 > buffer.len() {
                        break;
                    }
                    for i in 0..12 {
                        self.matrix[i] = f32::from_bits(buffer[p + i])
                    }
                    log::trace!(target: "geo", "matrix {:?}", self.matrix);
                    p += 12;
                }
                0x0c | 0x1c => {
                    if p + 3 > buffer.len() {
                        break;
                    }
                    for i in 0..3 {
                        self.matrix[9 + i] = f32::from_bits(buffer[p + i])
                    }
                    p += 3;
                }
                // Log data write. Same destination as 0x04 above, but the
                // geometrizer unpacks each word into four separate byte writes
                // (`geo_log_data`), so the address advances four times per word.
                0x14 => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    let mut a = buffer[p];
                    let n = buffer[p + 1] as usize;
                    p += 2;
                    if p + n > buffer.len() {
                        break;
                    }
                    for &d in &buffer[p..p + n] {
                        for shift in [0, 8, 16, 24] {
                            let byte = (d >> shift) as u8;
                            if a & 0x800000 != 0 {
                                self.texture_ram[(a as usize) & 0xffff] = byte as u16
                            } else {
                                self.log_ram[(a as usize) & 0x7fff] = byte
                            }
                            a = a.wrapping_add(1)
                        }
                    }
                    p += n;
                }
                // Geometrizer self-test: 32 FIFO pattern words, then a block
                // count and three words per block describing a polygon ROM
                // checksum. The results only drive test-mode LEDs, but the
                // words still have to be consumed or the rest of the list
                // would be parsed at the wrong offset.
                0x0e => {
                    if p + 33 > buffer.len() {
                        break;
                    }
                    p += 32;
                    let blocks = buffer[p] as usize;
                    p += 1;
                    let Some(end) = blocks.checked_mul(3).and_then(|n| p.checked_add(n)) else {
                        break;
                    };
                    if end > buffer.len() {
                        break;
                    }
                    p = end;
                }
                // Pushes Geometry DSP RAM to the rasterizer. We have no DSP RAM
                // to push, and no game is known to use it; consume the
                // address and count so parsing stays aligned.
                0x0d => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    p += 2;
                }
                // Dummy read cycle.
                0x10 => {
                    if p >= buffer.len() {
                        break;
                    }
                    p += 1;
                }
                // Debug code upload: flags, count, then three words per opcode.
                0x1d => {
                    if p + 2 > buffer.len() {
                        break;
                    }
                    let n = buffer[p + 1] as usize;
                    p += 2;
                    let Some(end) = n.checked_mul(3).and_then(|n| p.checked_add(n)) else {
                        break;
                    };
                    if end > buffer.len() {
                        break;
                    }
                    p = end;
                }
                // Jump to uploaded debug code.
                0x1e => {
                    if p >= buffer.len() {
                        break;
                    }
                    p += 1;
                }
                0x0f | 0x1f => break,
                0x00 => {}
                // `cmd` is masked to 5 bits and every value is handled above,
                // so this is unreachable. Bailing out is still the right thing
                // if that ever stops being true: a command of unknown length
                // means every following word is at an unknown offset.
                _ => break,
            }
            if self.polygons.len() >= 32768 {
                break;
            }
        }
    }

    #[inline]
    fn check_culling(&self, attr: u32, rear_face: bool, min_z: f32, max_z: f32) -> bool {
        if rear_face && (attr >> 17) & 1 == 0 {
            return true;
        }
        if (attr >> 8) & 3 == 0 {
            return true;
        }
        if self.master_z_clip != 0xff && (1.0 / min_z) as i32 > self.master_z_clip as i32 {
            return true;
        }
        max_z < 0.0
    }

    pub fn set_master_z_clip(&mut self, value: u32) {
        self.master_z_clip = value as u8;
    }

    pub fn master_z_clip(&self) -> u32 {
        self.master_z_clip as u32
    }
}

#[derive(Clone, Debug, Default)]
pub struct DisplayListStats {
    pub commands: [u32; 32],
    pub object_modes: [u32; 4],
    pub object_words: u64,
    pub jumps: u32,
    pub ended: bool,
    pub malformed: bool,
}

fn word(buf: &[u32], p: usize) -> Option<u32> {
    buf.get(p).copied()
}

/// Walks one display list and validates all command lengths. This is also the
/// command framing used by the renderer; keeping it independent makes corrupt
/// TGP output distinguishable from rasterization bugs.
pub fn inspect_display_list(sys: &Model2System) -> DisplayListStats {
    let buf = &sys.buffer_ram;
    let mut out = DisplayListStats::default();
    let mut p = ((sys.geo_read_start_address & 0x1ffff) >> 2) as usize;
    let mut mode = 0u32;

    for _ in 0..0x8000 {
        let Some(opcode) = word(buf, p) else {
            out.malformed = true;
            break;
        };
        p += 1;
        if opcode & 0x8000_0000 != 0 {
            p = ((opcode & 0x1ffff) >> 2) as usize;
            out.jumps += 1;
            continue;
        }
        let cmd = ((opcode >> 23) & 0x1f) as usize;
        out.commands[cmd] += 1;

        let need = |p: &mut usize, n: usize| -> bool {
            if p.checked_add(n).is_some_and(|e| e <= buf.len()) {
                *p += n;
                true
            } else {
                false
            }
        };

        let ok = match cmd {
            0x00 => true,
            0x01 | 0x11 => {
                if p + 4 <= buf.len() {
                    let count = buf[p + 3];
                    out.object_modes[(mode & 3) as usize] += 1;
                    out.object_words += if count == 0 { 0x100000 } else { count } as u64;
                    p += 4;
                    true
                } else {
                    false
                }
            }
            0x02 | 0x12 => {
                if !need(&mut p, 8) {
                    false
                } else {
                    loop {
                        let Some(attr) = word(buf, p) else {
                            break false;
                        };
                        p += 1;
                        if attr & 3 == 0 {
                            break true;
                        }
                        let n = 5 + if attr & 1 != 0 { 3 } else { 0 };
                        if !need(&mut p, n) {
                            break false;
                        }
                    }
                }
            }
            0x03 | 0x13 => need(&mut p, 6),
            0x04 => {
                if p + 2 > buf.len() {
                    false
                } else {
                    let count = buf[p + 1] as usize;
                    need(&mut p, 2 + count)
                }
            }
            0x05 | 0x15 => {
                if p + 2 > buf.len() {
                    false
                } else {
                    let count = buf[p + 1] as usize;
                    need(&mut p, 2 + count)
                }
            }
            0x06 => {
                if p + 2 > buf.len() {
                    false
                } else {
                    let count = buf[p + 1] as usize;
                    need(&mut p, 2 + count * 2)
                }
            }
            0x07 | 0x17 => {
                if let Some(v) = word(buf, p) {
                    mode = v;
                    p += 1;
                    true
                } else {
                    false
                }
            }
            0x08 | 0x18 | 0x16 | 0x10 | 0x1e => need(&mut p, 1),
            0x09 | 0x19 => need(&mut p, 2),
            0x0a | 0x1a | 0x0c | 0x1c => need(&mut p, 3),
            0x0b | 0x1b => need(&mut p, 12),
            0x0d => need(&mut p, 2),
            0x0e => {
                if !need(&mut p, 32) || p >= buf.len() {
                    false
                } else {
                    let blocks = buf[p] as usize;
                    need(&mut p, 1 + blocks * 3)
                }
            }
            0x0f | 0x1f => {
                out.ended = true;
                break;
            }
            0x14 => {
                if p + 2 > buf.len() {
                    false
                } else {
                    let count = buf[p + 1] as usize;
                    need(&mut p, 2 + count)
                }
            }
            0x1d => {
                if p + 2 > buf.len() {
                    false
                } else {
                    let count = buf[p + 1] as usize;
                    need(&mut p, 2 + count * 3)
                }
            }
            _ => false,
        };
        if !ok {
            out.malformed = true;
            break;
        }
    }
    out
}

fn u16_at(mem: &[u32], idx: usize) -> u16 {
    mem.get(idx >> 1)
        .map(|w| (w >> ((idx & 1) * 16)) as u16)
        .unwrap_or(0)
}

fn material_color(sys: &Model2System, color_base: u16, luma: u8) -> u32 {
    let pal = u16_at(&sys.palette_ram, 0x1000 + color_base as usize);
    let lum = (luma >> 2) as usize;
    if !sys.colorxlat_written {
        return crate::tilemap::pen_color(sys, 0x1000 + color_base);
    }
    let r = u16_at(&sys.colorxlat_ram, (((pal & 0x1f) as usize) << 8) + lum) as u32 & 255;
    let g = u16_at(
        &sys.colorxlat_ram,
        0x4000 / 2 + (((pal >> 5 & 0x1f) as usize) << 8) + lum,
    ) as u32
        & 255;
    let b = u16_at(
        &sys.colorxlat_ram,
        0x8000 / 2 + (((pal >> 10 & 0x1f) as usize) << 8) + lum,
    ) as u32
        & 255;
    let m = |v: u32| crate::tilemap::monitor(sys, v);
    0xff00_0000 | (m(r) << 16) | (m(g) << 8) | m(b)
}

fn edge(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    (p.0 - a.0) * (b.1 - a.1) - (p.1 - a.1) * (b.0 - a.0)
}

fn float_to_zval(v: f32, z_adjust: u32) -> u16 {
    let bits = v.to_bits() as i32;
    let mut exp = ((bits >> 23) & 255) - (((z_adjust as i32) >> 23) & 255);
    let mut mant = (bits as u32) & 0x7fffff;
    mant += 0x400;
    if mant > 0x7fffff {
        exp += 1;
        mant = (mant & 0x7fffff) >> 1
    }
    mant >>= 11;
    if bits < 0 || exp < -12 {
        0
    } else if exp < 0 {
        ((mant | 0x1000) >> (-exp)) as u16
    } else if exp < 15 {
        (((exp + 1) << 12) as u32 | mant) as u16
    } else {
        0xffff
    }
}

fn clip_plane(input: &[Vertex3], normal: Vertex3) -> Vec<Vertex3> {
    let mut out = Vec::with_capacity(input.len() + 1);
    if input.is_empty() {
        return out;
    }
    for i in 0..input.len() {
        let a = input[i];
        let b = input[(i + 1) % input.len()];
        let da = a.x * normal.x + a.y * normal.y + a.z * normal.z;
        let db = b.x * normal.x + b.y * normal.y + b.z * normal.z;
        let ai = da >= 0.0;
        let bi = db >= 0.0;
        if ai {
            out.push(a)
        }
        if ai != bi && da.is_finite() && db.is_finite() {
            let t = -da / (db - da);
            out.push(Vertex3 {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
                z: a.z + (b.z - a.z) * t,
                u: a.u + (b.u - a.u) * t,
                v: a.v + (b.v - a.v) * t,
            })
        }
    }
    out
}

/// Builds the frame's GPU triangle stream in the exact Model 2 priority
/// order. The four hardware viewport planes are clipped here; projection,
/// coverage and sampling belong to the WGPU pipeline.
///
/// With `out_w` above the board's 496, the render space widens to that many
/// native pixels and every polygon's horizontal viewport/frustum extent
/// widens around its center by the same factor -- more of the scene is
/// revealed on the sides, instead of the 4:3 image
/// being stretched. Projection itself is untouched, so objects keep their
/// shape; the extra pixels show geometry the 4:3 frustum used to clip.
pub fn gpu_vertices(sys: &Model2System, out_w: f32) -> Vec<GpuVertex> {
    let expand = out_w / 496.0;
    let shift = (out_w - 496.0) / 2.0;
    let mut polys: Vec<_> = sys.geometry.polygons.iter().collect();
    polys.sort_by(|a, b| {
        b.window
            .cmp(&a.window)
            .then_with(|| a.z_key.cmp(&b.z_key))
            .then_with(|| b.sequence.cmp(&a.sequence))
    });
    let mut out = Vec::with_capacity(polys.len() * 6);
    for (priority, poly) in polys.into_iter().enumerate() {
        if poly.v.len() < 3 || ((poly.texheader[0] >> 13) & 3) == 1 {
            continue;
        }
        let cx = poly.center[0] as f32;
        let d_left = (cx - poly.viewport[0] as f32) * expand;
        let d_right = (poly.viewport[2] as f32 - cx) * expand;
        let mut clipped = poly.v.clone();
        for plane in [
            Vertex3 {
                x: 1.0,
                z: d_left,
                ..Default::default()
            },
            Vertex3 {
                x: -1.0,
                z: d_right,
                ..Default::default()
            },
            Vertex3 {
                y: -1.0,
                z: (poly.viewport[3] - poly.center[1]) as f32,
                ..Default::default()
            },
            Vertex3 {
                y: 1.0,
                z: (poly.center[1] - poly.viewport[1]) as f32,
                ..Default::default()
            },
        ] {
            clipped = clip_plane(&clipped, plane);
            if clipped.len() < 3 {
                break;
            }
        }
        if clipped.len() < 3 {
            continue;
        }
        let color = material_color(sys, poly.color_base, poly.luma);
        let flags = poly.textured as u32
            | (((poly.texheader[0] & 0x8000) != 0) as u32) << 1
            | (((poly.texheader[0] & 0x2000) != 0) as u32) << 2;
        let s_cx = sys.crtc_xoffset as f32 + poly.center[0] as f32;
        let make = |v: Vertex3| {
            let z = v.z + f32::MIN_POSITIVE;
            GpuVertex {
                screen_z: [
                    s_cx + shift + v.x / z,
                    (384 - poly.center[1] as i32 + sys.crtc_yoffset as i32) as f32 - v.y / z,
                    v.z,
                    1.0 / z,
                ],
                uv_luma_lod: [v.u, v.v, poly.luma as f32, poly.texlod as f32],
                material: [
                    color,
                    poly.texheader[0] as u32,
                    poly.texheader[1] as u32,
                    priority as u32,
                ],
                texture: [
                    poly.texheader[2] as u32,
                    poly.texheader[3] as u32,
                    flags,
                    poly.texlod as u32,
                ],
                viewport: [
                    s_cx + shift - d_left,
                    s_cx + shift + d_right,
                    (384 - poly.viewport[3] as i32 + sys.crtc_yoffset as i32) as f32,
                    (384 - poly.viewport[1] as i32 + sys.crtc_yoffset as i32) as f32,
                ],
            }
        };
        for i in 1..clipped.len() - 1 {
            out.extend([make(clipped[0]), make(clipped[i]), make(clipped[i + 1])]);
        }
    }
    out
}

/// Per-polygon colour table used by the GPU sampler after texture luma has
/// been resolved. Entry `priority * 64 + luma` is already translated through
/// the game's DAC curve and cabinet monitor model.
pub fn gpu_color_lut(sys: &Model2System) -> Vec<u32> {
    let mut polys: Vec<_> = sys.geometry.polygons.iter().collect();
    polys.sort_by(|a, b| {
        b.window
            .cmp(&a.window)
            .then_with(|| a.z_key.cmp(&b.z_key))
            .then_with(|| b.sequence.cmp(&a.sequence))
    });
    let mut out = Vec::with_capacity(polys.len() * 64);
    for poly in polys {
        for luma in 0..64u8 {
            out.push(material_color(sys, poly.color_base, luma << 2));
        }
    }
    out
}

pub fn gpu_raster_triangles(sys: &Model2System) -> Vec<GpuRasterTriangle> {
    gpu_raster_triangles_ws(sys, 496.0)
}

pub fn gpu_raster_triangles_ws(sys: &Model2System, out_w: f32) -> Vec<GpuRasterTriangle> {
    gpu_vertices(sys, out_w)
        .chunks_exact(3)
        .map(|t| {
            // Keep u and v raw rather than pre-multiplying by 1/z. The reference
            // evaluates `wa * u * ra`, i.e. `(wa * u) * ra`; folding `u * ra` here
            // instead reassociates the product, and float multiplication is not
            // associative -- it moved sampled texels by a luma step.
            let cv = |v: &GpuVertex| {
                [
                    v.screen_z[0],
                    v.screen_z[1],
                    v.uv_luma_lod[0],
                    v.uv_luma_lod[1],
                ]
            };
            let a = cv(&t[0]);
            let b = cv(&t[1]);
            let c = cv(&t[2]);
            let area = edge((a[0], a[1]), (b[0], b[1]), (c[0], c[1]));
            GpuRasterTriangle {
                a,
                b,
                c,
                reciprocal_area: [t[0].screen_z[3], t[1].screen_z[3], t[2].screen_z[3], area],
                material: t[0].material,
                texture: [
                    t[0].texture[0],
                    t[0].uv_luma_lod[2] as u32,
                    t[0].texture[2],
                    t[0].texture[3],
                ],
                viewport: [
                    t[0].viewport[0] as i32,
                    t[0].viewport[1] as i32,
                    t[0].viewport[2] as i32,
                    t[0].viewport[3] as i32,
                ],
            }
        })
        .filter(|t| t.reciprocal_area[3].abs() >= 0.0001)
        .collect()
}

/// 16x16 screen-tile index for the exact compute rasterizer. Triangle order
/// inside every bin remains front-to-back, so binning changes no pixels.
pub fn gpu_triangle_bins(tris: &[GpuRasterTriangle], width: usize) -> (Vec<[u32; 2]>, Vec<u32>) {
    let bw = width.div_ceil(16);
    const BH: usize = 24;
    let (bw, xclamp) = (bw, (width - 1) as f32);
    let mut bins = vec![Vec::<u32>::new(); bw * BH];
    for (i, t) in tris.iter().enumerate() {
        let xmin = t.a[0]
            .min(t.b[0])
            .min(t.c[0])
            .floor()
            .max(t.viewport[0] as f32)
            .clamp(0.0, xclamp) as usize
            / 16;
        let xmax = t.a[0]
            .max(t.b[0])
            .max(t.c[0])
            .ceil()
            .min(t.viewport[1] as f32)
            .clamp(0.0, xclamp) as usize
            / 16;
        let ymin = t.a[1]
            .min(t.b[1])
            .min(t.c[1])
            .floor()
            .max(t.viewport[2] as f32)
            .clamp(0.0, 383.0) as usize
            / 16;
        let ymax = t.a[1]
            .max(t.b[1])
            .max(t.c[1])
            .ceil()
            .min(t.viewport[3] as f32)
            .clamp(0.0, 383.0) as usize
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

/// First renderer milestone: the real geometry, material base colour, luma,
/// viewport and front-to-back fill rule, without texture sampling yet.
pub fn rasterize_solids(sys: &Model2System, out: &mut [u32]) {
    const W: usize = crate::tilemap::SCREEN_W;
    const H: usize = crate::tilemap::SCREEN_H;
    // A byte map matches the hardware fill buffer and is substantially
    // cheaper to index than Rust's bit-packed `Vec<bool>` in the inner loop.
    let mut filled = vec![0u8; W * H];
    let mut polys: Vec<_> = sys.geometry.polygons.iter().collect();
    polys.sort_by(|a, b| {
        b.window
            .cmp(&a.window)
            .then_with(|| a.z_key.cmp(&b.z_key))
            .then_with(|| b.sequence.cmp(&a.sequence))
    });
    for poly in polys {
        if poly.v.len() < 3 {
            continue;
        }
        // Renderer 1 is an untextured translucent primitive. Hardware
        // deliberately emits no pixels for it.
        if ((poly.texheader[0] >> 13) & 3) == 1 {
            continue;
        }
        let left = (poly.center[0] - poly.viewport[0]) as f32;
        let right = (poly.viewport[2] - poly.center[0]) as f32;
        let top = (poly.viewport[3] - poly.center[1]) as f32;
        let bottom = (poly.center[1] - poly.viewport[1]) as f32;
        let mut clipped = poly.v.clone();
        for plane in [
            Vertex3 {
                x: 1.0,
                y: 0.0,
                z: left,
                u: 0.0,
                v: 0.0,
            },
            Vertex3 {
                x: -1.0,
                y: 0.0,
                z: right,
                u: 0.0,
                v: 0.0,
            },
            Vertex3 {
                x: 0.0,
                y: -1.0,
                z: top,
                u: 0.0,
                v: 0.0,
            },
            Vertex3 {
                x: 0.0,
                y: 1.0,
                z: bottom,
                u: 0.0,
                v: 0.0,
            },
        ] {
            clipped = clip_plane(&clipped, plane);
            if clipped.len() < 3 {
                break;
            }
        }
        if clipped.len() < 3 {
            continue;
        }
        // model2_3d_project divides by `pz + std::numeric_limits<float>::min()`
        // and never rejects a vertex: one sitting on the camera plane projects
        // to a huge but finite coordinate and still rasterises, clipped to the
        // viewport. Dropping the polygon on near-zero z instead deleted every
        // one straddling the origin, which is most of them during a camera cut.
        let project = |v: Vertex3| -> (f32, f32) {
            let z = v.z + f32::MIN_POSITIVE;
            (
                sys.crtc_xoffset as f32 + poly.center[0] as f32 + v.x / z,
                (384 - poly.center[1] as i32 + sys.crtc_yoffset as i32) as f32 - v.y / z,
            )
        };
        let pv: Vec<_> = clipped.iter().copied().map(project).collect();
        let xmin = (poly.viewport[0] as i32 + sys.crtc_xoffset as i32).clamp(0, W as i32 - 1);
        let xmax = (poly.viewport[2] as i32 + sys.crtc_xoffset as i32).clamp(0, W as i32 - 1);
        let ymin = (384 - poly.viewport[3] as i32 + sys.crtc_yoffset as i32).clamp(0, H as i32 - 1);
        let ymax = (384 - poly.viewport[1] as i32 + sys.crtc_yoffset as i32).clamp(0, H as i32 - 1);
        if xmin > xmax || ymin > ymax {
            continue;
        }
        for i in 1..pv.len() - 1 {
            let a = pv[0];
            let b = pv[i];
            let c = pv[i + 1];
            let area = edge(a, b, c);
            if area.abs() < 0.0001 {
                continue;
            }
            let bx0 = (a.0.min(b.0).min(c.0).floor() as i32).clamp(xmin, xmax);
            let bx1 = (a.0.max(b.0).max(c.0).ceil() as i32).clamp(xmin, xmax);
            let by0 = (a.1.min(b.1).min(c.1).floor() as i32).clamp(ymin, ymax);
            let by1 = (a.1.max(b.1).max(c.1).ceil() as i32).clamp(ymin, ymax);
            for y in by0..=by1 {
                for x in bx0..=bx1 {
                    let q = (x as f32 + 0.5, y as f32 + 0.5);
                    let e0 = edge(a, b, q);
                    let e1 = edge(b, c, q);
                    let e2 = edge(c, a, q);
                    if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0)
                        || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0)
                    {
                        let k = y as usize * W + x as usize;
                        if filled[k] == 0 {
                            if poly.texheader[0] & 0x8000 != 0 && ((x ^ y) & 1) == 0 {
                                continue;
                            }
                            let mut color = material_color(sys, poly.color_base, poly.luma);
                            if poly.textured {
                                let wa = e1 / area;
                                let wb = e2 / area;
                                let wc = e0 / area;
                                let va = clipped[0];
                                let vb = clipped[i];
                                let vc = clipped[i + 1];
                                // Same reciprocal as the projection above: the reference
                                // precomputes pz = 1/(z + FLT_MIN) per vertex and
                                // interpolates pz, u*pz and v*pz linearly.
                                let rz = |z: f32| 1.0 / (z + f32::MIN_POSITIVE);
                                let (ra, rb, rc) = (rz(va.z), rz(vb.z), rz(vc.z));
                                let iz = wa * ra + wb * rb + wc * rc;
                                if !iz.is_finite() || iz == 0.0 {
                                    continue;
                                }
                                let u = (wa * va.u * ra + wb * vb.u * rb + wc * vc.u * rc) / iz;
                                let v = (wa * va.v * ra + wb * vb.v * rb + wc * vc.v * rc) / iz;
                                let Some(tex) =
                                    sample_texel(sys, poly.texheader, u, v, 1.0 / iz, poly.texlod)
                                else {
                                    continue;
                                };
                                let lb = ((poly.texheader[1] & 255) as usize) << 7;
                                let li = lb + (tex as usize >> 1);
                                let lv = sys.luma_ram.get(li).copied().unwrap_or(0);
                                let l = ((lv as u16 * poly.luma as u16) / 256).min(63) as u8;
                                color = material_color(sys, poly.color_base, l << 2)
                            }
                            out[k] = color;
                            filled[k] = 0xff
                        }
                    }
                }
            }
        }
    }
}

/// Model 2's filtered 4bpp texture sampler. The intermediate value packs
/// luma in bits 0-7 and (for translucent textures) coverage in bits 16-23,
///. Keeping coverage through both filtering stages
/// is important: testing the original 4-bit texel after interpolation makes
/// fences, foliage and car windows either disappear or turn opaque.
fn sample_texel(
    sys: &Model2System,
    h: [u16; 4],
    u: f32,
    v: f32,
    z: f32,
    texlod: i32,
) -> Option<u8> {
    let width = 32usize << ((h[0] & 7) as usize);
    let height = 32usize << (((h[0] >> 3) & 7) as usize);
    let max_level = (usize::BITS - 1 - width.min(height).leading_zeros()).saturating_sub(1) as i32;
    let mml = -texlod + fast_log2(z);
    let level = (mml >> 7).clamp(0, max_level);
    let translucent = h[0] & 0x2000 != 0;
    let fetch = |level: i32| -> u32 {
        let micro = level == -1;
        let (tw, th, bx, by, sheet, mut uf, mut vf) = if micro {
            let primary = h[2] & 0x1000 != 0;
            let sheet = if primary {
                &sys.texture_ram0
            } else {
                &sys.texture_ram1
            };
            let shift = 1i32 << ((h[0] >> 10) & 3);
            (
                128usize,
                128usize,
                (((h[2] >> 13) & 1) as usize) * 128,
                (((h[2] >> 14) & 3) as usize) * 128,
                sheet,
                (u * 32.0) as i32 * shift,
                (v * 32.0) as i32 * shift,
            )
        } else {
            let tw = width >> level;
            let th = height >> level;
            let bx = ((32 * ((h[2] & 0x3f) as usize)).wrapping_sub(2048) >> level) & 2047;
            let by = ((32 * (((h[2] >> 6) & 0x1f) as usize)).wrapping_sub(1024) >> level) & 1023;
            let primary = h[2] & 0x1000 != 0;
            let sheet = if primary ^ (level & 1 != 0) {
                &sys.texture_ram1
            } else {
                &sys.texture_ram0
            };
            (
                tw,
                th,
                bx,
                by,
                sheet,
                ((u * 32.0) as i32) >> level,
                ((v * 32.0) as i32) >> level,
            )
        };
        if h[0] & 0x100 != 0 && (uf & ((tw as i32) << 8)) != 0 {
            uf = !uf
        }
        if h[0] & 0x200 != 0 && (vf & ((th as i32) << 8)) != 0 {
            vf = !vf
        }
        uf -= 0x80;
        vf -= 0x80;
        let mut fu = (uf & 255) as u32;
        let mut fv = (vf & 255) as u32;
        let mut x0 = ((uf >> 8) & (tw as i32 - 1)) as usize;
        let mut x1 = (x0 + 1) & (tw - 1);
        let mut y0 = ((vf >> 8) & (th as i32 - 1)) as usize;
        let mut y1 = (y0 + 1) & (th - 1);
        let wrap_x = h[0] & 0x40 != 0 && h[0] & 0x100 == 0;
        let wrap_y = h[0] & 0x80 != 0 && h[0] & 0x200 == 0;
        if !wrap_x && x1 == 0 {
            if fu >= 0x80 {
                x0 = 0;
                x1 = 1;
                fu = 0
            } else {
                x1 = x0;
                x0 = x0.saturating_sub(1);
                fu = 0x100
            }
        }
        if !wrap_y && y1 == 0 {
            if fv >= 0x80 {
                y0 = 0;
                y1 = 1;
                fv = 0
            } else {
                y1 = y0;
                y0 = y0.saturating_sub(1);
                fv = 0x100
            }
        }
        let pack = |t: u8| -> u32 {
            let l = (t as u32) << 4;
            if translucent && t != 0xf {
                l | 0x0080_0000
            } else {
                l
            }
        };
        let mut a = pack(get_texel(bx, by, x0, y0, sheet));
        let mut b = pack(get_texel(bx, by, x1, y0, sheet));
        let mut c = pack(get_texel(bx, by, x0, y1, sheet));
        let mut d = pack(get_texel(bx, by, x1, y1, sheet));
        if translucent {
            if a == 0xf0 {
                a = b & 0xff
            }
            if b == 0xf0 {
                b = a & 0xff
            }
            if c == 0xf0 {
                c = d & 0xff
            }
            if d == 0xf0 {
                d = c & 0xff
            }
        }
        let lerp = |x: u32, y: u32, f: u32| -> u32 {
            x.wrapping_add(y.wrapping_sub(x).wrapping_mul(f) >> 8) & 0x00ff_00ff
        };
        let mut ab = lerp(a, b, fu);
        let mut cd = lerp(c, d, fu);
        if translucent {
            if ab == 0xf0 {
                ab = cd & 0xff
            }
            if cd == 0xf0 {
                cd = ab & 0xff
            }
        }
        lerp(ab, cd, fv)
    };
    let lerp = |x: u32, y: u32, f: u32| -> u32 {
        x.wrapping_add(y.wrapping_sub(x).wrapping_mul(f) >> 8) & 0x00ff_00ff
    };
    let mut t = fetch(level);
    if mml > 0 && level < max_level {
        t = lerp(t, fetch(level + 1), ((mml & 127) << 1) as u32);
    } else if h[0] & 0x1000 != 0 && mml < 0 {
        let min_lod = ((h[0] >> 10) & 3) as i32;
        let f = ((-mml) >> min_lod).min(127) as u32;
        t = lerp(t, fetch(-1), f);
    }
    if translucent && t < 0x0040_0000 {
        None
    } else {
        Some((t & 0xff) as u8)
    }
}

#[inline]
fn fast_log2(value: f32) -> i32 {
    const TABLE: [u8; 128] = [
        0, 2, 5, 8, 11, 14, 16, 19, 22, 25, 27, 30, 33, 35, 38, 40, 43, 46, 48, 51, 53, 56, 58, 61,
        63, 65, 68, 70, 73, 75, 77, 80, 82, 84, 87, 89, 91, 93, 96, 98, 100, 102, 104, 106, 109,
        111, 113, 115, 117, 119, 121, 123, 125, 127, 129, 132, 134, 136, 138, 140, 141, 143, 145,
        147, 149, 151, 153, 155, 157, 159, 161, 162, 164, 166, 168, 170, 172, 173, 175, 177, 179,
        181, 182, 184, 186, 188, 189, 191, 193, 194, 196, 198, 200, 201, 203, 205, 206, 208, 209,
        211, 213, 214, 216, 218, 219, 221, 222, 224, 225, 227, 229, 230, 232, 233, 235, 236, 238,
        239, 241, 242, 244, 245, 247, 248, 250, 251, 253, 254,
    ];
    if value < 0.0 {
        return 0;
    }
    let bits = value.to_bits() >> 16;
    (((bits >> 7) as i32 - 127) << 8) | TABLE[(bits & 127) as usize] as i32
}
fn get_texel(bx: usize, by: usize, x: usize, y: usize, sheet: &[u32]) -> u8 {
    let mut x2 = bx + x;
    let mut y2 = by + y;
    if x2 >= 1024 {
        x2 -= 1024;
        y2 ^= 1024
    }
    let off = (y2 / 2) * 512 + x2 / 2;
    let mut t = sheet.get(off >> 1).copied().unwrap_or(0);
    if off & 1 != 0 {
        t >>= 16
    }
    if y & 1 == 0 {
        t >>= 8
    }
    if x & 1 == 0 {
        t >>= 4
    }
    (t & 15) as u8
}
