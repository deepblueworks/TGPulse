//! Command line parsing.
//!
//! The emulator is usable two ways and this decides which: name a romset and it
//! boots straight into it, name nothing and it opens the library window. Either
//! way the same settings apply, so every option here is also reachable from the
//! GUI's settings panel.

use std::path::PathBuf;

use tgpulse_core::config::Config;
use tgpulse_core::library;

/// What the front end should do once arguments are understood.
pub enum Command {
    /// Open the window; boot straight into a romset if one was named.
    Run { rom: Option<PathBuf> },
    /// Print the contents of the ROM directory and exit.
    ListRoms,
    /// Run the scriptable debugger over a romset.
    Debug { rom: PathBuf, script: Script },
    /// Print a message and exit successfully (`--help`, `--version`).
    Message(String),
}

/// Where the debugger reads its commands from.
pub enum Script {
    Inline(Vec<String>),
    File(PathBuf),
    Stdin,
}

pub struct Args {
    pub command: Command,
    pub config: Config,
}

/// Parses `std::env::args`. The error is a message ready to print.
pub fn parse() -> Result<Args, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The saved adjustments are for the interactive application; the debugger
    // and the listings stay on the shipped defaults so scripts and captures
    // are reproducible no matter what the settings window last did.
    let headless = args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--debug" | "--list" | "--help" | "-h" | "--version" | "-V"
        )
    });
    let mut base = Config::default();
    if !headless {
        crate::settings::Settings::load_or_create(&crate::settings::Settings::path())
            .apply_to(&mut base);
    }
    parse_from(args, base)
}

fn parse_from(args: Vec<String>, mut config: Config) -> Result<Args, String> {
    let mut rom: Option<String> = None;
    let mut list = false;
    let mut debug = false;
    let mut script: Option<Script> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // `next` consumes the following argument, naming the option in the
        // error so a missing value says which option wanted one.
        let next = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--roms" => config.rom_dir = PathBuf::from(next(&mut i)?),
            "--rom" => rom = Some(next(&mut i)?),
            "--list" => list = true,
            "--debug" => debug = true,
            "-c" => script = Some(Script::Inline(split_commands(&next(&mut i)?))),
            "-f" => script = Some(Script::File(PathBuf::from(next(&mut i)?))),
            "--cabinet" => config.cabinet = next(&mut i)?.parse()?,
            "--ssaa" => {
                let v = next(&mut i)?;
                config.ssaa = v
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=4).contains(n))
                    .ok_or_else(|| format!("bad --ssaa '{v}' (want 1..4)"))?;
            }
            "--volume" => {
                let v = next(&mut i)?;
                config.volume = v
                    .parse()
                    .map_err(|_| format!("bad --volume '{v}' (want a percentage)"))?;
            }
            "--rumble" => config.rumble = on_off(arg, &next(&mut i)?)?,
            "--i960-jit" => config.i960_jit = on_off(arg, &next(&mut i)?)?,
            "--smooth-shadows" => config.smooth_shadows = on_off(arg, &next(&mut i)?)?,
            "--widescreen" => config.widescreen = on_off(arg, &next(&mut i)?)?,
            "--widescreen-stretch-2d" => {
                config.widescreen_stretch_2d = on_off(arg, &next(&mut i)?)?
            }
            "--fullscreen" => config.fullscreen = on_off(arg, &next(&mut i)?)?,
            "--version" | "-V" => {
                return Ok(Args {
                    command: Command::Message(format!("tgpulse {}", env!("CARGO_PKG_VERSION"))),
                    config,
                })
            }
            "--help" | "-h" => {
                return Ok(Args {
                    command: Command::Message(HELP.to_string()),
                    config,
                })
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'\n\n{HELP}"))
            }
            other => rom = Some(other.to_string()),
        }
        i += 1;
    }

    if list {
        return Ok(Args {
            command: Command::ListRoms,
            config,
        });
    }

    let rom = match rom {
        Some(r) => Some(resolve(&config, &r)?),
        None => None,
    };

    let command = if debug {
        let rom = rom.ok_or("--debug needs a romset")?;
        Command::Debug {
            rom,
            script: script.unwrap_or(Script::Stdin),
        }
    } else {
        Command::Run { rom }
    };

    Ok(Args { command, config })
}

/// Accepts either a path to an archive or a short set name to look up in the
/// ROM directory, so `tgpulse vf2` works as well as `tgpulse roms/vf2.zip`.
fn resolve(config: &Config, arg: &str) -> Result<PathBuf, String> {
    let direct = PathBuf::from(arg);
    if direct.is_file() {
        return Ok(direct);
    }
    let in_dir = config.rom_dir.join(format!("{arg}.zip"));
    if in_dir.is_file() {
        return Ok(in_dir);
    }
    if let Some(entry) = library::find(&config.rom_dir, arg) {
        return Ok(entry.path);
    }
    Err(format!(
        "no romset '{arg}': not a file, and not in {}",
        config.rom_dir.display()
    ))
}

fn on_off(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        other => Err(format!("bad {name} '{other}' (want on|off)")),
    }
}

/// Splits `-c "run 60; regs"` into separate commands.
fn split_commands(s: &str) -> Vec<String> {
    s.split(';').map(|c| c.trim().to_string()).collect()
}

/// Prints the ROM directory as a table.
pub fn list_roms(config: &Config) -> String {
    let entries = library::scan(&config.rom_dir);
    if entries.is_empty() {
        return format!(
            "No romsets in {}.\nPut zipped romsets there and try again.",
            config.rom_dir.display()
        );
    }
    let width = entries.iter().map(|e| e.set.len()).max().unwrap_or(8);
    let mut out = format!(
        "{} romset(s) in {}\n",
        entries.len(),
        config.rom_dir.display()
    );
    for e in &entries {
        let board = e.board.map(|b| b.label()).unwrap_or("-");
        out += &format!(
            "  {:<width$}  {:<8}  {:<4}  {}  [{}]\n",
            e.set,
            board,
            e.year,
            e.title,
            e.status(),
        );
    }
    out
}

pub const HELP: &str = "\
TGPulse - a Sega Model 1 and Model 2 arcade emulator

Usage:
  tgpulse                       Open the ROM library
  tgpulse <set|path>            Boot a romset straight away
  tgpulse --list                List the ROM directory and exit
  tgpulse <set> --debug [-c ..] Run the scriptable debugger

ROMs:
  --roms <dir>          Where romsets live (default: roms)

Video:
  --ssaa 1..4           Supersamples per output pixel on the 3D layer
                        (default 2). The board itself draws without
                        antialiasing; 1 reproduces that exactly.
  --widescreen on|off   Render 16:9 with a widened field of view rather
                        than stretching the 4:3 image (default off).
  --widescreen-stretch-2d on|off
                        Stretch the 2D tile layers to fill the widened
                        frame (default on).
  --smooth-shadows on|off
                        Blend the hardware's stipple transparency instead
                        of reproducing its checkerboard (default on).
  --fullscreen on|off   Start fullscreen (default off).

Audio:
  --volume <pct>        Output volume, as a percentage of the board's own
                        level (default 100). SCSP titles mix quiet.

Machine:
  --cabinet twin|single Whether the network board is fitted (default
                        single, which skips the game's link check).
  --i960-jit on|off     Execute the i960 with the Cranelift dynarec
                        (default on). Off is the interpreter reference
                        for A/B testing.

Debugger:
  -c \"cmd; cmd\"         Run these commands, then exit
  -f <file>             Run a command script, then exit
                        With neither, commands are read from stdin.

  -h, --help            Show this help
  -V, --version         Show the version
";
