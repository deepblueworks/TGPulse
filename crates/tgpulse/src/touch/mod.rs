//! Touch input: the on-screen controls, and the menu built for a phone.
//!
//! A handset has no keyboard, no mouse and no cabinet, so both halves of the
//! interface have to be rebuilt around a finger. What is drawn over a running
//! game depends on the machine -- a racer wants a wheel and pedals, a fighter
//! wants a stick and three buttons, a lightgun game wants no controls at all
//! because the screen itself is the gun -- and that lives in `overlay`. What
//! replaces the desktop's ImGui windows lives in `menu`.
//!
//! Both are drawn through ImGui's *draw list* rather than its widgets: the
//! renderer already flattens draw lists into wgpu vertex buffers and already
//! copes with Android taking the surface away, so this reuses that path and
//! introduces no second one. No tap ever reaches ImGui. Hit testing is done
//! here against layouts computed from the window size, because touches arrive
//! between frames and cannot wait for one to be laid out.

mod menu;
mod overlay;

use winit::event::TouchPhase;

use tgpulse_core::config::Config;
use tgpulse_core::library::Entry;

use crate::bindings::Control;
use crate::gui::Action;
use crate::input::ControlScheme;

pub use menu::Menu;
pub use overlay::Overlay;

/// A round control, in physical pixels.
#[derive(Clone, Copy)]
pub(crate) struct Disc {
    pub x: f32,
    pub y: f32,
    pub r: f32,
}

impl Disc {
    fn contains(&self, x: f32, y: f32, slack: f32) -> bool {
        let (dx, dy) = (x - self.x, y - self.y);
        dx * dx + dy * dy <= (self.r + slack) * (self.r + slack)
    }
}

/// An axis-aligned box, in physical pixels.
#[derive(Clone, Copy, Default)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

// The controls sit over a game, so they are drawn as outlines that let the
// picture through rather than as opaque furniture.
const FILL_IDLE: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
const FILL_DOWN: [f32; 4] = [0.36, 0.66, 1.0, 0.45];
const EDGE_IDLE: [f32; 4] = [1.0, 1.0, 1.0, 0.45];
const EDGE_DOWN: [f32; 4] = [0.70, 0.88, 1.0, 0.95];
pub(crate) const TEXT: [f32; 4] = [1.0, 1.0, 1.0, 0.90];
pub(crate) const TEXT_DIM: [f32; 4] = [1.0, 1.0, 1.0, 0.55];

// ImGui allows one live draw list at a time, so every one of these takes the
// caller's rather than reaching for its own: a helper that fetched one would
// panic the moment it were used inside `with_clip_rect`.

pub(crate) fn draw_disc(
    ui: &imgui::Ui,
    dl: &imgui::DrawListMut<'_>,
    disc: Disc,
    down: bool,
    label: &str,
) {
    let (fill, edge) = if down {
        (FILL_DOWN, EDGE_DOWN)
    } else {
        (FILL_IDLE, EDGE_IDLE)
    };
    dl.add_circle([disc.x, disc.y], disc.r, fill)
        .filled(true)
        .num_segments(36)
        .build();
    dl.add_circle([disc.x, disc.y], disc.r, edge)
        .thickness(2.0)
        .num_segments(36)
        .build();
    if !label.is_empty() {
        centred_text(ui, dl, disc.x, disc.y, label, TEXT);
    }
}

pub(crate) fn draw_rect(dl: &imgui::DrawListMut<'_>, rect: Rect, down: bool, radius: f32) {
    let (fill, edge) = if down {
        (FILL_DOWN, EDGE_DOWN)
    } else {
        (FILL_IDLE, EDGE_IDLE)
    };
    let max = [rect.x + rect.w, rect.y + rect.h];
    dl.add_rect([rect.x, rect.y], max, fill)
        .filled(true)
        .rounding(radius)
        .build();
    dl.add_rect([rect.x, rect.y], max, edge)
        .thickness(2.0)
        .rounding(radius)
        .build();
}

pub(crate) fn centred_text(
    ui: &imgui::Ui,
    dl: &imgui::DrawListMut<'_>,
    cx: f32,
    cy: f32,
    text: &str,
    colour: [f32; 4],
) {
    let size = ui.calc_text_size(text);
    dl.add_text([cx - size[0] * 0.5, cy - size[1] * 0.5], colour, text);
}

