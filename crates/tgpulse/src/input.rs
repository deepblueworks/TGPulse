//! Controller and keyboard mapping onto the Model 1 I/O board's inputs.
//!
//! Daytona is a dedicated cabinet: a 270-degree wheel, two pedals, a 4-speed
//! H-pattern shifter and four coloured view buttons. None of that maps onto a
//! pad one-for-one, so what follows is the usual console-conversion compromise:
//! the wheel becomes the left stick, the pedals become the triggers, and the
//! H-pattern becomes a sequential shifter on the shoulder buttons.
//!
//! The analog ranges are the hardware's, not 0..255: the reference port definitions
//! give steering, throttle and brake a travel of 0x20..0xe0, centred at 0x80 for
//! the wheel and resting at 0x20 for both pedals.

use std::collections::HashSet;

use gilrs::ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Repeat, Replay, Ticks};
use winit::keyboard::KeyCode;

use tgpulse_core::config::Inputs;

use crate::bindings::{Bindings, Control, Sign, Source};

// --- Drive board command encoding -------------------------------------------
//
// Recovered by disassembling the drive board's own Z80 ROM (epr-16488a): the
// dispatch at 0x0520 debounces the byte, then 0x0328 selects the effect from
// `cmd & 0xf8` while 0x04c0 takes `cmd & 0x07` as the magnitude and scales the
// motor target with it. `0x10` lands on 0x04d4, which zeroes the target -- no
// force.
//
// The cabinet drives a torque motor on the wheel; a pad has two eccentric
// motors and no wheel to push. So the magnitude carries over honestly and the
// effect kind cannot: what is reproduced here is "how hard", not "which way".
/// Mask selecting the effect kind.
const DRIVE_KIND: u8 = 0xf8;
/// Mask selecting the force magnitude, 0..7.
const DRIVE_MAGNITUDE: u8 = 0x07;
/// Effect kind that means "no force".
const DRIVE_KIND_OFF: u8 = 0x10;
/// Only this range reaches the motor. The dispatch peels off `0x0x`, `0x7x`,
/// `0x8x` and `0xfx` before that -- they are init and mode traffic, not force,
/// and reading a magnitude out of them would rumble on handshakes.
const DRIVE_FORCE_FIRST: u8 = 0x10;
const DRIVE_FORCE_LAST: u8 = 0x6f;

/// Travel limits of the I/O board's ADC channels.
const ANALOG_MIN: i32 = 0x20;
const ANALOG_MAX: i32 = 0xe0;
/// Wheel centre.
const STEER_CENTRE: i32 = 0x80;

/// Per-frame travel when an axis is driven from a key rather than a stick.
/// These are the reference values for this game.
const STEER_KEYDELTA: i32 = 10;
const PEDAL_KEYDELTA: i32 = 20;

/// Stick deflection below which the wheel is treated as centred.
const STICK_DEADZONE: f32 = 0.15;
/// Trigger travel below which a pedal is treated as released.
const TRIGGER_DEADZONE: f32 = 0.05;

/// Active-low bits of IN0.
const IN0_COIN1: u8 = 0x01;
const IN0_COIN2: u8 = 0x02;
const IN0_TEST: u8 = 0x04;
const IN0_SERVICE: u8 = 0x08;
const IN0_START1: u8 = 0x10;
const IN0_VR1_RED: u8 = 0x20;
const IN0_VR2_BLUE: u8 = 0x40;
const IN0_VR3_YELLOW: u8 = 0x80;
/// Active-low bit of IN1.
const IN1_VR4_GREEN: u8 = 0x01;

// The 8-way joystick + 3-button IN.1 layout, all active-low. Virtua Fighter
// (Model 1) and Virtua Striker (Model 2B) use identical bits; only the button
// names differ -- Guard/Punch/Kick versus Long Pass/Shoot/Short Pass. From
// The reference and `( vstriker )`.
const IN1_JOY_BTN1: u8 = 0x01;
const IN1_JOY_BTN2: u8 = 0x02;
const IN1_JOY_BTN3: u8 = 0x04;
const IN1_JOY_DOWN: u8 = 0x10;
const IN1_JOY_UP: u8 = 0x20;
const IN1_JOY_RIGHT: u8 = 0x40;
const IN1_JOY_LEFT: u8 = 0x80;

// Star Wars Arcade IN.1, active-low. From the reference:
// three fire buttons for player 1, two for player 2.
const IN1_SWA_BTN1: u8 = 0x01; // P1 trigger / primary fire
const IN1_SWA_BTN2: u8 = 0x02; // P1 button 2
const IN1_SWA_BTN3: u8 = 0x10; // P1 button 3

// Star Wars' analog channels are two-axis flight sticks rather than a wheel with an 0x80 centre. The I/O
// board reads them on the same ADC channels 0/1/2 the racers use for
// wheel/accel/brake, so they travel out through `steer`/`accel`/`brake`:
// channel 0 = stick X, 1 = stick Y, 2 = throttle.
const SWA_CENTRE: i32 = 0x7f;
const SWA_STICK_RANGE: i32 = 100; // half-travel -> 0x1b..0xe3

// Virtua Cop lightgun. The gun trigger and the on-board start pump IN.1 bit 0
// (P1) / bit 1 (P2). The ADC coordinates span the cabinet's travel limits for P1_X and
// P1_Y; the front end maps the mouse across the window into these.
const IN1_VCOP_TRIGGER: u8 = 0x01;
const GUN_X_MIN: i32 = 0x083;
const GUN_X_MAX: i32 = 0x276;
const GUN_Y_MIN: i32 = 0x024;
const GUN_Y_MAX: i32 = 0x1a9;

/// Highest selectable gear; 0 is neutral.
const TOP_GEAR: usize = 4;

