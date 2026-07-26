//! The Dear ImGui front end.
//!
//! The interface is drawn over the emulated image and is the same whether a
//! game is running or not: with nothing loaded, the library window fills the
//! screen; with a game running, F1 brings the same windows back over it.
//!
//! Nothing here touches the machine. Each frame produces a list of `Action`s
//! that the application applies, which keeps the emulator's state in one place
//! and makes the UI replaceable.

mod platform;
mod renderer;

use std::path::PathBuf;
use std::time::Duration;

use tgpulse_core::config::Config;
use tgpulse_core::library::{self, Entry};
use tgpulse_core::tilemap::{SCREEN_H, SCREEN_W};

use crate::bindings::{Bindings, Control, Hotkey, Source};

pub use renderer::Renderer;

/// How much larger everything is drawn than on a desktop. A phone is held at
/// arm's length and has no pointer to aim with, so both the text and the hit
/// targets have to grow; everywhere else this is 1 and nothing changes.
const UI_SCALE: f32 = if cfg!(target_os = "android") { 2.5 } else { 1.0 };

/// What the interface is asking the application to do.
pub enum Action {
    Launch(PathBuf),
    CloseGame,
    Reset,
    SaveState(u32),
    LoadState(u32),
    /// A line typed into the debugger console.
    Debug(String),
    /// Settings were edited; the application re-reads them.
    SettingsChanged,
    /// Bindings were edited; the application re-reads and saves them.
    BindingsChanged,
    /// The ROM directory should be scanned again.
    RefreshLibrary,
    Quit,
}

/// Live numbers the status window shows.
#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub video_fps: f32,
    pub emulated_fps: f32,
    pub render_ms: f32,
}

pub struct Gui {
    context: imgui::Context,

    /// Whether the player wants the interface up.
    pub visible: bool,
    /// Set while something else should have the screen to itself.
    suppressed: bool,
    show_settings: bool,
    show_input: bool,
    show_debugger: bool,
    show_stats: bool,

    /// What the next key press should be bound to, while the panel is waiting
    /// for one.
    awaiting: Option<Awaiting>,

    entries: Vec<Entry>,
    selected: Option<usize>,
    library_error: Option<String>,

    debug_input: String,
    debug_log: Vec<String>,
    debug_follow: bool,

    /// Save-state slot the hotkeys and the buttons act on.
    pub state_slot: u32,
    captured: Option<(Awaiting, winit::keyboard::KeyCode)>,
}

/// What a pending key press will be bound to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Awaiting {
    Control(Control),
    Hotkey(Hotkey),
}

impl Gui {
    pub fn new(config: &Config) -> Self {
        let mut context = imgui::Context::create();
        context.set_ini_filename(None);
        // A sane display size before the first `Resized`: laying windows out
        // against a zero-sized display collapses them.
        context.io_mut().display_size = [(SCREEN_W * 2) as f32, (SCREEN_H * 2) as f32];
        style(&mut context);

        // Nothing on a handset is legible at the default 13 pixels, and the
        // touch interface draws its labels from this same atlas. The atlas is
        // not built until the renderer asks for it, so the size only has to be
        // settled before then.
        if UI_SCALE > 1.0 {
            context.fonts().add_font(&[imgui::FontSource::DefaultFontData {
                config: Some(imgui::FontConfig {
                    size_pixels: 13.0 * UI_SCALE,
                    ..imgui::FontConfig::default()
                }),
            }]);
            context.style_mut().scale_all_sizes(UI_SCALE);
        }

        Self {
            context,
            visible: true,
            suppressed: false,
            show_settings: false,
            show_input: false,
            awaiting: None,
            show_debugger: false,
            show_stats: false,
            entries: library::scan(&config.rom_dir),
            selected: None,
            library_error: None,
            debug_input: String::new(),
            debug_log: Vec::new(),
            debug_follow: true,
            state_slot: 0,
            captured: None,
        }
    }