pub(crate) fn text_at(
    dl: &imgui::DrawListMut<'_>,
    x: f32,
    y: f32,
    text: &str,
    colour: [f32; 4],
) {
    dl.add_text([x, y], colour, text);
}

/// A finger resting on a control: which one, where it landed, and where it has
/// since been dragged to. A stick needs all three to work out its deflection.
pub(crate) type Held = (usize, (f32, f32), (f32, f32));

/// A finger that is down, and what it is driving.
struct Pointer {
    id: u64,
    target: Target,
    start: (f32, f32),
    pos: (f32, f32),
    /// Set once the finger has travelled far enough to be a drag rather than a
    /// tap, so releasing it no longer counts as pressing what it started on.
    dragged: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Target {
    /// A control on the in-game overlay, by index.
    Control(usize),
    /// The picture itself, aiming a lightgun.
    Aim,
    /// A button on the menu.
    MenuButton(menu::Button),
    /// A finger in the game list: it scrolls the list, and if it never moved
    /// it launches whatever row it landed on.
    MenuList { row: Option<usize>, from: f32 },
    /// Dead space.
    None,
}

/// Everything the touchscreen drives.
pub struct TouchUi {
    /// Off on desktop, where there is a mouse and a keyboard.
    enabled: bool,
    size: (f32, f32),
    /// Where the picture sits inside the window, so an aiming touch can be
    /// expressed as a fraction of the image rather than of the screen.
    view: (f32, f32, f32, f32),
    overlay: Overlay,
    menu: Menu,
    pointers: Vec<Pointer>,
    /// Recomputed whenever a finger moves; what the machine reads.
    amounts: Vec<(Control, f32)>,
    aim: (f32, f32),
    firing: bool,
}

impl Default for TouchUi {
    fn default() -> Self {
        Self::new()
    }
}

impl TouchUi {
    pub fn new() -> Self {
        Self {
            enabled: false,
            size: (0.0, 0.0),
            view: (0.0, 0.0, 1.0, 1.0),
            overlay: Overlay::new(),
            menu: Menu::new(),
            pointers: Vec::new(),
            amounts: Vec::new(),
            aim: (0.5, 0.5),
            firing: false,
        }
    }

    /// Whether the touch interface replaces the desktop one. Android turns
    /// this on; everywhere else it stays off and nothing here runs.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.release_all();
            self.relayout();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn resize(&mut self, width: f32, height: f32) {
        if self.size != (width, height) {
            self.size = (width, height);
            self.release_all();
            self.relayout();
        }
    }

    /// Where the emulated picture is drawn, for turning an aiming touch into a
    /// position on the image.
    pub fn set_view(&mut self, view: (f32, f32, f32, f32)) {
        self.view = view;
    }

    /// The control layout follows the cabinet, so it changes with the game.
    pub fn set_scheme(&mut self, scheme: Option<ControlScheme>) {
        if self.overlay.scheme() != scheme {
            self.overlay.set_scheme(scheme);
            self.release_all();
            self.relayout();
        }
        self.menu.set_running(scheme.is_some());
    }

    pub fn set_menu_open(&mut self, open: bool) {
        if self.menu.open() != open {
            self.menu.set_open(open);
            self.release_all();
        }
    }

    fn relayout(&mut self) {
        let (w, h) = self.size;
        if self.enabled && w > 0.0 && h > 0.0 {
            self.overlay.layout(w, h);
            self.menu.layout(w, h);
        }
    }

    fn release_all(&mut self) {
        self.pointers.clear();
        self.amounts.clear();
        self.firing = false;
    }

    /// What the machine's controls read from the screen, as amounts in 0..1.
    pub fn amounts(&self) -> &[(Control, f32)] {
        &self.amounts
    }

    /// The lightgun aim, as a fraction of the picture, while a finger is on it.
    pub fn aim(&self) -> Option<(f32, f32)> {
        self.firing.then_some(self.aim)
    }

