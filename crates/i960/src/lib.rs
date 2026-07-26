pub mod bus;
pub mod cpu;
pub mod disasm; // If you want to use the disassembler for debugging

// Re-export major types to the root of the crate for easy access
// This allows you to do `use i960::I960Cpu` instead of `use i960::cpu::defs::I960Cpu`
pub use bus::Bus;
pub use cpu::defs::{StallState, FP, PFP, RIP, SP};
pub use cpu::int_controller::{I960_IRQ0, I960_IRQ1, I960_IRQ2, I960_IRQ3};
pub use cpu::I960Cpu;
pub use disasm::I960Disassembler;
