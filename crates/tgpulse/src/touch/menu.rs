//! The menu, built for a finger rather than a mouse.
//!
//! This is what replaces the desktop's ImGui windows on a handset: pick a
//! game, save and load, and change the settings that are worth changing on a
//! phone. The panels left behind are the ones that want a keyboard -- the
//! debugger console and the rebinding table -- because `winit`'s Android
//! backend cannot raise the soft one.
//!
//! Everything is a rectangle big enough to hit without aiming, and the game
//! list scrolls by dragging it, so nothing depends on a scrollbar the width of
//! a cursor.

use tgpulse_core::config::{Cabinet, Config};
use tgpulse_core::library::Entry;

use super::{centred_text, draw_rect, text_at, Rect, Target, TEXT, TEXT_DIM};
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
    CloseGame,
    OpenSettings,
    Back,
}

/// One line of the settings screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    Ssaa,
    Volume,
    Widescreen,
    Stretch2d,
    SmoothShadows,
    Rumble,
    Cabinet,
    ReverseLandscape,
}

impl Setting {
    /// Whether the value steps through a range, rather than simply flipping.
    fn stepped(self) -> bool {
        matches!(self, Setting::Ssaa | Setting::Volume)
    }

    fn label(self) -> &'static str {
        match self {
            Setting::Ssaa => "Supersampling",
            Setting::Volume => "Volume",
            Setting::Widescreen => "Widescreen",
            Setting::Stretch2d => "Stretch 2D layers",
            Setting::SmoothShadows => "Smooth shadows",
            Setting::Rumble => "Rumble",
            Setting::Cabinet => "Network board (twin)",
            Setting::ReverseLandscape => "Reverse landscape",
        }
    }

    /// A word about what it costs or what it changes. A phone player cannot
    /// hover for a tooltip, so the note is simply always on screen.
    fn note(self) -> &'static str {
        match self {
            Setting::Ssaa => "1 is the board's own output. Costs the most on a phone.",
            Setting::Volume => "SCSP titles mix quiet; try 400 for Sega Rally.",
            Setting::Widescreen => "Widens the 3D field of view to 16:9.",
            Setting::Stretch2d => "Stretches the sky and HUD across the wider frame.",
            Setting::SmoothShadows => "Blends the hardware's stipple instead of reproducing it.",
            Setting::Rumble => "Drive-board force sent to the pad's motors.",
            Setting::Cabinet => "Takes effect the next time a game is loaded.",
            Setting::ReverseLandscape => {
                "Turns the display around, for a cradle that holds the phone the other way."
            }
        }
    }

    fn value(self, config: &Config) -> String {
        match self {
            Setting::Ssaa => format!("{}x", config.ssaa),
            Setting::Volume => format!("{}%", config.volume),
            Setting::Widescreen => on_off(config.widescreen),
            Setting::Stretch2d => on_off(config.widescreen_stretch_2d),
            Setting::SmoothShadows => on_off(config.smooth_shadows),
            Setting::Rumble => on_off(config.rumble),
            Setting::Cabinet => match config.cabinet {
                Cabinet::Twin => "TWIN".to_string(),
                Cabinet::Single => "SINGLE".to_string(),
            },
            Setting::ReverseLandscape => on_off(config.reverse_landscape),
        }
    }

    /// Applies a tap. `delta` is the direction for a stepped setting and is
    /// ignored by the rest, which have only one other state to be in.
    fn apply(self, config: &mut Config, delta: i32) {
        match self {
            Setting::Ssaa => config.ssaa = (config.ssaa as i32 + delta).clamp(1, 4) as u32,
            Setting::Volume => {
                config.volume =
                    (config.volume as i32 + delta * VOLUME_STEP).clamp(0, VOLUME_MAX) as u32
            }
            Setting::Widescreen => config.widescreen = !config.widescreen,
            Setting::Stretch2d => config.widescreen_stretch_2d = !config.widescreen_stretch_2d,
            Setting::SmoothShadows => config.smooth_shadows = !config.smooth_shadows,
            Setting::Rumble => config.rumble = !config.rumble,
            Setting::Cabinet => {
                config.cabinet = match config.cabinet {
                    Cabinet::Twin => Cabinet::Single,
                    Cabinet::Single => Cabinet::Twin,
                }
            }
            Setting::ReverseLandscape => config.reverse_landscape = !config.reverse_landscape,
        }
    }
}

fn on_off(value: bool) -> String {
    if value { "ON" } else { "OFF" }.to_string()
}