pub struct InputState {
    gilrs: Option<gilrs::Gilrs>,
    /// A single always-running rumble effect whose gain we scale, rather than
    /// rebuilding an effect every time the game changes force.
    rumble: Option<Effect>,
    rumble_gain: f32,
    rumble_enabled: bool,
    keys: HashSet<KeyCode>,
    /// 0 = neutral, 1..4 = gears.
    gear: usize,
    /// Shoulder buttons shift once per press, not once per frame held.
    shift_up_held: bool,
    shift_down_held: bool,
    /// Current analog positions, retained between frames so key-driven axes can
    /// travel gradually the way a real pedal or wheel does.
    steer: i32,
    accel: i32,
    brake: i32,
    /// Mouse cursor as a fraction of the render area, [0, 1] left-to-right and
    /// top-to-bottom. Drives the lightgun aim.
    cursor: (f32, f32),
    /// Left mouse button (fire) and right (reload / point off-screen).
    mouse_fire: bool,
    mouse_reload: bool,
    /// Which game's control layout `poll` emits. The IN.1 bits and analog
    /// channels mean entirely different things between a racer, Virtua
    /// the joystick games' stick + buttons, Star Wars' flight sticks, and Virtua
    /// Cop's lightgun.
    scheme: ControlScheme,
    /// The ADC channel wiring of the loaded cabinet.
    analog_roles: [AnalogRole; 8],
    /// What the player has bound each control to.
    bindings: Bindings,
    /// What the on-screen controls are asking for, as amounts in 0..1. These
    /// sit alongside the bound sources rather than inside them: a thumb is not
    /// a key or a pad button, and the player never bound it to anything.
    touch: Vec<(Control, f32)>,
    /// A pad the platform reports for itself.
    external: ExternalPad,
}

/// A gamepad the platform hands over directly.
///
/// gilrs has no Android backend, so on a handset a controller arrives as
/// activity key events instead of through the library. Those are translated
/// into the same `gilrs::Button` and `gilrs::Axis` values a desktop pad
/// produces, which means the bindings -- including anything the player has
/// rebound -- resolve identically on both.
#[derive(Default)]
struct ExternalPad {
    buttons: HashSet<gilrs::Button>,
    left_x: f32,
    left_y: f32,
    /// Set once anything has been heard from the pad. Without it an absent
    /// controller would look like one resting perfectly at centre, and the
    /// analog paths would believe it.
    present: bool,
}

impl ExternalPad {
    fn axis(&self, axis: gilrs::Axis) -> f32 {
        match axis {
            gilrs::Axis::LeftStickX => self.left_x,
            gilrs::Axis::LeftStickY => self.left_y,
            _ => 0.0,
        }
    }
}

pub use tgpulse_core::roms_db::{AnalogRole, Scheme as ControlScheme};

/// Every physical axis a Model 1/2 cabinet can present, in the I/O chip's
/// 0..0xff ADC units. A scheme fills the ones its cabinet has; `scatter` then
/// places them on the channels that game's board actually wires them to.
#[derive(Clone, Copy)]
pub struct Axes {
    pub steer: u8,
    pub accel: u8,
    pub brake: u8,
    pub throttle: u8,
    pub stickx: u8,
    pub sticky: u8,
    pub stick2x: u8,
    pub stick2y: u8,
    pub gun1x: u8,
    pub gun1y: u8,
    pub gun2x: u8,
    pub gun2y: u8,
    pub roll: u8,
    pub pitch: u8,
    pub slide: u8,
    pub curving: u8,
    pub swing: u8,
    pub incline: u8,
    pub bat1: u8,
    pub bat2: u8,
    pub p1r: u8,
    pub p1l: u8,
    pub p2r: u8,
    pub p2l: u8,
}

impl Default for Axes {
    /// Everything centred: an untouched cabinet reads mid-scale on every
    /// channel, which is also what the cabinet's resting positions give.
    fn default() -> Self {
        Self {
            steer: 0x80,
            accel: 0x00,
            brake: 0x00,
            throttle: 0x00,
            stickx: 0x80,
            sticky: 0x80,
            stick2x: 0x80,
            stick2y: 0x80,
            gun1x: 0x80,
            gun1y: 0x80,
            gun2x: 0x80,
            gun2y: 0x80,
            roll: 0x80,
            pitch: 0x80,
            slide: 0x80,
            curving: 0x80,
            swing: 0x80,
            incline: 0x80,
            bat1: 0x00,
            bat2: 0x00,
            p1r: 0x00,
            p1l: 0x00,
            p2r: 0x00,
            p2l: 0x00,
        }
    }
}