    /// One finger's worth of news.
    pub fn on_touch(&mut self, id: u64, phase: TouchPhase, x: f32, y: f32) {
        if !self.enabled {
            return;
        }
        match phase {
            TouchPhase::Started => {
                let target = self.pick(x, y);
                // Buttons act on the press: a control that waits for the
                // release feels broken, and a machine control has to go down
                // the instant the thumb lands.
                match target {
                    Target::MenuButton(button) => self.menu.press(button),
                    Target::Control(index) => self.overlay.press(index),
                    _ => {}
                }
                self.pointers.push(Pointer {
                    id,
                    target,
                    start: (x, y),
                    pos: (x, y),
                    dragged: false,
                });
            }
            TouchPhase::Moved => {
                // The drag threshold is a fraction of the screen rather than a
                // pixel count, so it means the same at any density.
                let slop = self.size.1 * 0.02;
                let Some(pointer) = self.pointers.iter_mut().find(|p| p.id == id) else {
                    return;
                };
                pointer.pos = (x, y);
                if (x - pointer.start.0).abs() > slop || (y - pointer.start.1).abs() > slop {
                    pointer.dragged = true;
                }
                if let Target::MenuList { from, .. } = pointer.target {
                    let travelled = pointer.start.1 - y;
                    self.menu.set_scroll(from + travelled);
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(index) = self.pointers.iter().position(|p| p.id == id) else {
                    return;
                };
                let pointer = self.pointers.remove(index);
                // A row launches a game only if the finger stayed put: the
                // gesture that scrolls the list must not also start something.
                if phase == TouchPhase::Ended && !pointer.dragged {
                    if let Target::MenuList { row: Some(row), .. } = pointer.target {
                        self.menu.press_row(row);
                    }
                }
            }
        }
        self.recompute();
    }

    /// Which thing the finger landed on. The menu takes the whole screen while
    /// it is up, so the controls underneath cannot be pressed through it.
    fn pick(&self, x: f32, y: f32) -> Target {
        if self.menu.open() {
            return self.menu.pick(x, y);
        }
        if let Some(index) = self.overlay.pick(x, y) {
            return Target::Control(index);
        }
        // On a lightgun cabinet the picture is the gun, so a finger anywhere
        // the controls do not claim is an aim and a trigger pull.
        if self.overlay.scheme() == Some(ControlScheme::Gun) {
            return Target::Aim;
        }
        Target::None
    }

    /// Folds every finger down into one set of control amounts.
    fn recompute(&mut self) {
        let mut amounts: Vec<(Control, f32)> = Vec::new();
        let mut firing = false;
        let mut aim = self.aim;

        for pointer in &self.pointers {
            match pointer.target {
                Target::Control(index) => {
                    self.overlay
                        .amounts(index, pointer.start, pointer.pos, &mut |control, amount| {
                            if amount <= 0.0 {
                                return;
                            }
                            match amounts.iter_mut().find(|(c, _)| *c == control) {
                                // Two fingers on controls sharing a binding
                                // read as the firmer of the two, not the sum.
                                Some((_, held)) => *held = held.max(amount),
                                None => amounts.push((control, amount)),
                            }
                        });
                }
                Target::Aim => {
                    let (vx, vy, vw, vh) = self.view;
                    aim = (
                        ((pointer.pos.0 - vx) / vw).clamp(0.0, 1.0),
                        ((pointer.pos.1 - vy) / vh).clamp(0.0, 1.0),
                    );
                    firing = true;
                }
                _ => {}
            }
        }

        self.amounts = amounts;
        self.aim = aim;
        self.firing = firing;
    }

    /// Draws whichever half is in front, and resolves anything the player
    /// pressed since the last frame.
    pub fn render(
        &mut self,
        ui: &imgui::Ui,
        entries: &[Entry],
        running: Option<&str>,
        config: &mut Config,
        slot: &mut u32,
    ) -> Vec<Action> {
        if !self.enabled {
            return Vec::new();
        }
        if self.menu.open() {
            return self.menu.render(ui, entries, running, config, slot);
        }

        let held: Vec<Held> = self
            .pointers
            .iter()
            .filter_map(|p| match p.target {
                Target::Control(index) => Some((index, p.start, p.pos)),
                _ => None,
            })
            .collect();
        {
            let dl = ui.get_background_draw_list();
            self.overlay.render(ui, &dl, &held);
        }

        // The overlay's own buttons -- the menu key, and the one that folds the
        // controls away for a player holding a pad -- are answered here rather
        // than by the application, which has no idea they exist.
        for press in self.overlay.take_actions() {
            match press {
                overlay::Press::Menu => self.set_menu_open(true),
                overlay::Press::ToggleControls => {
                    self.overlay.toggle_collapsed();
                    let (w, h) = self.size;
                    self.overlay.layout(w, h);
                    // Folding the controls away renumbers them, so anything
                    // still held would be driving the wrong widget.
                    self.release_all();
                }
            }
        }
        Vec::new()
    }
}