    /// Re-reads the ROM directory.
    pub fn refresh_library(&mut self, config: &Config) {
        self.entries = library::scan(&config.rom_dir);
        if self.selected.is_some_and(|i| i >= self.entries.len()) {
            self.selected = None;
        }
    }

    /// Takes a key press for whatever the input panel is waiting to bind.
    /// Returns whether it was consumed.
    pub fn capture_key(&mut self, key: winit::keyboard::KeyCode) -> bool {
        let Some(awaiting) = self.awaiting.take() else {
            return false;
        };
        // Escape abandons the binding rather than binding Escape, which would
        // leave no way out of the panel.
        if key == winit::keyboard::KeyCode::Escape {
            return true;
        }
        self.captured = Some((awaiting, key));
        true
    }

    /// The binding the panel captured since the last call, if any.
    pub fn take_capture(&mut self) -> Option<(Awaiting, winit::keyboard::KeyCode)> {
        self.captured.take()
    }

    pub fn report_error(&mut self, message: impl Into<String>) {
        self.library_error = Some(message.into());
    }

    /// Appends debugger output.
    pub fn push_debug_output(&mut self, lines: impl IntoIterator<Item = String>) {
        self.debug_log.extend(lines);
        // Keep the console bounded; a `run` over thousands of frames can emit a
        // great deal and the scrollback is not the transcript of record.
        const LIMIT: usize = 4000;
        if self.debug_log.len() > LIMIT {
            self.debug_log.drain(..self.debug_log.len() - LIMIT);
        }
    }

    pub fn handle_event(
        &mut self,
        window: &winit::window::Window,
        event: &winit::event::WindowEvent,
    ) -> bool {
        let captured = platform::handle_event(self.context.io_mut(), window, event);
        // Events are only stolen from the machine while the interface is up.
        captured && self.showing()
    }