impl Axes {
    fn by_role(&self, role: AnalogRole) -> u8 {
        use AnalogRole::*;
        match role {
            None => 0xff,
            Steer => self.steer,
            Accel => self.accel,
            Brake => self.brake,
            Throttle => self.throttle,
            StickX => self.stickx,
            StickY => self.sticky,
            Stick2X => self.stick2x,
            Stick2Y => self.stick2y,
            Gun1X => self.gun1x,
            Gun1Y => self.gun1y,
            Gun2X => self.gun2x,
            Gun2Y => self.gun2y,
            Roll => self.roll,
            Pitch => self.pitch,
            Slide => self.slide,
            Curving => self.curving,
            Swing => self.swing,
            Incline => self.incline,
            Bat1 => self.bat1,
            Bat2 => self.bat2,
            P1R => self.p1r,
            P1L => self.p1l,
            P2R => self.p2r,
            P2L => self.p2l,
        }
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputState {
    pub fn new() -> Self {
        let mut gilrs = match gilrs::Gilrs::new() {
            Ok(g) => {
                for (_id, pad) in g.gamepads() {
                    log::info!(target: "input", "gamepad: {}", pad.name());
                }
                Some(g)
            }
            Err(e) => {
                log::warn!(target: "input", "no gamepad support: {e}");
                None
            }
        };
        let rumble = gilrs.as_mut().and_then(Self::build_rumble);
        Self {
            gilrs,
            rumble,
            rumble_gain: -1.0,
            rumble_enabled: false,
            keys: HashSet::new(),
            gear: 0,
            shift_up_held: false,
            shift_down_held: false,
            steer: STEER_CENTRE,
            accel: ANALOG_MIN,
            brake: ANALOG_MIN,
            cursor: (0.5, 0.5),
            mouse_fire: false,
            mouse_reload: false,
            scheme: ControlScheme::Racing,
            analog_roles: [AnalogRole::None; 8],
            bindings: Bindings::default(),
            touch: Vec::new(),
            external: ExternalPad::default(),
        }
    }

    /// Publishes what the on-screen controls are asking for. Called once a
    /// frame with the overlay's current state.
    pub fn set_touch(&mut self, amounts: &[(Control, f32)]) {
        self.touch.clear();
        self.touch.extend_from_slice(amounts);
    }

    /// A button on a pad the platform reports rather than gilrs. Only Android
    /// has such a pad; elsewhere gilrs sees every controller itself.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn set_pad_button(&mut self, button: gilrs::Button, pressed: bool) {
        self.external.present = true;
        if pressed {
            self.external.buttons.insert(button);
        } else {
            self.external.buttons.remove(&button);
        }
    }

    /// The left stick of such a pad, each axis in -1..1 and positive up.
    pub fn set_pad_stick(&mut self, x: f32, y: f32) {
        self.external.present = true;
        self.external.left_x = x.clamp(-1.0, 1.0);
        self.external.left_y = y.clamp(-1.0, 1.0);
    }

    /// Which physical axis each of the eight ADC channels carries, taken from
    /// the ROM database so a cabinet's wiring is data, not code.
    pub fn set_analog_roles(&mut self, roles: [AnalogRole; 8]) {
        self.analog_roles = roles;
    }

    /// Places the axes a scheme produced onto the channels this game reads.
    fn scatter(&self, axes: &Axes, out: &mut Inputs) {
        for (ch, role) in self.analog_roles.iter().enumerate() {
            out.analog[ch] = axes.by_role(*role);
        }
        // The Model 1 I/O board reads three fixed channels rather than the
        // 315-5649's mux, so keep those in step with the wheel/pedal axes.
        out.steer = axes.steer;
        out.accel = axes.accel;
        out.brake = axes.brake;
    }

    /// Records the mouse position as a fraction of the render area (0..1).
    pub fn on_cursor(&mut self, x: f32, y: f32) {
        self.cursor = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    }

    /// The lightgun aim as a fraction of the render area (0..1), for drawing
    /// the on-screen crosshair. Follows the mouse, or the pad's right stick.
    pub fn aim(&self) -> (f32, f32) {
        let mut aim = self.cursor;
        // The right stick nudges the aim from centre, for players without a
        // mouse. It is read directly rather than through a binding: it is a
        // pointing device, not a control that can be pressed.
        let rx = self.axis_value(gilrs::Axis::RightStickX);
        let ry = self.axis_value(gilrs::Axis::RightStickY);
        if rx.abs() > STICK_DEADZONE || ry.abs() > STICK_DEADZONE {
            aim = (
                (0.5 + rx * 0.5).clamp(0.0, 1.0),
                (0.5 - ry * 0.5).clamp(0.0, 1.0),
            );
        }
        aim
    }

    /// Records the mouse buttons: left fires, right reloads (points off-screen).
    pub fn on_mouse_button(&mut self, left: Option<bool>, right: Option<bool>) {
        if let Some(l) = left {
            self.mouse_fire = l;
        }
        if let Some(r) = right {
            self.mouse_reload = r;
        }
    }

    /// Selects the control layout `poll` emits for the running game.
    pub fn set_scheme(&mut self, scheme: ControlScheme) {
        self.scheme = scheme;
    }

    /// Builds the one rumble effect we keep alive, at full magnitude. Force is
    /// applied by scaling its gain, which is cheap enough to do every frame.
    fn build_rumble(g: &mut gilrs::Gilrs) -> Option<Effect> {
        let pad = g.gamepads().next().map(|(id, _)| id)?;
        let eff = EffectBuilder::new()
            .add_effect(BaseEffect {
                kind: BaseEffectType::Strong {
                    magnitude: u16::MAX,
                },
                scheduling: Replay {
                    play_for: Ticks::from_ms(1_000),
                    ..Default::default()
                },
                envelope: Default::default(),
            })
            .add_effect(BaseEffect {
                kind: BaseEffectType::Weak {
                    magnitude: u16::MAX,
                },
                scheduling: Replay {
                    play_for: Ticks::from_ms(1_000),
                    ..Default::default()
                },
                envelope: Default::default(),
            })
            .repeat(Repeat::Infinitely)
            .gamepads(&[pad])
            .finish(g)
            .ok()?;
        // Start silent; the game decides from here.
        eff.set_gain(0.0).ok()?;
        eff.play().ok()?;
        log::info!(target: "input", "force feedback available");
        Some(eff)
    }

    /// Applies the drive board's force command to the pad's motors.
    ///
    /// `cmd` is the byte the game sent to the drive board; see the constants
    /// above for where its meaning comes from. Call once per emulated frame with
    /// `Model2System::drive_cmd`.
    pub fn set_rumble(&mut self, cmd: u8) {
        if !self.rumble_enabled {
            return;
        }
        let Some(eff) = self.rumble.as_ref() else {
            return;
        };
        if !(DRIVE_FORCE_FIRST..=DRIVE_FORCE_LAST).contains(&cmd) {
            return; // not a force command; leave the motors as they were
        }
        let gain = if cmd & DRIVE_KIND == DRIVE_KIND_OFF {
            0.0
        } else {
            (cmd & DRIVE_MAGNITUDE) as f32 / DRIVE_MAGNITUDE as f32
        };
        if (gain - self.rumble_gain).abs() > f32::EPSILON {
            let _ = eff.set_gain(gain);
            self.rumble_gain = gain;
        }
    }

    pub fn enable_rumble(&mut self, on: bool) {
        self.rumble_enabled = on;
        if !on {
            if let Some(eff) = self.rumble.as_ref() {
                let _ = eff.set_gain(0.0);
            }
            self.rumble_gain = 0.0;
        }
    }

    pub fn on_key(&mut self, key: KeyCode, pressed: bool) {
        if pressed {
            self.keys.insert(key);
        } else {
            self.keys.remove(&key);
        }
    }

    fn held(&self, key: KeyCode) -> bool {
        self.keys.contains(&key)
    }

    pub fn set_bindings(&mut self, bindings: Bindings) {
        self.bindings = bindings;
    }

    /// Whether any source bound to `control` is active. A stick counts as
    /// pressed once it is past the deadzone, which is what makes the analog
    /// controls usable as digital ones.
    pub fn on(&self, control: Control) -> bool {
        self.amount(control) > 0.0
    }

    /// How far `control` is pressed, 0..1. Digital sources read as fully on,
    /// so a keyboard drives the same code path a trigger does.
    pub fn amount(&self, control: Control) -> f32 {
        let bound = self
            .bindings
            .sources(control)
            .iter()
            .map(|source| self.source_amount(*source))
            .fold(0.0, f32::max);
        bound.max(self.touch_amount(control))
    }

    fn touch_amount(&self, control: Control) -> f32 {
        self.touch
            .iter()
            .find(|(c, _)| *c == control)
            .map_or(0.0, |(_, amount)| *amount)
    }

    /// Whether something with real travel is driving the axes.
    ///
    /// A keyboard is not: its controls are on or off, so the schemes ramp them
    /// toward the ends of the range instead of reading them as positions. A
    /// stick, a trigger or a thumb on the screen has a position of its own and
    /// is read directly.
    fn has_analog(&self) -> bool {
        self.pad().is_some() || self.external.present || !self.touch.is_empty()
    }

    /// The travel of one pad axis, from whichever device is reporting it.
    fn axis_value(&self, axis: gilrs::Axis) -> f32 {
        let hardware = self.pad().map_or(0.0, |pad| pad.value(axis));
        let external = self.external.axis(axis);
        if external.abs() > hardware.abs() {
            external
        } else {
            hardware
        }
    }

    /// The signed travel of a pair of opposed controls, -1..1. Keys give the
    /// extremes; a stick gives everything in between.
    pub fn axis(&self, negative: Control, positive: Control) -> f32 {
        self.amount(positive) - self.amount(negative)
    }

    fn source_amount(&self, source: Source) -> f32 {
        match source {
            Source::Key(k) => f32::from(u8::from(self.held(k))),
            Source::Pad(b) => {
                let hardware = self.pad().is_some_and(|pad| pad.is_pressed(b));
                f32::from(u8::from(hardware || self.external.buttons.contains(&b)))
            }
            Source::PadAxis(a, sign) => {
                let value = self.axis_value(a);
                // Triggers rest at zero and only travel positive; sticks rest
                // centred and travel both ways. The deadzone differs to match.
                let value = match sign {
                    Sign::Positive => value,
                    Sign::Negative => -value,
                };
                let deadzone = match a {
                    gilrs::Axis::LeftZ | gilrs::Axis::RightZ => TRIGGER_DEADZONE,
                    _ => STICK_DEADZONE,
                };
                if value > deadzone {
                    value.min(1.0)
                } else {
                    0.0
                }
            }
        }
    }

    fn pad(&self) -> Option<gilrs::Gamepad<'_>> {
        self.gilrs
            .as_ref()
            .and_then(|g| g.gamepads().next().map(|(_id, pad)| pad))
    }

