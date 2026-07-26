//! The menu, built for a finger rather than a mouse.
//!
//! This is what replaces the desktop's ImGui windows on a handset. It covers
//! only what a phone player actually needs -- pick a game, save and load,
//! adjust the volume, put the game down -- because the rest of the desktop
//! interface (the debugger console, the rebinding panel) wants a keyboard that
//! is not there.
//!
//! Everything is a rectangle big enough to hit without aiming, and the game
//! list scrolls by dragging it, so nothing depends on a scrollbar the width of
//! a cursor.

use tgpulse_core::config::Config;
use tgpulse_core::library::Entry;

use super::{
    centred_text, draw_rect, text_at, Rect, Target, TEXT, TEXT_DIM,
};
use crate::gui::Action;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Resume,
    Refresh,
    Quit,
    SaveState,
    LoadState,
    SlotDown,
    SlotUp,
    Reset,
    VolumeDown,
    VolumeUp,
    CloseGame,
}

/// Volume moves in steps a player can hear, and stops where the mixer does.
const VOLUME_STEP: u32 = 10;
const VOLUME_MAX: u32 = 800;

pub struct Menu {
    open: bool,
    size: (f32, f32),
    /// Whether the layout is the one for a game in progress.
    running: bool,
    list: Rect,
    row_h: f32,
    rows: usize,
    scroll: f32,
    buttons: Vec<(Button, Rect, &'static str)>,
    pressed: Vec<Button>,
    launched: Option<usize>,
}

impl Menu {
    pub fn new() -> Self {
        Self {
            open: true,
            size: (0.0, 0.0),
            running: false,
            list: Rect::default(),
            row_h: 1.0,
            rows: 0,
            scroll: 0.0,
            buttons: Vec::new(),
            pressed: Vec::new(),
            launched: None,
        }
    }

    pub fn open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// A game in progress adds the row of things that can be done to it.
    pub fn set_running(&mut self, running: bool) {
        if self.running != running {
            self.running = running;
            let (w, h) = self.size;
            self.layout(w, h);
        }
    }

    pub fn press(&mut self, button: Button) {
        self.pressed.push(button);
    }

    pub fn press_row(&mut self, row: usize) {
        self.launched = Some(row);
    }

    pub fn set_scroll(&mut self, scroll: f32) {
        let span = (self.rows as f32 * self.row_h - self.list.h).max(0.0);
        self.scroll = scroll.clamp(0.0, span);
    }

    pub fn layout(&mut self, w: f32, h: f32) {
        self.size = (w, h);
        self.buttons.clear();
        if w <= 0.0 || h <= 0.0 {
            return;
        }

        let margin = 0.04 * h;
        let bar = 0.115 * h;
        let gap = 0.012 * h;

        // The header carries the things that are always available; a running
        // game adds a second row for the things that act on it.
        let mut right = w - margin;
        let mut header_button = |this: &mut Self, button: Button, label: &'static str, wide: f32| {
            let width = bar * wide;
            right -= width;
            this.buttons.push((
                button,
                Rect {
                    x: right,
                    y: margin,
                    w: width,
                    h: bar,
                },
                label,
            ));
            right -= gap;
        };
        header_button(self, Button::Quit, "QUIT", 1.6);
        header_button(self, Button::Refresh, "REFRESH", 2.2);
        if self.running {
            header_button(self, Button::Resume, "RESUME", 2.2);
        }

        let mut top = margin + bar + gap;
        if self.running {
            // Eight of them across the screen, evenly, so no one button is
            // harder to hit than another.
            const STRIP: [(Button, &str); 8] = [
                (Button::SaveState, "SAVE"),
                (Button::LoadState, "LOAD"),
                (Button::SlotDown, "SLOT -"),
                (Button::SlotUp, "SLOT +"),
                (Button::VolumeDown, "VOL -"),
                (Button::VolumeUp, "VOL +"),
                (Button::Reset, "RESET"),
                (Button::CloseGame, "CLOSE"),
            ];
            let count = STRIP.len() as f32;
            let width = (w - margin * 2.0 - gap * (count - 1.0)) / count;
            for (index, (button, label)) in STRIP.iter().enumerate() {
                self.buttons.push((
                    *button,
                    Rect {
                        x: margin + (width + gap) * index as f32,
                        y: top,
                        w: width,
                        h: bar,
                    },
                    label,
                ));
            }
            top += bar + gap;
        }

        self.row_h = 0.115 * h;
        self.list = Rect {
            x: margin,
            y: top,
            w: w - margin * 2.0,
            h: (h - margin - top).max(0.0),
        };
        self.set_scroll(self.scroll);
    }

    /// What the finger landed on.
    pub fn pick(&self, x: f32, y: f32) -> Target {
        for (button, rect, _) in &self.buttons {
            if rect.contains(x, y) {
                return Target::MenuButton(*button);
            }
        }
        if self.list.contains(x, y) {
            let index = ((y - self.list.y + self.scroll) / self.row_h).floor();
            let row = (index >= 0.0 && (index as usize) < self.rows).then_some(index as usize);
            return Target::MenuList {
                row,
                from: self.scroll,
            };
        }
        Target::None
    }

