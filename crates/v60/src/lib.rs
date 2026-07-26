//! NEC V60 (uPD70615) CPU core, for Sega Model 1.
//!
//!cpp`. The V60 is a byte-opcode CISC: the first byte
//! of each instruction selects a handler from a 256-entry table, each handler
//! consumes its operands through one of the addressing-mode readers and returns
//! the instruction's byte length. On Model 1 it runs with a 16-bit data bus and
//! a 24-bit address space (the V60 variant, not the 32-bit V70).
//!
//! This is the harness -- register file, bus, reset, fetch/dispatch -- with the
//! opcode and addressing-mode implementations filled in incrementally against
//! The reference. Every opcode not yet ported is counted rather than silently skipped,
//! so "what does the boot need next" is always answerable.

pub mod am;
pub mod bus;
pub mod cpu;
pub mod ops;

pub use bus::Bus;
pub use cpu::V60;
