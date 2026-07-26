//! The logo, decoded once and scaled down to the board's resolution.
//!
//! The artwork is far larger than a 496x384 screen, so it is box-filtered to
//! its final size at load and then blitted with no filtering at all: scaling
//! per frame would soften it, and the point is that it sits on a hard-edged
//! 1994 framebuffer.

/// The logo is drawn this wide, in native screen pixels.
const TARGET_WIDTH: usize = 176;

pub struct Logo {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
}

impl Logo {
    /// Reads `assets/logo.png`. A missing or unreadable file is not fatal --
    /// the attract screen simply has no logo on it.
    pub fn load() -> Option<Self> {
        let path = std::path::Path::new("assets").join("logo.png");
        let file = std::fs::File::open(&path)
            .map_err(|e| log::warn!(target: "attract", "no logo at {}: {e}", path.display()))
            .ok()?;
        let decoder = png::Decoder::new(std::io::BufReader::new(file));
        let mut reader = decoder
            .read_info()
            .map_err(|e| log::warn!(target: "attract", "logo: {e}"))
            .ok()?;
        let mut raw = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut raw)
            .map_err(|e| log::warn!(target: "attract", "logo: {e}"))
            .ok()?;

        let (w, h) = (info.width as usize, info.height as usize);
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            other => {
                log::warn!(target: "attract", "logo is {other:?}; want RGB or RGBA");
                return None;
            }
        };

        let source: Vec<[u16; 4]> = (0..w * h)
            .map(|i| {
                let p = i * channels;
                [
                    raw[p] as u16,
                    raw[p + 1] as u16,
                    raw[p + 2] as u16,
                    if channels == 4 {
                        raw[p + 3] as u16
                    } else {
                        255
                    },
                ]
            })
            .collect();

        Some(Self::downscale(&source, w, h))
    }

    /// Box-filters to `TARGET_WIDTH`, averaging in premultiplied alpha so the
    /// transparent border does not bleed its colour into the edges.
    fn downscale(source: &[[u16; 4]], w: usize, h: usize) -> Self {
        let width = TARGET_WIDTH.min(w);
        let height = (h * width / w).max(1);
        let mut pixels = vec![0u32; width * height];

        for y in 0..height {
            let y0 = y * h / height;
            let y1 = ((y + 1) * h / height).max(y0 + 1);
            for x in 0..width {
                let x0 = x * w / width;
                let x1 = ((x + 1) * w / width).max(x0 + 1);

                let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let p = source[sy * w + sx];
                        let alpha = p[3] as u32;
                        r += p[0] as u32 * alpha;
                        g += p[1] as u32 * alpha;
                        b += p[2] as u32 * alpha;
                        a += alpha;
                        n += 1;
                    }
                }
                let alpha = a / n.max(1);
                // Undo the premultiply. A fully transparent block has no
                // colour to recover, so it stays black.
                let (r, g, b) = match (r.checked_div(a), g.checked_div(a), b.checked_div(a)) {
                    (Some(r), Some(g), Some(b)) => (r, g, b),
                    _ => (0, 0, 0),
                };
                pixels[y * width + x] = (alpha << 24) | (r << 16) | (g << 8) | b;
            }
        }

        Self {
            pixels,
            width,
            height,
        }
    }

    /// Blits over an ARGB layer at `(x, y)`.
    ///
    /// The tile layers treat zero as "nothing here", so a pixel is written only
    /// where the logo is substantially opaque; the soft edge is thresholded
    /// rather than blended, which is what the hardware would have done with a
    /// sprite.
    pub fn draw(&self, out: &mut [u32], stride: usize, height: usize, x: i32, y: i32) {
        const OPAQUE_ENOUGH: u32 = 96;
        for row in 0..self.height {
            let py = y + row as i32;
            if py < 0 || py >= height as i32 {
                continue;
            }
            for column in 0..self.width {
                let px = x + column as i32;
                if px < 0 || px >= stride as i32 {
                    continue;
                }
                let pixel = self.pixels[row * self.width + column];
                if pixel >> 24 < OPAQUE_ENOUGH {
                    continue;
                }
                out[py as usize * stride + px as usize] = pixel | 0xFF00_0000;
            }
        }
    }
}
