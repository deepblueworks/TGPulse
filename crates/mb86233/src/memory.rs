pub trait Mb86233Bus {
    fn read_program(&mut self, addr: u32) -> u32;
    fn read_data(&mut self, addr: u32) -> u32;
    fn write_data(&mut self, addr: u32, data: u32);
    fn read_io(&mut self, addr: u32) -> u32;
    fn write_io(&mut self, addr: u32, data: u32);
    // RF (Register File) is effectively a small RAM bank
    fn read_rf(&mut self, addr: u32) -> u32;
    fn write_rf(&mut self, addr: u32, data: u32);

    /// Returns (and clears) a pending stall request from the last bus access.
    ///
    /// A FIFO port that had no data to give raises this; the CPU then rewinds
    /// to the start of the instruction and retries it, matching the flow
    /// control of the coprocessor FIFO.
    fn take_stall(&mut self) -> bool {
        false
    }

    /// Level of an external GPIO pin, sampled by conditional branches.
    ///
    /// The pins are inputs wired to board logic (on Model 2, pin 0 carries the
    /// atan comparator's |a| <= |b| result), so their state lives with the
    /// board, not in the CPU: an IO write the CPU itself performs can change a
    /// pin level that its very next instruction branches on.
    fn gpio(&mut self, _index: u32) -> bool {
        false
    }

    /// Whether an external device has asserted HALT. Unlike `take_stall`,
    /// this completes the current instruction and stops before fetching the
    /// next one. Model 2 uses it when the 8-word TGP output FIFO becomes
    /// full.
    fn halt_requested(&self) -> bool {
        false
    }
}