const SETTINGS: [Setting; 8] = [
    Setting::Ssaa,
    Setting::Volume,
    Setting::Widescreen,
    Setting::Stretch2d,
    Setting::SmoothShadows,
    Setting::Rumble,
    Setting::Cabinet,
    Setting::ReverseLandscape,
];

/// Volume moves in steps a player can hear, and stops where the mixer does.
const VOLUME_STEP: i32 = 10;
const VOLUME_MAX: i32 = 800;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Settings,
}

/// A laid-out settings line: the label, and the boxes that change it.
struct Row {
    setting: Setting,
    /// Present only on a stepped setting; a toggle is driven by `value` alone.
    minus: Option<Rect>,
    plus: Option<Rect>,
    /// Shows the current value, and flips it when the setting has only two.
    value: Rect,
    /// The whole line, for drawing the label against.
    line: Rect,
}

pub struct Menu {
    open: bool,
    screen: Screen,
    size: (f32, f32),
    /// Whether the layout is the one for a game in progress.
    running: bool,
    list: Rect,
    row_h: f32,
    rows: usize,
    scroll: f32,
    buttons: Vec<(Button, Rect, &'static str)>,
    settings: Vec<Row>,
    pressed: Vec<Button>,
    adjusted: Vec<(Setting, i32)>,
    launched: Option<usize>,
}

impl Menu {
    pub fn new() -> Self {
        Self {
            open: true,
            screen: Screen::Library,
            size: (0.0, 0.0),
            running: false,
            list: Rect::default(),
            row_h: 1.0,
            rows: 0,
            scroll: 0.0,
            buttons: Vec::new(),
            settings: Vec::new(),
            pressed: Vec::new(),
            adjusted: Vec::new(),
            launched: None,
        }
    }

    pub fn open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// A game in progress adds the things that can be done to it.
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

    pub fn adjust(&mut self, setting: Setting, delta: i32) {
        self.adjusted.push((setting, delta));
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
        self.settings.clear();
        if w <= 0.0 || h <= 0.0 {
            return;
        }

        let margin = 0.04 * h;
        let bar = 0.115 * h;
        let gap = 0.012 * h;

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

        match self.screen {
            Screen::Settings => {
                header_button(self, Button::Back, "BACK", 1.8);
                self.layout_settings(w, h, margin, bar, gap);
            }
            Screen::Library => {
                header_button(self, Button::Quit, "QUIT", 1.6);
                header_button(self, Button::Refresh, "REFRESH", 2.2);
                if self.running {
                    header_button(self, Button::Resume, "RESUME", 2.2);
                }
                self.layout_library(w, h, margin, bar, gap);
            }
        }
    }

    fn layout_library(&mut self, w: f32, h: f32, margin: f32, bar: f32, gap: f32) {
        // Settings, volume and the save slot are here whether or not a game is
        // running: they are the emulator's, not the game's, and a player with
        // no romsets yet still has to be able to reach them.
        let mut strip: Vec<(Button, &'static str)> = vec![
            (Button::OpenSettings, "SETTINGS"),
            (Button::SlotDown, "SLOT -"),
            (Button::SlotUp, "SLOT +"),
        ];
        if self.running {
            strip.extend([
                (Button::SaveState, "SAVE"),
                (Button::LoadState, "LOAD"),
                (Button::Reset, "RESET"),
                (Button::CloseGame, "CLOSE"),
            ]);
        }

        let top = margin + bar + gap;
        let count = strip.len() as f32;
        let width = (w - margin * 2.0 - gap * (count - 1.0)) / count;
        for (index, (button, label)) in strip.into_iter().enumerate() {
            self.buttons.push((
                button,
                Rect {
                    x: margin + (width + gap) * index as f32,
                    y: top,
                    w: width,
                    h: bar,
                },
                label,
            ));
        }

        let top = top + bar + gap;
        self.row_h = 0.115 * h;
        self.list = Rect {
            x: margin,
            y: top,
            w: w - margin * 2.0,
            h: (h - margin - top).max(0.0),
        };
        self.set_scroll(self.scroll);
    }

