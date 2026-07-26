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

#[test]
fn insbfr_inserts_right_justified_field() {
    const START: usize = 0x100;
    const DEST: usize = 0x200;

    let mut ram = Ram(vec![0; 0x400]);

    // INSBFR #0b010, 5[R5 bit 4], width 3:
    // 5D 18 F4 <imm32> 05 04 03
    let mut code = vec![0x5d, 0x18, 0xf4];
    code.extend_from_slice(&2u32.to_le_bytes());
    code.extend_from_slice(&[0x05, 0x04, 0x03]);
    ram.0[START..START + code.len()].copy_from_slice(&code);
    ram.0[DEST..DEST + 4].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[5] = DEST as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    // mask 0b111 at bit 4: 0xDD -> 0b1010_1101
    assert_eq!(ram.read_u32(DEST as u32), 0xaabb_ccad);
    assert_eq!(cpu.reg[PC], (START + code.len()) as u32);
    assert_eq!(cpu.op_unimpl[0x5d], 0);
}

#[test]
fn insbfl_inserts_left_justified_field_across_bytes() {
    const START: usize = 0x100;
    const DEST: usize = 0x200;

    let mut ram = Ram(vec![0; 0x400]);

    // INSBFL #0b1010 << 28, 5[R5 bit 6], width 4:
    // 5D 19 F4 <imm32> 05 06 04
    let mut code = vec![0x5d, 0x19, 0xf4];
    code.extend_from_slice(&0xa000_0000u32.to_le_bytes());
    code.extend_from_slice(&[0x05, 0x06, 0x04]);
    ram.0[START..START + code.len()].copy_from_slice(&code);
    ram.0[DEST..DEST + 4].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());

    let mut cpu = V60::new();
    cpu.reg[5] = DEST as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    // The 4-bit field straddles the byte boundary: bits 6-9 take 0b1010.
    assert_eq!(ram.read_u32(DEST as u32), 0xaabb_ce9d);
    assert_eq!(cpu.reg[PC], (START + code.len()) as u32);
    assert_eq!(cpu.op_unimpl[0x5d], 0);
}

#[test]
fn sch1bsu_finds_first_set_bit() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x200;

    let mut ram = Ram(vec![0; 0x400]);

    // SCH1BSU 5[R5 bit 3], width 8, R10:
    // 5B 22 05 03 08 6A   (subop 0x20 bit selects register-direct op2)
    ram.0[START..START + 6].copy_from_slice(&[0x5b, 0x22, 0x05, 0x03, 0x08, 0x6a]);
    ram.0[SOURCE] = 0b0001_0000;

    let mut cpu = V60::new();
    cpu.reg[5] = SOURCE as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    // From bit 3 the first set bit is bit 4: index 1.
    assert_eq!(cpu.reg[10], 1);
    assert!(!cpu.z, "Z is clear when the bit is found");
    assert_eq!(cpu.reg[28], SOURCE as u32);
    assert_eq!(cpu.reg[PC], (START + 6) as u32);
    assert_eq!(cpu.op_unimpl[0x5b], 0);
}

#[test]
fn sch0bsu_sets_z_when_no_clear_bit() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x200;

    let mut ram = Ram(vec![0; 0x400]);

    // SCH0BSU 5[R5 bit 0], width 8, R11:
    // 5B 20 05 00 08 6B
    ram.0[START..START + 6].copy_from_slice(&[0x5b, 0x20, 0x05, 0x00, 0x08, 0x6b]);
    ram.0[SOURCE] = 0xff;

    let mut cpu = V60::new();
    cpu.reg[5] = SOURCE as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    assert_eq!(cpu.reg[11], 8);
    assert!(cpu.z, "Z is set when the bit is not found");
    assert_eq!(cpu.reg[28], SOURCE as u32);
    assert_eq!(cpu.reg[PC], (START + 6) as u32);
    assert_eq!(cpu.op_unimpl[0x5b], 0);
}

#[test]
fn movbsu_copies_bit_string_upward_with_offset() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x200;
    const DEST: usize = 0x300;

    let mut ram = Ram(vec![0; 0x400]);

    // MOVBSU 5[R5 bit 0], width 12, 6[R6 bit 4]:
    // 5B 08 05 00 0C 06 04
    ram.0[START..START + 7].copy_from_slice(&[0x5b, 0x08, 0x05, 0x00, 0x0c, 0x06, 0x04]);
    ram.0[SOURCE..SOURCE + 2].copy_from_slice(&[0xcd, 0xab]);

    let mut cpu = V60::new();
    cpu.reg[5] = SOURCE as u32;
    cpu.reg[6] = DEST as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    // 12 source bits land at destination bit 4: 0xCD,0xAB -> 0xD0,0xBC.
    assert_eq!(&ram.0[DEST..DEST + 2], &[0xd0, 0xbc]);
    assert_eq!(cpu.reg[28], (SOURCE + 1) as u32);
    assert_eq!(cpu.reg[27], (DEST + 1) as u32);
    assert_eq!(cpu.reg[PC], (START + 7) as u32);
    assert_eq!(cpu.op_unimpl[0x5b], 0);
}

#[test]
fn movbsd_copies_bit_string_downward() {
    const START: usize = 0x100;
    const SOURCE: usize = 0x200;
    const DEST: usize = 0x300;

    let mut ram = Ram(vec![0; 0x400]);

    // MOVBSD 5[R5 bit 0], width 12, 6[R6 bit 0]:
    // 5B 09 05 00 0C 06 00
    ram.0[START..START + 7].copy_from_slice(&[0x5b, 0x09, 0x05, 0x00, 0x0c, 0x06, 0x00]);
    ram.0[SOURCE..SOURCE + 2].copy_from_slice(&[0xcd, 0xab]);

    let mut cpu = V60::new();
    cpu.reg[5] = SOURCE as u32;
    cpu.reg[6] = DEST as u32;
    cpu.reg[PC] = START as u32;
    cpu.run(&mut ram, 1);

    // Equal bit offsets: the 12-bit field copies straight across, leaving the
    // destination's high nibble untouched.
    assert_eq!(&ram.0[DEST..DEST + 2], &[0xcd, 0x0b]);
    assert_eq!(cpu.reg[28], SOURCE as u32);
    assert_eq!(cpu.reg[27], DEST as u32);
    assert_eq!(cpu.reg[PC], (START + 7) as u32);
    assert_eq!(cpu.op_unimpl[0x5b], 0);
}
