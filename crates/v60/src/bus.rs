//! Memory/IO interface the V60 talks to. On Model 1 the data bus is 16 bits
//! and the address bus 24, but the CPU issues byte/word/dword accesses that may
//! be unaligned, so the bus exposes all three widths little-endian. The
//! implementation is responsible for masking the address to its bus width.

pub trait Bus {
    fn read_u8(&mut self, addr: u32) -> u8;
    fn write_u8(&mut self, addr: u32, val: u8);

    /// Live state of the maskable IRQ line straight from the interrupt
    /// controller. The CPU latches its line via `assert_irq`, but a handler
    /// that acknowledges the controller *mid-instruction* (Model 1's ISR writes
    /// the GLUE irq-control port before `reti`) clears the device status while
    /// the latch is still asserted -- without this the CPU re-takes the same
    /// level after `reti`, running the vblank handler twice per frame. Buses
    /// that model an interrupt controller return `Some(active)`; the default
    /// `None` keeps the latched behaviour for simple buses/tests.
    fn irq_active(&self) -> Option<bool> {
        None
    }

    fn read_u16(&mut self, addr: u32) -> u16 {
        u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr.wrapping_add(1))])
    }
    fn write_u16(&mut self, addr: u32, val: u16) {
        let b = val.to_le_bytes();
        self.write_u8(addr, b[0]);
        self.write_u8(addr.wrapping_add(1), b[1]);
    }
    fn read_u32(&mut self, addr: u32) -> u32 {
        u32::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr.wrapping_add(1)),
            self.read_u8(addr.wrapping_add(2)),
            self.read_u8(addr.wrapping_add(3)),
        ])
    }
    fn write_u32(&mut self, addr: u32, val: u32) {
        let b = val.to_le_bytes();
        for (i, &byte) in b.iter().enumerate() {
            self.write_u8(addr.wrapping_add(i as u32), byte);
        }
    }

    /// IO space (`in`/`out` opcodes). The V60 has a distinct I/O space; a bus
    /// that does not separate it can leave these defaulting to program space.
    fn read_io8(&mut self, addr: u32) -> u8 {
        self.read_u8(addr)
    }
    fn write_io8(&mut self, addr: u32, val: u8) {
        self.write_u8(addr, val)
    }
    fn read_io16(&mut self, addr: u32) -> u16 {
        self.read_u16(addr)
    }
    fn write_io16(&mut self, addr: u32, val: u16) {
        self.write_u16(addr, val)
    }
    fn read_io32(&mut self, addr: u32) -> u32 {
        self.read_u32(addr)
    }
    fn write_io32(&mut self, addr: u32, val: u32) {
        self.write_u32(addr, val)
    }

    /// Whether external board logic has halted the CPU. The current
    /// instruction completes before this is sampled.
    fn halt_requested(&self) -> bool {
        false
    }
}