    fn layout_settings(&mut self, w: f32, h: f32, margin: f32, bar: f32, gap: f32) {
        // The lines are shorter than a header bar so all eight fit between
        // the header and the bottom of the screen without scrolling.
        let line_h = 0.095 * h;
        let line_gap = 0.010 * h;
        let box_w = line_h * 1.6;
        let step_w = line_h;

        let mut y = margin + bar + gap;
        for setting in SETTINGS {
            let line = Rect {
                x: margin,
                y,
                w: w - margin * 2.0,
                h: line_h,
            };
            let right = line.x + line.w;
            let (minus, plus, value) = if setting.stepped() {
                let plus = Rect {
                    x: right - step_w,
                    y,
                    w: step_w,
                    h: line_h,
                };
                let value = Rect {
                    x: plus.x - line_gap - box_w,
                    y,
                    w: box_w,
                    h: line_h,
                };
                let minus = Rect {
                    x: value.x - line_gap - step_w,
                    y,
                    w: step_w,
                    h: line_h,
                };
                (Some(minus), Some(plus), value)
            } else {
                (
                    None,
                    None,
                    Rect {
                        x: right - box_w,
                        y,
                        w: box_w,
                        h: line_h,
                    },
                )
            };
            self.settings.push(Row {
                setting,
                minus,
                plus,
                value,
                line,
            });
            y += line_h + line_gap;
        }
    }