    /// Moves `axis` toward `target` by at most `delta`.
    fn approach(axis: i32, target: i32, delta: i32) -> i32 {
        if axis < target {
            (axis + delta).min(target)
        } else {
            (axis - delta).max(target)
        }
    }

    fn shift(&mut self, up: bool) {
        if up {
            self.gear = (self.gear + 1).min(TOP_GEAR);
        } else {
            self.gear = self.gear.saturating_sub(1);
        }
    }

    /// Samples every device and publishes the result the way the I/O board sees
    /// it. Call once per emulated frame, before the board's input command runs.
    pub fn poll(&mut self, out: &mut Inputs) {
        // Drain the event queue so gilrs keeps its button/axis state current.
        if let Some(g) = self.gilrs.as_mut() {
            while g.next_event().is_some() {}
        }

        match self.scheme {
            ControlScheme::Joystick => {
                self.poll_joystick(out);
                return;
            }
            ControlScheme::Flight => {
                self.poll_swa(out);
                return;
            }
            ControlScheme::Gun => {
                self.poll_gun(out);
                return;
            }
            ControlScheme::Jetski => {
                self.poll_waverunr(out);
                return;
            }
            // Ski, skateboard and sled cabinets share one shape: a couple of
            // buttons plus one or two body-lean axes driven from the stick.
            ControlScheme::Skate | ControlScheme::Ski | ControlScheme::Sled => {
                self.poll_body(out);
                return;
            }
            // A bike's levers and lean map onto the same wheel/pedal inputs;
            // only the channel wiring differs, and that comes from the database.
            ControlScheme::Racing | ControlScheme::Bike => {}
        }

        let mut in0: u8 = 0xff;
        let mut in1: u8 = !0x70;

        // --- gamepad ---------------------------------------------------------
        let mut pad_steer: Option<f32> = None;
        let (mut pad_accel, mut pad_brake) = (0.0f32, 0.0f32);
        let (mut want_up, mut want_down) = (false, false);

        if self.has_analog() {
            // The wheel takes the signed travel of the two steer controls,
            // rescaled past the deadzone so it starts moving from rest rather
            // than jumping. The pedals are one-sided, so their travel is the
            // amount alone -- a trigger gives the middle of the range, a key
            // gives the end of it.
            let x = self.axis(Control::SteerLeft, Control::SteerRight);
            pad_steer = Some(if x.abs() > STICK_DEADZONE {
                x.signum() * (x.abs() - STICK_DEADZONE) / (1.0 - STICK_DEADZONE)
            } else {
                0.0
            });
            pad_accel = self.amount(Control::Throttle);
            pad_brake = self.amount(Control::Brake);

            want_up = self.on(Control::GearUp);
            want_down = self.on(Control::GearDown);

            // Coin plus both shift paddles is a deliberate two-hand gesture, so
            // the test menu cannot be opened by accident mid-race. It is the one
            // combination rather than a binding, because it exists to be awkward.
            if self.on(Control::Coin1) && want_up && want_down {
                in0 &= !IN0_TEST;
            }
        }

        // --- keyboard --------------------------------------------------------
        let k_left = self.on(Control::Left);
        let k_right = self.on(Control::Right);
        let k_accel = self.on(Control::Up);
        let k_brake = self.on(Control::Down);

        if self.on(Control::ViewRed) {
            in0 &= !IN0_VR1_RED;
        }
        if self.on(Control::ViewBlue) {
            in0 &= !IN0_VR2_BLUE;
        }
        if self.on(Control::ViewYellow) {
            in0 &= !IN0_VR3_YELLOW;
        }
        if self.on(Control::ViewGreen) {
            in1 &= !IN1_VR4_GREEN;
        }
        if self.on(Control::Start1) {
            in0 &= !IN0_START1;
        }
        if self.on(Control::Coin1) {
            in0 &= !IN0_COIN1;
        }
        if self.on(Control::Coin2) {
            in0 &= !IN0_COIN2;
        }
        if self.on(Control::Test) {
            in0 &= !IN0_TEST;
        }
        if self.on(Control::Service) {
            in0 &= !IN0_SERVICE;
        }

        want_up |= self.on(Control::GearUp);
        want_down |= self.on(Control::GearDown);

        // --- sequential shifter ---------------------------------------------
        // Edge-triggered: holding the button must not run through every gear.
        if want_up && !self.shift_up_held {
            self.shift(true);
        }
        if want_down && !self.shift_down_held {
            self.shift(false);
        }
        self.shift_up_held = want_up;
        self.shift_down_held = want_down;

        // --- analog ----------------------------------------------------------
        let half = (ANALOG_MAX - STEER_CENTRE) as f32;
        match pad_steer {
            // A stick has a position of its own, so it drives the wheel
            // directly; only fall back to key travel when it is centred.
            Some(s) if s.abs() > 0.0 => {
                self.steer = (STEER_CENTRE as f32 + s * half).round() as i32;
            }
            _ => {
                let target = if k_left {
                    ANALOG_MIN
                } else if k_right {
                    ANALOG_MAX
                } else {
                    STEER_CENTRE
                };
                self.steer = Self::approach(self.steer, target, STEER_KEYDELTA);
            }
        }

        let pedal = |cur: i32, pad: f32, key: bool| -> i32 {
            if pad > TRIGGER_DEADZONE {
                (ANALOG_MIN as f32 + pad * (ANALOG_MAX - ANALOG_MIN) as f32).round() as i32
            } else {
                let target = if key { ANALOG_MAX } else { ANALOG_MIN };
                Self::approach(cur, target, PEDAL_KEYDELTA)
            }
        };
        self.accel = pedal(self.accel, pad_accel, k_accel);
        self.brake = pedal(self.brake, pad_brake, k_brake);

        self.steer = self.steer.clamp(ANALOG_MIN, ANALOG_MAX);
        self.accel = self.accel.clamp(ANALOG_MIN, ANALOG_MAX);
        self.brake = self.brake.clamp(ANALOG_MIN, ANALOG_MAX);

        out.in0 = in0;
        out.in1 = in1;
        let axes = Axes {
            steer: self.steer as u8,
            accel: self.accel as u8,
            brake: self.brake as u8,
            // A bike's throttle grip is the same pedal input on another channel.
            throttle: self.accel as u8,
            ..Default::default()
        };
        self.scatter(&axes, out);
        out.set_gear(self.gear);
    }