    /// Builds a frame and returns what the user asked for.
    ///
    /// `running` is the title of the game currently loaded, if any.
    // A frame needs the renderer, the clock, and every piece of state the
    // windows can edit; bundling them into a struct would only move the list.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        renderer: &mut Renderer,
        dt: Duration,
        running: Option<&str>,
        config: &mut Config,
        bindings: &mut Bindings,
        stats: Stats,
        touch: Option<&mut crate::touch::TouchUi>,
    ) -> Vec<Action> {
        self.context.io_mut().delta_time = dt.as_secs_f32().max(1.0 / 1000.0);

        // A handset drives the touch interface instead of these windows: they
        // are built for a pointer that can hover and a keyboard that can type,
        // and it has neither.
        if let Some(touch) = touch.filter(|t| t.enabled()) {
            let ui = self.context.frame();
            let actions = touch.render(ui, &self.entries, running, config, &mut self.state_slot);
            renderer.capture(self.context.render());
            return actions;
        }

        let mut actions = Vec::new();
        if !self.showing() {
            // Still render, so the stats window can stay up during play.
            if self.show_stats {
                let ui = self.context.frame();
                stats_window(ui, stats);
                renderer.capture(self.context.render());
            }
            return actions;
        }

        // Fields the closures need but cannot borrow through `self`.
        let (
            mut show_settings,
            mut show_input,
            mut show_debugger,
            mut show_stats,
            mut selected,
            mut debug_input,
            mut state_slot,
        ) = (
            self.show_settings,
            self.show_input,
            self.show_debugger,
            self.show_stats,
            self.selected,
            std::mem::take(&mut self.debug_input),
            self.state_slot,
        );
        let entries = &self.entries;
        let debug_log = &self.debug_log;
        let debug_follow = &mut self.debug_follow;
        let library_error = &self.library_error;
        let mut awaiting = self.awaiting;
        let mut refresh = false;

        let ui = self.context.frame();

        ui.main_menu_bar(|| {
            ui.menu("File", || {
                if ui.menu_item("Refresh library") {
                    refresh = true;
                }
                if ui
                    .menu_item_config("Close game")
                    .enabled(running.is_some())
                    .build()
                {
                    actions.push(Action::CloseGame);
                }
                ui.separator();
                if ui.menu_item("Quit") {
                    actions.push(Action::Quit);
                }
            });
            ui.menu("Machine", || {
                let enabled = running.is_some();
                if ui.menu_item_config("Reset").enabled(enabled).build() {
                    actions.push(Action::Reset);
                }
                ui.separator();
                if ui
                    .menu_item_config(format!("Save state (slot {state_slot})"))
                    .shortcut("F5")
                    .enabled(enabled)
                    .build()
                {
                    actions.push(Action::SaveState(state_slot));
                }
                if ui
                    .menu_item_config(format!("Load state (slot {state_slot})"))
                    .shortcut("F7")
                    .enabled(enabled)
                    .build()
                {
                    actions.push(Action::LoadState(state_slot));
                }
            });
            ui.menu("View", || {
                ui.checkbox("Settings", &mut show_settings);
                ui.checkbox("Input", &mut show_input);
                ui.checkbox("Debugger", &mut show_debugger);
                ui.checkbox("Statistics", &mut show_stats);
            });
            // The right-hand side of the bar is a status line.
            match running {
                Some(title) => {
                    ui.text_disabled(format!("  |  {title}  |  F1 hides this, F11 fullscreen"));
                }
                None => ui.text_disabled("  |  no game loaded"),
            }
        });

        // The library is the main screen's window, not an overlay: with a game
        // running there is nothing to pick, and it would sit over the picture.
        // Close the game and it comes back.
        if running.is_none() {
            library_window(
                ui,
                ui.io().display_size,
                entries,
                library_error.as_deref(),
                &mut selected,
                &mut refresh,
                &mut actions,
            );
        }

        if show_settings {
            settings_window(ui, config, &mut show_settings, &mut actions);
        }
        if show_input {
            input_window(ui, bindings, &mut awaiting, &mut show_input, &mut actions);
        }
        if show_debugger {
            debugger_window(
                ui,
                debug_log,
                &mut debug_input,
                debug_follow,
                &mut show_debugger,
                &mut actions,
            );
        }
        if show_stats {
            stats_window(ui, stats);
        }

        let _ = &mut state_slot;
        renderer.capture(self.context.render());

        self.show_settings = show_settings;
        self.show_input = show_input;
        self.awaiting = awaiting;
        self.show_debugger = show_debugger;
        self.show_stats = show_stats;
        self.selected = selected;
        self.debug_input = debug_input;
        self.state_slot = state_slot;
        if refresh {
            self.refresh_library(config);
            self.library_error = None;
        }
        actions
    }

    /// Builds a renderer for the current surface. The interface's own state
    /// outlives it, so a suspended activity comes back with its library scan,
    /// its console scrollback and its open windows intact.
    pub fn build_renderer(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Renderer {
        let atlas = self.context.fonts().build_rgba32_texture();
        Renderer::new(device, queue, format, &atlas)
    }

    /// Hides the interface without forgetting that the player wanted it up.
    pub fn set_suppressed(&mut self, suppressed: bool) {
        self.suppressed = suppressed;
    }

    fn showing(&self) -> bool {
        self.visible && !self.suppressed
    }

    pub fn set_display_size(&mut self, width: u32, height: u32) {
        self.context.io_mut().display_size = [width as f32, height as f32];
    }
}

