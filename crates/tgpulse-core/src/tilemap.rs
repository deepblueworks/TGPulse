//! HLE of the Sega System 24 tilemap chip, which draws Model 2's 2D/text layer.
//!
//! Layout:
//!   tile_ram 0x8000 u16. Four 64x64 tilemaps of 8x8 tiles at u16 offsets
//!             0x0000 (0s), 0x1000 (0w), 0x2000 (1s), 0x3000 (1w).
//!             0x5000+n = h-scroll, 0x5004+n = v-scroll.
//!   char_ram 0x40000 u16 of 4bpp 8x8 character bitmaps, 32 bytes each.
//!
//! Tile entry: bits 0-13 character code, bits 7-14 colour, bit 15 category
//! (category 1 draws in front of the 3D layer, category 0 behind).

use crate::system::Model2System;

/// The data a segas24 tile layer needs, abstracted over the two boards. Model 2
/// packs these RAMs into 32-bit words (i960 bus); Model 1 stores raw bytes (V60
/// bus), so the accessors are logical (indexed in u16/u32 units), not raw slices.
pub trait TileSource {
    fn tile_u16(&self, idx: usize) -> u16;
    fn char_word(&self, idx: usize) -> u32;
    fn palette_u16(&self, idx: usize) -> u16;
    fn colorxlat_u16(&self, idx: usize) -> u16;
    fn colorxlat_written(&self) -> bool;
    fn monitor_gamma(&self, v: u32) -> u32;
}

impl TileSource for Model2System {
    fn tile_u16(&self, idx: usize) -> u16 {
        u16_at(&self.tile_ram, idx)
    }
    fn char_word(&self, idx: usize) -> u32 {
        self.char_ram.get(idx).copied().unwrap_or(0)
    }
    fn palette_u16(&self, idx: usize) -> u16 {
        u16_at(&self.palette_ram, idx)
    }
    fn colorxlat_u16(&self, idx: usize) -> u16 {
        u16_at(&self.colorxlat_ram, idx)
    }
    fn colorxlat_written(&self) -> bool {
        self.colorxlat_written
    }
    fn monitor_gamma(&self, v: u32) -> u32 {
        self.monitor[(v & 0xff) as usize] as u32
    }
}

pub const SCREEN_W: usize = 496;
pub const SCREEN_H: usize = 384;

/// Tilemaps are 64x64 tiles of 8x8 pixels and wrap at 512.
const MAP_MASK: u32 = 511;

/// The tile index is 14 bits wide.
const TILE_MASK: u16 = 0x3fff;

/// Reads a 16-bit device word out of a 32-bit-word backing store. The reference u16
/// handlers on the i960's little-endian bus put device word 2i in the low half
/// of CPU word i.
#[inline]
fn u16_at(mem: &[u32], idx: usize) -> u16 {
    match mem.get(idx >> 1) {
        Some(w) => (*w >> ((idx & 1) * 16)) as u16,
        None => 0,
    }
}

