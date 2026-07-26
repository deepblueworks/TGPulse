//! The application: one window, one renderer, and at most one running machine.
//!
//! A session can be started and stopped without leaving the program, which is
//! what lets the library window launch a game and get back afterwards. The
//! Model 1 and Model 2 boards differ enough in their render path that the
//! session keeps them apart, but everything around them -- timing, input,
//! audio, battery-backed storage, save states -- is shared.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use winit::{
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{Fullscreen, Window, WindowBuilder},
};

use tgpulse_core::config::{Config, System};
use tgpulse_core::debugger::Debugger;
use tgpulse_core::tilemap::{self, SCREEN_H, SCREEN_W};
use tgpulse_core::{library, loader, model1::Model1System, model1_video, nvram, savestate};
use tgpulse_core::{roms_db::AnalogRole, system::Model2System};

use crate::attract::Attract;
use crate::bindings::{Bindings, Hotkey};
use crate::gui::{Action, Gui, Stats};
use crate::input::{ControlScheme, InputState};
use crate::platform::audio::Audio;
use crate::platform::video::Model2Video;
use crate::touch::TouchUi;

/// 25 MHz i960 over the exact Model 2 raster period: a 16 MHz pixel clock over
/// 656x424 total pixels gives 57.524 Hz.
const CYCLES_PER_FRAME: i32 = 434_600;
const FRAME_NANOS: u64 = 17_384_000;

/// How many frames may be caught up in one pass before emulated time is simply
/// resynchronised. Without a cap, a window drag turns into a burst of fast
/// forward when the loop resumes.
const MAX_CATCH_UP: u32 = 6;

/// Frames between battery-backed RAM flushes, so a kill rather than a clean
/// close does not lose the last few minutes of play.
const NVRAM_FLUSH_INTERVAL: u32 = 900;

/// Emulated frames run per displayed frame while fast forward is held.
const FAST_FORWARD_FRAMES: u32 = 4;

enum Machine {
    Model1(Box<Model1System>),
    Model2(Box<Model2System>),
}

struct Session {
    machine: Machine,
    /// Short set name; names the NVRAM and save-state files.
    set: String,
    title: String,
    input: InputState,
    audio: Audio,
    scheme: ControlScheme,
    nvram_countdown: u32,
    /// Buffers reused every frame rather than reallocated.
    background: Vec<u32>,
    foreground: Vec<u32>,
}

impl Session {
    fn open(path: &Path, config: &Config) -> Result<(Self, Config), String> {
        let entry = library::describe(path);
        let set = entry.set.clone();
        let title = if entry.is_known() {
            entry.title.clone()
        } else {
            entry.set.clone()
        };

        let mut config = config.clone();
        config.rom_path = path.to_string_lossy().into_owned();
        config.system = System::detect(&config.rom_path);

        let scheme = entry.scheme.unwrap_or(ControlScheme::Joystick);
        let roles = analog_roles(&config.rom_path);

        let machine = match config.system {
            System::Model1 => {
                let roms = loader::load_model1_zip(&config.rom_path)?;
                Machine::Model1(Box::new(Model1System::with_config(&roms, config.clone())))
            }
            System::Model2 => {
                let roms = loader::load_model2_zip(&config.rom_path)?;
                Machine::Model2(Box::new(Model2System::with_config(&roms, config.clone())))
            }
        };

        let mut input = InputState::new();
        input.enable_rumble(config.rumble);
        input.set_scheme(scheme);
        input.set_analog_roles(roles);

        let sample_rate = match &machine {
            Machine::Model1(s) => s.sound.sample_rate(),
            Machine::Model2(s) => s.sound.sample_rate(),
        };
        let audio = Audio::new(sample_rate, config.volume);

        let mut session = Session {
            machine,
            set,
            title,
            input,
            audio,
            scheme,
            nvram_countdown: NVRAM_FLUSH_INTERVAL,
            background: vec![0; SCREEN_W * SCREEN_H],
            foreground: vec![0; SCREEN_W * SCREEN_H],
        };
        session.load_nvram();
        log::info!(target: "app", "{} ({:?} controls)", session.title, session.scheme);
        Ok((session, config))
    }