    /// Joystick mapping (Virtua Fighter, Virtua Striker): IN.0 keeps
    /// coin/start/test/service; IN.1 carries
    /// player 1's three attack buttons and 8-way joystick (all active-low).
    /// steer/accel/brake stay neutral -- vf ignores the analog channels.
    fn poll_joystick(&mut self, out: &mut Inputs) {
        let mut in0: u8 = 0xff;
        let mut in1: u8 = 0xff;
        let (mut up, mut down, mut left, mut right) = (false, false, false, false);

        // Guard, punch and kick, on whatever the player bound them to.
        if self.on(Control::Button1) {
            in1 &= !IN1_JOY_BTN1;
        }
        if self.on(Control::Button2) {
            in1 &= !IN1_JOY_BTN2;
        }
        if self.on(Control::Button3) {
            in1 &= !IN1_JOY_BTN3;
        }
        left |= self.on(Control::Left);
        right |= self.on(Control::Right);
        up |= self.on(Control::Up);
        down |= self.on(Control::Down);
        if self.on(Control::Start1) {
            in0 &= !IN0_START1;
        }
        if self.on(Control::Coin1) {
            in0 &= !IN0_COIN1;
        }
        if self.on(Control::Coin2) {
            in0 &= !IN0_COIN2;
        }
        if self.on(Control::Test) {
            in0 &= !IN0_TEST;
        }
        if self.on(Control::Service) {
            in0 &= !IN0_SERVICE;
        }

        if left {
            in1 &= !IN1_JOY_LEFT;
        }
        if right {
            in1 &= !IN1_JOY_RIGHT;
        }
        if up {
            in1 &= !IN1_JOY_UP;
        }
        if down {
            in1 &= !IN1_JOY_DOWN;
        }

        out.in0 = in0;
        out.in1 = in1;
        out.in2 = 0xff;
        self.scatter(&Axes::default(), out);
    }

