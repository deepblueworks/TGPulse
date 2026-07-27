//! A scriptable debugger for the Model 1/2 machine.
//!
//! It is built for being driven by a program -- a script, a shell loop, or a
//! language model -- rather than typed at interactively:
//!
//!   * **Non-interactive.** Commands come from a file, from `-c`, or from
//!     stdin, run to completion, and the process exits with a status. There is
//!     no curses UI to get stuck in.
//!   * **Every line is parseable.** Output is `key=value` fields on one line
//!     per record, prefixed by the record kind, so `grep`/`awk` are enough to
//!     read it and no output ever needs to be interpreted from a screen.
//!   * **Every command reports.** A command that did nothing says so, and an
//!     error names the command and the reason instead of failing silently.
//!   * **Reproducible.** The machine is deterministic and save states are
//!     exact, so "run to here, poke, continue" can be replayed verbatim.
//!
//! Run `dbg <rom.zip> -c help` for the command list.

use crate::config::{Config, Inputs};
use crate::model1::Model1System;
use crate::{geometry, loader, model1_video, savestate, system::Model2System, tilemap};
use i960::Bus as _;
use v60::Bus as _;

/// Takes everything emitted since the last call.
impl Debugger {
    pub fn take_output(&mut self) -> Vec<String> {
        std::mem::take(&mut self.out)
    }
}

const HELP: &str = "\
commands (one per line; # starts a comment)
  run <frames>                 advance the machine, stopping early on a breakpoint
  until <addr> [maxframes]     run until the i960 reaches addr (default 600 frames) *
  break <addr>                 add an i960 breakpoint *
  unbreak [addr]               remove one breakpoint, or all of them *
  step [n]                     execute n i960 instructions, printing each *
  itrace <n> <file>            record the next n executed addresses to a file *
  regs [main|sharc|tgpx4|tgp]  dump a processor's registers
  state                        one-line summary of the whole machine
  mem <addr> [count]           hex dump of 32-bit words (default 16)
  poke <addr> <value>          write one 32-bit word
  pokeb <addr> <value>         write one byte
  memb <addr> [count]          hex dump of bytes
  find <value> <start> <end>   search memory for a 32-bit value
  dis <addr> [count]           disassemble i960 code (default 16) *
  geo                          display-list and geometry summary *
  irq                          interrupt request/enable, lines and last vector *
  fifo                         coprocessor FIFO depths and counters *
  input <field> <value>        set in0/in1/in2/steer/accel/brake/analogN
  coin [frames]                hold coin for a few frames, then start
  save <slot> | load <slot>    save state to/from states/<game>.<slot>.state *
  nvram save|defaults          write NVRAM, or record it as this game's default
  testmenu <frames>            hold the TEST button so the service menu opens
  screenshot <file.ppm>        write the current frame
  vertices                     triangle count of the current display list *
  echo <text>                  print a marker line
  quit                         stop reading commands
addresses and values accept 0x hex or decimal
* works on the Model 2 machine only";

/// Collects one output line, in the same `kind key=value` form the command
/// line prints.
macro_rules! out {
    ($self:ident, $($arg:tt)*) => {
        $self.out.push(format!($($arg)*))
    };
}

/// A debugging session over one machine.
///
/// The engine never writes to a stream of its own: `exec` appends to `out`,
/// which the caller drains. That is what lets the command line and the GUI
/// console share it.
pub struct Debugger {
    machine: Machine,
    pub game: String,
    pub frames: u64,
    out: Vec<String>,
}

/// The board the session is running. Most commands work on either; the ones
/// that name a processor or a Model 2 device are Model 2 only.
enum Machine {
    Model1(Box<Model1System>),
    Model2(Box<Model2System>),
}

fn num(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u32>()
            .ok()
            .or_else(|| u32::from_str_radix(s, 16).ok())
    }
}

impl Debugger {
    /// The Model 2 machine, for the commands the command loop has already
    /// established are Model 2 only.
    fn sys(&mut self) -> &mut Model2System {
        match &mut self.machine {
            Machine::Model2(sys) => sys,
            Machine::Model1(_) => unreachable!("guarded as model-2-only"),
        }
    }

