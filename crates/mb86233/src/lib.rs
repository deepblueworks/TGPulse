//! Fujitsu MB86233 "TGP", the geometry coprocessor on the Sega Model 1 board
//! and on the Model 2 and 2A video boards.

pub mod addressing;
pub mod alu;
pub mod alu_logic;
pub mod cpu_state;
pub mod decode;
pub mod memory;
pub mod types;

pub use cpu_state::Mb86233;
pub use memory::Mb86233Bus;
pub use types::*;
