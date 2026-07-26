use v60::cpu::PC;
use v60::{Bus, V60};

struct Ram(Vec<u8>);

impl Bus for Ram {
    fn read_u8(&mut self, address: u32) -> u8 {
        self.0.get(address as usize).copied().unwrap_or(0)
    }

    fn write_u8(&mut self, address: u32, value: u8) {
        if let Some(destination) = self.0.get_mut(address as usize) {
            *destination = value;
        }
    }
}

fn run_immediate_ten(opcode: u8, destination: u8, low: u32, high: u32) -> V60 {
    const START: usize = 0x100;

    let mut ram = Ram(vec![0; 0x200]);
    ram.0[START..START + 3].copy_from_slice(&[
        opcode,
        0x20 | destination,
        0xea, // ImmediateQuick 10
    ]);

    let mut cpu = V60::new();
    cpu.reg[PC] = START as u32;
    cpu.reg[destination as usize] = low;
    cpu.reg[destination as usize + 1] = high;
    cpu.cy = true;
    cpu.ov = true;
    cpu.run(&mut ram, 1);

    assert_eq!(cpu.reg[PC], (START + 3) as u32);
    assert_eq!(cpu.op_count[opcode as usize], 1);
    assert_eq!(cpu.op_unimpl[opcode as usize], 0);
    assert!(cpu.cy, "X arithmetic must preserve CY");
    assert!(cpu.ov, "X arithmetic must preserve OV");

    cpu
}

#[test]
fn divx_matches_virtua_racing_boot_encoding() {
    // FF8AFE: A6 20 EA = DIVX #10, R0:R1.
    let cpu = run_immediate_ten(0xa6, 0, 123, 0);

    assert_eq!(cpu.reg[0], 12);
    assert_eq!(cpu.reg[1], 3);
    assert!(!cpu.s);
    assert!(!cpu.z);
}

#[test]
fn divx_signed_remainder_follows_dividend() {
    let cpu = run_immediate_ten(0xa6, 0, (-123i64) as u32, u32::MAX);

    assert_eq!(cpu.reg[0], (-12i32) as u32);
    assert_eq!(cpu.reg[1], (-3i32) as u32);
    assert!(cpu.s);
    assert!(!cpu.z);
}

#[test]
fn divux_uses_the_complete_destination_pair() {
    let cpu = run_immediate_ten(0xb6, 2, 5, 1);

    assert_eq!(cpu.reg[2], 429_496_730);
    assert_eq!(cpu.reg[3], 1);
    assert!(!cpu.s);
    assert!(!cpu.z);
}

#[test]
fn mulx_writes_a_signed_64_bit_product() {
    let cpu = run_immediate_ten(0x86, 4, (-3i32) as u32, 0x1234_5678);

    assert_eq!(cpu.reg[4], (-30i32) as u32);
    assert_eq!(cpu.reg[5], u32::MAX);
    assert!(cpu.s);
    assert!(!cpu.z);
}

#[test]
fn mulux_writes_an_unsigned_64_bit_product() {
    let cpu = run_immediate_ten(0x96, 6, 0x8000_0000, 0x1234_5678);

    assert_eq!(cpu.reg[6], 0);
    assert_eq!(cpu.reg[7], 5);
    assert!(!cpu.s);
    assert!(!cpu.z);
}
