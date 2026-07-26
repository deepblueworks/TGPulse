//! Allow unused constants for completeness of the hardware definition
#![allow(dead_code)]

// Status Register (ST) Flags
pub const F_ZRC: u32 = 0x00000001; // Zero C
pub const F_ZRD: u32 = 0x00000002; // Zero D
pub const F_SGC: u32 = 0x00000004; // Sign C
pub const F_SGD: u32 = 0x00000008; // Sign D
pub const F_CPC: u32 = 0x00000010; // Compare C
pub const F_CPD: u32 = 0x00000020; // Compare D
pub const F_OVC: u32 = 0x00000040; // Overflow C
pub const F_OVD: u32 = 0x00000080; // Overflow D
pub const F_UNC: u32 = 0x00000100; // Underflow C
pub const F_UND: u32 = 0x00000200; // Underflow D
pub const F_DVZC: u32 = 0x00000400; // Divide by Zero C
pub const F_DVZD: u32 = 0x00000800; // Divide by Zero D
pub const F_CA: u32 = 0x00001000;
pub const F_CPP: u32 = 0x00002000;
pub const F_OVM: u32 = 0x00004000;
pub const F_UNM: u32 = 0x00008000;

pub const F_SIF0: u32 = 0x00010000;
pub const F_SIF1: u32 = 0x00020000;
pub const F_SOF0: u32 = 0x00040000;

pub const F_PIF: u32 = 0x00100000;
pub const F_POF: u32 = 0x00200000;
pub const F_PAIF: u32 = 0x00400000;
pub const F_PAOF: u32 = 0x00800000;
pub const F_F0S: u32 = 0x01000000;
pub const F_F1S: u32 = 0x02000000;
pub const F_IT: u32 = 0x04000000;
pub const F_ZX0: u32 = 0x08000000;
pub const F_ZX1: u32 = 0x10000000;
pub const F_ZX2: u32 = 0x20000000;
pub const F_ZC0: u32 = 0x40000000; // Zero C0
pub const F_ZC1: u32 = 0x80000000; // Zero C1

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegId {
    B0 = 0x00,
    B1 = 0x01,
    X0 = 0x02,
    X1 = 0x03,
    I0 = 0x05,
    I1 = 0x06,
    SP = 0x08,
    VSM = 0x0A,
    C0 = 0x0C,
    C1 = 0x0D,
    A = 0x10,
    AExp = 0x11,
    AMant = 0x12,
    B = 0x13,
    BExp = 0x14,
    BMant = 0x15,
    D = 0x19,
    DExp = 0x1A,
    DMant = 0x1B,
    P = 0x1C,
    PExp = 0x1D,
    PMant = 0x1E,
    SFT = 0x1F,
    RPC = 0x34,
    MASK = 0x3C,
}

#[inline(always)]
pub fn sext(val: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    ((val << shift) as i32 >> shift) as u32
}
