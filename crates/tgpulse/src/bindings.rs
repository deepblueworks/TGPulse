//! What the player presses, and what it means.
//!
//! The cabinets have physical controls a keyboard and a pad do not: a
//! 270-degree wheel, an H-pattern shifter, a lightgun, handlebars. Rather than
//! let every control scheme reach for keys and pad buttons itself, each names
//! the abstract control it wants -- `Control::Button1`, `Control::Throttle` --
//! and this resolves it through a table the player can change.
//!
//! Emulator hotkeys work the same way, in their own table, so they can be moved
//! off keys a game wants.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use gilrs::{Axis, Button};
use winit::keyboard::KeyCode;

/// A cabinet control, named by what it does rather than where it is.
///
/// Not every machine has every one of these: a scheme reads the ones its
/// cabinet had, so binding `GearUp` does nothing in a fighting game.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Control {
    // Direction, as an 8-way stick or the digital edges of an analog control.
    Up,
    Down,
    Left,
    Right,
    // Attack/action buttons, in the order the I/O board reports them.
    Button1,
    Button2,
    Button3,
    Button4,
    // Cabinet furniture.
    Coin1,
    Coin2,
    Start1,
    Start2,
    Test,
    Service,
    // Daytona's four coloured view buttons.
    ViewRed,
    ViewBlue,
    ViewYellow,
    ViewGreen,
    // Racing.
    Throttle,
    Brake,
    SteerLeft,
    SteerRight,
    GearUp,
    GearDown,
    // Lightgun.
    Fire,
    Reload,
    // Bikes, jetskis, skis and skateboards.
    LeanLeft,
    LeanRight,
    ViewChange,
}

impl Control {
    pub const ALL: &'static [Control] = &[
        Control::Up,
        Control::Down,
        Control::Left,
        Control::Right,
        Control::Button1,
        Control::Button2,
        Control::Button3,
        Control::Button4,
        Control::Coin1,
        Control::Coin2,
        Control::Start1,
        Control::Start2,
        Control::Test,
        Control::Service,
        Control::ViewRed,
        Control::ViewBlue,
        Control::ViewYellow,
        Control::ViewGreen,
        Control::Throttle,
        Control::Brake,
        Control::SteerLeft,
        Control::SteerRight,
        Control::GearUp,
        Control::GearDown,
        Control::Fire,
        Control::Reload,
        Control::LeanLeft,
        Control::LeanRight,
        Control::ViewChange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Control::Up => "Up",
            Control::Down => "Down",
            Control::Left => "Left",
            Control::Right => "Right",
            Control::Button1 => "Button 1",
            Control::Button2 => "Button 2",
            Control::Button3 => "Button 3",
            Control::Button4 => "Button 4",
            Control::Coin1 => "Coin 1",
            Control::Coin2 => "Coin 2",
            Control::Start1 => "Start 1",
            Control::Start2 => "Start 2",
            Control::Test => "Test",
            Control::Service => "Service",
            Control::ViewRed => "View (red)",
            Control::ViewBlue => "View (blue)",
            Control::ViewYellow => "View (yellow)",
            Control::ViewGreen => "View (green)",
            Control::Throttle => "Throttle",
            Control::Brake => "Brake",
            Control::SteerLeft => "Steer left",
            Control::SteerRight => "Steer right",
            Control::GearUp => "Gear up",
            Control::GearDown => "Gear down",
            Control::Fire => "Fire",
            Control::Reload => "Reload",
            Control::LeanLeft => "Lean left",
            Control::LeanRight => "Lean right",
            Control::ViewChange => "Change view",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Control::Up => "up",
            Control::Down => "down",
            Control::Left => "left",
            Control::Right => "right",
            Control::Button1 => "button1",
            Control::Button2 => "button2",
            Control::Button3 => "button3",
            Control::Button4 => "button4",
            Control::Coin1 => "coin1",
            Control::Coin2 => "coin2",
            Control::Start1 => "start1",
            Control::Start2 => "start2",
            Control::Test => "test",
            Control::Service => "service",
            Control::ViewRed => "view_red",
            Control::ViewBlue => "view_blue",
            Control::ViewYellow => "view_yellow",
            Control::ViewGreen => "view_green",
            Control::Throttle => "throttle",
            Control::Brake => "brake",
            Control::SteerLeft => "steer_left",
            Control::SteerRight => "steer_right",
            Control::GearUp => "gear_up",
            Control::GearDown => "gear_down",
            Control::Fire => "fire",
            Control::Reload => "reload",
            Control::LeanLeft => "lean_left",
            Control::LeanRight => "lean_right",
            Control::ViewChange => "view_change",
        }
    }

    fn from_key(s: &str) -> Option<Control> {
        Control::ALL.iter().copied().find(|c| c.key() == s)
    }
}

