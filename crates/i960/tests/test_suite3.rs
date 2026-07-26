use i960::bus::Bus;
use i960::cpu::I960Cpu;

// --- Mock System Implementation (Bus) ---
struct System {
    ram: [u8; 0x10000],
}

impl System {
    fn new() -> Self {
        Self { ram: [0; 0x10000] }
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        let addr = addr as usize;
        if addr + 4 <= self.ram.len() {
            self.ram[addr] = (val & 0xFF) as u8;
            self.ram[addr + 1] = ((val >> 8) & 0xFF) as u8;
            self.ram[addr + 2] = ((val >> 16) & 0xFF) as u8;
            self.ram[addr + 3] = ((val >> 24) & 0xFF) as u8;
        }
    }

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

// --- Test Cases ---

#[test]
fn test_suite3() {
    println!("--- Starting i960 Advanced Instruction Test Suite ---");
    test_bit_ops();
    test_atomic_ops();
    test_burst_moves();
    println!("--- All Tests Finished ---");
}

fn test_bit_ops() {
    println!("\n[Test] Bit Manipulation (scanbit)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // 1. scanbit r3, r4
    let op_scanbit = 0x64200083;

    let program = [
        op_scanbit, 0x08000000, // b .
    ];

    sys.load_program(0x100, &program);
    cpu.ip = 0x100;
    cpu.r[3] = 0x00000080;

    cpu.execute_run(&mut sys, 10);

    println!("  [DEBUG] scanbit(0x80) result: {}", cpu.r[4]);
    assert_eq!(cpu.r[4], 7, "scanbit failed: Expected 7, got {}", cpu.r[4]);
    println!("  -> Passed");
}

fn test_atomic_ops() {
    println!("\n[Test] Atomic Operations (atadd)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // 1. atadd r3, r4, r5
    let op_atadd = 0x61290103;

    let program = [op_atadd, 0x08000000];

    sys.load_program(0x100, &program);
    sys.write_u32(0x200, 50); // Initial memory value

    cpu.ip = 0x100;
    cpu.r[3] = 0x200; // Address
    cpu.r[4] = 10; // Increment

    cpu.execute_run(&mut sys, 20);

    let mem_val = sys.read_u32(0x200);
    println!("  [DEBUG] Memory[0x200]: {} (Expected 60)", mem_val);
    println!("  [DEBUG] r5 (Old Val): {} (Expected 50)", cpu.r[5]);

    assert_eq!(mem_val, 60, "atadd failed: Memory was not incremented.");
    assert_eq!(
        cpu.r[5], 50,
        "atadd failed: Old value not stored in dest register."
    );
    println!("  -> Passed");
}

fn test_burst_moves() {
    println!("\n[Test] Burst Register Moves (movl, synmov)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // 1. movl r4, r6 (Opcode 0x5E)
    //    Move Long (64-bit): r6 = r4; r7 = r5
    //    Encoding: 0x5E000000 | (6 << 19) | 4 -> 0x5E300004
    let op_movl = 0x5E300004;

    // 2. synmov r8, r9 (Opcode 0x60)
    //    Implementation: Memory copy from *r8 to *r9
    //    We will set r8 = 0x300 (Source), r9 = 0x400 (Dest)
    //    Encoding: 0x60000000 | (9 << 19) | 8 -> 0x60480008
    let op_synmov = 0x60480008;

    let program = [op_movl, op_synmov, 0x08000000];

    sys.load_program(0x100, &program);
    cpu.ip = 0x100;

    // Setup for movl
    cpu.r[4] = 0xAAAA;
    cpu.r[5] = 0xBBBB;

    // Setup for synmov
    cpu.r[8] = 0x300; // Source Address
    cpu.r[9] = 0x400; // Dest Address
    sys.write_u32(0x300, 0xCAFEBABE); // Value in memory

    cpu.execute_run(&mut sys, 20);

    // Verify movl
    println!(
        "  [DEBUG] movl result: r6={:X}, r7={:X}",
        cpu.r[6], cpu.r[7]
    );
    if cpu.r[6] == 0xAAAA && cpu.r[7] == 0xBBBB {
        println!("  -> movl Passed");
    } else {
        println!("  -> movl FAILED");
    }

    // Verify synmov
    let dest_val = sys.read_u32(0x400);
    println!("  [DEBUG] synmov result: Mem[0x400]={:X}", dest_val);

    if dest_val == 0xCAFEBABE {
        println!("  -> synmov Passed");
    } else {
        println!("  -> synmov FAILED");
    }
}