fn library_window(
    ui: &imgui::Ui,
    display: [f32; 2],
    entries: &[Entry],
    error: Option<&str>,
    selected: &mut Option<usize>,
    refresh: &mut bool,
    actions: &mut Vec<Action>,
) {
    // Centred on the display the first time it is laid out. `FirstUseEver`
    // rather than `Always` so dragging it somewhere else sticks.
    ui.window("ROM library")
        .size([560.0, 380.0], imgui::Condition::FirstUseEver)
        .position(
            [display[0] * 0.5, display[1] * 0.5],
            imgui::Condition::FirstUseEver,
        )
        .position_pivot([0.5, 0.5])
        .build(|| {
            if ui.button("Refresh") {
                *refresh = true;
            }
            ui.same_line();
            let can_launch = selected
                .and_then(|i| entries.get(i))
                .is_some_and(Entry::is_known);
            ui.enabled(can_launch, || {
                if ui.button("Play") {
                    if let Some(entry) = selected.and_then(|i| entries.get(i)) {
                        actions.push(Action::Launch(entry.path.clone()));
                    }
                }
            });
            ui.same_line();
            ui.text_disabled(format!("{} romset(s)", entries.len()));

            if let Some(error) = error {
                ui.text_colored([1.0, 0.45, 0.4, 1.0], error);
            }
            ui.separator();

            ui.child_window("list")
                .size([0.0, -ui.frame_height_with_spacing() * 3.0])
                .build(|| {
                    if entries.is_empty() {
                        ui.text_wrapped(
                            "Nothing here yet. Put zipped romsets in the roms \
                             directory and press Refresh.",
                        );
                        return;
                    }
                    for (i, entry) in entries.iter().enumerate() {
                        let label = if entry.is_known() {
                            format!("{}  ({})", entry.title, entry.set)
                        } else {
                            format!("{}  -- unrecognised", entry.set)
                        };
                        let colour = if !entry.is_known() {
                            [0.6, 0.6, 0.6, 1.0]
                        } else if entry.is_complete() {
                            [0.9, 0.9, 0.9, 1.0]
                        } else {
                            [1.0, 0.8, 0.4, 1.0]
                        };
                        let token = ui.push_style_color(imgui::StyleColor::Text, colour);
                        if ui
                            .selectable_config(&label)
                            .selected(*selected == Some(i))
                            .build()
                        {
                            *selected = Some(i);
                        }
                        token.pop();
                        // Double-click launches, which is what a list like this
                        // is expected to do.
                        if ui.is_item_hovered()
                            && ui.is_mouse_double_clicked(imgui::MouseButton::Left)
                            && entry.is_known()
                        {
                            actions.push(Action::Launch(entry.path.clone()));
                        }
                    }
                });

            ui.separator();
            match selected.and_then(|i| entries.get(i)) {
                Some(entry) => {
                    ui.text(format!(
                        "{}  {}  {}",
                        entry.year,
                        entry.manufacturer,
                        entry.board.map(|b| b.label()).unwrap_or("unknown board"),
                    ));
                    ui.text_disabled(entry.path.display().to_string());
                    if entry.missing.is_empty() {
                        ui.text_disabled(entry.status());
                    } else {
                        ui.text_colored(
                            [1.0, 0.8, 0.4, 1.0],
                            format!("missing: {}", entry.missing.join(", ")),
                        );
                    }
                }
                None => ui.text_disabled("Select a romset."),
            }
        });
}