/// Something the emulator itself does, rather than the machine.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Hotkey {
    ToggleMenu,
    Fullscreen,
    SaveState,
    LoadState,
    NextSlot,
    PreviousSlot,
    Reset,
    Pause,
    FastForward,
}

impl Hotkey {
    pub const ALL: &'static [Hotkey] = &[
        Hotkey::ToggleMenu,
        Hotkey::Fullscreen,
        Hotkey::SaveState,
        Hotkey::LoadState,
        Hotkey::NextSlot,
        Hotkey::PreviousSlot,
        Hotkey::Reset,
        Hotkey::Pause,
        Hotkey::FastForward,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Hotkey::ToggleMenu => "Show/hide menu",
            Hotkey::Fullscreen => "Fullscreen",
            Hotkey::SaveState => "Save state",
            Hotkey::LoadState => "Load state",
            Hotkey::NextSlot => "Next state slot",
            Hotkey::PreviousSlot => "Previous state slot",
            Hotkey::Reset => "Reset machine",
            Hotkey::Pause => "Pause",
            Hotkey::FastForward => "Fast forward (hold)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Hotkey::ToggleMenu => "toggle_menu",
            Hotkey::Fullscreen => "fullscreen",
            Hotkey::SaveState => "save_state",
            Hotkey::LoadState => "load_state",
            Hotkey::NextSlot => "next_slot",
            Hotkey::PreviousSlot => "previous_slot",
            Hotkey::Reset => "reset",
            Hotkey::Pause => "pause",
            Hotkey::FastForward => "fast_forward",
        }
    }

    fn from_key(s: &str) -> Option<Hotkey> {
        Hotkey::ALL.iter().copied().find(|h| h.key() == s)
    }
}

/// One physical thing that can drive a control.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Key(KeyCode),
    Pad(Button),
    /// A stick or trigger past the deadzone in one direction.
    PadAxis(Axis, Sign),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sign {
    Positive,
    Negative,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Key(k) => write!(f, "{}", key_name(*k)),
            Source::Pad(b) => write!(f, "Pad {}", pad_button_name(*b)),
            Source::PadAxis(a, s) => write!(
                f,
                "Pad {}{}",
                pad_axis_name(*a),
                match s {
                    Sign::Positive => "+",
                    Sign::Negative => "-",
                }
            ),
        }
    }
}

/// Everything the player has bound.
#[derive(Clone, Debug)]
pub struct Bindings {
    pub controls: BTreeMap<Control, Vec<Source>>,
    pub hotkeys: BTreeMap<Hotkey, KeyCode>,
}