/// Resolves one of the 4096 tile pens to a packed 0xAARRGGBB colour.
///
/// The palette is xBBBBBGGGGGRRRRR; each 5-bit component is expanded through
/// the colour-translation RAM into 8 bits.
pub fn pen_color<S: TileSource>(sys: &S, pen: u16) -> u32 {
    let palcolor = sys.palette_u16(pen as usize);
    let r5 = (palcolor & 0x1f) as usize;
    let g5 = ((palcolor >> 5) & 0x1f) as usize;
    let b5 = ((palcolor >> 10) & 0x1f) as usize;

    let (r, g, b) = if sys.colorxlat_written() {
        (
            sys.colorxlat_u16(0x0080 / 2 + r5 * 0x100) as u32 & 0xFF,
            sys.colorxlat_u16(0x4080 / 2 + g5 * 0x100) as u32 & 0xFF,
            sys.colorxlat_u16(0x8080 / 2 + b5 * 0x100) as u32 & 0xFF,
        )
    } else {
        // Before the game programs the translation RAM, expand 5 bits to 8 so
        // the screen is legible instead of black.
        let e = |v: usize| ((v << 3) | (v >> 2)) as u32;
        (e(r5), e(g5), e(b5))
    };

    let (r, g, b) = (
        sys.monitor_gamma(r),
        sys.monitor_gamma(g),
        sys.monitor_gamma(b),
    );
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

/// Model 2 cabinet monitor gamma, used by the 3D solid rasterizer.
#[inline]
pub(crate) fn monitor(sys: &Model2System, v: u32) -> u32 {
    sys.monitor[(v & 0xff) as usize] as u32
}

/// Fetches the 4bpp pixel at (x, y) of character `code`.
///
/// The character layout is `{8,8, 4bpp, planes {0,1,2,3}, xoffs STEP8(0,4),
/// yoffs STEP8(0,32)}` with an LE bit-address xormask of 8, which works out to
/// nibbles in the byte order b1,b1,b0,b0,b3,b3,b2,b2 across the row.
#[inline]
fn char_pixel<S: TileSource>(sys: &S, code: u16, x: u32, y: u32) -> u8 {
    let word = sys.char_word((code as usize) * 8 + y as usize);
    let b = word.to_le_bytes();
    match x {
        0 => b[1] >> 4,
        1 => b[1] & 0xF,
        2 => b[0] >> 4,
        3 => b[0] & 0xF,
        4 => b[3] >> 4,
        5 => b[3] & 0xF,
        6 => b[2] >> 4,
        _ => b[2] & 0xF,
    }
}

/// Draws one 64x64 tilemap layer into `out`.
///
/// `category` selects which tiles to draw (bit 15 of the tile entry); `opaque`
/// draws pen 0 instead of treating it as transparent.
///
/// Layers pair up as (0s, 0w) and (1s, 1w): a per-scanline mask bitmap decides,
/// for each group of 8 pixels, which half of the pair is visible. The `s` layer
/// shows where the mask bit is 0 and the `w` layer where it is 1, so an all-zero
/// mask means the `w` layers draw nothing at all.
fn draw_layer<S: TileSource>(
    sys: &S,
    out: &mut [u32],
    palette: &[u32; 4096],
    layer: usize,
    category: u16,
    opaque: bool,
) {
    let base = layer * 0x1000;
    let win = layer & 1 != 0;
    let mask_base = if layer & 2 != 0 { 0x6800 } else { 0x6000 };

    let hscr_raw = sys.tile_u16(0x5000 + layer);
    let vscr_raw = sys.tile_u16(0x5004 + layer);

    // Layer disable
    if vscr_raw & 0x8000 != 0 {
        return;
    }

    // The reference: hscr = (-hscr) & 0x1ff; vscr = (+vscr) & 0x1ff. The scroll values
    // are the tilemap coordinate that lands at screen (0,0).
    let hscr = (hscr_raw.wrapping_neg() & 0x1ff) as u32;
    let vscr = (vscr_raw & 0x1ff) as u32;

    for sy in 0..SCREEN_H as u32 {
        let my = (sy + vscr) & MAP_MASK;
        let ty = my >> 3;
        let py = my & 7;

        // The mask is 4 words per scanline, each word covering 128 pixels as 16
        // groups of 8, MSB first.
        let mask_row = mask_base + (sy as usize) * 4;

        for sx in 0..SCREEN_W as u32 {
            let mword = sys.tile_u16(mask_row + (sx >> 7) as usize);
            let mbit = (mword >> (15 - ((sx >> 3) & 15))) & 1 != 0;
            if mbit != win {
                continue;
            }

            let mx = (sx + hscr) & MAP_MASK;
            let tx = mx >> 3;
            let px = mx & 7;

            let val = sys.tile_u16(base + (ty * 64 + tx) as usize);
            if (val >> 15) != category {
                continue;
            }

            let code = val & TILE_MASK;
            let nib = char_pixel(sys, code, px, py);
            if nib == 0 && !opaque {
                continue;
            }

            let color = (val >> 7) & 0xff;
            let pen = color * 16 + nib as u16;
            out[(sy as usize) * SCREEN_W + sx as usize] = palette[pen as usize];
        }
    }
}

/// Renders the full 2D layer into a 496x384 0xAARRGGBB framebuffer.
///
/// Follows the layer order in the reference. The 3D
/// layer would be composited between the two category passes.
/// Everything the compositor draws *behind* the 3D layer, split out of
/// `render` so the GPU rasterizer can start from an identical background and
/// the two paths can be compared pixel for pixel.
/// Draws one name-table plane straight into a row range, with no mask/window
/// selection -- the segas24 vertical-split mode replaces the mask split with a
/// screen split, so each region shows a whole plane.
#[allow(clippy::too_many_arguments)]
fn draw_plane_region<S: TileSource>(
    sys: &S,
    out: &mut [u32],
    palette: &[u32; 4096],
    plane: usize,
    hscr: u32,
    vscr: u32,
    y0: usize,
    y1: usize,
    opaque: bool,
) {
    let base = plane * 0x1000;
    for sy in y0..y1 {
        let my = (sy as u32 + vscr) & MAP_MASK;
        let (ty, py) = (my >> 3, my & 7);
        for sx in 0..SCREEN_W as u32 {
            let mx = (sx + hscr) & MAP_MASK;
            let (tx, px) = (mx >> 3, mx & 7);
            let val = sys.tile_u16(base + (ty * 64 + tx) as usize);
            if (val >> 15) != 0 {
                continue;
            }
            let nib = char_pixel(sys, val & TILE_MASK, px, py);
            if nib == 0 && !opaque {
                continue;
            }
            let pen = ((val >> 7) & 0xff) * 16 + nib as u16;
            out[sy * SCREEN_W + sx as usize] = palette[pen as usize];
        }
    }
}

/// segas24's special window/scroll mode. When the control word (the vscroll of
/// the even plane in a pair) has bits 0x6000 set, the plane pair is not
/// mask-split into `s`/`w` but screen-split: the reference, `ctrl &
/// 0x6000`. Daytona uses case 1 (a horizontal cut) for the sky -- the top of
/// the screen shows one plane, the bottom the other, which is how the blue sky
/// reaches the top instead of the panorama's wrapped ground band.
///
/// Returns false when the mode is not this case, so the caller falls back to
/// the ordinary mask-based draw.
fn draw_split_pair<S: TileSource>(
    sys: &S,
    out: &mut [u32],
    palette: &[u32; 4096],
    plane: usize,
    opaque: bool,
) -> bool {
    let hscr_raw = sys.tile_u16(0x5000 + plane);
    // For an even plane, 0x5004 + (plane & 2) == 0x5004 + plane, so the control
    // word is this plane's own vscroll register.
    let ctrl = sys.tile_u16(0x5004 + plane);
    if ctrl & 0x6000 == 0 || ctrl & 0x8000 != 0 {
        return false; // ordinary mode, or the pair is disabled
    }
    // Per-line hscroll (bit 15 of hscr) and cases 2/3 are not yet needed by any
    // exercised game; fall back rather than draw them wrong.
    if hscr_raw & 0x8000 != 0 || (ctrl & 0x6000) >> 13 != 1 {
        return false;
    }

    let vscr = (ctrl & 0x1ff) as u32;
    let hscr = (hscr_raw.wrapping_neg() & 0x1ff) as u32;
    let split = (ctrl.wrapping_neg() & 0x1ff) as usize;

    // The reference: `if(!((-vscr) & 0x200)) layer ^= 1;` then top = layer, bottom = layer^1.
    let mut top = plane;
    if ctrl.wrapping_neg() & 0x200 == 0 {
        top ^= 1;
    }
    let bottom = top ^ 1;

    let split = split.min(SCREEN_H);
    draw_plane_region(sys, out, palette, top, hscr, vscr, 0, split, opaque);
    draw_plane_region(
        sys, out, palette, bottom, hscr, vscr, split, SCREEN_H, opaque,
    );
    true
}

/// The tile layers drawn behind the 3D image. Shared by `render` and
/// `render_background` so the CPU compositor and the GPU renderer start from
/// exactly the same background.
fn draw_bg_layers<S: TileSource>(sys: &S, out: &mut [u32], palette: &[u32; 4096]) {
    // Opaque pair (planes 2/3). The even plane's control word may put it in the
    // vertical-split mode; otherwise draw both planes the ordinary way.
    if !draw_split_pair(sys, out, palette, 2, true) {
        draw_layer(sys, out, palette, 3, 0, true);
        draw_layer(sys, out, palette, 2, 0, true);
    }
    // Transparent pair (planes 0/1).
    if !draw_split_pair(sys, out, palette, 0, false) {
        draw_layer(sys, out, palette, 1, 0, false);
        draw_layer(sys, out, palette, 0, 0, false);
    }
}

pub fn render_background<S: TileSource>(sys: &S, out: &mut [u32]) {
    let palette: [u32; 4096] = std::array::from_fn(|i| pen_color(sys, i as u16));
    out.fill(palette[0]);
    draw_bg_layers(sys, out, &palette);
}

pub fn render(sys: &Model2System, out: &mut [u32]) {
    // Palette and translation RAM are stable for one frame. Resolve each pen
    // once instead of repeating three table lookups plus gamma per tile pixel.
    let palette: [u32; 4096] = std::array::from_fn(|i| pen_color(sys, i as u16));
    let backdrop = palette[0];
    out.fill(backdrop);

    draw_bg_layers(sys, out, &palette);

    crate::geometry::rasterize_solids(sys, out);

    // In front of the 3D layer.
    for layer in (0..=3).rev() {
        draw_layer(sys, out, &palette, layer, 1, false);
    }
}

/// Produces an alpha mask/colour image for the tile categories in front of
/// 3D. Used by the compute rasterizer to preserve the exact composition order.
pub fn render_foreground<S: TileSource>(sys: &S, out: &mut [u32]) {
    out.fill(0);
    let palette: [u32; 4096] = std::array::from_fn(|i| pen_color(sys, i as u16));
    for layer in (0..=3).rev() {
        draw_layer(sys, out, &palette, layer, 1, false);
    }
}
