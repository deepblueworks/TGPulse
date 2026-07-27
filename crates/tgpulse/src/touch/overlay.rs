//! The controls drawn over a running game.
//!
//! Each cabinet gets the controls it actually had. The layout is arithmetic on
//! the window size rather than a table of pixel positions, so it lands in the
//! same place on a phone and on a tablet, and everything is sized from the
//! short edge so a control stays the size of a thumb whatever the aspect.
//!
//! Nothing here knows about the I/O board. A touch resolves to `Control`
//! amounts -- the same abstract controls a pad or a keyboard drives -- so
//! every control scheme works through the screen without knowing that touch
//! exists.

use super::{centred_text, draw_disc, Disc, Held, TEXT_DIM};
use crate::bindings::Control;
use crate::input::ControlScheme;

/// An overlay button that drives the emulator rather than the machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Press {
    /// Bring the menu up.
    Menu,
    /// Fold the controls away, for a player holding a pad.
    ToggleControls,
}

enum Kind {
    Button(Control),
    /// A thumbstick. It recentres wherever the thumb lands, which is what
    /// makes a stick with no rim to feel for usable at all; `vertical` is
    /// absent on a wheel, which only turns.
    Stick {
        left: Control,
        right: Control,
        vertical: Option<(Control, Control)>,
    },
    Emulator(Press),
}

struct Widget {
    kind: Kind,
    label: &'static str,
    disc: Disc,
    /// Extra reach beyond the drawn edge. A stick is given a lot of it so a
    /// thumb landing near the base still takes it.
    slack: f32,
}

pub struct Overlay {
    scheme: Option<ControlScheme>,
    collapsed: bool,
    widgets: Vec<Widget>,
    actions: Vec<Press>,
}