fn settings_window(
    ui: &imgui::Ui,
    config: &mut Config,
    open: &mut bool,
    actions: &mut Vec<Action>,
) {
    ui.window("Settings")
        .size([420.0, 340.0], imgui::Condition::FirstUseEver)
        .opened(open)
        .build(|| {
            let mut changed = false;

            ui.text_disabled("Video");
            let mut ssaa = config.ssaa as i32;
            if ui.slider("Supersampling", 1, 4, &mut ssaa) {
                config.ssaa = ssaa as u32;
                changed = true;
            }
            ui.text_disabled("1 is the board's own output, without antialiasing.");
            changed |= ui.checkbox("Widescreen", &mut config.widescreen);
            changed |= ui.checkbox(
                "Stretch 2D layers when widescreen",
                &mut config.widescreen_stretch_2d,
            );
            changed |= ui.checkbox("Smooth shadows", &mut config.smooth_shadows);
            ui.text_disabled("Blends the hardware's stipple instead of reproducing it.");
            // Applied at once, but not remembered: fullscreen is how the
            // window is being looked at, not a preference.
            if ui.checkbox("Fullscreen", &mut config.fullscreen) {
                actions.push(Action::SettingsChanged);
            }
            ui.same_line();
            ui.text_disabled("(this run only)");

            ui.separator();
            ui.text_disabled("Audio");
            let mut volume = config.volume as i32;
            if ui.slider("Volume %", 0, 800, &mut volume) {
                config.volume = volume as u32;
                changed = true;
            }
            ui.text_disabled("SCSP titles mix quiet; try 400 for Sega Rally.");

            ui.separator();
            ui.text_disabled("Machine");
            changed |= ui.checkbox("Force feedback to pad rumble", &mut config.rumble);
            let mut twin = config.cabinet == tgpulse_core::config::Cabinet::Twin;
            if ui.checkbox("Network board fitted (twin cabinet)", &mut twin) {
                config.cabinet = if twin {
                    tgpulse_core::config::Cabinet::Twin
                } else {
                    tgpulse_core::config::Cabinet::Single
                };
                changed = true;
            }
            ui.text_disabled("Takes effect the next time a game is loaded.");

            ui.separator();
            if ui.button("Revert to defaults") {
                let shipped = Config::default();
                config.ssaa = shipped.ssaa;
                config.widescreen = shipped.widescreen;
                config.widescreen_stretch_2d = shipped.widescreen_stretch_2d;
                config.smooth_shadows = shipped.smooth_shadows;
                config.fullscreen = shipped.fullscreen;
                config.volume = shipped.volume;
                config.rumble = shipped.rumble;
                config.cabinet = shipped.cabinet;
                changed = true;
            }

            if changed {
                actions.push(Action::SettingsChanged);
            }
        });
}

/// Controls and hotkeys, with click-to-rebind.
///
/// Only keyboard rebinding happens here: a pad binding needs the pad to be
/// held still while the panel reads it, which is a different interaction. The
/// pad bindings each control already carries are listed so they are at least
/// discoverable, and the config file can be edited to change them.
fn input_window(
    ui: &imgui::Ui,
    bindings: &mut Bindings,
    awaiting: &mut Option<Awaiting>,
    open: &mut bool,
    actions: &mut Vec<Action>,
) {
    ui.window("Input")
        .size([560.0, 520.0], imgui::Condition::FirstUseEver)
        .opened(open)
        .build(|| {
            if awaiting.is_some() {
                ui.text_colored([1.0, 0.85, 0.3, 1.0], "Press a key, or Escape to cancel.");
            } else {
                ui.text_disabled("Click a binding, then press the key you want.");
            }
            ui.same_line();
            if ui.button("Revert to defaults") {
                *bindings = Bindings::default();
                actions.push(Action::BindingsChanged);
            }
            ui.separator();

            if let Some(tabs) = ui.tab_bar("input_tabs") {
                if let Some(tab) = ui.tab_item("Emulator") {
                    for hotkey in Hotkey::ALL {
                        let bound = bindings
                            .hotkey(*hotkey)
                            .map(|k| Source::Key(k).to_string())
                            .unwrap_or_else(|| "-".into());
                        binding_row(
                            ui,
                            hotkey.label(),
                            &bound,
                            *awaiting == Some(Awaiting::Hotkey(*hotkey)),
                            || *awaiting = Some(Awaiting::Hotkey(*hotkey)),
                        );
                    }
                    tab.end();
                }
                if let Some(tab) = ui.tab_item("Cabinet") {
                    ui.text_disabled(
                        "Not every machine has every control: a racing cabinet reads                          the wheel and pedals, a fighting one reads the stick.",
                    );
                    ui.separator();
                    for control in Control::ALL {
                        let sources: Vec<String> = bindings
                            .sources(*control)
                            .iter()
                            .map(ToString::to_string)
                            .collect();
                        let bound = if sources.is_empty() {
                            "-".to_string()
                        } else {
                            sources.join(", ")
                        };
                        binding_row(
                            ui,
                            control.label(),
                            &bound,
                            *awaiting == Some(Awaiting::Control(*control)),
                            || *awaiting = Some(Awaiting::Control(*control)),
                        );
                    }
                    tab.end();
                }
                tabs.end();
            }
        });
}