    /// Star Wars Arcade: a two-axis flight stick (aim), a throttle, and three
    /// fire buttons. IN.1 layout from the reference; the
    /// stick and throttle ride the same ADC channels 0/1/2 the racers use, so
    /// they go out through `steer`/`accel`/`brake`.
    fn poll_swa(&mut self, out: &mut Inputs) {
        let mut in0: u8 = 0xff;
        let mut in1: u8 = 0xff;

        // Aim axes and throttle as fractions in [-1, 1]; buttons.
        let (mut aim_x, mut aim_y, mut throttle) = (0.0f32, 0.0f32, 0.0f32);
        let (mut fire1, mut fire2, mut fire3) = (false, false, false);

        if self.has_analog() {
            aim_x = self.axis(Control::Left, Control::Right);
            aim_y = self.axis(Control::Down, Control::Up);
            // The throttle runs both ways: forward on its own control, reverse
            // on the brake.
            throttle = self.amount(Control::Throttle) - self.amount(Control::Brake);
        }
        fire1 |= self.on(Control::Button1);
        fire2 |= self.on(Control::Button2);
        fire3 |= self.on(Control::Button3);

        if self.on(Control::Left) {
            aim_x = -1.0;
        }
        if self.on(Control::Right) {
            aim_x = 1.0;
        }
        if self.on(Control::Up) {
            aim_y = 1.0;
        }
        if self.on(Control::Down) {
            aim_y = -1.0;
        }
        if self.on(Control::Throttle) {
            throttle = 1.0;
        }
        if self.on(Control::Brake) {
            throttle = -1.0;
        }
        fire1 |= self.on(Control::Button1);
        fire2 |= self.on(Control::Button2);
        fire3 |= self.on(Control::Button3);
        if self.on(Control::Start1) {
            in0 &= !IN0_START1;
        }
        if self.on(Control::Coin1) {
            in0 &= !IN0_COIN1;
        }
        if self.on(Control::Coin2) {
            in0 &= !IN0_COIN2;
        }
        if self.on(Control::Test) {
            in0 &= !IN0_TEST;
        }
        if self.on(Control::Service) {
            in0 &= !IN0_SERVICE;
        }

        if fire1 {
            in1 &= !IN1_SWA_BTN1;
        }
        if fire2 {
            in1 &= !IN1_SWA_BTN2;
        }
        if fire3 {
            in1 &= !IN1_SWA_BTN3;
        }

        // Map a [-1, 1] fraction to the ADC's 0x7f-centred range.
        let axis =
            |frac: f32| (SWA_CENTRE + (frac * SWA_STICK_RANGE as f32) as i32).clamp(0, 0xff) as u8;
        out.in0 = in0;
        out.in1 = in1;
        out.in2 = 0xff;
        // The stick's X axis and the throttle are reversed: a rightward or
        // forward push lowers the ADC value. The Y axis is direct.
        let axes = Axes {
            stickx: axis(-aim_x),
            sticky: axis(aim_y),
            throttle: axis(-throttle),
            stick2x: axis(-aim_x),
            stick2y: axis(aim_y),
            steer: axis(-aim_x),
            accel: axis(aim_y),
            brake: axis(-throttle),
            ..Default::default()
        };
        self.scatter(&axes, out);
    }

    /// Wave Runner: a jet-ski cabinet. The handle bar and the throttle lever
    /// are the two controls a player actually holds; roll and pitch come from
    /// the seat's tilt sensors, which we drive from the same stick so the ski
    /// leans into a turn. IN.0 puts START1 on bit 6 rather than the usual bit
    /// 4, and IN.1 carries a single View button (the reference
    /// `INPUT_PORTS_START( waverunr )`).
    fn poll_waverunr(&mut self, out: &mut Inputs) {
        const IN0_WR_START1: u8 = 0x40;
        const IN1_WR_VIEW: u8 = 0x01;

        let mut in0: u8 = 0xff;
        let mut in1: u8 = 0xff;

        let (mut handle, mut throttle) = (0.0f32, 0.0f32);

        if self.has_analog() {
            handle = self.axis(Control::LeanLeft, Control::LeanRight);
            throttle = self.amount(Control::Throttle);
        }
        if self.on(Control::ViewChange) {
            in1 &= !IN1_WR_VIEW;
        }

        if self.on(Control::Left) {
            handle = -1.0;
        }
        if self.on(Control::Right) {
            handle = 1.0;
        }
        if self.on(Control::Up) {
            throttle = 1.0;
        }
        if self.on(Control::Start1) {
            in0 &= !IN0_WR_START1;
        }
        if self.on(Control::Coin1) {
            in0 &= !IN0_COIN1;
        }
        if self.on(Control::Coin2) {
            in0 &= !IN0_COIN2;
        }
        if self.on(Control::Test) {
            in0 &= !IN0_TEST;
        }
        if self.on(Control::Service) {
            in0 &= !IN0_SERVICE;
        }

        // The handle bar is a centred axis. The throttle is a lever with a
        // physical rest position at one end of its travel, not a centred stick:
        // A 0x80 default would be the neutral of a centred stick, and parking
        // there means half throttle with nothing held. The channel is reversed:
        // full throttle sits at the low end.
        let axis = |frac: f32| (0x80 + (frac * 0x7f as f32) as i32).clamp(0, 0xff) as u8;
        out.in0 = in0;
        out.in1 = in1;
        // Bit 3 is the safety sensor, which reads low until the seat reports
        // a rider; the reference leaves it that way and the game still runs.
        out.in2 = 0xf7;
        let axes = Axes {
            steer: axis(handle),
            // Roll and pitch are the seat's tilt sensors. A desk player has
            // nothing to lean, so both sit at their neutral reading.
            roll: 0x80,
            pitch: 0x80,
            throttle: (0xff - (throttle.clamp(0.0, 1.0) * 255.0) as i32).clamp(0, 0xff) as u8,
            ..Default::default()
        };
        self.scatter(&axes, out);
    }