    fn step_frame(&mut self) {
        match &mut self.machine {
            Machine::Model1(sys) => {
                sys.run_slice(crate::model1::CYCLES_PER_FRAME);
                sys.trigger_vblank();
            }
            Machine::Model2(sys) => {
                sys.run_slice(434_600);
                sys.trigger_vblank();
            }
        }
        self.frames += 1;
    }

    /// Runs up to `frames` frames, returning early if a breakpoint fires.
    /// Only the i960 has breakpoints; a Model 1 run always goes the distance.
    fn run(&mut self, frames: u64) -> Option<u32> {
        if let Machine::Model2(sys) = &mut self.machine {
            sys.main_cpu.bp_hit = None;
        }
        for _ in 0..frames {
            self.step_frame();
            if let Machine::Model2(sys) = &mut self.machine {
                if let Some(pc) = sys.main_cpu.bp_hit {
                    return Some(pc);
                }
            }
        }
        None
    }

    /// The cabinet inputs, which both boards keep in the same encoding.
    fn inputs(&mut self) -> &mut Inputs {
        match &mut self.machine {
            Machine::Model1(sys) => &mut sys.inputs,
            Machine::Model2(sys) => &mut sys.inputs,
        }
    }

    /// Pulls the worker thread's coprocessor state back into the system
    /// fields, so the read-only commands see the live DSP. A no-op on the
    /// single-threaded path.
    fn sync_copro(&mut self) {
        if let Machine::Model2(sys) = &mut self.machine {
            sys.copro_pause_sync_from_worker();
            sys.copro_resume();
        }
    }

    fn read_u32(&mut self, addr: u32) -> u32 {
        match &mut self.machine {
            Machine::Model1(sys) => sys.read_u32(addr),
            Machine::Model2(sys) => sys.read_u32(addr),
        }
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        match &mut self.machine {
            Machine::Model1(sys) => sys.write_u32(addr, value),
            Machine::Model2(sys) => sys.write_u32(addr, value),
        }
    }

    fn read_byte(&mut self, addr: u32) -> u8 {
        match &mut self.machine {
            Machine::Model1(sys) => sys.read_u8(addr),
            Machine::Model2(sys) => sys.read_byte(addr),
        }
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        match &mut self.machine {
            Machine::Model1(sys) => sys.write_u8(addr, value),
            Machine::Model2(sys) => sys.write_byte(addr, value),
        }
    }

    fn nvram_blocks(&self) -> (Vec<u8>, Vec<u8>) {
        match &self.machine {
            Machine::Model1(sys) => sys.nvram_blocks(),
            Machine::Model2(sys) => sys.nvram_blocks(),
        }
    }

    fn state_line(&self) -> String {
        match &self.machine {
            Machine::Model1(sys) => format!(
                "state frame={} v60_ppc={:08X} fifo_in={} fifo_out={}",
                self.frames,
                sys.main_cpu.ppc,
                sys.copro_fifo_in.len(),
                sys.copro_fifo_out.len(),
            ),
            Machine::Model2(sys) => format!(
                "state frame={} cycles={} i960_ip={:08X} sharc_pc={:06X} tgpx4_pc={:04X} \
                 fifo_in={} fifo_out={} geo_rd={:08X} geo_wr={:08X} irq={:08X}",
                self.frames,
                sys.machine_cycles,
                sys.main_cpu.ip,
                sys.sharc.pc,
                sys.tgpx4.pc,
                sys.copro_fifo_in.len(),
                sys.copro_fifo_out.len(),
                sys.geo_read_start_address,
                sys.geo_write_start_address,
                sys.irq_request,
            ),
        }
    }

