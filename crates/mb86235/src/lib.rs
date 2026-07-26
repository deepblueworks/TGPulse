//! Fujitsu MB86235 "TGPx4" DSP, the geometry coprocessor on the Sega Model 2C
//! video board (Wave Runner, The House of the Dead, Top Skater, Sega Ski,...).
//!
//! The part
//! is a 64-bit-instruction VLIW DSP: each word carries an ALU operation and one
//! or two transfer operations that issue together. Instructions are dispatched
//! on `op[63:61]`, which selects how those slots are packed.
//!
//! On Model 2C the i960 uploads the microcode a 32-bit half at a time into the
//! 4096-entry program RAM, then releases the core; from there it reads command
//! words from the input FIFO, transforms geometry, and writes results to the
//! output FIFO -- the same contract the MB86234 TGP and the SHARC fulfil on the
//! earlier boards.

mod alu;
mod memory;
pub mod ops;
mod state;
mod trans;

pub use state::Mb86235;

/// The world outside the DSP's own program RAM: the Model 2 coprocessor FIFOs,
/// the buffer RAM it shares with the i960, and the coprocessor data ROM.
pub trait Mb86235Bus {
    /// Read a 32-bit word from the DSP's external data space.
    fn data_read(&mut self, addr: u32) -> u32;
    /// Write a 32-bit word to the DSP's external data space.
    fn data_write(&mut self, addr: u32, data: u32);
    /// Pop a command word from the input FIFO, if one is waiting.
    fn fifo_in_pop(&mut self) -> Option<u32>;
    /// Push a result word to the output FIFO.
    fn fifo_out_push(&mut self, data: u32);
    /// FIFO state, as the IFF/IFE/OFF/OFE branch conditions read it.
    fn fifo_in_empty(&self) -> bool {
        true
    }
    fn fifo_in_full(&self) -> bool {
        false
    }
    fn fifo_out_empty(&self) -> bool {
        true
    }
    fn fifo_out_full(&self) -> bool {
        false
    }
}
