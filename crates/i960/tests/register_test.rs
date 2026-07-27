// src/main.rs
#![allow(unused)]

mod common;

use i960::bus::Bus;
use i960::cpu::I960Cpu;

// --- System Implementation ---
struct System {
    ram: [u8; 0x10000],
}

impl System {
    fn new() -> Self {
        Self { ram: [0; 0x10000] }
    }

    fn load_code(&mut self, addr: u32, code: &[u32]) {
        for (i, &word) in code.iter().enumerate() {
            self.write_u32(addr + (i as u32 * 4), word);
        }
    }
}

impl Bus for System {
    fn read_byte(&mut self, addr: u32) -> u8 {
        if (addr as usize) < self.ram.len() {
            self.ram[addr as usize]
        } else {
            0
        }
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if addr == 0x0800 {
            // Magic UART
            print!("{}", val as char);
            use std::io::Write;
            std::io::stdout().flush().unwrap();
        } else if (addr as usize) < self.ram.len() {
            self.ram[addr as usize] = val;
        }
    }
}

#[test]
fn register_test() {
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // REGISTER WINDOW TEST PROGRAM (CORRECTED)
    // ----------------------------

    let program_main = [
        0x8C200041, // 0x100: lda 0x41 ('A'), r4
        // call 0x120.
        // Displacement calculation:
        // Target (0x120) - Current IP after fetch (0x108) = 0x18.
        // Encoding requires (Val - 4), so we encode 0x1C (28).
        // 28 in hex is 1C.
        0x0900001C, // 0x104: call 0x120
        0x92200800, // 0x108: st r4, 0x0800 (UART) - Should print 'A'
        0x08000000, // 0x10C: b . (Infinite Loop)
    ];

    let program_sub = [
        0x8C200046, // 0x120: lda 0x46 ('F'), r4 (Overwrites r4 in THIS window)
        0x0a000000, // 0x124: ret (Restores previous window)
    ];

    println!("Loading Register Window Test...");
    sys.load_code(0x100, &program_main);
    sys.load_code(0x120, &program_sub);

    cpu.ip = 0x100;

    println!("Running CPU...");
    println!("-------------------------------------");
    println!("Expected Output: A");
    print!("Actual Output:   ");
    use std::io::Write;
    std::io::stdout().flush().unwrap();

    loop {
        common::run(&mut cpu, &mut sys, 100);

        // Break if stuck at infinite loop (0x10C)
        if cpu.ip == 0x10C {
            break;
        }

        // Safety break
        if cpu.ip > 0x200 {
            println!("\nError: CPU runaway to 0x{:x}", cpu.ip);
            break;
        }
    }
    println!("\n-------------------------------------");
    println!("Test Finished.");
}