    fn load_nvram(&mut self) {
        if let Machine::Model2(sys) = &mut self.machine {
            sys.snapshot_game = self.set.clone();
        }
        let (backup_len, eeprom_len) = match &self.machine {
            Machine::Model1(sys) => sys.nvram_sizes(),
            Machine::Model2(sys) => sys.nvram_sizes(),
        };
        match nvram::load(&self.set, backup_len, eeprom_len) {
            Some((b, e)) => {
                match &mut self.machine {
                    Machine::Model1(sys) => sys.set_nvram_blocks(&b, &e),
                    Machine::Model2(sys) => sys.set_nvram_blocks(&b, &e),
                }
                log::info!(target: "nvram", "loaded {}", nvram::path_for(&self.set).display());
            }
            None => log::info!(target: "nvram", "none saved; the game will initialise it"),
        }
    }

    fn save_nvram(&self) {
        let (b, e) = match &self.machine {
            Machine::Model1(sys) => sys.nvram_blocks(),
            Machine::Model2(sys) => sys.nvram_blocks(),
        };
        nvram::save(&self.set, &b, &e);
    }

    /// Advances one emulated frame.
    fn step(&mut self) {
        match &mut self.machine {
            Machine::Model1(sys) => {
                self.input.poll(&mut sys.inputs);
                sys.run_slice(tgpulse_core::model1::CYCLES_PER_FRAME);
                self.audio.push(sys.sound.samples.drain(..));
                self.input.set_rumble(sys.drive_cmd);
                sys.trigger_vblank();
            }
            Machine::Model2(sys) => {
                self.input.poll(&mut sys.inputs);
                sys.run_slice(CYCLES_PER_FRAME);
                self.audio.push(sys.sound.drain_samples());
                self.input.set_rumble(sys.drive_cmd);
                sys.trigger_vblank();
            }
        }
        self.nvram_countdown -= 1;
        if self.nvram_countdown == 0 {
            self.nvram_countdown = NVRAM_FLUSH_INTERVAL;
            self.save_nvram();
        }
    }

    fn reset(&mut self, config: &Config) {
        match Session::open(Path::new(&config.rom_path), config) {
            Ok((fresh, _)) => {
                self.save_nvram();
                *self = fresh;
            }
            Err(e) => log::error!(target: "app", "reset failed: {e}"),
        }
    }
}

/// The ADC channel wiring for a romset, from the ROM database rather than
/// guessed from the file name.
fn analog_roles(rom_path: &str) -> [AnalogRole; 8] {
    loader::archive_names(rom_path)
        .ok()
        .and_then(|names| tgpulse_core::roms_db::identify(&names))
        .map(|def| def.analog_roles)
        .unwrap_or([AnalogRole::None; 8])
}

pub fn run(config: Config, rom: Option<PathBuf>) -> Result<(), String> {
    run_with(EventLoop::new().map_err(|e| e.to_string())?, config, rom)
}

/// Runs against an event loop the caller built. Android needs to construct it
/// from the activity handle, so it cannot be created here.
pub fn run_with(
    event_loop: EventLoop<()>,
    config: Config,
    rom: Option<PathBuf>,
) -> Result<(), String> {
    let window = WindowBuilder::new()
        .with_title("TGPulse")
        .with_inner_size(winit::dpi::LogicalSize::new(
            SCREEN_W as u32 * 2,
            SCREEN_H as u32 * 2,
        ))
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    if config.fullscreen {
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    }

    let mut app = App::new(config, rom);
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop
        .run(move |event, elwt| app.handle(&window, event, elwt))
        .map_err(|e| e.to_string())
}

/// The renderer and the interface, which exist only while the window has a
/// drawable surface.
///
/// On a desktop that is the whole run, but Android takes the surface away
/// whenever the activity is backgrounded and gives back a different one, so
/// both are built on `Resumed` and dropped on `Suspended`. The machine keeps
/// running across that; only the ability to draw comes and goes.
struct Presenter {
    video: Model2Video,
    ui: crate::gui::Renderer,
}

struct App {
    config: Config,
    presenter: Option<Presenter>,
    gui: Gui,
    session: Option<Session>,
    debugger: Option<Debugger>,
    /// A romset named on the command line, launched once there is a surface.
    pending_rom: Option<PathBuf>,

    fullscreen: bool,
    paused: bool,
    fast_forward: bool,
    /// Hotkeys and control bindings, shared with each session's input.
    bindings: Bindings,
    /// What is shown when no game is loaded.
    attract: Attract,
    frame_duration: Duration,
    last_frame: Instant,
    last_present: Instant,
    stats_at: Instant,
    presented: u32,
    emulated: u32,
    stats: Stats,
    /// The attract screen's own layers, so it does not borrow a session's.
    idle_background: Vec<u32>,
    idle_foreground: Vec<u32>,
    /// The settings the current renderer was built for. They are fixed when
    /// the pipelines are created, so changing one means building it again.
    video_settings: VideoSettings,
    /// The on-screen controls and the phone menu. Dormant on desktop.
    touch: TouchUi,
    /// Devices that have reported a real press. See `on_touch` for why a
    /// touchscreen has to be told apart from a gamepad's sticks.
    touch_devices: std::collections::HashSet<winit::event::DeviceId>,
}

