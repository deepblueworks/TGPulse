//! Simple state container
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AluState {
    pub stmask: u32,
    pub stset: u32,
    pub r1: u32,
    pub r2: u32,
}

impl Default for AluState {
    fn default() -> Self {
        Self::new()
    }
}

impl AluState {
    pub fn new() -> Self {
        Self {
            stmask: 0,
            stset: 0,
            r1: 0,
            r2: 0,
        }
    }
}

pub fn set_exp(val: u32, exp: u32) -> u32 {
    (val & 0x807fffff) | ((exp & 0xff) << 23)
}

pub fn set_mant(val: u32, mant: u32) -> u32 {
    (val & 0x07f800000) | ((mant & 0x00800000) << 8) | (mant & 0x007fffff)
}

pub fn get_exp(val: u32) -> u32 {
    (val >> 23) & 0xff
}

pub fn get_mant(val: u32) -> u32 {
    if (val & 0x80000000) != 0 {
        val | 0x7f800000
    } else {
        val & 0x807fffff
    }
}
