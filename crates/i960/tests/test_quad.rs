// examples/test_quad.rs

mod common;

use i960::bus::Bus;
use i960::cpu::I960Cpu;

// --- Mock System (Boilerplate) ---
// This acts as our motherboard/RAM for the test
struct System {
    ram: [u8; 0x10000],
}

impl System {
    fn new() -> Self {
        Self { ram: [0; 0x10000] }
    }

    // Helper to write 32-bit values (Little Endian)
    fn write_u32(&mut self, addr: u32, val: u32) {
        let addr = addr as usize;
        if addr + 4 <= self.ram.len() {
            self.ram[addr] = (val & 0xFF) as u8;
            self.ram[addr + 1] = ((val >> 8) & 0xFF) as u8;
            self.ram[addr + 2] = ((val >> 16) & 0xFF) as u8;
            self.ram[addr + 3] = ((val >> 24) & 0xFF) as u8;
        }
    }

    // Helper to read 32-bit values
    fn read_u32(&mut self, addr: u32) -> u32 {
        let addr = addr as usize;
        if addr + 4 <= self.ram.len() {
            let b0 = self.ram[addr] as u32;
            let b1 = self.ram[addr + 1] as u32;
            let b2 = self.ram[addr + 2] as u32;
            let b3 = self.ram[addr + 3] as u32;
            b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
        } else {
            0
        }
    }

    fn load_program(&mut self, addr: u32, opcodes: &[u32]) {
        for (i, &word) in opcodes.iter().enumerate() {
            self.write_u32(addr + (i as u32 * 4), word);
        }
    }
}

// Implement the Bus trait so the CPU can use it
impl Bus for System {
    fn read_byte(&mut self, addr: u32) -> u8 {
        if (addr as usize) < self.ram.len() {
            self.ram[addr as usize]
        } else {
            0
        }
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if (addr as usize) < self.ram.len() {
            self.ram[addr as usize] = val;
        }
    }
}

/// The examples start the CPU by hand rather than through reset, so it has no
/// PRCB. Give it one pointing at scratch RAM with a zeroed interrupt table.
/// Without it `prcb + 20` reads back 0, and the pending-interrupt scan then
/// treats the program itself as the interrupt table -- a first opcode with bit
/// 31 set reads as a pending NMI, and the scan clears the bit back out of it.
fn setup_interrupts(sys: &mut System, cpu: &mut I960Cpu) {
    const PRCB: u32 = 0x0F00;
    const INT_TABLE: u32 = 0x0F80;
    cpu.prcb = PRCB;
    sys.write_u32(PRCB + 20, INT_TABLE);
}

#[test]
fn test_quad() {
    println!("--- Starting i960 Quad-Word (ldq/stq) Verification ---");

    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    // 1. Setup Data Pattern in Memory at 0x100
    // We simulate 4 floats (Vertices data: X, Y, Z, Padding)
    // 0x11111111, 0x22222222, 0x33333333, 0x44444444
    let data = [0x11111111, 0x22222222, 0x33333333, 0x44444444];
    sys.write_u32(0x100, data[0]);
    sys.write_u32(0x104, data[1]);
    sys.write_u32(0x108, data[2]);
    sys.write_u32(0x10C, data[3]);

    println!("Initialized Memory[0x100..0x110] with pattern.");

    // 2. The Program
    // We will use register r4 (Index 4) as the base destination.
    // i960 requires quad transfers to align to registers 0, 4, 8, 12...

    // Instruction 1: ldq 0x100, r4
    // Op: 0xB0 (ldq)
    // Reg: r4 (0x4 << 19) = 0x00200000
    // Address: 0x100
    // Encoding: 0xB0200100

    // Instruction 2: stq r4, 0x200
    // Op: 0xB2 (stq)
    // Reg: r4 (0x4 << 19) = 0x00200000
    // Address: 0x200
    // Encoding: 0xB2200200

    // Instruction 3: b. (Halt)

    let program = [
        0xB0200100, // ldq 0x100, r4
        0xB2200200, // stq r4, 0x200
        0x08000000, // b .
    ];

    sys.load_program(0x00, &program);
    cpu.ip = 0x00;

    // 3. Run CPU
    // Quad instructions take multiple cycles. Running 50 cycles is plenty.
    println!("Executing Program...");
    common::run(&mut cpu, &mut sys, 50);

    // 4. Verify Load Logic (ldq)
    // Did r4, r5, r6, r7 get loaded correctly?
    println!("\n[Check 1] Verifying Register Loads (ldq)...");
    let r4 = cpu.r[4];
    let r5 = cpu.r[5];
    let r6 = cpu.r[6];
    let r7 = cpu.r[7];

    println!("  r4: {:08X} (Expected: {:08X})", r4, data[0]);
    println!("  r5: {:08X} (Expected: {:08X})", r5, data[1]);
    println!("  r6: {:08X} (Expected: {:08X})", r6, data[2]);
    println!("  r7: {:08X} (Expected: {:08X})", r7, data[3]);

    assert_eq!(r4, data[0], "r4 load failed");
    assert_eq!(r5, data[1], "r5 load failed");
    assert_eq!(r6, data[2], "r6 load failed");
    assert_eq!(r7, data[3], "r7 load failed");
    println!("  -> PASS");

    // 5. Verify Store Logic (stq)
    // Did memory at 0x200 receive the data?
    println!("\n[Check 2] Verifying Memory Stores (stq)...");
    let m0 = sys.read_u32(0x200);
    let m1 = sys.read_u32(0x204);
    let m2 = sys.read_u32(0x208);
    let m3 = sys.read_u32(0x20C);

    println!("  Mem[0x200]: {:08X}", m0);
    println!("  Mem[0x204]: {:08X}", m1);
    println!("  Mem[0x208]: {:08X}", m2);
    println!("  Mem[0x20C]: {:08X}", m3);

    assert_eq!(m0, data[0], "Store index 0 failed");
    assert_eq!(m1, data[1], "Store index 1 failed");
    assert_eq!(m2, data[2], "Store index 2 failed");
    assert_eq!(m3, data[3], "Store index 3 failed");
    println!("  -> PASS");

    println!("\n--- Test 'Quad-Word' Completed Successfully ---");
}