impl App {
    fn new(config: Config, pending_rom: Option<PathBuf>) -> Self {
        let now = Instant::now();
        let fullscreen = config.fullscreen;
        Self {
            gui: Gui::new(&config),
            config,
            presenter: None,
            session: None,
            debugger: None,
            pending_rom,
            // Matches what `run_with` did to the window, so starting with
            // --fullscreen suppresses the interface the same way the hotkey
            // does.
            fullscreen,
            paused: false,
            fast_forward: false,
            bindings: Bindings::load_or_create(&Bindings::path()),
            attract: Attract::new(),
            frame_duration: Duration::from_nanos(FRAME_NANOS),
            last_frame: now,
            last_present: now,
            stats_at: now,
            presented: 0,
            emulated: 0,
            stats: Stats::default(),
            idle_background: vec![0; SCREEN_W * SCREEN_H],
            idle_foreground: vec![0; SCREEN_W * SCREEN_H],
            video_settings: VideoSettings::default(),
            touch: {
                let mut touch = TouchUi::new();
                // A desktop has a mouse and a keyboard and wants neither of
                // these; a handset has neither and needs both.
                touch.set_enabled(cfg!(target_os = "android"));
                touch
            },
            touch_devices: std::collections::HashSet::new(),
        }
    }

    /// Rebuilds the renderer for a board, then starts the machine.
    fn launch(&mut self, window: &Window, rom: &Path) {
        if self.open_session(window, rom) {
            self.build_presenter(window);
        }
    }

    /// Loads a romset and makes it the running machine. Returns whether it
    /// worked; the renderer is the caller's business, because the two boards
    /// need different pipelines and the order matters at startup.
    fn open_session(&mut self, window: &Window, rom: &Path) -> bool {
        if let Some(session) = &self.session {
            session.save_nvram();
        }
        self.session = None;
        self.debugger = None;

        match Session::open(rom, &self.config) {
            Ok((mut session, config)) => {
                self.config = config;
                window.set_title(&format!("TGPulse - {}", session.title));
                session.input.set_bindings(self.bindings.clone());
                self.session = Some(session);
                self.last_frame = Instant::now();
                true
            }
            Err(e) => {
                log::error!(target: "app", "{e}");
                self.gui.report_error(e);
                false
            }
        }
    }

    fn close_game(&mut self, window: &Window) {
        if let Some(session) = self.session.take() {
            session.save_nvram();
        }
        self.debugger = None;
        window.set_title("TGPulse");
        self.gui.visible = true;
        // With nothing running there is nothing for the overlay to control, so
        // the phone goes back to the library rather than to an empty screen.
        self.touch.set_menu_open(true);
        self.build_presenter(window);
    }

    /// The two boards need different compute pipelines, and the settings that
    /// shape them are fixed when the renderer is built, so switching either
    /// means building a new one -- and with it a new UI renderer, since the
    /// wgpu device does not survive.
    /// Builds the renderer and the interface for the window's current surface.
    ///
    /// A window carries one surface at a time, so the previous one is dropped
    /// before wgpu is asked for another -- it refuses with "native window is in
    /// use" otherwise. The two boards need different compute pipelines and the
    /// settings that shape them are fixed at construction, so switching either
    /// comes through here.
    fn build_presenter(&mut self, window: &Window) {
        self.presenter = None;

        // No session means the attract screen, which is drawn as Model 1
        // quads, so that pipeline is the one to build.
        let model1 = !matches!(
            self.session.as_ref().map(|s| &s.machine),
            Some(Machine::Model2(_))
        );
        // With nothing loaded the screen is the attract scene, which is our own
        // artwork rather than a 4:3 board image: it is rendered to the shape of
        // the window instead of being pillarboxed inside it. The tile layers
        // are left unstretched so the logo and the prompt keep their shape.
        //
        // Both are arguments to the renderer's constructor, and the renderer is
        // rebuilt whenever a game is loaded or closed, so a game is handed
        // exactly the settings it was before.
        let (widescreen, stretch_2d) = match self.session {
            Some(_) => (self.config.widescreen, self.config.widescreen_stretch_2d),
            None => (true, false),
        };
        let video = pollster::block_on(async {
            if model1 {
                Model2Video::new_model1(
                    window,
                    self.config.ssaa,
                    self.config.smooth_shadows,
                    widescreen,
                    stretch_2d,
                )
                .await
            } else {
                Model2Video::new(
                    window,
                    self.config.ssaa,
                    false,
                    self.config.smooth_shadows,
                    widescreen,
                    stretch_2d,
                )
                .await
            }
        });
        let ui = self
            .gui
            .build_renderer(video.device(), video.queue(), video.surface_format());
        let size = window.inner_size();
        self.gui.set_display_size(size.width, size.height);
        self.touch.resize(size.width as f32, size.height as f32);
        self.video_settings = VideoSettings::of(&self.config);
        self.presenter = Some(Presenter { video, ui });
    }