impl Default for Bindings {
    /// The layout the emulator shipped with before any of this was
    /// configurable, so an existing muscle memory keeps working.
    fn default() -> Self {
        use Control as C;
        let key = |k| Source::Key(k);
        let pad = |b| Source::Pad(b);
        let axis = |a, s| Source::PadAxis(a, s);

        let controls = BTreeMap::from([
            (
                C::Up,
                vec![
                    key(KeyCode::ArrowUp),
                    key(KeyCode::KeyW),
                    pad(Button::DPadUp),
                    axis(Axis::LeftStickY, Sign::Positive),
                ],
            ),
            (
                C::Down,
                vec![
                    key(KeyCode::ArrowDown),
                    key(KeyCode::KeyS),
                    pad(Button::DPadDown),
                    axis(Axis::LeftStickY, Sign::Negative),
                ],
            ),
            (
                C::Left,
                vec![
                    key(KeyCode::ArrowLeft),
                    key(KeyCode::KeyA),
                    pad(Button::DPadLeft),
                    axis(Axis::LeftStickX, Sign::Negative),
                ],
            ),
            (
                C::Right,
                vec![
                    key(KeyCode::ArrowRight),
                    key(KeyCode::KeyD),
                    pad(Button::DPadRight),
                    axis(Axis::LeftStickX, Sign::Positive),
                ],
            ),
            (C::Button1, vec![key(KeyCode::KeyJ), pad(Button::West)]),
            (C::Button2, vec![key(KeyCode::KeyK), pad(Button::South)]),
            (C::Button3, vec![key(KeyCode::KeyL), pad(Button::East)]),
            (C::Button4, vec![key(KeyCode::KeyI), pad(Button::North)]),
            (C::Coin1, vec![key(KeyCode::Digit5), pad(Button::Select)]),
            (C::Coin2, vec![key(KeyCode::Digit6)]),
            (
                C::Start1,
                vec![
                    key(KeyCode::Enter),
                    key(KeyCode::NumpadEnter),
                    pad(Button::Start),
                ],
            ),
            (C::Start2, vec![key(KeyCode::Digit2)]),
            (C::Test, vec![key(KeyCode::F2)]),
            // Not F1: a hotkey is resolved before the machine sees the key, so
            // a control sharing one with the menu toggle can never fire.
            (C::Service, vec![key(KeyCode::F8)]),
            (C::ViewRed, vec![key(KeyCode::KeyZ), pad(Button::West)]),
            (C::ViewBlue, vec![key(KeyCode::KeyX), pad(Button::North)]),
            (C::ViewYellow, vec![key(KeyCode::KeyC), pad(Button::East)]),
            (C::ViewGreen, vec![key(KeyCode::KeyV)]),
            (
                C::Throttle,
                vec![
                    key(KeyCode::KeyW),
                    key(KeyCode::ArrowUp),
                    axis(Axis::RightZ, Sign::Positive),
                    pad(Button::RightTrigger2),
                ],
            ),
            (
                C::Brake,
                vec![
                    key(KeyCode::KeyS),
                    key(KeyCode::ArrowDown),
                    axis(Axis::LeftZ, Sign::Positive),
                    pad(Button::LeftTrigger2),
                ],
            ),
            (
                C::SteerLeft,
                vec![
                    key(KeyCode::ArrowLeft),
                    key(KeyCode::KeyA),
                    axis(Axis::LeftStickX, Sign::Negative),
                ],
            ),
            (
                C::SteerRight,
                vec![
                    key(KeyCode::ArrowRight),
                    key(KeyCode::KeyD),
                    axis(Axis::LeftStickX, Sign::Positive),
                ],
            ),
            (
                C::GearUp,
                vec![key(KeyCode::KeyE), pad(Button::RightTrigger)],
            ),
            (
                C::GearDown,
                vec![key(KeyCode::KeyQ), pad(Button::LeftTrigger)],
            ),
            (
                C::Fire,
                vec![
                    key(KeyCode::Space),
                    pad(Button::South),
                    axis(Axis::RightZ, Sign::Positive),
                ],
            ),
            (
                C::Reload,
                vec![
                    key(KeyCode::KeyR),
                    pad(Button::East),
                    axis(Axis::LeftZ, Sign::Positive),
                ],
            ),
            (
                C::LeanLeft,
                vec![
                    key(KeyCode::ArrowLeft),
                    key(KeyCode::KeyA),
                    axis(Axis::LeftStickX, Sign::Negative),
                ],
            ),
            (
                C::LeanRight,
                vec![
                    key(KeyCode::ArrowRight),
                    key(KeyCode::KeyD),
                    axis(Axis::LeftStickX, Sign::Positive),
                ],
            ),
            (C::ViewChange, vec![key(KeyCode::Space), pad(Button::South)]),
        ]);

        let hotkeys = BTreeMap::from([
            (Hotkey::ToggleMenu, KeyCode::F1),
            (Hotkey::Fullscreen, KeyCode::F11),
            (Hotkey::SaveState, KeyCode::F5),
            (Hotkey::LoadState, KeyCode::F7),
            (Hotkey::NextSlot, KeyCode::F6),
            (Hotkey::PreviousSlot, KeyCode::F4),
            (Hotkey::Reset, KeyCode::F3),
            (Hotkey::Pause, KeyCode::F9),
            (Hotkey::FastForward, KeyCode::Tab),
        ]);

        Self { controls, hotkeys }
    }
}