/// One "name.... binding" row whose right-hand side is the button.
fn binding_row(
    ui: &imgui::Ui,
    label: &str,
    bound: &str,
    listening: bool,
    mut on_click: impl FnMut(),
) {
    ui.text(label);
    ui.same_line_with_pos(220.0);
    let caption = if listening {
        "  ...  ".to_string()
    } else {
        format!("{bound}##{label}")
    };
    let token =
        listening.then(|| ui.push_style_color(imgui::StyleColor::Button, [0.55, 0.42, 0.12, 1.0]));
    if ui.button_with_size(caption, [300.0, 0.0]) {
        on_click();
    }
    if let Some(token) = token {
        token.pop();
    }
}

fn debugger_window(
    ui: &imgui::Ui,
    log: &[String],
    input: &mut String,
    follow: &mut bool,
    open: &mut bool,
    actions: &mut Vec<Action>,
) {
    ui.window("Debugger")
        .size([620.0, 400.0], imgui::Condition::FirstUseEver)
        .opened(open)
        .build(|| {
            ui.checkbox("Follow output", follow);
            ui.same_line();
            if ui.button("help") {
                actions.push(Action::Debug("help".into()));
            }
            ui.same_line();
            if ui.button("state") {
                actions.push(Action::Debug("state".into()));
            }
            ui.same_line();
            if ui.button("regs") {
                actions.push(Action::Debug("regs".into()));
            }
            ui.separator();

            ui.child_window("log")
                .size([0.0, -ui.frame_height_with_spacing()])
                .horizontal_scrollbar(true)
                .build(|| {
                    for line in log {
                        ui.text(line);
                    }
                    if *follow {
                        ui.set_scroll_here_y_with_ratio(1.0);
                    }
                });

            ui.set_next_item_width(-1.0);
            if ui
                .input_text("##command", input)
                .enter_returns_true(true)
                .build()
            {
                if !input.trim().is_empty() {
                    actions.push(Action::Debug(std::mem::take(input)));
                }
                ui.set_keyboard_focus_here();
            }
        });
}

fn stats_window(ui: &imgui::Ui, stats: Stats) {
    ui.window("Statistics")
        .size([220.0, 92.0], imgui::Condition::Always)
        .position([12.0, 40.0], imgui::Condition::FirstUseEver)
        .resizable(false)
        .collapsible(false)
        .build(|| {
            ui.text(format!("Display   {:6.1} fps", stats.video_fps));
            ui.text(format!("Emulated  {:6.1} fps", stats.emulated_fps));
            ui.text(format!("Layers    {:6.2} ms", stats.render_ms));
        });
}

/// A dark, slightly rounded theme, sized for a window that is usually showing a
/// 4:3 image behind it.
fn style(context: &mut imgui::Context) {
    let style = context.style_mut();
    style.window_rounding = 4.0;
    style.frame_rounding = 3.0;
    style.grab_rounding = 3.0;
    style.window_padding = [10.0, 10.0];
    style.item_spacing = [8.0, 6.0];
    style.window_border_size = 0.0;
    style.colors[imgui::StyleColor::WindowBg as usize] = [0.07, 0.08, 0.10, 0.94];
    style.colors[imgui::StyleColor::TitleBgActive as usize] = [0.16, 0.29, 0.48, 1.0];
    style.colors[imgui::StyleColor::Header as usize] = [0.20, 0.36, 0.58, 0.80];
    style.colors[imgui::StyleColor::HeaderHovered as usize] = [0.26, 0.46, 0.72, 0.90];
    style.colors[imgui::StyleColor::Button as usize] = [0.20, 0.30, 0.44, 1.0];
    style.colors[imgui::StyleColor::ButtonHovered as usize] = [0.28, 0.42, 0.62, 1.0];
}