    /// The surface has become available. A romset named on the command line is
    /// opened first, so the renderer is built for the board it needs rather
    /// than built twice.
    fn on_resumed(&mut self, window: &Window) {
        // The All files access grant is given in the system settings, with the
        // app in the background; coming back is the moment to notice it and
        // move the library to the shared folder.
        #[cfg(target_os = "android")]
        if crate::storage::has_all_files_access() {
            if let Some(dir) = crate::storage::shared_rom_dir() {
                if self.config.rom_dir != dir {
                    let _ = std::fs::create_dir_all(&dir);
                    self.config.rom_dir = dir;
                    let config = self.config.clone();
                    self.gui.refresh_library(&config);
                }
            }
        }
        if self.presenter.is_some() {
            return;
        }
        if let Some(rom) = self.pending_rom.take() {
            self.open_session(window, &rom);
        }
        self.build_presenter(window);
    }

    fn handle(
        &mut self,
        window: &Window,
        event: Event<()>,
        elwt: &winit::event_loop::EventLoopWindowTarget<()>,
    ) {
        match event {
            Event::Resumed => self.on_resumed(window),
            Event::Suspended => {
                // The surface is about to go away; the machine keeps its state.
                self.presenter = None;
            }
            Event::WindowEvent { event, .. } => {
                let Some(presenter) = &mut self.presenter else {
                    return;
                };
                let captured = self.gui.handle_event(window, &event);
                match event {
                    WindowEvent::CloseRequested => {
                        if let Some(session) = &self.session {
                            session.save_nvram();
                        }
                        elwt.exit();
                    }
                    WindowEvent::Resized(size) => {
                        presenter.video.resize(size);
                        self.gui.set_display_size(size.width, size.height);
                        self.touch.resize(size.width as f32, size.height as f32);
                    }
                    WindowEvent::Touch(finger) => {
                        let view = presenter.video.view_rect();
                        self.touch.set_view(view);
                        self.on_touch(finger);
                    }
                    WindowEvent::KeyboardInput { event, .. } => {
                        self.on_key(window, &event, captured)
                    }
                    WindowEvent::CursorMoved { position, .. } if !captured => {
                        if let Some(session) = &mut self.session {
                            let (vx, vy, vw, vh) = presenter.video.view_rect();
                            session.input.on_cursor(
                                (position.x as f32 - vx) / vw,
                                (position.y as f32 - vy) / vh,
                            );
                        }
                    }
                    WindowEvent::MouseInput { state, button, .. } if !captured => {
                        if let Some(session) = &mut self.session {
                            let pressed = state == ElementState::Pressed;
                            match button {
                                MouseButton::Left => {
                                    session.input.on_mouse_button(Some(pressed), None)
                                }
                                MouseButton::Right => {
                                    session.input.on_mouse_button(None, Some(pressed))
                                }
                                _ => {}
                            }
                        }
                    }
                    WindowEvent::RedrawRequested => self.redraw(window),
                    _ => {}
                }
            }
            Event::AboutToWait => {
                self.advance();
                window.request_redraw();
            }
            _ => {}
        }
    }

    /// One finger, or one stick that is pretending to be one.
    ///
    /// Android delivers a gamepad's sticks as motion events, and winit turns
    /// every motion event into a touch: the "location" of one of those is the
    /// stick's own -1..1 travel rather than a place on the screen. They cannot
    /// be told apart by their coordinates alone -- a finger really can land on
    /// pixel (1, 1) -- but a touchscreen always announces a contact with a
    /// press before it reports movement, and a stick never does. So a device
    /// that has only ever moved is a stick, and is read as one.
    fn on_touch(&mut self, finger: winit::event::Touch) {
        let (x, y) = (finger.location.x as f32, finger.location.y as f32);
        if finger.phase == winit::event::TouchPhase::Started {
            self.touch_devices.insert(finger.device_id);
        }
        if !self.touch_devices.contains(&finger.device_id) {
            // Guard the range as well: a motion event from a device we somehow
            // missed the press of would otherwise slam the stick to its stop.
            if (-1.05..=1.05).contains(&x) && (-1.05..=1.05).contains(&y) {
                if let Some(session) = &mut self.session {
                    // Android counts Y downward; a pad counts it up.
                    session.input.set_pad_stick(x, -y);
                }
            }
            return;
        }
        self.touch.on_touch(finger.id, finger.phase, x, y);
    }