impl Bindings {
    pub fn sources(&self, control: Control) -> &[Source] {
        self.controls.get(&control).map_or(&[], Vec::as_slice)
    }

    pub fn hotkey(&self, hotkey: Hotkey) -> Option<KeyCode> {
        self.hotkeys.get(&hotkey).copied()
    }

    /// The hotkey a key press triggers, if any.
    pub fn hotkey_for(&self, key: KeyCode) -> Option<Hotkey> {
        self.hotkeys
            .iter()
            .find(|(_, bound)| **bound == key)
            .map(|(hotkey, _)| *hotkey)
    }

    /// Replaces every source bound to a control.
    pub fn bind(&mut self, control: Control, sources: Vec<Source>) {
        self.controls.insert(control, sources);
    }

    pub fn bind_hotkey(&mut self, hotkey: Hotkey, key: KeyCode) {
        // A key drives one hotkey; taking it from another is the intent.
        self.hotkeys.retain(|_, bound| *bound != key);
        self.hotkeys.insert(hotkey, key);
    }

    /// Reads the bindings, writing the defaults out first if there is no file
    /// yet -- so the format is discoverable and hand-editable without having to
    /// change something in the interface to make one appear.
    pub fn load_or_create(path: &Path) -> Self {
        if !path.exists() {
            let defaults = Self::default();
            if let Err(e) = defaults.save(path) {
                log::warn!(target: "input", "cannot write {}: {e}", path.display());
            }
            return defaults;
        }
        Self::load(path)
    }

    pub fn path() -> PathBuf {
        PathBuf::from("config").join("input.conf")
    }

    /// Reads the file, falling back to the defaults for anything it does not
    /// mention -- so a file written by an older build still works, and so a
    /// control the player has not touched keeps its shipped binding.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut bindings = Self::default();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                log::warn!(target: "input", "{}:{}: not a binding", path.display(), number + 1);
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if let Some(control) = Control::from_key(name) {
                let sources: Vec<Source> = value
                    .split(',')
                    .filter_map(|s| parse_source(s.trim()))
                    .collect();
                bindings.controls.insert(control, sources);
            } else if let Some(hotkey) = Hotkey::from_key(name) {
                if let Some(key) = parse_key(value) {
                    bindings.hotkeys.insert(hotkey, key);
                }
            } else {
                log::warn!(target: "input", "{}:{}: unknown control '{name}'", path.display(), number + 1);
            }
        }
        log::info!(target: "input", "bindings from {}", path.display());
        bindings
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut out = String::from(
            "# TGPulse input bindings.\n\
             #\n\
             # Each control takes a comma-separated list of sources. A source is a\n\
             # key name, `pad:<button>`, or `pad:<axis>+` / `pad:<axis>-` for a stick\n\
             # or trigger. Delete a line to go back to the shipped binding.\n\n",
        );
        out += "# Cabinet controls\n";
        for (control, sources) in &self.controls {
            let list: Vec<String> = sources.iter().map(source_token).collect();
            out += &format!("{} = {}\n", control.key(), list.join(", "));
        }
        out += "\n# Emulator hotkeys\n";
        for (hotkey, key) in &self.hotkeys {
            out += &format!("{} = {}\n", hotkey.key(), key_token(*key));
        }
        std::fs::write(path, out).map_err(|e| e.to_string())
    }
}

