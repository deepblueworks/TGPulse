//! TGPulse: a Sega Model 1 and Model 2 arcade emulator.
//!
//! This binary is the front end. The machine itself lives in `tgpulse-core`
//! and knows nothing about windows, GPUs or controllers.

mod app;
mod attract;
mod bindings;
mod cli;
mod gui;
mod input;
mod platform;
mod settings;
mod touch;

use std::io::{BufRead, Write};

use tgpulse_core::debugger::Debugger;

fn main() {
    // Diagnostics are off unless asked for. `RUST_LOG=info` gives the running
    // commentary; individual subsystems have their own targets, so
    // `RUST_LOG=warn,geo=trace` narrows it to the geometry engine.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = match cli::parse() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let result = match args.command {
        cli::Command::Message(text) => {
            println!("{text}");
            Ok(())
        }
        cli::Command::ListRoms => {
            print!("{}", cli::list_roms(&args.config));
            Ok(())
        }
        cli::Command::Debug { rom, script } => run_debugger(&rom, script),
        cli::Command::Run { rom } => app::run(args.config, rom),
    };

    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

/// The scriptable debugger, driven from a script, a `-c` string or stdin.
fn run_debugger(rom: &std::path::Path, script: cli::Script) -> Result<(), String> {
    let mut debugger = Debugger::open(&rom.to_string_lossy())?;
    println!("ready game={} rom={}", debugger.game, rom.display());

    fn run(debugger: &mut Debugger, line: &str) -> bool {
        let keep_going = debugger.exec(line);
        let mut out = std::io::stdout().lock();
        for line in debugger.take_output() {
            let _ = writeln!(out, "{line}");
        }
        let _ = out.flush();
        keep_going
    }

    match script {
        cli::Script::Inline(commands) => {
            for line in commands {
                if !run(&mut debugger, &line) {
                    break;
                }
            }
        }
        cli::Script::File(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            for line in text.lines() {
                if !run(&mut debugger, line) {
                    break;
                }
            }
        }
        cli::Script::Stdin => {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if !run(&mut debugger, &line) {
                    break;
                }
            }
        }
    }
    Ok(())
}