    fn on_key(&mut self, window: &Window, event: &winit::event::KeyEvent, captured: bool) {
        // winit maps an Android pad's d-pad onto the arrow keys and hands back
        // everything else as a raw platform keycode, so the face and shoulder
        // buttons arrive here unrecognised.
        #[cfg(target_os = "android")]
        if let PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Android(code)) =
            event.physical_key
        {
            if let Some(button) = android_pad_button(code) {
                let pressed = event.state == ElementState::Pressed;
                if let Some(session) = &mut self.session {
                    session.input.set_pad_button(button, pressed);
                }
            }
            return;
        }

        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        let pressed = event.state == ElementState::Pressed;

        // The rebinding panel wants the raw key, not what it is currently
        // bound to, so it gets first refusal on the press.
        if pressed && self.gui.capture_key(code) {
            return;
        }

        if let Some(hotkey) = self.bindings.hotkey_for(code) {
            if matches!(hotkey, Hotkey::FastForward) {
                self.fast_forward = pressed;
            } else if pressed {
                self.run_hotkey(window, hotkey);
            }
            return;
        }

        // The machine only sees keys the interface did not take.
        if !captured {
            if let Some(session) = &mut self.session {
                session.input.on_key(code, pressed);
            }
        }
    }

    fn run_hotkey(&mut self, window: &Window, hotkey: Hotkey) {
        match hotkey {
            Hotkey::ToggleMenu => {
                if self.session.is_some() {
                    self.gui.visible = !self.gui.visible;
                }
            }
            Hotkey::Fullscreen => {
                // Only the window changes: see `Settings` for why this is not
                // written down.
                self.fullscreen = !self.fullscreen;
                self.config.fullscreen = self.fullscreen;
                window.set_fullscreen(if self.fullscreen {
                    Some(Fullscreen::Borderless(None))
                } else {
                    None
                });
            }
            Hotkey::SaveState => self.save_state(self.gui.state_slot),
            Hotkey::LoadState => self.load_state(self.gui.state_slot),
            Hotkey::NextSlot => {
                self.gui.state_slot = (self.gui.state_slot + 1) % 10;
                log::info!(target: "state", "slot {}", self.gui.state_slot);
            }
            Hotkey::PreviousSlot => {
                self.gui.state_slot = (self.gui.state_slot + 9) % 10;
                log::info!(target: "state", "slot {}", self.gui.state_slot);
            }
            Hotkey::Reset => {
                let config = self.config.clone();
                if let Some(session) = &mut self.session {
                    session.reset(&config);
                }
            }
            Hotkey::Pause => {
                self.paused = !self.paused;
                log::info!(target: "app", "{}", if self.paused { "paused" } else { "running" });
            }
            // Held rather than pressed; handled by the caller.
            Hotkey::FastForward => {}
        }
    }

    fn save_state(&mut self, slot: u32) {
        let Some(Session {
            machine: Machine::Model2(sys),
            set,
            ..
        }) = self.session.as_ref()
        else {
            return;
        };
        match savestate::save_to_file(sys, set, slot) {
            Ok(p) => log::info!(target: "state", "saved slot {slot} -> {}", p.display()),
            Err(e) => log::error!(target: "state", "save failed: {e}"),
        }
    }

    fn load_state(&mut self, slot: u32) {
        let Some(Session {
            machine: Machine::Model2(sys),
            set,
            ..
        }) = self.session.as_mut()
        else {
            return;
        };
        match savestate::load_from_file(sys, set, slot) {
            Ok(p) => log::info!(target: "state", "loaded slot {slot} <- {}", p.display()),
            Err(e) => log::error!(target: "state", "load failed: {e}"),
        }
    }

    /// Runs emulated time forward to catch up with the wall clock.
    fn advance(&mut self) {
        // The screen's controls are sampled once per displayed frame, not once
        // per emulated one: catching up several frames must not replay a thumb
        // press as several distinct presses.
        if let Some(session) = &mut self.session {
            session.input.set_touch(self.touch.amounts());
            if let Some((x, y)) = self.touch.aim() {
                session.input.on_cursor(x, y);
            }
        }
        let Some(session) = &mut self.session else {
            self.last_frame = Instant::now();
            return;
        };
        if self.paused {
            self.last_frame = Instant::now();
            return;
        }
        if self.fast_forward {
            // Run a burst per displayed frame rather than chasing the clock,
            // which is what makes it fast rather than merely uncapped.
            for _ in 0..FAST_FORWARD_FRAMES {
                session.step();
                self.emulated += 1;
            }
            self.last_frame = Instant::now();
            return;
        }
        // Emulated time follows the board's raster clock. Rendering may drop
        // frames; the machine must never run in slow motion because of it.
        let now = Instant::now();
        let mut catch_up = 0;
        while now.duration_since(self.last_frame) >= self.frame_duration && catch_up < MAX_CATCH_UP
        {
            self.last_frame += self.frame_duration;
            session.step();
            self.emulated += 1;
            catch_up += 1;
        }
        if catch_up == MAX_CATCH_UP && now.duration_since(self.last_frame) >= self.frame_duration {
            self.last_frame = now;
        }
    }

    fn redraw(&mut self, window: &Window) {
        let Some((native_width, exact, model1_compute)) = self.presenter.as_ref().map(|p| {
            (
                p.video.native_width() as f32,
                p.video.exact_compute_enabled(),
                p.video.model1_compute_enabled(),
            )
        }) else {
            return;
        };
        let render_start = Instant::now();
        let layers = self.draw_layers(native_width, exact, model1_compute);
        self.stats.render_ms = render_start.elapsed().as_secs_f32() * 1000.0;

        let now = Instant::now();
        let dt = now.duration_since(self.last_present);
        self.last_present = now;

        let title = self.session.as_ref().map(|s| s.title.clone());
        // Destructured so the renderer, the interface, the settings and the
        // session are borrowed as the separate fields they are: a frame needs
        // several at once, and going through `self` methods would not allow it.
        let App {
            presenter,
            gui,
            session,
            config,
            bindings,
            stats,
            fullscreen,
            idle_background,
            idle_foreground,
            touch,
            ..
        } = self;
        let Some(Presenter { video, ui }) = presenter else {
            return;
        };
        // The controls follow the cabinet, so the overlay is told which one is
        // loaded before it draws itself.
        touch.set_scheme(session.as_ref().map(|s| s.scheme));
        // Fullscreen is the "get out of the way" mode: the interface is
        // suppressed there whatever the player last toggled, so the picture
        // fills the screen with nothing over it. A handset is always
        // fullscreen, which is why its interface is not one of these windows.
        gui.set_suppressed(*fullscreen);
        let actions = gui.frame(
            ui,
            dt,
            title.as_deref(),
            config,
            bindings,
            *stats,
            Some(touch),
        );
        let empty = Vec::new();
        match layers {
            Layers::Model2 { triangles, colors } => {
                let (fb, fg, tex0, tex1, luma) = match session
                    .as_ref()
                    .map(|s| (&s.background, &s.foreground, &s.machine))
                {
                    Some((bg, fg, Machine::Model2(sys))) => (
                        bg,
                        fg,
                        sys.texture_ram0.as_slice(),
                        sys.texture_ram1.as_slice(),
                        sys.luma_ram.as_slice(),
                    ),
                    _ => (&empty, &empty, [].as_slice(), [].as_slice(), [].as_slice()),
                };
                video.present(Some(ui), fb, &colors, tex0, tex1, luma, &triangles, fg);
            }
            Layers::Model1 { quads } => {
                let (fb, fg) = match session.as_ref() {
                    Some(s) => (&s.background, &s.foreground),
                    None => (&*idle_background, &*idle_foreground),
                };
                video.present_model1(Some(ui), fb, &quads, fg);
            }
        }

        self.presented += 1;
        let elapsed = self.stats_at.elapsed().as_secs_f32();
        if elapsed >= 0.5 {
            self.stats.video_fps = self.presented as f32 / elapsed;
            self.stats.emulated_fps = self.emulated as f32 / elapsed;
            self.presented = 0;
            self.emulated = 0;
            self.stats_at = Instant::now();
        }

        if let Some((awaiting, key)) = self.gui.take_capture() {
            match awaiting {
                crate::gui::Awaiting::Hotkey(hotkey) => self.bindings.bind_hotkey(hotkey, key),
                crate::gui::Awaiting::Control(control) => {
                    // Rebinding replaces the keyboard sources and keeps the pad
                    // ones, so binding a key does not silently unbind the pad.
                    let mut sources: Vec<crate::bindings::Source> = self
                        .bindings
                        .sources(control)
                        .iter()
                        .copied()
                        .filter(|s| !matches!(s, crate::bindings::Source::Key(_)))
                        .collect();
                    sources.insert(0, crate::bindings::Source::Key(key));
                    self.bindings.bind(control, sources);
                }
            }
            self.apply_bindings();
        }

        self.apply(window, actions);
    }

    fn apply(&mut self, window: &Window, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Launch(path) => self.launch(window, &path),
                Action::CloseGame => self.close_game(window),
                Action::Reset => {
                    let config = self.config.clone();
                    if let Some(session) = &mut self.session {
                        session.reset(&config);
                    }
                }
                Action::SaveState(slot) => self.save_state(slot),
                Action::LoadState(slot) => self.load_state(slot),
                Action::Debug(line) => self.run_debug_command(&line),
                Action::SettingsChanged => {
                    window.set_fullscreen(if self.config.fullscreen {
                        Some(Fullscreen::Borderless(None))
                    } else {
                        None
                    });
                    self.fullscreen = self.config.fullscreen;
                    if let Some(session) = &mut self.session {
                        session.audio.set_volume(self.config.volume);
                        session.input.enable_rumble(self.config.rumble);
                    }
                    // Supersampling and the widescreen framing shape the
                    // pipelines themselves, so editing one only takes effect
                    // once the renderer has been built again.
                    if self.video_settings != VideoSettings::of(&self.config) {
                        self.build_presenter(window);
                    }
                    self.save_settings();
                }
                Action::BindingsChanged => self.apply_bindings(),
                Action::RefreshLibrary => {
                    let config = self.config.clone();
                    self.gui.refresh_library(&config);
                }
                Action::Quit => {
                    if let Some(session) = &self.session {
                        session.save_nvram();
                    }
                    std::process::exit(0);
                }
            }
        }
    }

    /// Pushes the edited bindings to the running machine and to disk, so a
    /// rebinding takes effect at once and survives a restart.
    fn apply_bindings(&mut self) {
        if let Some(session) = &mut self.session {
            session.input.set_bindings(self.bindings.clone());
        }
        if let Err(e) = self.bindings.save(&Bindings::path()) {
            log::error!(target: "input", "cannot save bindings: {e}");
        }
    }

    /// Writes the current adjustments out, so the next run starts the way this
    /// one was left.
    fn save_settings(&mut self) {
        let settings = crate::settings::Settings::from_config(&self.config);
        if let Err(e) = settings.save(&crate::settings::Settings::path()) {
            log::error!(target: "settings", "cannot save settings: {e}");
        }
    }

    /// The console drives the same engine the command line does, over a machine
    /// of its own: sharing the running one would let a `run` command fight the
    /// frame loop for the same state.
    fn run_debug_command(&mut self, line: &str) {
        if self.debugger.is_none() {
            if self.config.rom_path.is_empty() {
                self.gui
                    .push_debug_output(["error cmd=open reason=no romset loaded".to_string()]);
                return;
            }
            match Debugger::open(&self.config.rom_path) {
                Ok(d) => self.debugger = Some(d),
                Err(e) => {
                    self.gui
                        .push_debug_output([format!("error cmd=open reason={e}")]);
                    return;
                }
            }
        }
        let debugger = self.debugger.as_mut().expect("just opened");
        self.gui.push_debug_output([format!("> {line}")]);
        debugger.exec(line);
        let out = debugger.take_output();
        self.gui.push_debug_output(out);
    }

    /// Draws the tile layers and produces whatever geometry the compute pass
    /// needs, into the session's own buffers.
    fn draw_layers(&mut self, native_width: f32, exact: bool, model1_compute: bool) -> Layers {
        let Some(session) = &mut self.session else {
            self.attract.background(&mut self.idle_background);
            self.attract.foreground(&mut self.idle_foreground);
            return Layers::Model1 {
                quads: self.attract.quads(native_width as u32),
            };
        };

        match &mut session.machine {
            Machine::Model2(sys) => {
                if exact {
                    tilemap::render_background(&**sys, &mut session.background);
                    tilemap::render_foreground(&**sys, &mut session.foreground);
                } else {
                    tilemap::render(sys, &mut session.background);
                    session.foreground.fill(0);
                }
                if session.scheme == ControlScheme::Gun {
                    let (x, y) = session.input.aim();
                    let target = if exact {
                        &mut session.foreground
                    } else {
                        &mut session.background
                    };
                    draw_crosshair(
                        target,
                        (x * SCREEN_W as f32) as i32,
                        (y * SCREEN_H as f32) as i32,
                    );
                }
                Layers::Model2 {
                    triangles: if exact {
                        tgpulse_core::geometry::gpu_raster_triangles_ws(sys, native_width)
                    } else {
                        Vec::new()
                    },
                    colors: if exact {
                        tgpulse_core::geometry::gpu_color_lut(sys)
                    } else {
                        Vec::new()
                    },
                }
            }
            Machine::Model1(sys) => {
                tilemap::render_background(&**sys, &mut session.background);
                let quads = if model1_compute {
                    tilemap::render_foreground(&**sys, &mut session.foreground);
                    model1_video::gpu_quads_ws(sys, native_width)
                } else {
                    model1_video::render_below_hud(sys, &mut session.background);
                    tilemap::render_foreground(&**sys, &mut session.foreground);
                    for (dst, &src) in session.background.iter_mut().zip(session.foreground.iter())
                    {
                        if src != 0 {
                            *dst = src;
                        }
                    }
                    session.foreground.fill(0);
                    Vec::new()
                };
                Layers::Model1 { quads }
            }
        }
    }
}

