// examples/verification.rs

use mb86233::{Mb86233, Mb86233Bus};

// --- Mock Bus Implementation ---
struct MockBus {
    program: Vec<u32>,
    data: Vec<u32>,
    io: Vec<u32>,
    rf: Vec<u32>,
}

impl MockBus {
    fn new(prog_size: usize) -> Self {
        Self {
            program: vec![0; prog_size],
            data: vec![0; 1024],
            io: vec![0; 1024],
            rf: vec![0; 32],
        }
    }

    fn load_program(&mut self, opcodes: &[u32]) {
        for (i, &op) in opcodes.iter().enumerate() {
            self.program[i] = op;
        }
    }

    // Helper to preload data for tests
    fn write_mem(&mut self, addr: usize, val: u32) {
        self.data[addr] = val;
    }
    fn write_io_mem(&mut self, addr: usize, val: u32) {
        self.io[addr] = val;
    }
}

impl Mb86233Bus for MockBus {
    fn read_program(&mut self, addr: u32) -> u32 {
        self.program.get(addr as usize).cloned().unwrap_or(0)
    }
    fn read_data(&mut self, addr: u32) -> u32 {
        self.data.get(addr as usize).cloned().unwrap_or(0)
    }
    fn write_data(&mut self, addr: u32, data: u32) {
        if let Some(cell) = self.data.get_mut(addr as usize) {
            *cell = data;
        }
    }
    fn read_io(&mut self, addr: u32) -> u32 {
        self.io.get(addr as usize).cloned().unwrap_or(0)
    }
    fn write_io(&mut self, addr: u32, data: u32) {
        if let Some(cell) = self.io.get_mut(addr as usize) {
            *cell = data;
        }
    }
    fn read_rf(&mut self, addr: u32) -> u32 {
        self.rf.get(addr as usize).cloned().unwrap_or(0)
    }
    fn write_rf(&mut self, addr: u32, data: u32) {
        if let Some(cell) = self.rf.get_mut(addr as usize) {
            *cell = data;
        }
    }
}

// --- Tests ---

fn run_verification() {
    let mut bus = MockBus::new(100);
    let mut cpu = Mb86233::new();

    // 1. Integer Math Test
    let ops_int = vec![0x50000005, 0x5900000A, 0x03600000];
    bus.load_program(&ops_int);
    cpu.reset();
    cpu.execute(&mut bus, 3);
    assert_eq!(cpu.d, 5);
    println!("Integer Math: PASS");

    // 2. Float Math Test
    bus.write_mem(0, 0x3FC00000); // 1.5
    bus.write_mem(1, 0x40200000); // 2.5
    let ops_float = vec![0x1C1DA000, 0x1C1DB201, 0x1CDF2010, 0x00000000];
    bus.load_program(&ops_float);
    cpu.reset();
    cpu.execute(&mut bus, 4);
    assert_eq!(cpu.d, 0x40800000);
    println!("Float Math: PASS");

    // 3. Loop Logic Test
    bus.write_mem(0, 1);
    bus.write_io_mem(0, 1);
    let ops_loop = vec![0x59000000, 0x50000001, 0x3C040004, 0x03440000];
    bus.load_program(&ops_loop);
    cpu.reset();
    cpu.execute(&mut bus, 7);
    assert_eq!(cpu.d, 4);
    println!("Loop Logic: PASS");
}

fn main() {
    println!("Running MB86233 Library Verification...");
    run_verification();
}