fn source_token(source: &Source) -> String {
    match source {
        Source::Key(k) => key_token(*k),
        Source::Pad(b) => format!("pad:{}", pad_button_token(*b)),
        Source::PadAxis(a, s) => format!(
            "pad:{}{}",
            pad_axis_token(*a),
            match s {
                Sign::Positive => "+",
                Sign::Negative => "-",
            }
        ),
    }
}

fn parse_source(token: &str) -> Option<Source> {
    if let Some(rest) = token.strip_prefix("pad:") {
        if let Some(name) = rest.strip_suffix('+') {
            return pad_axis_from_token(name).map(|a| Source::PadAxis(a, Sign::Positive));
        }
        if let Some(name) = rest.strip_suffix('-') {
            return pad_axis_from_token(name).map(|a| Source::PadAxis(a, Sign::Negative));
        }
        return pad_button_from_token(rest).map(Source::Pad);
    }
    parse_key(token).map(Source::Key)
}

/// Keys are written as winit names it it, which are stable and unambiguous.
fn key_token(key: KeyCode) -> String {
    format!("{key:?}")
}

fn parse_key(token: &str) -> Option<KeyCode> {
    KEYS.iter()
        .copied()
        .find(|k| format!("{k:?}").eq_ignore_ascii_case(token))
}

/// A friendlier spelling for the interface; the file keeps the exact name.
fn key_name(key: KeyCode) -> String {
    let raw = format!("{key:?}");
    for prefix in ["Key", "Digit", "Numpad", "Arrow"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return match prefix {
                "Numpad" => format!("Num {rest}"),
                "Arrow" => rest.to_string(),
                _ => rest.to_string(),
            };
        }
    }
    raw
}

fn pad_button_token(button: Button) -> String {
    format!("{button:?}")
}

fn pad_button_from_token(token: &str) -> Option<Button> {
    PAD_BUTTONS
        .iter()
        .copied()
        .find(|b| format!("{b:?}").eq_ignore_ascii_case(token))
}

/// Pad buttons named as they are printed on the two common layouts, since
/// "South" means nothing to anyone holding the controller.
fn pad_button_name(button: Button) -> &'static str {
    match button {
        Button::South => "A / Cross",
        Button::East => "B / Circle",
        Button::West => "X / Square",
        Button::North => "Y / Triangle",
        Button::LeftTrigger => "L1 / LB",
        Button::RightTrigger => "R1 / RB",
        Button::LeftTrigger2 => "L2 / LT",
        Button::RightTrigger2 => "R2 / RT",
        Button::Select => "Select",
        Button::Start => "Start",
        Button::LeftThumb => "L3",
        Button::RightThumb => "R3",
        Button::DPadUp => "D-pad up",
        Button::DPadDown => "D-pad down",
        Button::DPadLeft => "D-pad left",
        Button::DPadRight => "D-pad right",
        _ => "button",
    }
}

fn pad_axis_token(axis: Axis) -> String {
    format!("{axis:?}")
}

fn pad_axis_from_token(token: &str) -> Option<Axis> {
    PAD_AXES
        .iter()
        .copied()
        .find(|a| format!("{a:?}").eq_ignore_ascii_case(token))
}