    /// The current frame, composited the way the front end draws it.
    fn frame_pixels(&mut self) -> Vec<u32> {
        let mut fb = vec![0u32; tilemap::SCREEN_W * tilemap::SCREEN_H];
        match &mut self.machine {
            Machine::Model1(sys) => {
                tilemap::render_background(&**sys, &mut fb);
                model1_video::render_below_hud(sys, &mut fb);
                let mut fg = vec![0u32; tilemap::SCREEN_W * tilemap::SCREEN_H];
                tilemap::render_foreground(&**sys, &mut fg);
                for (dst, &src) in fb.iter_mut().zip(fg.iter()) {
                    if src != 0 {
                        *dst = src;
                    }
                }
            }
            Machine::Model2(sys) => tilemap::render(sys, &mut fb),
        }
        fb
    }

    /// Loads a romset and prepares a session over it, for whichever board the
    /// set belongs to.
    pub fn open(rom_path: &str) -> Result<Self, String> {
        Self::open_with_config(rom_path, Config::default())
    }

    /// `open` with a caller-supplied configuration (the front end passes its
    /// own through, so `--copro-mt off` reaches the machine here too).
    pub fn open_with_config(rom_path: &str, config: Config) -> Result<Self, String> {
        let cfg = Config {
            rom_path: rom_path.to_string(),
            ..config
        };
        let names = loader::archive_names(&cfg.rom_path).unwrap_or_default();
        let def = crate::roms_db::identify(&names);
        let game = def
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "unknown".into());

        let machine = if def.is_some_and(|d| d.board.is_model1()) {
            let roms = loader::load_model1_zip(&cfg.rom_path)?;
            let sys = Model1System::with_config(&roms, cfg);
            Machine::Model1(Box::new(sys))
        } else {
            let roms = loader::load_model2_zip(&cfg.rom_path)?;
            let mut sys = Model2System::with_config(&roms, cfg);
            sys.snapshot_game = game.clone();
            Machine::Model2(Box::new(sys))
        };