/// An Android gamepad button, named as the `gilrs::Button` a desktop pad would
/// report for it.
///
/// Translating into gilrs' vocabulary rather than inventing a second one means
/// the default bindings -- and anything the player has since rebound -- resolve
/// identically on a handset and on a desktop. The numbers are the framework's
/// own `KEYCODE_BUTTON_*` constants.
#[cfg(target_os = "android")]
fn android_pad_button(code: u32) -> Option<gilrs::Button> {
    use gilrs::Button;
    Some(match code {
        96 => Button::South,          // BUTTON_A
        97 => Button::East,           // BUTTON_B
        99 => Button::West,           // BUTTON_X
        100 => Button::North,         // BUTTON_Y
        102 => Button::LeftTrigger,   // BUTTON_L1
        103 => Button::RightTrigger,  // BUTTON_R1
        104 => Button::LeftTrigger2,  // BUTTON_L2
        105 => Button::RightTrigger2, // BUTTON_R2
        106 => Button::LeftThumb,     // BUTTON_THUMBL
        107 => Button::RightThumb,    // BUTTON_THUMBR
        108 => Button::Start,         // BUTTON_START
        109 => Button::Select,        // BUTTON_SELECT
        110 => Button::Mode,          // BUTTON_MODE
        _ => return None,
    })
}

