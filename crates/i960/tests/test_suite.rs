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

    // Helper to write u32 (Little Endian)
    fn write_u32(&mut self, addr: u32, val: u32) {
        self.write_byte(addr, (val & 0xFF) as u8);
        self.write_byte(addr + 1, ((val >> 8) & 0xFF) as u8);
        self.write_byte(addr + 2, ((val >> 16) & 0xFF) as u8);
        self.write_byte(addr + 3, ((val >> 24) & 0xFF) as u8);
    }

    // Helper to read u32 for verification
    fn read_u32(&mut self, addr: u32) -> u32 {
        let b0 = self.read_byte(addr) as u32;
        let b1 = self.read_byte(addr + 1) as u32;
        let b2 = self.read_byte(addr + 2) as u32;
        let b3 = self.read_byte(addr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn load_program(&mut self, addr: u32, opcodes: &[u32]) {
        for (i, &word) in opcodes.iter().enumerate() {
            self.write_u32(addr + (i as u32 * 4), word);
        }
    }
}

// Implement the Bus trait as required by i960
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
fn test_suite() {
    println!("--- Starting i960 Integration Test Suite ---");
    test_memory_store_load();
    test_alu_add();
    test_branch_logic();
    test_complex_addressing();
    test_fpu_math();
    test_register_windows();
    println!("--- All Tests Passed Successfully ---");
}

fn test_memory_store_load() {
    println!("[Test] Memory Access (Store/Load)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    let program = [
        0x92180100, // st r3, 0x0100
        0x08000000, // b . (Infinite Loop)
    ];

    sys.load_program(0x00, &program);
    cpu.r[3] = 0xCAFEBABE;
    cpu.ip = 0x00;
    cpu.execute_run(&mut sys, 10);

    let val = sys.read_u32(0x100);
    assert_eq!(
        val, 0xCAFEBABE,
        "Memory write failed: Expected 0xCAFEBABE, got 0x{:08X}",
        val
    );
    println!("  -> Passed");
}

fn test_alu_add() {
    println!("[Test] ALU Operation (Add)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    let program = [
        0x5C180E0A, // mov 10, r3
        0x5C200E14, // mov 20, r4
        0x59290083, // addi r3, r4, r5
        0x92280200, // st r5, 0x200
    ];

    sys.load_program(0x00, &program);
    cpu.ip = 0x00;
    cpu.execute_run(&mut sys, 20);

    let result = sys.read_u32(0x200);
    assert_eq!(
        result, 30,
        "ALU Add failed: 10 + 20 should be 30, got {}",
        result
    );
    println!("  -> Passed");
}

fn test_branch_logic() {
    println!("[Test] Control Flow (Compare & Branch)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    let program = [
        0x5C180E0A, // 0x00: mov 10, r3
        0x5C200E14, // 0x04: mov 20, r4
        0x5A110083, // 0x08: cmpi r3, r4  (Flags = Less)
        0x11000020, // 0x0C: bg 0x20      (Should NOT take branch)
        0x14000008, // 0x10: bl +8 bytes -> Jumps to 0x18 (Target)
        0x92180BAD, // 0x14: st r3, 0xBAD (Fail if executed)
        0x92180200, // 0x18: st r3, 0x200 (Success)
        0x08000000, // 0x1C: b . (Halt)
    ];

    sys.load_program(0x00, &program);
    cpu.ip = 0x00;
    cpu.execute_run(&mut sys, 50);

    let val = sys.read_u32(0x200);
    assert_eq!(
        val, 10,
        "Branch Logic failed: Did not reach success marker."
    );
    let bad_val = sys.read_u32(0xBAD);
    assert_eq!(
        bad_val, 0,
        "Branch Logic failed: Executed skipped instruction!"
    );

    println!("  -> Passed");
}

fn test_complex_addressing() {
    println!("[Test] Complex Addressing (Base + Index*Scale + Disp)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    // Goal: ld r4, 0x10(r5)[r6*4]
    // Base (r5) = 0x200, Index (r6) = 0x10, Scale = 4, Disp = 0x10
    // Target Address = 0x200 + (0x10 * 4) + 0x10 = 0x250
    sys.write_u32(0x250, 0x88776655);

    // Opcode construction (ld, MEMB format)
    let inst_word: u32 =
        0x90000000 | 0x00200000 | 0x00014000 | 0x00001000 | 0x00003C00 | 0x00000100 | 0x00000006;

    let program = [
        inst_word,  // ld r4, ...
        0x00000010, // Displacement 0x10
        0x08000000, // b .
    ];

    sys.load_program(0x00, &program);
    cpu.ip = 0x00;
    cpu.r[5] = 0x200; // Base
    cpu.r[6] = 0x10; // Index

    cpu.execute_run(&mut sys, 10);

    assert_eq!(
        cpu.r[4], 0x88776655,
        "Complex Addressing failed: Expected 0x88776655 in r4, got 0x{:08X}",
        cpu.r[4]
    );
    println!("  -> Passed");
}

fn test_fpu_math() {
    println!("[Test] FPU Math (Sqrt, Sin, Cos)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    let program = [
        0x68200403, // sqrtr
        0x68280603, // sinr
        0x68380686, // cosr
        0x08000000, // Halt
    ];

    sys.load_program(0x00, &program);
    cpu.ip = 0x00;

    // Inputs
    cpu.r[3] = f32::to_bits(4.0);
    cpu.r[6] = f32::to_bits(0.0);

    cpu.execute_run(&mut sys, 1000);

    let res_sqrt = f32::from_bits(cpu.r[4]);
    let res_sin = f32::from_bits(cpu.r[5]);
    let res_cos = f32::from_bits(cpu.r[7]);

    println!("  [DEBUG] Sqrt(4.0) = {}", res_sqrt);
    println!("  [DEBUG] Sin(4.0)  = {}", res_sin);
    println!("  [DEBUG] Cos(0.0)  = {}", res_cos);

    assert!((res_sqrt - 2.0).abs() < 1e-5, "FPU Sqrt failed");
    assert!((res_sin - 4.0f32.sin()).abs() < 1e-5, "FPU Sin failed");
    assert!((res_cos - 0.0f32.cos()).abs() < 1e-5, "FPU Cos failed");

    println!("  -> Passed");
}

fn test_register_windows() {
    println!("[Test] Register Windows (Call/Ret)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();
    setup_interrupts(&mut sys, &mut cpu);

    // Program:
    // 0x00: mov 10, r4    (0x5C200E0A) - Sets Caller's r4 = 10
    // 0x04: call 0x10     (0x0900000C) - Calls subroutine, saves Caller's r4
    // 0x08: st r4, 0x300  (0x92200300) - Stores r4 (should be 10) to memory
    // 0x0C: b.           (0x08000000)
    // 0x10: mov 20, r4    (0x5C200E14) - Sets Callee's r4 = 20 (Local)
    // 0x14: ret           (0x0A000000) - Restores Caller's r4

    let program: [u32; 6] = [
        0x5C200E0A, 0x0900000C, 0x92200300, 0x08000000, 0x5C200E14, 0x0A000000,
    ];

    sys.load_program(0x00, &program);
    cpu.ip = 0x00;

    // Ensure SP is initialized reasonably
    if cpu.r[1] == 0 {
        cpu.r[1] = 0x1000;
    }

    println!("  [DEBUG] Initial SP: {:08X}", cpu.r[1]);

    cpu.execute_run(&mut sys, 200);

    let val = sys.read_u32(0x300);
    println!("  [DEBUG] Retrieved r4 after return: {}", val);

    assert_eq!(
        val, 10,
        "Register Window failed: Caller's r4 was clobbered or not restored. Expected 10, got {}",
        val
    );
    println!("  -> Passed");
}
