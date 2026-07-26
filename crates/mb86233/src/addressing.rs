use crate::cpu_state::Mb86233;
use crate::types::sext;

impl Mb86233 {
    /// Calculate Effective Address for Bank 0 (using x0, b0)
    pub fn ea_pre_0(&self, r: u32) -> u16 {
        match r & 0x180 {
            0x000 => (r & 0x7f) as u16,

            0x080 | 0x100 => ((r & 0x7f) as u16)
                .wrapping_add(self.b0)
                .wrapping_add(self.x0),

            0x180 => match r & 0x60 {
                0x00 => self.b0.wrapping_add(self.x0),
                0x20 => self.x0,
                0x40 => self.b0.wrapping_add(self.x0 & self.vsmr),
                0x60 => self.x0 & self.vsmr,
                _ => 0,
            },

            _ => 0,
        }
    }

    pub fn ea_post_0(&mut self, r: u32) {
        if (r & 0x100) == 0 {
            return;
        }

        if (r & 0x080) == 0 {
            self.x0 = self.x0.wrapping_add(self.i0);
        } else {
            self.x0 = self.x0.wrapping_add(sext(r, 5) as u16);
        }
    }

    /// Calculate Effective Address for Bank 1 (using x1, b1)
    pub fn ea_pre_1(&self, r: u32) -> u16 {
        match r & 0x180 {
            0x000 => (r & 0x7f) as u16,

            0x080 | 0x100 => ((r & 0x7f) as u16)
                .wrapping_add(self.b1)
                .wrapping_add(self.x1),

            0x180 => match r & 0x60 {
                0x00 => self.b1.wrapping_add(self.x1),
                0x20 => self.x1,
                0x40 => self.b1.wrapping_add(self.x1 & self.vsmr),
                0x60 => self.x1 & self.vsmr,
                _ => 0,
            },

            _ => 0,
        }
    }

    pub fn ea_post_1(&mut self, r: u32) {
        if (r & 0x100) == 0 {
            return;
        }

        if (r & 0x080) == 0 {
            self.x1 = self.x1.wrapping_add(self.i1);
        } else {
            self.x1 = self.x1.wrapping_add(sext(r, 5) as u16);
        }
    }
}