    /// Virtua Cop: the mouse is the lightgun. Its position maps across the ADC
    /// range; the left button fires (IN.1 bit 0), the right reloads by pointing
    /// off-screen. A gamepad's right stick aims for players without a mouse.
    fn poll_gun(&mut self, out: &mut Inputs) {
        let mut in0: u8 = 0xff;
        let mut in1: u8 = 0xff;

        let (mut nx, mut ny) = self.cursor;
        let mut fire = self.mouse_fire;
        let mut reload = self.mouse_reload;

        // The right stick nudges the aim from centre for pad-only players.
        let rx = self.axis_value(gilrs::Axis::RightStickX);
        let ry = self.axis_value(gilrs::Axis::RightStickY);
        if rx.abs() > STICK_DEADZONE || ry.abs() > STICK_DEADZONE {
            nx = 0.5 + rx * 0.5;
            ny = 0.5 - ry * 0.5;
        }

        fire |= self.on(Control::Fire);
        reload |= self.on(Control::Reload);
        if self.on(Control::Start1) {
            in0 &= !IN0_START1;
        }
        if self.on(Control::Coin1) {
            in0 &= !IN0_COIN1;
        }
        if self.on(Control::Coin2) {
            in0 &= !IN0_COIN2;
        }
        if self.on(Control::Test) {
            in0 &= !IN0_TEST;
        }
        if self.on(Control::Service) {
            in0 &= !IN0_SERVICE;
        }

        // Reloading is done by shooting off-screen: report the gun off-screen
        // and pull the trigger, which is exactly what the cabinet's gun does.
        if reload {
            fire = true;
        }
        if fire {
            in1 &= !IN1_VCOP_TRIGGER;
        }

        let lerp = |t: f32, lo: i32, hi: i32| (lo as f32 + t * (hi - lo) as f32) as u16;
        out.in0 = in0;
        out.in1 = in1;
        out.in2 = 0xff;
        out.gun_x = lerp(nx, GUN_X_MIN, GUN_X_MAX);
        out.gun_y = lerp(ny, GUN_Y_MIN, GUN_Y_MAX);
        out.gun_offscreen = reload;
        // The mounted-gun cabinets (Gunblade, Rail Chase 2, Behind Enemy Lines)
        // read aim straight off the ADC instead of the gun interface board, so
        // publish the same aim there as an 8-bit value. Player 2's gun stays
        // centred: one mouse only drives player 1.
        let byte = |t: f32| (t.clamp(0.0, 1.0) * 255.0) as u8;
        let axes = Axes {
            gun1x: byte(nx),
            gun1y: byte(ny),
            gun2x: 0x80,
            gun2y: 0x80,
            ..Default::default()
        };
        self.scatter(&axes, out);
    }