    pub fn render(
        &mut self,
        ui: &imgui::Ui,
        entries: &[Entry],
        running: Option<&str>,
        config: &mut Config,
        slot: &mut u32,
    ) -> Vec<Action> {
        self.rows = entries.len();
        self.set_scroll(self.scroll);

        let (w, h) = self.size;
        let dl = ui.get_background_draw_list();
        // The menu is modal, so it dims whatever is behind it rather than
        // letting a moving picture compete with the text.
        dl.add_rect([0.0, 0.0], [w, h], [0.03, 0.04, 0.06, 0.92])
            .filled(true)
            .build();

        let margin = 0.04 * h;
        let line = ui.calc_text_size("X")[1];
        let radius = 0.012 * h;

        let title = running.unwrap_or("TGPulse");
        text_at(&dl, margin, margin + line * 0.2, title, TEXT);
        text_at(
            &dl,
            margin,
            margin + line * 1.4,
            &format!("slot {}   volume {}%", *slot, config.volume),
            TEXT_DIM,
        );

        for (_, rect, label) in &self.buttons {
            draw_rect(&dl, *rect, false, radius);
            centred_text(
                ui,
                &dl,
                rect.x + rect.w * 0.5,
                rect.y + rect.h * 0.5,
                label,
                TEXT,
            );
        }

        if entries.is_empty() {
            text_at(
                &dl,
                self.list.x,
                self.list.y + line,
                "No romsets found.",
                TEXT,
            );
            text_at(
                &dl,
                self.list.x,
                self.list.y + line * 2.4,
                &format!("Put zipped romsets in {}", config.rom_dir.display()),
                TEXT_DIM,
            );
        } else {
            // Rows are clipped to the list rather than laid out to fit it, so
            // a part-scrolled row is cut off instead of spilling over the bar
            // above it.
            dl.with_clip_rect(
                [self.list.x, self.list.y],
                [self.list.x + self.list.w, self.list.y + self.list.h],
                || {
                    let first = (self.scroll / self.row_h).floor().max(0.0) as usize;
                    let visible = (self.list.h / self.row_h).ceil() as usize + 1;
                    for (index, entry) in entries.iter().enumerate().skip(first).take(visible) {
                        let y = self.list.y + index as f32 * self.row_h - self.scroll;
                        let rect = Rect {
                            x: self.list.x,
                            y,
                            w: self.list.w,
                            h: self.row_h - 4.0,
                        };
                        draw_rect(&dl, rect, false, radius);
                        text_at(&dl, rect.x + margin * 0.5, y + line * 0.35, &entry.title, TEXT);
                        let mut detail = entry.set.clone();
                        if !entry.year.is_empty() {
                            detail.push_str(&format!("   {}", entry.year));
                        }
                        if !entry.missing.is_empty() {
                            detail.push_str(&format!("   {} file(s) missing", entry.missing.len()));
                        }
                        text_at(
                            &dl,
                            rect.x + margin * 0.5,
                            y + line * 1.6,
                            &detail,
                            TEXT_DIM,
                        );
                    }
                },
            );
        }

        self.resolve(entries, config, slot)
    }

    /// Turns everything pressed since the last frame into work for the
    /// application. Volume and the state slot are settled here because they
    /// are the menu's own business, not the emulator's.
    fn resolve(&mut self, entries: &[Entry], config: &mut Config, slot: &mut u32) -> Vec<Action> {
        let mut actions = Vec::new();
        for button in std::mem::take(&mut self.pressed) {
            match button {
                Button::Resume => self.open = false,
                Button::Refresh => actions.push(Action::RefreshLibrary),
                Button::Quit => actions.push(Action::Quit),
                Button::SaveState => actions.push(Action::SaveState(*slot)),
                Button::LoadState => actions.push(Action::LoadState(*slot)),
                Button::SlotDown => *slot = (*slot + 9) % 10,
                Button::SlotUp => *slot = (*slot + 1) % 10,
                Button::Reset => actions.push(Action::Reset),
                Button::VolumeDown => {
                    config.volume = config.volume.saturating_sub(VOLUME_STEP);
                    actions.push(Action::SettingsChanged);
                }
                Button::VolumeUp => {
                    config.volume = (config.volume + VOLUME_STEP).min(VOLUME_MAX);
                    actions.push(Action::SettingsChanged);
                }
                Button::CloseGame => {
                    actions.push(Action::CloseGame);
                    self.open = true;
                }
            }
        }
        if let Some(row) = self.launched.take() {
            if let Some(entry) = entries.get(row) {
                actions.push(Action::Launch(entry.path.clone()));
                // Starting a game puts the menu away; the overlay's menu key
                // brings it back.
                self.open = false;
            }
        }
        actions
    }
}
