pub mod core;
pub mod defs;
pub mod int_controller;
pub mod opcodes;
pub mod utils; // <--- Add this line

pub use defs::I960Cpu;
pub use defs::{FP, PFP, RIP, SP};
// Re-export IRQ constants for easy access
pub use int_controller::{I960_IRQ0, I960_IRQ1, I960_IRQ2, I960_IRQ3};