    /// What the finger landed on.
    pub fn pick(&self, x: f32, y: f32) -> Target {
        for (button, rect, _) in &self.buttons {
            if rect.contains(x, y) {
                return Target::MenuButton(*button);
            }
        }
        for row in &self.settings {
            if row.minus.is_some_and(|r| r.contains(x, y)) {
                return Target::MenuAdjust(row.setting, -1);
            }
            if row.plus.is_some_and(|r| r.contains(x, y)) {
                return Target::MenuAdjust(row.setting, 1);
            }
            if row.value.contains(x, y) {
                // Tapping the value of a stepped setting would be ambiguous,
                // so only a two-state one answers to it.
                if !row.setting.stepped() {
                    return Target::MenuAdjust(row.setting, 0);
                }
            }
        }
        if self.screen == Screen::Library && self.list.contains(x, y) {
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

        let title = match self.screen {
            Screen::Settings => "Settings",
            Screen::Library => running.unwrap_or("TGPulse"),
        };
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

        match self.screen {
            Screen::Settings => self.render_settings(ui, &dl, config, radius, line),
            Screen::Library => self.render_library(&dl, entries, config, radius, margin, line),
        }

        self.resolve(entries, config, slot)
    }

    fn render_settings(
        &self,
        ui: &imgui::Ui,
        dl: &imgui::DrawListMut<'_>,
        config: &Config,
        radius: f32,
        line: f32,
    ) {
        for row in &self.settings {
            let mid = row.line.y + row.line.h * 0.5;
            text_at(
                dl,
                row.line.x + row.line.h * 0.2,
                mid - line * 1.0,
                row.setting.label(),
                TEXT,
            );
            text_at(
                dl,
                row.line.x + row.line.h * 0.2,
                mid + line * 0.1,
                row.setting.note(),
                TEXT_DIM,
            );
            for (rect, label) in [(row.minus, "-"), (row.plus, "+")] {
                let Some(rect) = rect else { continue };
                draw_rect(dl, rect, false, radius);
                centred_text(ui, dl, rect.x + rect.w * 0.5, mid, label, TEXT);
            }
            draw_rect(dl, row.value, false, radius);
            centred_text(
                ui,
                dl,
                row.value.x + row.value.w * 0.5,
                mid,
                &row.setting.value(config),
                TEXT,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_library(
        &self,
        dl: &imgui::DrawListMut<'_>,
        entries: &[Entry],
        config: &Config,
        radius: f32,
        margin: f32,
        line: f32,
    ) {
        if entries.is_empty() {
            text_at(dl, self.list.x, self.list.y + line, "No romsets found.", TEXT);
            text_at(
                dl,
                self.list.x,
                self.list.y + line * 2.4,
                &format!("Put zipped romsets in {}", config.rom_dir.display()),
                TEXT_DIM,
            );
            return;
        }
        // Rows are clipped to the list rather than laid out to fit it, so a
        // part-scrolled row is cut off instead of spilling over the bar above.
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
                    draw_rect(dl, rect, false, radius);
                    text_at(dl, rect.x + margin * 0.5, y + line * 0.35, &entry.title, TEXT);
                    let mut detail = entry.set.clone();
                    if !entry.year.is_empty() {
                        detail.push_str(&format!("   {}", entry.year));
                    }
                    if !entry.missing.is_empty() {
                        detail.push_str(&format!("   {} file(s) missing", entry.missing.len()));
                    }
                    text_at(dl, rect.x + margin * 0.5, y + line * 1.6, &detail, TEXT_DIM);
                }
            },
        );
    }

    /// Turns everything pressed since the last frame into work for the
    /// application. The state slot is settled here because it is the menu's
    /// own business, not the emulator's.
    fn resolve(&mut self, entries: &[Entry], config: &mut Config, slot: &mut u32) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut relayout = false;

        for (setting, delta) in std::mem::take(&mut self.adjusted) {
            setting.apply(config, delta);
            actions.push(Action::SettingsChanged);
        }

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
                Button::CloseGame => {
                    actions.push(Action::CloseGame);
                    self.open = true;
                }
                Button::OpenSettings => {
                    self.screen = Screen::Settings;
                    relayout = true;
                }
                Button::Back => {
                    self.screen = Screen::Library;
                    relayout = true;
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

        if relayout {
            let (w, h) = self.size;
            self.layout(w, h);
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (f32, f32) = (2400.0, 1080.0);

    fn menu(screen: Screen, running: bool) -> Menu {
        let mut menu = Menu::new();
        menu.running = running;
        menu.screen = screen;
        menu.layout(SCREEN.0, SCREEN.1);
        menu
    }

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    fn on_screen(r: &Rect) -> bool {
        r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= SCREEN.0 && r.y + r.h <= SCREEN.1
    }

    /// Eight settings have to fit between the header and the bottom edge
    /// without scrolling, and none may sit on top of another.
    #[test]
    fn the_settings_screen_fits_and_does_not_overlap() {
        let menu = menu(Screen::Settings, false);
        assert_eq!(menu.settings.len(), SETTINGS.len());

        let mut boxes: Vec<Rect> = menu.buttons.iter().map(|(_, r, _)| *r).collect();
        for row in &menu.settings {
            assert!(on_screen(&row.line), "{:?} runs off screen", row.setting);
            boxes.extend(row.minus);
            boxes.extend(row.plus);
            boxes.push(row.value);
        }
        for (index, a) in boxes.iter().enumerate() {
            assert!(on_screen(a), "a control runs off screen: {:?}", (a.x, a.y));
            for b in boxes.iter().skip(index + 1) {
                assert!(!overlaps(a, b), "two controls overlap");
            }
        }
    }

    /// Every setting must be reachable, and a stepped one needs both
    /// directions -- a value box alone cannot say which way to go.
    #[test]
    fn every_setting_has_the_controls_it_needs() {
        let menu = menu(Screen::Settings, false);
        for row in &menu.settings {
            if row.setting.stepped() {
                assert!(row.minus.is_some() && row.plus.is_some(), "{:?}", row.setting);
                let minus = row.minus.unwrap();
                assert!(matches!(
                    menu.pick(minus.x + minus.w * 0.5, minus.y + minus.h * 0.5),
                    Target::MenuAdjust(s, -1) if s == row.setting
                ));
            } else {
                assert!(row.minus.is_none() && row.plus.is_none(), "{:?}", row.setting);
                assert!(matches!(
                    menu.pick(row.value.x + row.value.w * 0.5, row.value.y + row.value.h * 0.5),
                    Target::MenuAdjust(s, 0) if s == row.setting
                ));
            }
        }
    }

    /// Settings and the save slot belong to the emulator, not to the game, so
    /// they have to be reachable with nothing loaded -- which is exactly the
    /// state a player is in before they have any romsets.
    #[test]
    fn settings_are_reachable_with_no_game_running() {
        let menu = menu(Screen::Library, false);
        for wanted in [Button::OpenSettings, Button::SlotUp, Button::SlotDown] {
            assert!(
                menu.buttons.iter().any(|(b, _, _)| *b == wanted),
                "{wanted:?} is missing from the idle library screen",
            );
        }
    }

    /// The stepped settings must stop at the ends of their range rather than
    /// wrapping to a value the renderer cannot build.
    #[test]
    fn stepped_settings_clamp() {
        let mut config = Config::default();

        config.ssaa = 1;
        Setting::Ssaa.apply(&mut config, -1);
        assert_eq!(config.ssaa, 1, "supersampling went below 1");
        config.ssaa = 4;
        Setting::Ssaa.apply(&mut config, 1);
        assert_eq!(config.ssaa, 4, "supersampling went above 4");

        config.volume = 0;
        Setting::Volume.apply(&mut config, -1);
        assert_eq!(config.volume, 0, "volume went negative");
        config.volume = VOLUME_MAX as u32;
        Setting::Volume.apply(&mut config, 1);
        assert_eq!(config.volume, VOLUME_MAX as u32, "volume passed the maximum");
    }

    /// A two-state setting flips whichever way it is tapped.
    #[test]
    fn toggles_flip() {
        let mut config = Config::default();
        for setting in SETTINGS.iter().filter(|s| !s.stepped()) {
            let before = setting.value(&config);
            setting.apply(&mut config, 0);
            assert_ne!(before, setting.value(&config), "{setting:?} did not change");
        }
    }
}