fn pad_axis_name(axis: Axis) -> &'static str {
    match axis {
        Axis::LeftStickX => "left stick X",
        Axis::LeftStickY => "left stick Y",
        Axis::RightStickX => "right stick X",
        Axis::RightStickY => "right stick Y",
        Axis::LeftZ => "left trigger",
        Axis::RightZ => "right trigger",
        _ => "axis",
    }
}

/// The keys offered for binding. Anything a cabinet button might reasonably
/// live on; the exotic ones are left out so the picker stays readable.
pub const KEYS: &[KeyCode] = &[
    KeyCode::KeyA,
    KeyCode::KeyB,
    KeyCode::KeyC,
    KeyCode::KeyD,
    KeyCode::KeyE,
    KeyCode::KeyF,
    KeyCode::KeyG,
    KeyCode::KeyH,
    KeyCode::KeyI,
    KeyCode::KeyJ,
    KeyCode::KeyK,
    KeyCode::KeyL,
    KeyCode::KeyM,
    KeyCode::KeyN,
    KeyCode::KeyO,
    KeyCode::KeyP,
    KeyCode::KeyQ,
    KeyCode::KeyR,
    KeyCode::KeyS,
    KeyCode::KeyT,
    KeyCode::KeyU,
    KeyCode::KeyV,
    KeyCode::KeyW,
    KeyCode::KeyX,
    KeyCode::KeyY,
    KeyCode::KeyZ,
    KeyCode::Digit0,
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
    KeyCode::Space,
    KeyCode::Enter,
    KeyCode::NumpadEnter,
    KeyCode::Tab,
    KeyCode::Backspace,
    KeyCode::ShiftLeft,
    KeyCode::ShiftRight,
    KeyCode::ControlLeft,
    KeyCode::ControlRight,
    KeyCode::AltLeft,
    KeyCode::AltRight,
    KeyCode::Comma,
    KeyCode::Period,
    KeyCode::Slash,
    KeyCode::Semicolon,
    KeyCode::Quote,
    KeyCode::BracketLeft,
    KeyCode::BracketRight,
    KeyCode::Minus,
    KeyCode::Equal,
    KeyCode::Backquote,
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
    KeyCode::F11,
    KeyCode::F12,
];

pub const PAD_BUTTONS: &[Button] = &[
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::LeftTrigger2,
    Button::RightTrigger2,
    Button::Select,
    Button::Start,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];

pub const PAD_AXES: &[Axis] = &[
    Axis::LeftStickX,
    Axis::LeftStickY,
    Axis::RightStickX,
    Axis::RightStickY,
    Axis::LeftZ,
    Axis::RightZ,
];

impl FromStr for Source {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        parse_source(s).ok_or_else(|| format!("unknown input source '{s}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join("tgpulse-bindings-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("input.conf");

        let mut written = Bindings::default();
        written.bind_hotkey(Hotkey::SaveState, KeyCode::F8);
        written.bind(
            Control::Button1,
            vec![
                Source::Key(KeyCode::KeyZ),
                Source::Pad(Button::North),
                Source::PadAxis(Axis::RightZ, Sign::Positive),
            ],
        );
        written.save(&path).expect("save");

        let read = Bindings::load(&path);
        assert_eq!(read.hotkey(Hotkey::SaveState), Some(KeyCode::F8));
        assert_eq!(
            read.sources(Control::Button1),
            written.sources(Control::Button1)
        );
        // Everything untouched still matches the defaults.
        assert_eq!(
            read.sources(Control::Coin1),
            Bindings::default().sources(Control::Coin1)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_drives_one_hotkey() {
        let mut bindings = Bindings::default();
        bindings.bind_hotkey(Hotkey::Reset, KeyCode::F5);
        // F5 was Save state; taking it leaves Save state unbound rather than
        // firing both.
        assert_eq!(bindings.hotkey(Hotkey::Reset), Some(KeyCode::F5));
        assert_eq!(bindings.hotkey(Hotkey::SaveState), None);
    }
}
