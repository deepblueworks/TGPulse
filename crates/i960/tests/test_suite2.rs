mod common;

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
fn test_suite2() {
    println!("--- Starting i960 Extended Instruction Coverage Test ---");
    test_callx();
    test_calls();
    test_rem_mod();
    println!("--- All Tests Finished ---");
}

fn test_callx() {
    println!("\n[Test] callx (Indirect Call)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // Setup:
    // 0x100: callx (r4)
    // 0x104: st r4, 0x300 (Should be skipped by call, executed after ret)
    // 0x108: b.
    //
    // 0x200: (Target Function)
    // 0x200: mov 0x1A, r16 (Signal success - 26)
    // 0x204: ret

    // Initialize SP
    cpu.r[1] = 0x1000;

    // Set r4 to target address 0x200
    cpu.r[4] = 0x200;

    // Encoding callx (r4):
    // Opcode: 0x86, Mode: 4 (Reg Indirect), Base: r4
    let op_callx = 0x86012000;

    let program_main = [
        op_callx,   // 0x100: callx (r4)
        0x92200300, // 0x104: st r4, 0x300
        0x08000000, // 0x108: b .
    ];
    sys.load_program(0x100, &program_main);

    // Encoding mov 0x1A, r16:
    // Op: 5C
    // Dst: r16 (16) -> 0x00800000
    // Lit: 1 (Bit 11) -> 0x00000800
    // Sub: C (1100) -> 0x00000600
    // Val: 1A (11010) -> 0x0000001A
    // Sum: 0x5C800E1A
    let program_func = [
        0x5C800E1A, // 0x200: mov 0x1A, r16
        0x0A000000, // 0x204: ret
    ];
    sys.load_program(0x200, &program_func);

    cpu.ip = 0x100;
    common::run(&mut cpu, &mut sys, 50);

    // Verify
    assert_eq!(
        cpu.r[16], 0x1A,
        "callx failed: Target function did not execute (r16 != 0x1A)"
    );
    let val = sys.read_u32(0x300);
    assert_eq!(
        val, 0x200,
        "callx failed: Did not return correctly to execute store instruction."
    );

    println!("  -> Passed");
}

fn test_calls() {
    println!("\n[Test] calls (System Call)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // Memory Map:
    // 0x1000: SAT (System Address Table)
    // 0x2000: SPT (System Procedure Table)
    // 0x3000: Syscall Target Function

    // 1. Setup SAT
    cpu.sat = 0x1000;
    // Offset 152 in SAT points to SPT
    sys.write_u32(0x1000 + 152, 0x2000);

    // 2. Setup SPT
    // Entry = Base + 48 + (Index * 4)
    // We will use Index 0. Target = 0x2000 + 48 = 0x2030.
    sys.write_u32(0x2000 + 48, 0x3000);

    // 3. Setup Target Function at 0x3000
    // mov 0x1B, r16 (27); ret
    // Encoding: 0x5C800E1B
    let isr = [
        0x5C800E1B, // mov 0x1B, r16
        0x0A000000, // ret
    ];
    sys.load_program(0x3000, &isr);

    // 4. Setup Main Program
    // calls 0
    let main_prog = [
        0x66000000, // calls 0
        0x08000000, // b .
    ];
    sys.load_program(0x100, &main_prog);

    // Init SP
    cpu.r[1] = 0x4000;
    cpu.ip = 0x100;

    common::run(&mut cpu, &mut sys, 50);

    assert_eq!(
        cpu.r[16], 0x1B,
        "calls failed: System call target did not execute."
    );
    println!("  -> Passed");
}

fn test_rem_mod() {
    println!("\n[Test] rem/mod (Integer Arithmetic)...");
    let mut sys = System::new();
    let mut cpu = I960Cpu::new();

    // Opcode 0x74: remi, modi, divi
    // Format: REG
    // Test: modi r3, r4, r5  (r5 = r4 % r3)
    // r3 (src1) = 10
    // r4 (src2) = 23
    // r5 (dst)  = 3 (Expected)

    // Encoding:
    // 0x74 << 24 | r5 (5) << 19 | r4 (4) << 14 | sub (9) << 7 | r3 (3)
    let op_modi = 0x74000000 | (5 << 19) | (4 << 14) | (9 << 7) | 3;

    let program = [op_modi, 0x08000000];

    sys.load_program(0x100, &program);
    cpu.ip = 0x100;
    cpu.r[3] = 10;
    cpu.r[4] = 23;
    cpu.r[5] = 0;

    common::run(&mut cpu, &mut sys, 10);

    println!("  [DEBUG] modi: 23 % 10 = {} (Expected 3)", cpu.r[5]);

    assert_eq!(
        cpu.r[5], 3,
        "modi failed: Incorrect result or unimplemented opcode."
    );
    println!("  -> Passed");
}
