//! Shared execution helper for the integration tests.
//!
//! The assembled test programs are the dynarec's correctness oracle: with
//! the `jit` feature on (the default) every program runs through the JIT,
//! exercising the lowering and the fallback against the same assertions the
//! interpreter has always passed.
//!
//! * `I960_JIT=0` forces the interpreter, for A/B runs.
//! * `I960_DUALRUN=1` runs interpreter and JIT in lockstep on a recorded bus
//!   trace and diffs architectural state after every 64-cycle chunk.

use i960::bus::Bus;
use i960::cpu::I960Cpu;

#[cfg(feature = "jit")]
pub fn run<B: Bus + 'static>(cpu: &mut I960Cpu, bus: &mut B, cycles: i32) {
    let jit_on = std::env::var("I960_JIT").map(|v| v != "0").unwrap_or(true);
    if !jit_on {
        cpu.execute_run(bus, cycles);
    } else if std::env::var_os("I960_DUALRUN").is_some() {
        i960::cpu::jit::dualrun::run(cpu, bus, cycles);
    } else {
        cpu.execute_run_jit(bus, cycles);
    }
}

#[cfg(not(feature = "jit"))]
pub fn run<B: Bus + 'static>(cpu: &mut I960Cpu, bus: &mut B, cycles: i32) {
    cpu.execute_run(bus, cycles);
}