impl Overlay {
    pub fn new() -> Self {
        Self {
            scheme: None,
            collapsed: false,
            widgets: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub fn scheme(&self) -> Option<ControlScheme> {
        self.scheme
    }

    pub fn set_scheme(&mut self, scheme: Option<ControlScheme>) {
        self.scheme = scheme;
    }

    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn take_actions(&mut self) -> Vec<Press> {
        std::mem::take(&mut self.actions)
    }

    /// Records a press on an emulator button. Machine controls are read from
    /// the fingers that are down, so only these need remembering.
    pub fn press(&mut self, index: usize) {
        if let Some(Widget {
            kind: Kind::Emulator(press),
            ..
        }) = self.widgets.get(index)
        {
            self.actions.push(*press);
        }
    }

    /// The control nearest the finger, of those it landed on.
    pub fn pick(&self, x: f32, y: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (index, widget) in self.widgets.iter().enumerate() {
            if !widget.disc.contains(x, y, widget.slack) {
                continue;
            }
            let (dx, dy) = (x - widget.disc.x, y - widget.disc.y);
            let distance = dx * dx + dy * dy;
            if best.is_none_or(|(_, best)| distance < best) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    /// What one finger on `index` is asking the machine for.
    pub fn amounts(
        &self,
        index: usize,
        start: (f32, f32),
        pos: (f32, f32),
        push: &mut dyn FnMut(Control, f32),
    ) {
        let Some(widget) = self.widgets.get(index) else {
            return;
        };
        match &widget.kind {
            Kind::Button(control) => push(*control, 1.0),
            Kind::Stick {
                left,
                right,
                vertical,
            } => {
                let r = widget.disc.r;
                let mut dx = (pos.0 - start.0) / r;
                let mut dy = (pos.1 - start.1) / r;
                match vertical {
                    // Two axes travel as one vector, so a diagonal is not
                    // allowed to reach further than a straight push.
                    Some((up, down)) => {
                        let length = (dx * dx + dy * dy).sqrt();
                        if length > 1.0 {
                            dx /= length;
                            dy /= length;
                        }
                        if dy < 0.0 {
                            push(*up, -dy);
                        } else {
                            push(*down, dy);
                        }
                    }
                    // A wheel ignores how far up or down the thumb has drifted;
                    // clamping the pair would make it turn less for it.
                    None => dx = dx.clamp(-1.0, 1.0),
                }
                if dx < 0.0 {
                    push(*left, -dx);
                } else {
                    push(*right, dx);
                }
            }
            Kind::Emulator(_) => {}
        }
    }

    /// `held` is every finger currently on a control, with where it landed and
    /// where it has been dragged to.
    pub fn render(
        &self,
        ui: &imgui::Ui,
        dl: &imgui::DrawListMut<'_>,
        held: &[Held],
    ) {
        for (index, widget) in self.widgets.iter().enumerate() {
            let finger = held.iter().find(|(i, _, _)| *i == index);
            let down = finger.is_some();
            match widget.kind {
                Kind::Stick { .. } => {
                    // While held the base follows the thumb, so the player can
                    // see where the stick has been recentred to.
                    let base = finger.map_or((widget.disc.x, widget.disc.y), |(_, start, _)| *start);
                    draw_disc(
                        ui,
                        dl,
                        Disc {
                            x: base.0,
                            y: base.1,
                            r: widget.disc.r,
                        },
                        false,
                        "",
                    );
                    let knob = finger.map_or(base, |(_, start, pos)| {
                        let r = widget.disc.r;
                        let (mut dx, mut dy) = (pos.0 - start.0, pos.1 - start.1);
                        let length = (dx * dx + dy * dy).sqrt();
                        if length > r {
                            dx = dx / length * r;
                            dy = dy / length * r;
                        }
                        (start.0 + dx, start.1 + dy)
                    });
                    draw_disc(
                        ui,
                        dl,
                        Disc {
                            x: knob.0,
                            y: knob.1,
                            r: widget.disc.r * 0.45,
                        },
                        down,
                        "",
                    );
                    if !widget.label.is_empty() && finger.is_none() {
                        centred_text(
                            ui,
                            dl,
                            widget.disc.x,
                            widget.disc.y + widget.disc.r * 0.62,
                            widget.label,
                            TEXT_DIM,
                        );
                    }
                }
                _ => draw_disc(ui, dl, widget.disc, down, widget.label),
            }
        }
    }

    /// Places the controls for the loaded cabinet.
    pub fn layout(&mut self, w: f32, h: f32) {
        self.widgets.clear();

        // Sized from the short edge: a thumb is the same size whatever the
        // screen is, so the controls have to be too.
        let unit = h;
        let margin = 0.045 * unit;
        let small = 0.042 * unit;
        let button = 0.082 * unit;
        let stick = 0.125 * unit;

        let mut disc = |x: f32, y: f32, r: f32, kind: Kind, label: &'static str, slack: f32| {
            self.widgets.push(Widget {
                kind,
                label,
                disc: Disc { x, y, r },
                slack,
            });
        };

        // The menu key is always reachable: it is the only way back to the
        // library, and on a phone there is no F1 to fall back on.
        disc(
            margin + small,
            margin + small,
            small,
            Kind::Emulator(Press::Menu),
            "MENU",
            0.0,
        );
        disc(
            margin + small * 3.4,
            margin + small,
            small,
            Kind::Emulator(Press::ToggleControls),
            if self.collapsed { "SHOW" } else { "HIDE" },
            0.0,
        );

        let Some(scheme) = self.scheme else {
            return;
        };
        if self.collapsed {
            return;
        }

        disc(
            w - margin - small,
            margin + small,
            small,
            Kind::Button(Control::Start1),
            "START",
            0.0,
        );
        disc(
            w - margin - small * 3.4,
            margin + small,
            small,
            Kind::Button(Control::Coin1),
            "COIN",
            0.0,
        );

        // The action buttons sit in a diamond, in the order the I/O board
        // reports them and in the same places the pad bindings use.
        let offset = button * 1.7;
        let (cx, cy) = (
            w - margin - button - offset,
            h - margin - button - offset,
        );
        let diamond = |this: &mut Self, controls: &[(Control, &'static str)]| {
            const PLACES: [(f32, f32); 4] = [(-1.0, 0.0), (0.0, 1.0), (1.0, 0.0), (0.0, -1.0)];
            for ((control, label), (px, py)) in controls.iter().zip(PLACES) {
                this.widgets.push(Widget {
                    kind: Kind::Button(*control),
                    label,
                    disc: Disc {
                        x: cx + px * offset,
                        y: cy + py * offset,
                        r: button,
                    },
                    slack: 0.0,
                });
            }
        };

        let stick_at = (margin + stick, h - margin - stick);
        match scheme {
            // A wheel and two pedals, with the shifter on the buttons above
            // them -- the same compromise the pad mapping makes.
            ControlScheme::Racing | ControlScheme::Bike => {
                self.widgets.push(Widget {
                    kind: Kind::Stick {
                        left: Control::SteerLeft,
                        right: Control::SteerRight,
                        vertical: None,
                    },
                    label: "STEER",
                    disc: Disc {
                        x: stick_at.0,
                        y: stick_at.1,
                        r: stick,
                    },
                    slack: stick * 0.6,
                });
                let mut disc = |x, y, r, kind, label| {
                    self.widgets.push(Widget {
                        kind,
                        label,
                        disc: Disc { x, y, r },
                        slack: 0.0,
                    });
                };
                disc(
                    w - margin - button * 1.2,
                    h - margin - button * 1.2,
                    button * 1.2,
                    Kind::Button(Control::Throttle),
                    "GAS",
                );
                disc(
                    w - margin - button * 3.7,
                    h - margin - button * 1.0,
                    button,
                    Kind::Button(Control::Brake),
                    "BRAKE",
                );
                disc(
                    w - margin - button * 1.2,
                    h - margin - button * 3.8,
                    button * 0.72,
                    Kind::Button(Control::GearUp),
                    "G+",
                );
                disc(
                    w - margin - button * 3.4,
                    h - margin - button * 3.4,
                    button * 0.72,
                    Kind::Button(Control::GearDown),
                    "G-",
                );
                disc(
                    w - margin - small,
                    margin + small * 3.4,
                    small,
                    Kind::Button(Control::ViewRed),
                    "VIEW",
                );
            }
            // An 8-way stick and the attack buttons.
            ControlScheme::Joystick => {
                self.push_stick(stick_at, stick, Control::Left, Control::Right, true, "");
                diamond(
                    self,
                    &[
                        (Control::Button1, "1"),
                        (Control::Button2, "2"),
                        (Control::Button3, "3"),
                        (Control::Button4, "4"),
                    ],
                );
            }
            // The screen is the gun: no stick, and the trigger doubles as a
            // tap anywhere on the picture.
            ControlScheme::Gun => {
                let mut disc = |x, y, r, kind, label| {
                    self.widgets.push(Widget {
                        kind,
                        label,
                        disc: Disc { x, y, r },
                        slack: 0.0,
                    });
                };
                disc(
                    w - margin - button * 1.3,
                    h - margin - button * 1.3,
                    button * 1.3,
                    Kind::Button(Control::Fire),
                    "FIRE",
                );
                disc(
                    w - margin - button * 3.9,
                    h - margin - button * 1.1,
                    button * 1.1,
                    Kind::Button(Control::Reload),
                    "RELOAD",
                );
            }
            // A flight stick, three fire buttons, and a throttle that runs
            // both ways.
            ControlScheme::Flight => {
                self.push_stick(stick_at, stick, Control::Left, Control::Right, true, "");
                diamond(
                    self,
                    &[
                        (Control::Button1, "1"),
                        (Control::Button2, "2"),
                        (Control::Button3, "3"),
                    ],
                );
                let mut disc = |x, y, r, kind, label| {
                    self.widgets.push(Widget {
                        kind,
                        label,
                        disc: Disc { x, y, r },
                        slack: 0.0,
                    });
                };
                disc(
                    margin + stick * 0.55,
                    h - margin - stick * 2.0 - button * 1.1,
                    button * 0.75,
                    Kind::Button(Control::Throttle),
                    "THR+",
                );
                disc(
                    margin + stick * 1.75,
                    h - margin - stick * 2.0 - button * 1.1,
                    button * 0.75,
                    Kind::Button(Control::Brake),
                    "THR-",
                );
            }
            // Handlebars and a throttle lever.
            ControlScheme::Jetski => {
                self.push_stick(
                    stick_at,
                    stick,
                    Control::LeanLeft,
                    Control::LeanRight,
                    false,
                    "LEAN",
                );
                let mut disc = |x, y, r, kind, label| {
                    self.widgets.push(Widget {
                        kind,
                        label,
                        disc: Disc { x, y, r },
                        slack: 0.0,
                    });
                };
                disc(
                    w - margin - button * 1.2,
                    h - margin - button * 1.2,
                    button * 1.2,
                    Kind::Button(Control::Throttle),
                    "GAS",
                );
                disc(
                    w - margin - small,
                    margin + small * 3.4,
                    small,
                    Kind::Button(Control::ViewChange),
                    "VIEW",
                );
            }
            // A footplate: lean in two axes, a couple of buttons, and one
            // pedal per side.
            ControlScheme::Skate | ControlScheme::Ski | ControlScheme::Sled => {
                self.push_stick(
                    stick_at,
                    stick,
                    Control::LeanLeft,
                    Control::LeanRight,
                    true,
                    "LEAN",
                );
                diamond(
                    self,
                    &[
                        (Control::Button1, "1"),
                        (Control::Button2, "2"),
                        (Control::Button3, "3"),
                    ],
                );
                let mut disc = |x, y, r, kind, label| {
                    self.widgets.push(Widget {
                        kind,
                        label,
                        disc: Disc { x, y, r },
                        slack: 0.0,
                    });
                };
                disc(
                    w - margin - small,
                    margin + small * 3.4,
                    small,
                    Kind::Button(Control::Throttle),
                    "R",
                );
                disc(
                    w - margin - small * 3.4,
                    margin + small * 3.4,
                    small,
                    Kind::Button(Control::Brake),
                    "L",
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_stick(
        &mut self,
        at: (f32, f32),
        r: f32,
        left: Control,
        right: Control,
        vertical: bool,
        label: &'static str,
    ) {
        self.widgets.push(Widget {
            kind: Kind::Stick {
                left,
                right,
                vertical: vertical.then_some((Control::Up, Control::Down)),
            },
            label,
            disc: Disc {
                x: at.0,
                y: at.1,
                r,
            },
            slack: r * 0.6,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A landscape handset, which is what the activity forces.
    const SCREEN: (f32, f32) = (2400.0, 1080.0);

    /// Every cabinet, so a new scheme cannot be added without a layout.
    const SCHEMES: [ControlScheme; 8] = [
        ControlScheme::Racing,
        ControlScheme::Bike,
        ControlScheme::Joystick,
        ControlScheme::Gun,
        ControlScheme::Flight,
        ControlScheme::Jetski,
        ControlScheme::Skate,
        ControlScheme::Ski,
    ];

    fn laid_out(scheme: ControlScheme) -> Overlay {
        let mut overlay = Overlay::new();
        overlay.set_scheme(Some(scheme));
        overlay.layout(SCREEN.0, SCREEN.1);
        overlay
    }

    /// Two controls drawn on top of each other would leave one of them
    /// permanently unreachable, and there is no way to see that without a
    /// handset to look at.
    #[test]
    fn no_two_controls_overlap() {
        for scheme in SCHEMES {
            let overlay = laid_out(scheme);
            for (index, a) in overlay.widgets.iter().enumerate() {
                for b in overlay.widgets.iter().skip(index + 1) {
                    let (dx, dy) = (a.disc.x - b.disc.x, a.disc.y - b.disc.y);
                    let apart = (dx * dx + dy * dy).sqrt();
                    assert!(
                        apart >= a.disc.r + b.disc.r,
                        "{scheme:?}: {} and {} overlap ({apart:.1} apart, {:.1} needed)",
                        a.label,
                        b.label,
                        a.disc.r + b.disc.r,
                    );
                }
            }
        }
    }

    /// A control drawn off the edge is a control that cannot be pressed.
    #[test]
    fn every_control_is_on_screen() {
        for scheme in SCHEMES {
            let overlay = laid_out(scheme);
            for widget in &overlay.widgets {
                let disc = widget.disc;
                assert!(
                    disc.x - disc.r >= 0.0
                        && disc.y - disc.r >= 0.0
                        && disc.x + disc.r <= SCREEN.0
                        && disc.y + disc.r <= SCREEN.1,
                    "{scheme:?}: {} runs off the screen",
                    widget.label,
                );
            }
        }
    }

    /// Every scheme has to offer a way to put a coin in and a way back to the
    /// menu, or the game is unplayable however good the rest of it is.
    #[test]
    fn every_layout_can_reach_the_menu_and_the_coin_slot() {
        for scheme in SCHEMES {
            let overlay = laid_out(scheme);
            assert!(
                overlay.widgets.iter().any(|w| matches!(
                    w.kind,
                    Kind::Emulator(Press::Menu)
                )),
                "{scheme:?} has no menu key",
            );
            for control in [Control::Coin1, Control::Start1] {
                assert!(
                    overlay
                        .widgets
                        .iter()
                        .any(|w| matches!(w.kind, Kind::Button(c) if c == control)),
                    "{scheme:?} has no {control:?}",
                );
            }
        }
    }

    /// Folded away, the controls have to stop taking presses -- otherwise a
    /// player using a pad still cannot touch the picture.
    #[test]
    fn collapsing_leaves_only_the_emulator_keys() {
        let mut overlay = laid_out(ControlScheme::Joystick);
        overlay.toggle_collapsed();
        overlay.layout(SCREEN.0, SCREEN.1);
        assert!(overlay
            .widgets
            .iter()
            .all(|w| matches!(w.kind, Kind::Emulator(_))));
    }

    fn amount_of(overlay: &Overlay, index: usize, from: (f32, f32), to: (f32, f32)) -> Vec<(Control, f32)> {
        let mut out = Vec::new();
        overlay.amounts(index, from, to, &mut |control, amount| out.push((control, amount)));
        out
    }

    /// The stick is what makes an analog cabinet playable by thumb: half its
    /// travel has to read as half, not as fully pressed.
    #[test]
    fn a_stick_reports_partial_travel() {
        let overlay = laid_out(ControlScheme::Joystick);
        let index = overlay
            .widgets
            .iter()
            .position(|w| matches!(w.kind, Kind::Stick { .. }))
            .expect("a joystick cabinet has a stick");
        let r = overlay.widgets[index].disc.r;
        let at = (overlay.widgets[index].disc.x, overlay.widgets[index].disc.y);

        let half = amount_of(&overlay, index, at, (at.0 + r * 0.5, at.1));
        let right = half.iter().find(|(c, _)| *c == Control::Right).expect("right");
        assert!((right.1 - 0.5).abs() < 0.01, "half travel read as {}", right.1);

        // Dragged past the rim it saturates rather than running away.
        let past = amount_of(&overlay, index, at, (at.0 + r * 4.0, at.1));
        let right = past.iter().find(|(c, _)| *c == Control::Right).expect("right");
        assert!((right.1 - 1.0).abs() < 0.01, "over-travel read as {}", right.1);
    }

    /// A wheel only turns. A thumb that drifts up the screen while steering
    /// must not steer any less for it, and must not invent a vertical control
    /// the cabinet does not have.
    #[test]
    fn a_wheel_ignores_vertical_drift() {
        let overlay = laid_out(ControlScheme::Racing);
        let index = overlay
            .widgets
            .iter()
            .position(|w| matches!(w.kind, Kind::Stick { .. }))
            .expect("a racing cabinet has a wheel");
        let r = overlay.widgets[index].disc.r;
        let at = (overlay.widgets[index].disc.x, overlay.widgets[index].disc.y);

        let drifted = amount_of(&overlay, index, at, (at.0 + r * 0.5, at.1 - r * 0.9));
        let steer = drifted
            .iter()
            .find(|(c, _)| *c == Control::SteerRight)
            .expect("steer");
        assert!((steer.1 - 0.5).abs() < 0.01, "drift changed the lock to {}", steer.1);
        assert!(
            !drifted
                .iter()
                .any(|(c, _)| matches!(c, Control::Up | Control::Down)),
            "a wheel produced a vertical control",
        );
    }
}