/// The parts of the configuration the renderer bakes in at construction.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct VideoSettings {
    ssaa: u32,
    smooth_shadows: bool,
    widescreen: bool,
    stretch_2d: bool,
}

impl VideoSettings {
    fn of(config: &Config) -> Self {
        Self {
            ssaa: config.ssaa,
            smooth_shadows: config.smooth_shadows,
            widescreen: config.widescreen,
            stretch_2d: config.widescreen_stretch_2d,
        }
    }
}

enum Layers {
    Model2 {
        triangles: Vec<tgpulse_core::geometry::GpuRasterTriangle>,
        colors: Vec<u32>,
    },
    Model1 {
        quads: Vec<tgpulse_core::model1_video::GpuQuad>,
    },
}

/// A lightgun crosshair, drawn into an ARGB `SCREEN_W`x`SCREEN_H` layer. Red
/// arms with a black edge, so it stays readable over any scene, and a gap in
/// the middle so it does not hide what is being aimed at.
fn draw_crosshair(buf: &mut [u32], cx: i32, cy: i32) {
    const RED: u32 = 0xFFFF_2020;
    const BLACK: u32 = 0xFF00_0000;
    const ARM: i32 = 12;
    const GAP: i32 = 3;

    let mut put = |x: i32, y: i32, c: u32| {
        if (0..SCREEN_W as i32).contains(&x) && (0..SCREEN_H as i32).contains(&y) {
            buf[y as usize * SCREEN_W + x as usize] = c;
        }
    };
    for d in GAP..=ARM {
        for &s in &[-d, d] {
            put(cx + s, cy, RED);
            put(cx + s, cy + 1, RED);
            put(cx + s, cy - 1, BLACK);
            put(cx + s, cy + 2, BLACK);
            put(cx, cy + s, RED);
            put(cx + 1, cy + s, RED);
            put(cx - 1, cy + s, BLACK);
            put(cx + 2, cy + s, BLACK);
        }
    }
    put(cx, cy, RED);
}
