//! The attract screen: what the emulator shows with no game loaded.
//!
//! It is drawn the way a Model 1 game would draw it. The scene is built as
//! world-space quads, transformed and projected on the CPU, painter-sorted, and
//! handed to the same compute rasterizer the board's own geometry goes through
//! -- so it is flat-shaded, unfiltered and z-sorted back to front, with all the
//! sorting artefacts that implies. Nothing here is a modern renderer pretending
//! to be an old one; it is the old one.
//!
//! The subject is the logo's island, in motion: sea, sand, a palm tree with a
//! red coconut, and gulls. On Christmas Day the palm is a fir and it snows.

mod font;
mod logo;
mod scene;

use std::time::Instant;

use tgpulse_core::model1_video::GpuQuad;
use tgpulse_core::tilemap::{SCREEN_H, SCREEN_W};

use logo::Logo;

/// The prompt's colours: warm yellow at the top of the letterform running to
/// orange at its foot, over a hard black outline. Sega's own prompts of the
/// period are shaded this way.
const PROMPT_STYLE: font::Style = font::Style {
    top: 0xFFFF_E24A,
    bottom: 0xFFF0_8A18,
    outline: 0xFF10_0804,
};

/// How long one blink of the prompt lasts, and how much of it the text is up
/// for. Arcade prompts hold longer than they hide.
const BLINK_PERIOD: f32 = 1.05;
const BLINK_DUTY: f32 = 0.62;

pub struct Attract {
    started: Instant,
    logo: Option<Logo>,
    scene: scene::Scene,
}

impl Attract {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            logo: Logo::load(),
            scene: scene::Scene::new(),
        }
    }

    fn elapsed(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    /// The 3D layer, as quads for the hardware rasterizer.
    pub fn quads(&self, render_width: u32) -> Vec<GpuQuad> {
        self.scene.build(self.elapsed(), render_width)
    }

    /// The sky, drawn into the background tile layer.
    pub fn background(&self, out: &mut [u32]) {
        self.scene.sky(self.elapsed(), out);
    }

    /// The logo and the prompt, drawn into the foreground tile layer, which the
    /// compositor lays over the 3D.
    pub fn foreground(&self, out: &mut [u32]) {
        out.fill(0);
        let t = self.elapsed();

        if let Some(logo) = &self.logo {
            logo.draw(out, SCREEN_W, SCREEN_H, 8, 6);
        }

        // The prompt blinks: it is there or it is not. Dimming it instead --
        // which is what this did -- reads as a colour cycle rather than as a
        // cabinet asking for a coin.
        if (t / BLINK_PERIOD).fract() >= BLINK_DUTY {
            return;
        }

        const PROMPT: &str = "LOAD A GAME";
        const SCALE: usize = 2;
        let width = font::width(PROMPT, SCALE);
        let x = (SCREEN_W as i32 - width as i32) / 2;
        let y = SCREEN_H as i32 - (font::GLYPH_H * SCALE) as i32 - 24;
        font::draw(out, SCREEN_W, SCREEN_H, x, y, PROMPT, SCALE, &PROMPT_STYLE);
    }
}

impl Default for Attract {
    fn default() -> Self {
        Self::new()
    }
}