    /// Ski, skateboard and sled cabinets. All three are a footplate the player
    /// leans on: one or two lean axes plus a couple of buttons, so they share a
    /// mapping and the ROM database decides which channels carry which axis.
    fn poll_body(&mut self, out: &mut Inputs) {
        let mut in0: u8 = 0xff;
        let mut in1: u8 = 0xff;
        let (mut lean_x, mut lean_y) = (0.0f32, 0.0f32);
        let (mut left, mut right) = (0.0f32, 0.0f32);

        if self.has_analog() {
            lean_x = self.axis(Control::LeanLeft, Control::LeanRight);
            lean_y = self.axis(Control::Down, Control::Up);
            // The foot pedals, one per side.
            right = self.amount(Control::Throttle);
            left = self.amount(Control::Brake);
        }
        if self.on(Control::Button1) {
            in1 &= !0x01;
        }
        if self.on(Control::Button2) {
            in1 &= !0x02;
        }
        if self.on(Control::Button3) {
            in1 &= !0x04;
        }

        if self.on(Control::Left) {
            lean_x = -1.0;
        }
        if self.on(Control::Right) {
            lean_x = 1.0;
        }
        if self.on(Control::Up) {
            lean_y = 1.0;
        }
        if self.on(Control::Down) {
            lean_y = -1.0;
        }
        if self.on(Control::Start1) {
            in0 &= !IN0_START1;
        }
        if self.on(Control::Coin1) {
            in0 &= !IN0_COIN1;
        }
        if self.on(Control::Coin2) {
            in0 &= !IN0_COIN2;
        }
        if self.on(Control::Test) {
            in0 &= !IN0_TEST;
        }
        if self.on(Control::Service) {
            in0 &= !IN0_SERVICE;
        }

        let axis = |frac: f32| (0x80 + (frac * 0x7f as f32) as i32).clamp(0, 0xff) as u8;
        let pedal = |frac: f32| (frac.clamp(0.0, 1.0) * 255.0) as u8;
        out.in0 = in0;
        out.in1 = in1;
        out.in2 = 0xff;
        let axes = Axes {
            slide: axis(lean_x),
            swing: axis(lean_x),
            curving: axis(lean_x),
            incline: axis(lean_y),
            // Power Sled's four foot pedals: the triggers drive one seat's
            // pair, and the other seat stays at rest.
            p1r: pedal(right),
            p1l: pedal(left),
            steer: axis(lean_x),
            ..Default::default()
        };
        self.scatter(&axes, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::Source;

    /// Every scheme, so a new one cannot be added without being covered.
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

    /// The cabinet furniture every machine has, and the IN0 bit each pulls low.
    /// Wave Runner puts start on a different bit, which the check allows for.
    const FURNITURE: [(Control, u8); 4] = [
        (Control::Coin1, IN0_COIN1),
        (Control::Coin2, IN0_COIN2),
        (Control::Test, IN0_TEST),
        (Control::Service, IN0_SERVICE),
    ];

    fn state(scheme: ControlScheme) -> InputState {
        let mut input = InputState::new();
        input.set_scheme(scheme);
        input
    }

    /// Coin, test and service must reach the machine through the *binding*,
    /// not through whatever key the binding happens to hold.
    ///
    /// Rebinding to a key nothing is hardcoded to is the point: a scheme that
    /// reads `KeyCode::Digit5` directly passes any test that presses Digit5,
    /// and fails this one -- which is exactly how the pad came to be ignored
    /// on some schemes while the keyboard worked.
    #[test]
    fn every_scheme_reads_the_binding_not_the_key() {
        for scheme in SCHEMES {
            for (control, bit) in FURNITURE {
                let mut input = state(scheme);
                let mut bindings = Bindings::default();
                bindings.bind(control, vec![Source::Key(KeyCode::F12)]);
                input.set_bindings(bindings);

                let mut out = Inputs::default();
                input.on_key(KeyCode::F12, true);
                input.poll(&mut out);
                assert_eq!(
                    out.in0 & bit,
                    0,
                    "{scheme:?} ignores {control:?} when it is bound elsewhere",
                );

                input.on_key(KeyCode::F12, false);
                input.poll(&mut out);
                assert_ne!(out.in0 & bit, 0, "{scheme:?} leaves {control:?} stuck on");
            }
        }
    }

    /// The on-screen controls have to reach every scheme, for the same reason
    /// the bindings do: a scheme that reads a key or a pad directly ignores the
    /// screen, and on a handset the screen is all there is.
    #[test]
    fn every_scheme_reads_the_on_screen_controls() {
        for scheme in SCHEMES {
            for (control, bit) in FURNITURE {
                let mut input = state(scheme);
                let mut out = Inputs::default();

                input.set_touch(&[(control, 1.0)]);
                input.poll(&mut out);
                assert_eq!(
                    out.in0 & bit,
                    0,
                    "{scheme:?} ignores {control:?} pressed on the screen",
                );

                input.set_touch(&[]);
                input.poll(&mut out);
                assert_ne!(out.in0 & bit, 0, "{scheme:?} leaves {control:?} stuck on");
            }
        }
    }

    /// A thumb has travel, so it drives the wheel to a position. Reading it as
    /// a key would slam the wheel to full lock the moment it moved at all.
    #[test]
    fn a_partly_turned_wheel_lands_between_the_stops() {
        let mut input = state(ControlScheme::Racing);
        let mut out = Inputs::default();
        input.set_touch(&[(Control::SteerRight, 0.5)]);
        input.poll(&mut out);
        assert!(
            out.steer > STEER_CENTRE as u8 && out.steer < ANALOG_MAX as u8,
            "half a turn read as {:#04x}",
            out.steer,
        );
    }

    /// A key has no travel, so it must keep ramping instead. This is the case
    /// the analog path is not allowed to swallow.
    #[test]
    fn a_key_still_ramps_the_wheel() {
        let mut input = state(ControlScheme::Racing);
        let mut bindings = Bindings::default();
        bindings.bind(Control::Left, vec![Source::Key(KeyCode::F12)]);
        input.set_bindings(bindings);

        let mut out = Inputs::default();
        input.on_key(KeyCode::F12, true);
        input.poll(&mut out);
        assert_eq!(
            out.steer,
            (STEER_CENTRE - STEER_KEYDELTA) as u8,
            "one frame of a held key should move the wheel one step",
        );
    }

    /// A pad the platform reports itself -- which is how Android delivers one
    /// -- resolves through the same bindings a gilrs pad does.
    #[test]
    fn a_platform_pad_reaches_the_machine() {
        let mut input = state(ControlScheme::Joystick);
        let mut out = Inputs::default();

        // Button 1 ships bound to the West face button.
        input.set_pad_button(gilrs::Button::West, true);
        input.poll(&mut out);
        assert_eq!(out.in1 & IN1_JOY_BTN1, 0, "a platform pad button is ignored");

        input.set_pad_button(gilrs::Button::West, false);
        input.poll(&mut out);
        assert_ne!(out.in1 & IN1_JOY_BTN1, 0, "it stayed pressed");
    }

    /// Start has to work the same way. Its bit differs on Wave Runner, whose
    /// cabinet wires it to 0x40 rather than 0x10.
    #[test]
    fn every_scheme_reads_the_start_binding() {
        for scheme in SCHEMES {
            let mut input = state(scheme);
            let mut bindings = Bindings::default();
            bindings.bind(Control::Start1, vec![Source::Key(KeyCode::F12)]);
            input.set_bindings(bindings);

            let mut out = Inputs::default();
            input.on_key(KeyCode::F12, true);
            input.poll(&mut out);
            let start = if scheme == ControlScheme::Jetski {
                0x40
            } else {
                IN0_START1
            };
            assert_eq!(out.in0 & start, 0, "{scheme:?} ignores a rebound start");
        }
    }
}