        let mut debugger = Self {
            machine,
            game,
            frames: 0,
            out: Vec::new(),
        };
        // The same battery-backed state the front end would load, so a session
        // here starts from the machine a player would see -- including the
        // shipped defaults that put a game in English.
        let (backup_len, eeprom_len) = match &debugger.machine {
            Machine::Model1(sys) => sys.nvram_sizes(),
            Machine::Model2(sys) => sys.nvram_sizes(),
        };
        if let Some((b, e)) = crate::nvram::load(&debugger.game, backup_len, eeprom_len) {
            match &mut debugger.machine {
                Machine::Model1(sys) => sys.set_nvram_blocks(&b, &e),
                Machine::Model2(sys) => sys.set_nvram_blocks(&b, &e),
            }
        }
        Ok(debugger)
    }

    /// The command reference, for `help` and for a usage message.
    pub fn help() -> &'static str {
        HELP
    }

    /// Runs one command line. Returns false when the session should end;
    /// whatever it produced is waiting in `take_output`.
    pub fn exec(&mut self, line: &str) -> bool {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return true;
        }
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("");
        let args: Vec<&str> = it.collect();
        let arg = |i: usize| args.get(i).copied();

        // Commands that name a processor or a Model 2 device only exist on the
        // Model 2 machine.
        const MODEL2_ONLY: &[&str] = &[
            "until", "break", "unbreak", "step", "itrace", "dis", "geo", "irq", "fifo", "vertices",
            "save", "load",
        ];
        if MODEL2_ONLY.contains(&cmd) && matches!(self.machine, Machine::Model1(_)) {
            out!(self, "error cmd={cmd} reason=model-2-only");
            return true;
        }

        match cmd {
            "help" => out!(self, "{HELP}"),
            "echo" => out!(self, "echo {}", args.join(" ")),
            "quit" | "exit" => return false,

            "run" => {
                let n = arg(0).and_then(num).unwrap_or(1) as u64;
                match self.run(n) {
                    Some(pc) => out!(
                        self,
                        "run stopped=breakpoint pc={pc:08X} frame={}",
                        self.frames
                    ),
                    None => out!(self, "run stopped=frames frame={}", self.frames),
                }
            }

            "until" => {
                let Some(addr) = arg(0).and_then(num) else {
                    out!(self, "error cmd=until reason=need-address");
                    return true;
                };
                let max = arg(1).and_then(num).unwrap_or(600) as u64;
                self.sys().main_cpu.breakpoints.push(addr);
                let hit = self.run(max);
                self.sys().main_cpu.breakpoints.retain(|a| *a != addr);
                match hit {
                    Some(pc) => out!(self, "until hit=1 pc={pc:08X} frame={}", self.frames),
                    None => out!(self, "until hit=0 frame={} note=not-reached", self.frames),
                }
            }

            "break" => match arg(0).and_then(num) {
                Some(a) => {
                    self.sys().main_cpu.breakpoints.push(a);
                    let count = self.sys().main_cpu.breakpoints.len();
                    out!(self, "break add={a:08X} count={count}");
                }
                None => out!(self, "error cmd=break reason=need-address"),
            },

            "unbreak" => match arg(0).and_then(num) {
                Some(a) => {
                    self.sys().main_cpu.breakpoints.retain(|x| *x != a);
                    let count = self.sys().main_cpu.breakpoints.len();
                    out!(self, "unbreak removed={a:08X} count={count}");
                }
                None => {
                    self.sys().main_cpu.breakpoints.clear();
                    out!(self, "unbreak removed=all count=0");
                }
            },

            "step" => {
                for _ in 0..arg(0).and_then(num).unwrap_or(1) {
                    let ip = self.sys().main_cpu.ip;
                    let text =
                        i960::disasm::I960Disassembler::disassemble(ip, |x| self.sys().read_u32(x));
                    out!(self, "step {ip:08X} {text}");
                    self.sys().step_instruction();
                }
            }

            // Records every instruction address executed, for diffing against
            // a reference emulator's trace.
            "itrace" => {
                let (Some(n), Some(path)) = (arg(0).and_then(num), arg(1)) else {
                    out!(self, "error cmd=itrace reason=need-count-and-path");
                    return true;
                };
                self.sys().main_cpu.trace = Some((vec![0u32; n as usize], 0));
                self.sys().main_cpu.trace_frozen = false;
                while !self.sys().main_cpu.trace_frozen {
                    self.step_frame();
                    if self.frames > 4000 {
                        break;
                    }
                }
                let (buf, pos) = self.sys().main_cpu.trace.take().unwrap();
                let text: String = buf[..pos].iter().map(|a| format!("{a:08X}\n")).collect();
                match std::fs::write(path, text) {
                    Ok(()) => out!(self, "itrace count={pos} path={path} frame={}", self.frames),
                    Err(e) => out!(self, "error cmd=itrace reason={e}"),
                }
            }

            "state" => {
                self.sync_copro();
                out!(self, "{}", self.state_line())
            }

            "regs" => {
                self.sync_copro();
                match &mut self.machine {
                Machine::Model1(sys) => match arg(0).unwrap_or("main") {
                    "main" | "v60" => {
                        let c = &sys.main_cpu;
                        out!(self, "regs cpu=v60 ppc={:08X}", c.ppc);
                        for (i, chunk) in c.reg[0..32].chunks(8).enumerate() {
                            let v: Vec<String> = chunk.iter().map(|x| format!("{x:08X}")).collect();
                            out!(self, "  r{}-{} {}", i * 8, i * 8 + 7, v.join(" "));
                        }
                    }
                    "tgp" => out!(self, "regs cpu=tgp pc={:04X}", sys.tgp_cpu.pc),
                    other => out!(self, "error cmd=regs reason=unknown-cpu value={other}"),
                },
                Machine::Model2(sys) => match arg(0).unwrap_or("main") {
                    "main" => {
                        let c = &sys.main_cpu;
                        out!(
                            self,
                            "regs cpu=main ip={:08X} ac={:08X} pc={:08X}",
                            c.ip,
                            c.ac,
                            c.pc
                        );
                        for (i, chunk) in c.r[0..16].chunks(8).enumerate() {
                            let v: Vec<String> = chunk.iter().map(|x| format!("{x:08X}")).collect();
                            out!(self, "  r{}-{} {}", i * 8, i * 8 + 7, v.join(" "));
                        }
                        for (i, chunk) in c.r[16..32].chunks(8).enumerate() {
                            let v: Vec<String> = chunk.iter().map(|x| format!("{x:08X}")).collect();
                            out!(self, "  g{}-{} {}", i * 8, i * 8 + 7, v.join(" "));
                        }
                    }
                    "sharc" => {
                        let s = &sys.sharc;
                        out!(
                            self,
                            "regs cpu=sharc pc={:06X} insns={} unimpl={} astat={:08X} stky={:08X}",
                            s.pc,
                            s.insns,
                            s.unimpl_count,
                            s.astat,
                            s.stky
                        );
                        let v: Vec<String> = s.r.iter().map(|x| format!("{x:08X}")).collect();
                        out!(self, "  r0-15 {}", v.join(" "));
                    }
                    "tgpx4" => {
                        let t = &sys.tgpx4;
                        out!(
                            self,
                            "regs cpu=tgpx4 pc={:04X} insns={} st={:08X} eb={:08X} eo={:08X} sp={:08X}",
                            t.pc, t.insns, t.st, t.eb, t.eo, t.sp
                        );
                        for (n, b) in [
                            ("aa", &t.aa),
                            ("ab", &t.ab),
                            ("ma", &t.ma),
                            ("mb", &t.mb),
                            ("ar", &t.ar),
                        ] {
                            let v: Vec<String> = b.iter().map(|x| format!("{x:08X}")).collect();
                            out!(self, "  {n} {}", v.join(" "));
                        }
                    }
                    "tgp" => out!(self, "regs cpu=tgp pc={:04X}", sys.tgp_cpu.pc),
                    other => out!(self, "error cmd=regs reason=unknown-cpu value={other}"),
                },
                }
            }

            "mem" => {
                let Some(base) = arg(0).and_then(num) else {
                    out!(self, "error cmd=mem reason=need-address");
                    return true;
                };
                let count = arg(1).and_then(num).unwrap_or(16);
                for row in 0..count.div_ceil(4) {
                    let a = base.wrapping_add(row * 16);
                    let w: Vec<String> = (0..4)
                        .map(|i| format!("{:08X}", self.read_u32(a.wrapping_add(i * 4))))
                        .collect();
                    out!(self, "mem {a:08X} {}", w.join(" "));
                }
            }

            "poke" => match (arg(0).and_then(num), arg(1).and_then(num)) {
                (Some(a), Some(v)) => {
                    self.write_u32(a, v);
                    out!(self, "poke {a:08X} <= {v:08X}");
                }
                _ => out!(self, "error cmd=poke reason=need-address-and-value"),
            },

            // Byte-granular access, for exercising a device the way the game
            // does rather than a word at a time.
            "pokeb" => match (arg(0).and_then(num), arg(1).and_then(num)) {
                (Some(a), Some(v)) => {
                    self.write_byte(a, v as u8);
                    out!(self, "pokeb {a:08X} <= {:02X}", v as u8);
                }
                _ => out!(self, "error cmd=pokeb reason=need-address-and-value"),
            },

            "memb" => {
                let Some(base) = arg(0).and_then(num) else {
                    out!(self, "error cmd=memb reason=need-address");
                    return true;
                };
                let n = arg(1).and_then(num).unwrap_or(8);
                let b: Vec<String> = (0..n)
                    .map(|i| format!("{:02X}", self.read_byte(base.wrapping_add(i))))
                    .collect();
                out!(self, "memb {base:08X} {}", b.join(" "));
            }

            "find" => {
                let (Some(v), Some(s), Some(e)) = (
                    arg(0).and_then(num),
                    arg(1).and_then(num),
                    arg(2).and_then(num),
                ) else {
                    out!(self, "error cmd=find reason=need-value-start-end");
                    return true;
                };
                let mut hits = 0;
                let mut a = s;
                while a < e {
                    if self.read_u32(a) == v {
                        out!(self, "find {a:08X}");
                        hits += 1;
                        if hits >= 64 {
                            out!(self, "find note=truncated-at-64");
                            break;
                        }
                    }
                    a = a.wrapping_add(4);
                }
                out!(self, "find hits={hits}");
            }

            "dis" => {
                let Some(mut a) = arg(0).and_then(num) else {
                    out!(self, "error cmd=dis reason=need-address");
                    return true;
                };
                for _ in 0..arg(1).and_then(num).unwrap_or(16) {
                    let word = self.sys().read_u32(a);
                    let text =
                        i960::disasm::I960Disassembler::disassemble(a, |x| self.sys().read_u32(x));
                    out!(self, "dis {a:08X} {word:08X} {text}");
                    a = a.wrapping_add(4);
                }
            }

            "geo" => {
                let st = geometry::inspect_display_list(self.sys());
                let cmds: Vec<String> = st
                    .commands
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| **c > 0)
                    .map(|(i, c)| format!("{i:02X}:{c}"))
                    .collect();
                let (rd, wr) = (
                    self.sys().geo_read_start_address,
                    self.sys().geo_write_start_address,
                );
                out!(
                    self,
                    "geo rd={rd:08X} wr={wr:08X} objects={} words={} ended={} malformed={} cmds=[{}]",
                    st.object_modes.iter().sum::<u32>(),
                    st.object_words,
                    st.ended,
                    st.malformed,
                    cmds.join(" ")
                );
            }

            "irq" => {
                let (req, ena, lines, vector, handler, taken) = (
                    self.sys().irq_request,
                    self.sys().irq_enable,
                    self.sys().main_cpu.irq_line_state,
                    self.sys().main_cpu.last_interrupt_vector,
                    self.sys().main_cpu.last_interrupt_handler,
                    self.sys().main_cpu.interrupt_count,
                );
                out!(
                    self,
                    "irq req={req:08X} ena={ena:08X} lines={lines:?} vector={vector} handler={handler:08X} taken={taken}"
                );
            }

            "fifo" => {
                self.sync_copro();
                let (fifo_in, fifo_out, copro_halted, main_stall, copro_stall) = (
                    self.sys().copro_fifo_in.len(),
                    self.sys().copro_fifo_out.len(),
                    self.sys().copro_halted,
                    self.sys().main_stall,
                    self.sys().copro_stall,
                );
                out!(
                    self,
                    "fifo in={fifo_in} out={fifo_out} copro_halted={copro_halted} main_stall={main_stall} copro_stall={copro_stall}"
                );
            }

            "vertices" => {
                let v = geometry::gpu_vertices(self.sys(), 496.0);
                out!(self, "vertices count={} triangles={}", v.len(), v.len() / 3);
            }

            "input" => match (arg(0), arg(1).and_then(num)) {
                (Some(f), Some(v)) => {
                    let ok = match f {
                        "in0" => {
                            self.inputs().in0 = v as u8;
                            true
                        }
                        "in1" => {
                            self.inputs().in1 = v as u8;
                            true
                        }
                        "in2" => {
                            self.inputs().in2 = v as u8;
                            true
                        }
                        // The three dedicated analog controls, which do not
                        // live in the ADC channel array.
                        "steer" => {
                            self.inputs().steer = v as u8;
                            true
                        }
                        "accel" => {
                            self.inputs().accel = v as u8;
                            true
                        }
                        "brake" => {
                            self.inputs().brake = v as u8;
                            true
                        }
                        _ => match f
                            .strip_prefix("analog")
                            .and_then(|n| n.parse::<usize>().ok())
                        {
                            Some(ch) if ch < 8 => {
                                self.inputs().analog[ch] = v as u8;
                                true
                            }
                            _ => false,
                        },
                    };
                    if ok {
                        out!(self, "input {f}={v:02X}");
                    } else {
                        out!(self, "error cmd=input reason=unknown-field value={f}");
                    }
                }
                _ => out!(self, "error cmd=input reason=need-field-and-value"),
            },

            // Getting a game to gameplay by hand is fiddly, so this bundles the
            // coin/start dance the cabinets expect.
            "coin" => {
                let start_bit = arg(0).and_then(num).unwrap_or(0x10) as u8;
                for i in 0..5 {
                    for _ in 0..12 {
                        self.inputs().in0 &= !0x01;
                        self.step_frame();
                    }
                    self.inputs().in0 |= 0x01;
                    for _ in 0..48 {
                        self.step_frame();
                    }
                    let _ = i;
                }
                for _ in 0..12 {
                    self.inputs().in0 &= !start_bit;
                    self.step_frame();
                }
                self.inputs().in0 |= start_bit;
                out!(
                    self,
                    "coin inserted=5 start_bit={start_bit:02X} frame={}",
                    self.frames
                );
            }

            "save" => {
                let slot = arg(0).and_then(num).unwrap_or(0);
                let Machine::Model2(sys) = &mut self.machine else {
                    unreachable!("guarded as model-2-only")
                };
                match savestate::save_to_file(sys, &self.game, slot) {
                    Ok(p) => out!(self, "save slot={slot} path={}", p.display()),
                    Err(e) => out!(self, "error cmd=save reason={e}"),
                }
            }

            "load" => {
                let slot = arg(0).and_then(num).unwrap_or(0);
                let result = match &mut self.machine {
                    Machine::Model2(sys) => savestate::load_from_file(sys, &self.game, slot),
                    Machine::Model1(_) => unreachable!("guarded as model-2-only"),
                };
                match result {
                    Ok(p) => out!(
                        self,
                        "load slot={slot} path={} {}",
                        p.display(),
                        self.state_line()
                    ),
                    Err(e) => out!(self, "error cmd=load reason={e}"),
                }
            }

            // Region, cabinet type and difficulty live in battery-backed RAM
            // on these machines, reachable only from the game's own test menu,
            // so configuring a game means: open the menu, set it, then record
            // the resulting NVRAM as the default for future first boots.
            "nvram" => {
                let (b, e) = self.nvram_blocks();
                match arg(0).unwrap_or("save") {
                    "save" => {
                        crate::nvram::save(&self.game, &b, &e);
                        out!(self, "nvram saved game={}", self.game);
                    }
                    "defaults" => match crate::nvram::save_defaults(&self.game, &b, &e) {
                        Ok(p) => out!(self, "nvram defaults path={}", p.display()),
                        Err(err) => out!(self, "error cmd=nvram reason={err}"),
                    },
                    other => out!(self, "error cmd=nvram reason=unknown-mode value={other}"),
                }
            }

            "testmenu" => {
                let n = arg(0).and_then(num).unwrap_or(120) as u64;
                for _ in 0..n {
                    self.inputs().in0 &= !0x04;
                    self.step_frame();
                }
                self.inputs().in0 |= 0x04;
                out!(self, "testmenu held={n} frame={}", self.frames);
            }

            // Dumps a memory range as raw little-endian words, for diffing
            // against a reference emulator frame by frame.
            "dump" => {
                let (Some(base), Some(len), Some(path)) =
                    (arg(0).and_then(num), arg(1).and_then(num), arg(2))
                else {
                    out!(self, "error cmd=dump reason=need-address-length-path");
                    return true;
                };
                let mut out = Vec::with_capacity(len as usize * 4);
                for i in 0..len {
                    out.extend_from_slice(&self.read_u32(base.wrapping_add(i * 4)).to_le_bytes());
                }
                match std::fs::write(path, &out) {
                    Ok(()) => out!(self, "dump base={base:08X} words={len} path={path}"),
                    Err(e) => out!(self, "error cmd=dump reason={e}"),
                }
            }

            "screenshot" => {
                let path = arg(0).unwrap_or("screen.ppm");
                let fb = self.frame_pixels();
                match write_ppm(path, &fb) {
                    Ok(()) => out!(self, "screenshot path={path}"),
                    Err(e) => out!(self, "error cmd=screenshot reason={e}"),
                }
            }

            other => out!(self, "error cmd={other} reason=unknown-command"),
        }
        true
    }
}

fn write_ppm(path: &str, fb: &[u32]) -> Result<(), String> {
    let (w, h) = (tilemap::SCREEN_W, tilemap::SCREEN_H);
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in fb {
        out.push((px >> 16) as u8);
        out.push((px >> 8) as u8);
        out.push(*px as u8);
    }
    std::fs::write(path, out).map_err(|e| e.to_string())
}
