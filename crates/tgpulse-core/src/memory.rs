//! The i960 and TGP address maps. Ranges are listed in ascending address
//! order, which is also the order the board's own documentation lists them,
//! so a map can be checked against a reference by reading down the page.

use crate::system::Model2System;
use i960::bus::Bus;
use mb86233::Mb86233Bus;

/// Reads a word from a region, returning 0 past the end.
fn region_r(region: &[u32], byte_off: u32) -> u32 {
    region.get((byte_off >> 2) as usize).copied().unwrap_or(0)
}

fn ram_r(ram: &[u32], byte_off: u32) -> u32 {
    ram.get((byte_off >> 2) as usize).copied().unwrap_or(0)
}

fn ram_w(ram: &mut [u32], byte_off: u32, val: u32) {
    if let Some(slot) = ram.get_mut((byte_off >> 2) as usize) {
        *slot = val;
    }
}

impl Bus for Model2System {
    /// The regions wired to the i960's burst bus cycle.
    ///
    /// Only memory answers a burst with successive words; everything else --
    /// the board registers, the coprocessor ports, the I/O chip -- sees one
    /// address for the whole transfer and returns the same word each time.
    /// Virtua Fighter 2 depends on this: its per-frame budget check does an
    /// `ldl` from the timer block and uses the *second* register, which on
    /// hardware is another copy of timer 2 rather than timer 3. Advancing the
    /// address there hands it a timer the game re-arms only on expiry, so the
    /// elapsed time reads several frames long and the game drops the
    /// characters' hair to save time it thinks it has already spent.
    ///
    /// This mirrors the `BURST` flags on the board's own address map.
    fn burst_capable(&self, addr: u32) -> bool {
        matches!(addr,
            // Program ROM, and the model2o RAM/ROM window below it.
            0x0000_0000..=0x0023_FFFF
            // Work RAM.
            | 0x0050_0000..=0x005F_FFFF
            // Geometrizer program and the coprocessor function port.
            | 0x0080_4000..=0x0080_7FFF
            | 0x0088_0000..=0x0088_3FFF
            // Display-list buffer RAM, with its mirror.
            | 0x0090_0000..=0x0097_FFFF
            // segas24 tilemap, character RAM and their mirrors.
            | 0x0100_0000..=0x0100_FFFF
            | 0x0102_0000..=0x0102_0003
            | 0x0108_0000..=0x010F_FFFF
            | 0x0111_0000..=0x0111_FFFF
            | 0x0112_0000..=0x0112_0003
            | 0x0118_0000..=0x011F_FFFF
            // Palette and colour translation.
            | 0x0180_0000..=0x0180_3FFF
            | 0x0181_0000..=0x0181_BFFF
            // Link board shared RAM, and its mirror.
            | 0x01A0_0000..=0x01A0_3FFF
            | 0x01A1_0000..=0x01A1_3FFF
            // Backup RAM, and the 2b/2c scratch RAM.
            | 0x01D0_0000..=0x01D0_3FFF
            | 0x01D8_0000..=0x01D8_FFFF
            // Data ROM banks.
            | 0x0200_0000..=0x03FF_FFFF
            | 0x0600_0000..=0x06FF_FFFF
            // 2b/2c texture, luma and framebuffer RAM.
            | 0x1100_0000..=0x113F_FFFF
            | 0x1140_0000..=0x1140_FFFF
            | 0x1160_0000..=0x116F_FFFF
            // Original/2a texture and luma RAM, with their mirrors.
            | 0x1200_0000..=0x123F_FFFF
            | 0x1240_0000..=0x127F_FFFF
            | 0x1280_0000..=0x1281_FFFF
        )
    }

    fn read_u32(&mut self, addr: u32) -> u32 {
        // The i960 permits unaligned ordinal loads. The region maps below
        // are word-backed and therefore only handle aligned accesses; build
        // an unaligned value byte-for-byte instead of silently rounding the
        // address down with `byte_off >> 2`.
        if addr & 3 != 0 {
            return u32::from_le_bytes([
                self.read_byte(addr),
                self.read_byte(addr.wrapping_add(1)),
                self.read_byte(addr.wrapping_add(2)),
                self.read_byte(addr.wrapping_add(3)),
            ]);
        }
        match addr {
            // --- ROM ---
            0x0000_0000..=0x001F_FFFF => region_r(&self.maincpu_rom, addr),

            // --- model2o: 128KB RAM, then a window onto maincpu ROM +0x20000 ---
            0x0020_0000..=0x0021_FFFF => ram_r(&self.ram_low, addr - 0x0020_0000),
            0x0022_0000..=0x0023_FFFF => region_r(&self.maincpu_rom, addr - 0x0022_0000 + 0x20000),

            // --- Work RAM ---
            0x0050_0000..=0x005F_FFFF => ram_r(&self.work_ram, addr - 0x0050_0000),

            // --- Geometrizer ---
            0x0080_0000..=0x0080_3FFF => match addr & 0x3FFF {
                0x2008 => self.geo_write_start_address,
                0x3008 => self.geo_read_start_address,
                _ => 0,
            },
            // geo_prg_r: reading the geometrizer program port returns all-ones.
            0x0080_4000..=0x0080_7FFF => 0xFFFF_FFFF,

            // --- Coprocessor FIFO ---
            0x0088_4000..=0x0088_7FFF => match self.copro_fifo_out.pop_front() {
                Some(v) => {
                    log::trace!(target: "fifo", "i960 pop  {v:08X} (out {})", self.copro_fifo_out.len());
                    v
                }
                None => {
                    log::trace!(target: "fifo", "i960 STALL (out empty)");
                    self.main_stall = true;
                    0
                }
            },

            // --- Buffer RAM (mirror 0x60000) ---
            0x0090_0000..=0x0097_FFFF => ram_r(&self.buffer_ram, addr & 0x1_FFFF),

            // --- TGP / video registers ---
            0x0098_0000..=0x0098_0003 => self.copro_ctl,
            // fifo_control_r: 1 when the output FIFO is *empty*, 0 otherwise.
            0x0098_0004..=0x0098_0007 => {
                if self.copro_fifo_out.is_empty() {
                    1
                } else {
                    0
                }
            }
            0x0098_000C..=0x0098_000F => self.videoctl_r(),
            0x0098_0030..=0x0098_003F => {
                // tgpid_r is a byte handler; the CPU reads it a byte at a time.
                const ID: [u8; 16] = [
                    0, b'T', b'A', b'H', 0, b'A', b'K', b'O', 0, b'Z', b'A', b'K', 0, b'M', b'T',
                    b'K',
                ];
                let base = ((addr - 0x0098_0030) & 0xC) as usize;
                u32::from_le_bytes([ID[base], ID[base + 1], ID[base + 2], ID[base + 3]])
            }

            // --- System registers ---
            0x00E0_0000..=0x00E0_0037 => ram_r(&self.cpu_ctl, addr - 0x00E0_0000),
            0x00E8_0000..=0x00E8_0003 => self.irq_request,
            0x00E8_0004..=0x00E8_0007 => self.irq_enable,
            // The reference computes a running timer from the time elapsed since it was
            // programmed, at the instant of the read. Games seed a random
            // number generator from the low bits, so answering with the value
            // as of the last scheduler boundary makes them deterministic --
            // Virtua Fighter 2 shuffles by drawing indices until it finds an
            // unused one and never terminates.
            0x00F0_0000..=0x00F0_000F => {
                let i = ((addr - 0x00F0_0000) >> 2) as usize;
                if self.timer_running[i] {
                    let elapsed = self.now_cycles().saturating_sub(self.timer_start[i]);
                    self.timer_orig[i].saturating_sub(elapsed as u32)
                } else {
                    self.timer_vals[i]
                }
            }

            // --- segas24 tilemap device (mirror 0x110000) ---
            0x0100_0000..=0x0100_FFFF | 0x0111_0000..=0x0111_FFFF => {
                ram_r(&self.tile_ram, addr & 0xFFFF)
            }
            0x0104_0000..=0x0104_0003 | 0x0114_0000..=0x0114_0003 => self.crtc_xraw as u32,
            0x0106_0000..=0x0106_0003 | 0x0116_0000..=0x0116_0003 => self.crtc_yraw as u32,
            0x0108_0000..=0x010F_FFFF | 0x0118_0000..=0x011F_FFFF => {
                ram_r(&self.char_ram, addr & 0x7_FFFF)
            }

            // --- Palette / color translation ---
            0x0180_0000..=0x0180_3FFF => ram_r(&self.palette_ram, addr - 0x0180_0000),
            0x0181_0000..=0x0181_BFFF => ram_r(&self.colorxlat_ram, addr - 0x0181_0000),

            // --- M2COMM network board (mirror 0x10000) ---
            // A cabinet without the board leaves this slot unloaded, so the bus
            // floats high. That is how the game tells the board is missing: it
            // clears cn, reads it back, and a fitted board would drive bit 0 low.
            0x01A0_0000..=0x01A1_FFFF if !self.comm_present => 0xFFFF_FFFF,
            0x01A0_0000..=0x01A0_3FFF | 0x01A1_0000..=0x01A1_3FFF => {
                log::trace!(target: "comm", "R share {:08X}", addr);
                let b = (addr & 0x3FFC) as usize;
                u32::from_le_bytes([
                    self.comm_shared[b],
                    self.comm_shared[b + 1],
                    self.comm_shared[b + 2],
                    self.comm_shared[b + 3],
                ])
            }
            // cn at byte 0x01a04000, fg at byte 0x01a04002.
            0x01A0_4000..=0x01A0_4003 | 0x01A1_4000..=0x01A1_4003 => {
                // Reading fg is also how the game clocks the board along
                // between V-blanks.
                self.comm_read_fg();
                let cn = self.comm_cn | 0xFE;
                let fg = (self.comm_fg as u32 | ((!(self.comm_zfg as u32)) << 7) | 0x7E) as u8;
                u32::from_le_bytes([cn, 0xFF, fg, 0xFF])
            }
            0x01A0_4004..=0x01A1_FFFF => 0xFFFF_FFFF,

            // --- MB8421 dual-port RAM (I/O board) ---
            0x01C0_0000..=0x01C0_001F if self.has_io5649() => self.io5649_r(addr - 0x01C0_0000),
            // The MB8421 dual-port RAM is the *Model 1 style* I/O board, fitted
            // only to the original Model 2. The 2A/2B/2C boards replace it with
            // the 315-5649 in the low 0x20 bytes and leave the rest of the
            // window unmapped -- and the boot code tells the two apart by
            // writing to 0x01c00024 and seeing whether the value reads back.
            // Answering there makes a 2A board look like a 2-original one.
            0x01C0_0000..=0x01C0_0FFF if !self.has_io5649() => self.dpram_r(addr - 0x01C0_0000),
            // i8251 UART to the sound board (odd bytes of the word: the reference maps
            // it umask16(0x00ff)). 0x01c80000 is data, 0x01c80002 is status.
            // The 2B/2C boards move the sound UART to 0x009c0000 and widen it
            // to umask32 (one register per dword) from 2A's umask16 pair at
            // 0x01c80000. Missing this window silently drops every sound
            // command the game sends.
            0x009C_0000..=0x009C_0007 => {
                if addr & 4 == 0 {
                    self.sound.take_reply() as u32
                } else {
                    let mut status = 0x05u32;
                    if self.sound.reply_ready() {
                        status |= 0x02;
                    }
                    status
                }
            }
            0x01C8_0000..=0x01C8_0003 => {
                let data = self.sound.take_reply() as u32;
                // Our transmitter never blocks, so TxRDY|TxEMPTY are always up;
                // RxRDY reflects whether the board actually answered.
                let mut status = 0x05u32;
                if self.sound.reply_ready() {
                    status |= 0x02;
                }
                data | (status << 16)
            }

            // --- Backup SRAM ---
            0x01D0_0000..=0x01D0_3FFF => {
                let v = ram_r(&self.backup_ram, addr - 0x01D0_0000);
                log::trace!(target: "backup", "R {:08X} = {:08X}", addr, v);
                v
            }

            // --- Data ROM ---
            0x0200_0000..=0x03FF_FFFF => region_r(&self.main_data, addr - 0x0200_0000),
            0x0600_0000..=0x06FF_FFFF => region_r(&self.main_data, addr - 0x0600_0000 + 0x100_0000),

            // --- Renderer ---
            0x1000_0000..=0x101F_FFFF => self.render_mode_r(),
            0x1040_0000..=0x105F_FFFF => 0, // polygon_count_r
            0x1080_0000..=0x1080_0003 => 0,

            // --- Texture / luma RAM (mirror 0x200000) ---
            //
            // The 2B/2C video boards move these: texture RAM sits at
            // 0x11000000 as a 2MB aperture per bank -- 2B writes it as two
            // adjacent 1MB maps sharing one tag, 2C as a single 2MB map over
            // the identical range -- and the luma RAM at
            // 0x11400000, where the original and 2A boards use 0x12000000 /
            // 0x12800000. Without the 2B window the game's texture uploads
            // land nowhere and every polygon samples black.
            0x1100_0000..=0x111F_FFFF => ram_r(&self.texture_ram0, addr & 0x1F_FFFF),
            0x1120_0000..=0x113F_FFFF => ram_r(&self.texture_ram1, addr & 0x1F_FFFF),
            0x1140_0000..=0x1140_FFFF => self.luma_ram[((addr - 0x1140_0000) >> 1) as usize] as u32,
            0x1200_0000..=0x123F_FFFF => ram_r(&self.texture_ram0, addr & 0x1F_FFFF),
            0x1240_0000..=0x127F_FFFF => ram_r(&self.texture_ram1, addr & 0x1F_FFFF),
            0x1280_0000..=0x1281_FFFF => self.luma_ram[((addr - 0x1280_0000) >> 2) as usize] as u32,

            _ => 0,
        }
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        if addr & 3 != 0 {
            for (i, byte) in val.to_le_bytes().into_iter().enumerate() {
                self.write_byte(addr.wrapping_add(i as u32), byte);
            }
            return;
        }
        match addr {
            // --- ROM (nopw) ---
            0x0000_0000..=0x001F_FFFF | 0x0022_0000..=0x0023_FFFF => {}

            0x0020_0000..=0x0021_FFFF => ram_w(&mut self.ram_low, addr - 0x0020_0000, val),

            0x0050_0000..=0x005F_FFFF => ram_w(&mut self.work_ram, addr - 0x0050_0000, val),

            // --- Geometrizer ---
            0x0080_0000..=0x0080_3FFF => self.geo_w(addr & 0x3FFF, val),
            0x0080_4000..=0x0080_7FFF => self.geo_prg_w(val),

            // --- Coprocessor function port ---
            0x0088_0000..=0x0088_3FFF => {
                // The *word* offset supplies the tag, so the byte address
                // shifts by 4, not 2.
                let d = (val & 0x800F_FFFF) | (((addr >> 4) & 0xFF) << 23);
                log::trace!(target: "fifo", "i960 func {:08X} (addr {:08X})", d, addr);
                self.copro_fifo_in.push_back(d);
            }
            0x0088_4000..=0x0088_7FFF => {
                log::trace!(target: "fifo", "i960 push {val:08X}");
                self.copro_fifo_w(val)
            }
            // The SHARC's I/O-processor window (Model 2B). The i960 configures
            // the coprocessor's DMA through here.
            0x008C_0000..=0x008C_0FFF if self.is_sharc() => {
                let offset = (addr - 0x008C_0000) >> 2;
                let parked = self.parked_sharc.take().expect("sharc placeholder");
                let mut sharc = std::mem::replace(&mut self.sharc, parked);
                sharc.external_iop_write(self, offset, val);
                self.parked_sharc = Some(std::mem::replace(&mut self.sharc, sharc));
            }

            // --- Buffer RAM (mirror 0x60000) ---
            0x0090_0000..=0x0097_FFFF => ram_w(&mut self.buffer_ram, addr & 0x1_FFFF, val),

            // --- TGP / video registers ---
            0x0098_0000..=0x0098_0003 => self.copro_ctl_w(val),
            0x0098_0008..=0x0098_000B => self.geo_ctl_w(val),
            0x0098_000C..=0x0098_000F => self.video_ctl = val,

            // --- System registers ---
            0x00E0_0000..=0x00E0_0037 => ram_w(&mut self.cpu_ctl, addr - 0x00E0_0000, val),
            0x00E8_0000..=0x00E8_0003 => {
                // irq_ack_w. The reference re-runs `irq_update` here: acking has to
                // drop the CPU's interrupt line, otherwise a later re-assert
                // of the same line produces no edge and the interrupt is
                // never dispatched again.
                self.irq_request &= val;
                self.irq_update();
            }
            0x00E8_0004..=0x00E8_0007 => {
                // The reference re-evaluates the sound-ready line when the mask lands
                // (`irq_mask_delayed_update`), so re-enabling bit 10 while the
                // UART is ready asserts it again immediately. The game's sound
                // task masks the line, acks, then unmasks -- and relies on that
                // re-assert to be re-entered.
                self.irq_enable_pending = Some((val, self.now_cycles() + 2));
            }

            // --- Timers ---
            0x00F0_0000..=0x00F0_000F => {
                let idx = ((addr - 0x00F0_0000) >> 2) as usize;
                self.timer_vals[idx] = val;
                self.timer_orig[idx] = val;
                self.timer_start[idx] = self.now_cycles();
                self.timer_running[idx] = true;
            }

            // --- segas24 tilemap device (mirror 0x110000) ---
            0x0100_0000..=0x0100_FFFF | 0x0111_0000..=0x0111_FFFF => {
                ram_w(&mut self.tile_ram, addr & 0xFFFF, val)
            }
            0x0102_0000..=0x0102_0003 | 0x0112_0000..=0x0112_0003 => {} // ABSEL, always 0
            0x0104_0000..=0x0104_0003 | 0x0114_0000..=0x0114_0003 => {
                self.crtc_xraw = val as u16;
                self.crtc_xoffset = 84i16.wrapping_add(self.crtc_xraw as i16);
            }
            0x0106_0000..=0x0106_0003 | 0x0116_0000..=0x0116_0003 => {
                self.crtc_yraw = val as u16;
                self.crtc_yoffset = 130i16.wrapping_add(self.crtc_yraw as i16);
            }
            0x0107_0000..=0x0107_0003 | 0x0117_0000..=0x0117_0003 => {} // sync switch
            0x0108_0000..=0x010F_FFFF | 0x0118_0000..=0x011F_FFFF => {
                if (addr & 0x7_FFFF) < 0x20 {
                    log::trace!(target: "char",
                        "[CH] W {:08X} (char0 off {:02X}) = {:08X}",
                        addr,
                        addr & 0x7_FFFF,
                        val
                    );
                }
                ram_w(&mut self.char_ram, addr & 0x7_FFFF, val)
            }

            // --- Palette / color translation ---
            0x0180_0000..=0x0180_3FFF => ram_w(&mut self.palette_ram, addr - 0x0180_0000, val),
            0x0181_0000..=0x0181_BFFF => {
                self.colorxlat_written = true;
                self.colorxlat_dirty = true;
                log::trace!(target: "colorxlat", "W {addr:08X} = {val:08X}");
                ram_w(&mut self.colorxlat_ram, addr - 0x0181_0000, val)
            }
            0x0181_C000..=0x0181_C003 => self.geometry.set_master_z_clip(val),

            // --- M2COMM network board (mirror 0x10000) ---
            0x01A0_0000..=0x01A1_FFFF if !self.comm_present => {}
            0x01A0_0000..=0x01A0_3FFF | 0x01A1_0000..=0x01A1_3FFF => {
                log::trace!(target: "comm", "W share {:08X} = {:08X}", addr, val);
                let b = (addr & 0x3FFC) as usize;
                let by = val.to_le_bytes();
                self.comm_shared[b..b + 4].copy_from_slice(&by);
            }
            0x01A0_4000..=0x01A0_4003 | 0x01A1_4000..=0x01A1_4003 => {
                log::trace!(target: "comm", "W cn/fg {:08X} = {:08X}", addr, val);
                self.comm_cn_w(val as u8);
                self.comm_fg = (val >> 16) as u8 & 0x01;
            }
            0x01A0_4004..=0x01A1_FFFF => {}

            0x01C0_0000..=0x01C0_001F if self.has_io5649() => {
                self.io5649_w(addr - 0x01C0_0000, val)
            }
            0x01C0_0000..=0x01C0_0FFF if !self.has_io5649() => {
                self.dpram_w(addr - 0x01C0_0000, val)
            }
            0x009C_0000..=0x009C_0007 => {
                log::trace!(target: "sound", "W {:08X} = {:08X}", addr, val);
                if addr & 4 == 0 {
                    self.sound.send(val as u8);
                } else {
                    self.sound.control(val as u8);
                }
                self.sound_ready_update();
            }
            0x01C8_0000..=0x01C8_0003 => {
                self.sound.send(val as u8);
                self.sound.control((val >> 16) as u8);
                self.sound_ready_update();
            }

            0x01D0_0000..=0x01D0_3FFF => {
                log::trace!(target: "backup", "W {:08X} = {:08X}", addr, val);
                ram_w(&mut self.backup_ram, addr - 0x01D0_0000, val);
                // The settings block is only meaningful once the game has
                // sealed it, so the cabinet is applied off the back of a write
                // rather than at reset.
                if addr < 0x01D0_0100 {
                    self.nv_apply_cabinet();
                }
            }

            // --- Data ROM (read-only) ---
            0x0200_0000..=0x03FF_FFFF | 0x0600_0000..=0x06FF_FFFF => {}

            // --- Renderer ---
            0x1000_0000..=0x101F_FFFF => self.render_mode_ctl = val & 0x0000_4005,

            // --- Texture / luma RAM (mirror 0x200000); see the read path for
            // why the 2B/2C boards need their own windows. ---
            0x1100_0000..=0x111F_FFFF => ram_w(&mut self.texture_ram0, addr & 0x1F_FFFF, val),
            0x1120_0000..=0x113F_FFFF => ram_w(&mut self.texture_ram1, addr & 0x1F_FFFF, val),
            0x1140_0000..=0x1140_FFFF => {
                self.luma_ram[((addr - 0x1140_0000) >> 1) as usize] = val as u8
            }
            0x1200_0000..=0x123F_FFFF => {
                Self::texture_w(&mut self.texture_ram0, addr & 0x1F_FFFF, val)
            }
            0x1240_0000..=0x127F_FFFF => {
                Self::texture_w(&mut self.texture_ram1, addr & 0x1F_FFFF, val)
            }
            0x1280_0000..=0x1281_FFFF => {
                self.luma_ram[((addr - 0x1280_0000) >> 2) as usize] = val as u8
            }

            _ => {}
        }
    }

    fn read_byte(&mut self, addr: u32) -> u8 {
        // The 315-5649 is an 8-bit device with side effects: reading register
        // 0x0f hands back the current analog channel *and steps the mux*.
        // Widening a byte access to the containing word would touch the
        // neighbouring register too and advance the mux an extra time, which
        // shuffles the cabinet's analog axes.
        if let 0x01C0_0000..=0x01C0_001F = addr {
            if self.has_io5649() {
                return match Self::io5649_reg_of(addr - 0x01C0_0000) {
                    Some(reg) => self.io5649_byte(reg),
                    None => 0xff,
                };
            }
        }
        let word = self.read_u32(addr & !3);
        ((word >> ((addr & 3) * 8)) & 0xFF) as u8
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if let 0x01C0_0000..=0x01C0_001F = addr {
            if self.has_io5649() {
                if let Some(reg) = Self::io5649_reg_of(addr - 0x01C0_0000) {
                    self.io5649_byte_w(reg, val);
                }
                return;
            }
        }
        if self.write_sub_word(addr, val as u32) {
            return;
        }
        let aligned = addr & !3;
        let shift = (addr & 3) * 8;
        let mask = 0xFFu32 << shift;
        // A read-modify-write must not disturb registers with read side
        // effects, so anything of that kind is handled above.
        let old = self.read_u32(aligned);
        self.write_u32(aligned, (old & !mask) | ((val as u32) << shift));
    }

    fn read_u16(&mut self, addr: u32) -> u16 {
        let aperture = |ram: &[u32], off: u32| {
            let n = (off >> 2) as usize;
            let w = ram.get(n >> 1).copied().unwrap_or(0);
            if n & 1 == 0 {
                w as u16
            } else {
                (w >> 16) as u16
            }
        };
        match addr {
            0x1200_0000..=0x123f_ffff => aperture(&self.texture_ram0, addr & 0x1f_ffff),
            0x1240_0000..=0x127f_ffff => aperture(&self.texture_ram1, addr & 0x1f_ffff),
            // The 315-5649 is an 8-bit device on alternate byte lanes, so a
            // 16-bit access there really is two independent register accesses.
            0x01C0_0000..=0x01C0_001F if self.has_io5649() => {
                let lo = self.read_byte(addr) as u16;
                let hi = self.read_byte(addr.wrapping_add(1)) as u16;
                lo | (hi << 8)
            }
            _ => {
                // Everything else is a 32-bit port: one bus cycle, then take
                // the half that was asked for. Splitting it into two byte
                // reads would run the cycle twice, and on a register with read
                // side effects -- the coprocessor FIFO above all -- that pops
                // two words where the game asked for one.
                if addr & 3 == 3 {
                    // Straddles two words; unavoidable as two accesses.
                    let lo = self.read_byte(addr) as u16;
                    let hi = self.read_byte(addr.wrapping_add(1)) as u16;
                    return lo | (hi << 8);
                }
                let word = self.read_u32(addr & !3);
                (word >> ((addr & 2) * 8)) as u16
            }
        }
    }

    fn write_u16(&mut self, addr: u32, val: u16) {
        // Must come before the split into two byte writes below: a register
        // that acts on the write would otherwise be poked twice for one store.
        if self.write_sub_word(addr, val as u32) {
            return;
        }
        match addr {
            0x1100_0000..=0x111f_ffff => {
                Self::texture_w16(&mut self.texture_ram0, addr & 0x0f_ffff, val)
            }
            0x1120_0000..=0x113f_ffff => {
                Self::texture_w16(&mut self.texture_ram1, addr & 0x0f_ffff, val)
            }
            0x1200_0000..=0x123f_ffff => {
                Self::texture_w(&mut self.texture_ram0, addr & 0x1f_ffff, val as u32)
            }
            0x1240_0000..=0x127f_ffff => {
                Self::texture_w(&mut self.texture_ram1, addr & 0x1f_ffff, val as u32)
            }
            // The 315-5649 really is an 8-bit device on alternate byte lanes,
            // so a 16-bit access there is two independent register accesses.
            0x01C0_0000..=0x01C0_001F if self.has_io5649() => {
                self.write_byte(addr, val as u8);
                self.write_byte(addr.wrapping_add(1), (val >> 8) as u8)
            }
            _ => {
                // Everything else is a 32-bit port, so one read-modify-write
                // rather than two. Splitting it runs the bus cycle twice, and
                // on a register that acts on access that is one poke too many.
                if addr & 3 == 3 {
                    self.write_byte(addr, val as u8);
                    self.write_byte(addr.wrapping_add(1), (val >> 8) as u8);
                    return;
                }
                let aligned = addr & !3;
                let shift = (addr & 2) * 8;
                let mask = 0xFFFFu32 << shift;
                let old = self.read_u32(aligned);
                self.write_u32(aligned, (old & !mask) | ((val as u32) << shift));
            }
        }
    }

    fn take_irq_lines(&mut self) -> Option<[bool; 4]> {
        self.pending_irq_lines.take()
    }

    fn take_stall(&mut self) -> bool {
        std::mem::take(&mut self.main_stall)
    }
}

impl Model2System {
    /// Handles a byte or short write to a register that must not be reached
    /// through a read-modify-write. Returns true when it dealt with the write.
    ///
    /// Sub-word writes on the i960's 32-bit bus assert byte enables: the device
    /// sees a write and nothing else. Our word-backed regions do need the
    /// neighbouring bytes, so those go the read-modify-write route -- but that
    /// read is an artefact of how the map is stored, and several of these
    /// registers *do something* when read. Letting the artefact reach them is a
    /// real bug, not a cosmetic one:
    ///
    /// * 0x884000 pops the coprocessor's output FIFO and asks the CPU to stall
    ///   when it is empty. Daytona feeds the TGP with alternating `stis`/`st`
    ///   at the start of a race (`0xfae8`..), so the phantom read threw away
    ///   TGP output and then livelocked the i960 retrying the store forever --
    ///   the machine kept running and the HUD kept drawing, but no display list
    ///   was ever built again, which is exactly the frozen track.
    /// * the comm board's `cn`/`fg` pair sit in one word, so a read-modify-write
    ///   of either re-triggers the other.
    ///
    /// The reference handlers take the data with no `mem_mask` and simply act on it,
    /// which is what this reproduces.
    fn write_sub_word(&mut self, addr: u32, val: u32) -> bool {
        match addr {
            // Write-only register: the written data is handed straight to the handler
            // regardless of width. Letting a byte/16-bit store fall through to
            // the generic read-modify-write path reads back 0 here and undoes
            // the value -- and a master z-clip of 0 culls every polygon in
            // front of the eye.
            0x0181_C000..=0x0181_C003 => self.geometry.set_master_z_clip(val),
            0x0088_4000..=0x0088_7FFF => {
                log::trace!(target: "fifo", "i960 push {val:08X}");
                self.copro_fifo_w(val)
            }
            // The SHARC's I/O-processor window (Model 2B). The i960 configures
            // the coprocessor's DMA through here.
            0x008C_0000..=0x008C_0FFF if self.is_sharc() => {
                let offset = (addr - 0x008C_0000) >> 2;
                let parked = self.parked_sharc.take().expect("sharc placeholder");
                let mut sharc = std::mem::replace(&mut self.sharc, parked);
                sharc.external_iop_write(self, offset, val);
                self.parked_sharc = Some(std::mem::replace(&mut self.sharc, sharc));
            }
            // The UART's data register hands a byte to the sound board, and
            // reading it takes the board's reply away. A read-modify-write of
            // the enclosing word would both eat that reply and send a byte the
            // game never wrote.
            // i8251: 0x01c80000 is the data register and 0x01c80002 the
            // mode/command register (its read side is the status). Reading the
            // data register takes the board's reply away, so a read-modify-write
            // of the enclosing word would eat the reply and send a byte the game
            // never wrote.
            0x009C_0000 => {
                self.sound.send(val as u8);
                self.sound_ready_update();
            }
            0x009C_0004 => {
                self.sound.control(val as u8);
                self.sound_ready_update();
            }
            0x009C_0001..=0x009C_0003 | 0x009C_0005..=0x009C_0007 => {}
            0x01C8_0000 => {
                self.sound.send(val as u8);
                self.sound_ready_update();
            }
            0x01C8_0002 => {
                self.sound.control(val as u8);
                self.sound_ready_update();
            }
            0x01C8_0001 | 0x01C8_0003 => {}
            0x01A0_0000..=0x01A1_FFFF if !self.comm_present => {}
            // 2B/2C luma RAM is a 16-bit device on the low byte (umask16
            // 0x00ff), so the game writes it a half-word at a time -- one luma
            // byte per 16-bit word. Handling it here keeps a sub-word store
            // from being split into two byte writes that miss the window.
            0x1140_0000..=0x1140_FFFF => {
                self.luma_ram[((addr - 0x1140_0000) >> 1) as usize] = val as u8
            }
            0x01A0_4000 | 0x01A1_4000 => self.comm_cn_w(val as u8),
            0x01A0_4002 | 0x01A1_4002 => self.comm_fg = val as u8 & 0x01,
            _ => return false,
        }
        true
    }

    /// `offset` is the byte offset within 0x00800000.
    fn geo_w(&mut self, offset: u32, val: u32) {
        if offset < 0x1000 {
            if val & 0x8000_0000 != 0 {
                let r = (val & 0x800F_FFFF) | (((offset >> 4) & 0x3F) << 23);
                self.push_geo_data(r);
            } else if offset & 0xF == 0 {
                let mut r = (val & 0x000F_FFFF) | (((offset >> 4) & 0x3F) << 23);
                // Eye mode occupies bits 29-30 for function 1.
                if ((offset >> 4) & 0x3F) == 1 {
                    r |= ((offset >> 10) & 3) << 29;
                }
                self.push_geo_data(r);
            }
        } else if offset == 0x1008 {
            log::trace!(target: "geo", "W wr_start = {:08X}", val);
            self.geo_write_start_address = val & 0xFFFFF;
        } else if offset == 0x3008 {
            log::trace!(target: "geo", "W rd_start = {:08X}", val);
            self.geo_read_start_address = val & 0xFFFFF;
        }
    }

    /// Mirrors the reference: microcode upload while geo_ctl bit 31 is set,
    /// otherwise geometry data.
    fn geo_prg_w(&mut self, val: u32) {
        if self.geo_ctl & 0x8000_0000 != 0 {
            self.geo_cnt += 1;
        } else {
            self.push_geo_data(val);
        }
    }

    ///  The phase is exposed in bit 3 in the
    /// 30 Hz raster mode, or bit 2 in 60 Hz mode; it is not a high-halfword
    /// counter.
    fn videoctl_r(&mut self) -> u32 {
        let phase = if self.render_mode_ctl & 4 == 0 {
            (self.frame_num & 2) << 1
        } else {
            (self.frame_num & 1) << 2
        };
        phase | (self.video_ctl & 3)
    }

    fn render_mode_r(&mut self) -> u32 {
        self.render_mode_ctl
    }

    /// Model 2 texture RAM's host aperture accepts one 16-bit halfword per
    /// 32-bit i960 write. Even/odd aperture dwords select the low/high half
    /// of the rasterizer word respectively.
    /// Half-word store into the 2B/2C texture RAM, which is plain 32-bit RAM
    /// rather than the original board's 16-bit aperture.
    fn texture_w16(ram: &mut [u32], byte_off: u32, data: u16) {
        if let Some(slot) = ram.get_mut((byte_off >> 2) as usize) {
            if byte_off & 2 == 0 {
                *slot = (*slot & 0xffff_0000) | data as u32;
            } else {
                *slot = (*slot & 0x0000_ffff) | ((data as u32) << 16);
            }
        }
    }

    /// The original board's texture window is a 16-bit aperture: one 16-bit
    /// texel word per 32-bit CPU address.
    fn texture_w(ram: &mut [u32], byte_off: u32, data: u32) {
        let aperture_word = (byte_off >> 2) as usize;
        let Some(dst) = ram.get_mut(aperture_word >> 1) else {
            return;
        };
        if aperture_word & 1 == 0 {
            *dst = (*dst & 0xffff_0000) | (data & 0xffff);
        } else {
            *dst = (*dst & 0x0000_ffff) | ((data & 0xffff) << 16);
        }
    }

    /// Sega 315-5649 I/O chip, the direct replacement for the Model 1 I/O
    /// board on the 2A/2B/2C video boards. The reference maps it at 0x01c00000..1f with
    /// umask32(0x00ff00ff), so -- exactly like the dual-port RAM -- one CPU
    /// word exposes two consecutive device offsets.
    ///
    /// Port wiring for these boards (`model2b`/`model2a`/`model2c` configs):
    ///   A out eeprom B in IN0 C in IN1 D in IN2
    ///   E out billboard F out lamps G in dipswitches
    ///   0x0f is the auto-incrementing analog channel.
    fn io5649_byte(&mut self, offset: u32) -> u8 {
        let i = self.inputs;
        match offset {
            0x00 => self.io5649_ports[0],
            // Port B is IN0, except while the game holds the EEPROM in control
            // mode -- then it reads the serial data line back on bit 5.
            0x01 => {
                if self.io5649_ctrlmode {
                    0xC0 | ((self.eeprom.do_read() as u8) << 5) | 0x10 | (i.in0 & 0x0F)
                } else {
                    i.in0
                }
            }
            0x02 => i.in1,
            0x03 => i.in2,
            0x04 | 0x05 => self.io5649_ports[offset as usize],
            0x06 => i.dsw[0],
            // RS-422 channel 2 input. The 2A/2C light-gun cabinets hang the
            // gun interface board (837-12079) off it: the game writes a mux
            // index to channel 2 and reads one byte of gun data back. Index
            // 0..7 selects {P1_Y, P1_X, P2_Y, P2_X} low/high byte; anything
            // from 8 up returns the off-screen flags.
            0x0c => self.lightgun_mux_r(),
            // RS-422 status: receive buffers full, transmit buffers empty.
            0x0d => 0x0c,
            0x0f => {
                // Analog channels 0..7, auto-incrementing on each read. How
                // many are wired depends on the cabinet.
                let ch = self.io5649_analog;
                self.io5649_analog = (self.io5649_analog + 1) & 7;
                log::trace!(target: "io", "analog read ch{ch} = {:02X}", i.analog[ch as usize]);
                i.analog[ch as usize]
            }
            _ => 0xff,
        }
    }

    /// The 315-5649 register a CPU byte address selects, given the umask32
    /// 0x00ff00ff wiring: one 32-bit word carries two consecutive device
    /// registers, in byte lanes 0 and 2. The odd lanes are not wired to the
    /// device at all -- treating them as aliases makes a 16-bit access read
    /// the same register twice, which steps the analog mux an extra time.
    #[inline]
    fn io5649_reg_of(off: u32) -> Option<u32> {
        if off & 1 != 0 {
            return None;
        }
        Some((off >> 2) * 2 + ((off & 2) >> 1))
    }

    fn io5649_r(&mut self, off: u32) -> u32 {
        let idx = (off >> 1) & !1;
        let lo = self.io5649_byte(idx) as u32;
        let hi = self.io5649_byte(idx + 1) as u32;
        log::trace!(target: "io", "R {:02X}/{:02X} = {:02X} {:02X}", idx, idx + 1, lo, hi);
        lo | (hi << 16)
    }

    /// The gun interface board's mux, read back through the I/O chip's second
    /// serial channel.
    fn lightgun_mux_r(&mut self) -> u8 {
        let i = self.inputs;
        let mux = self.io5649_gun_mux;
        log::trace!(target: "io", "gun mux read idx={mux}");
        if mux >= 8 {
            // 0xfffc with bit 0 set when player 1 is aimed off-screen and bit
            // 1 for player 2; the game reads the low byte.
            let mut data: u16 = 0xfffc;
            if i.gun_offscreen {
                data |= 1;
            }
            return data as u8;
        }
        let port = [i.gun_y, i.gun_x, 0x200u16, 0x200u16][(mux >> 1) as usize];
        if mux & 1 != 0 {
            (port >> 8) as u8
        } else {
            port as u8
        }
    }

    /// Writes one 315-5649 register.
    fn io5649_byte_w(&mut self, reg: u32, byte: u8) {
        let n = reg as usize;
        if n < self.io5649_ports.len() {
            self.io5649_ports[n] = byte;
        }
        if n == 0 {
            // Port A: bit 0 control mode, bit 5 DI, bit 6 CS, bit 7 CLK.
            self.io5649_ctrlmode = byte & 0x01 != 0;
            self.eeprom.di_write(byte & 0x20 != 0);
            self.eeprom.cs_write(byte & 0x40 != 0);
            self.eeprom.clk_write(byte & 0x80 != 0);
        }
        if n == 0x0a {
            // RS-422 channel 2 output: selects which byte of gun data the
            // next channel-2 read returns.
            self.io5649_gun_mux = byte as u32;
        }
        if n == 0x0f {
            self.io5649_analog = (byte & 0x07) as u32;
            log::trace!(target: "io", "analog channel := {}", byte & 0x07);
        }
    }

    fn io5649_w(&mut self, off: u32, val: u32) {
        let idx = ((off >> 1) & !1) as usize;
        for (n, byte) in [(idx, val as u8), (idx + 1, (val >> 16) as u8)] {
            if n < self.io5649_ports.len() {
                self.io5649_ports[n] = byte;
            }
            // Register 0x0f is the analog mux: writing it picks the channel the
            // next read returns, and reads auto-increment from there. Dropping
            // this write leaves the counter free-running, which shuffles the
            // axes on any cabinet that does not read all eight in order.
            if n == 0x0f {
                self.io5649_analog = (byte & 0x07) as u32;
                log::trace!(target: "io", "analog channel := {}", byte & 0x07);
            }
            if n == 0 {
                // Port A: bit 0 control mode, bit 5 DI, bit 6 CS, bit 7 CLK.
                self.io5649_ctrlmode = byte & 0x01 != 0;
                self.eeprom.di_write(byte & 0x20 != 0);
                self.eeprom.cs_write(byte & 0x40 != 0);
                self.eeprom.clk_write(byte & 0x80 != 0);
            }
        }
    }

    /// The MB8421 is an 8-bit device on byte lanes 0 and 2 (umask32 0x00ff00ff),
    /// so one CPU word exposes two consecutive device bytes.
    fn dpram_r(&mut self, off: u32) -> u32 {
        let idx = (off >> 1) as usize & !1;
        let lo = self.dpram[idx] as u32;
        let hi = self.dpram[idx + 1] as u32;
        let val = lo | (hi << 16);
        log::trace!(target: "io", " R dev {:03X} = {:02X} {:02X}", idx, lo, hi);
        val
    }

    fn dpram_w(&mut self, off: u32, val: u32) {
        let idx = (off >> 1) as usize & !1;
        log::trace!(
            target: "io",
            "W dev {idx:03X} = {:02X} {:02X}",
            val & 0xFF,
            (val >> 16) & 0xFF
        );
        self.dpram[idx] = val as u8;
        self.dpram[idx + 1] = (val >> 16) as u8;
        self.io_board_update(idx);
    }

    /// HLE of the Sega Model 1 I/O board (a Z80 running epr-14869c). The board
    /// is only reachable through the dual-port RAM, so implementing the command
    /// protocol replaces the Z80 entirely.
    fn io_board_update(&mut self, idx: usize) {
        if idx == crate::system::IO_CMD {
            let cmd = self.dpram[crate::system::IO_CMD];
            if cmd != 0 {
                self.io_board_command(cmd);
                // Acknowledge: the board zeroes the command register when done.
                self.dpram[crate::system::IO_CMD] = 0;
            }
        }
    }
}

// --- TGP (MB86234) address maps ---
impl Mb86233Bus for Model2System {
    fn read_program(&mut self, addr: u32) -> u32 {
        self.tgp_program_ram
            .get(addr as usize)
            .copied()
            .unwrap_or(0)
    }

    fn read_data(&mut self, addr: u32) -> u32 {
        self.tgp_data_ram.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_data(&mut self, addr: u32, data: u32) {
        if let Some(slot) = self.tgp_data_ram.get_mut(addr as usize) {
            *slot = data;
        }
    }

    /// copro_tgp_io_map. This space is *modal*: writing the bank register (RF
    /// port 3) with bits 22-23 set selects a view of external memory over the
    /// whole 0x0000-0xffff range, shadowing the math registers at 0x20-0x2b;
    /// clearing those bits disables the view and the math registers come back.
    ///
    /// Routing unconditionally by address instead is wrong in both directions:
    /// with the bank selected, a track-data read at offset 0x20-0x2b would
    /// return a sincos table entry, and a data write there would corrupt the
    /// sincos/atan base registers used by every transform afterwards.
    fn read_io(&mut self, addr: u32) -> u32 {
        if self.copro_bank_reg & 0xc0_0000 != 0 {
            return self.copro_memory_r(addr);
        }
        match addr {
            0x20..=0x23 => self.copro_sincos_r(addr - 0x20),
            0x24..=0x27 => self.copro_atan_r(),
            0x28..=0x29 => self.copro_inv_r(addr - 0x28),
            0x2A..=0x2B => self.copro_isqrt_r(addr - 0x2A),
            // View disabled: nothing else is mapped.
            _ => 0,
        }
    }

    fn write_io(&mut self, addr: u32, data: u32) {
        if self.copro_bank_reg & 0xc0_0000 != 0 {
            return self.copro_memory_w(addr, data);
        }
        match addr {
            0x20..=0x23 => self.copro_sincos_base = data,
            0x24..=0x27 => {
                self.copro_atan_base[(addr - 0x24) as usize] = data;
                // The atan comparator drives the TGP's GPIO0 pin; the microcode
                // branches on it to pick the octant. It must be visible to the
                // very next instruction, so it lives here on the board -- the
                // CPU object in `self.tgp_cpu` is a placeholder while the real
                // one is executing, and state written to it is thrown away.
                self.copro_gpio0 = (self.copro_atan_base[0] & 0x7fff_ffff)
                    <= (self.copro_atan_base[1] & 0x7fff_ffff);
            }
            0x28..=0x29 => self.copro_inv_base = data,
            0x2A..=0x2B => self.copro_isqrt_base = data,
            _ => {}
        }
    }

    fn read_rf(&mut self, addr: u32) -> u32 {
        match addr {
            1 => match self.copro_fifo_in.pop_front() {
                Some(v) => {
                    log::trace!(target: "fifo", "TGP pop  in={v:08X} (in {})", self.copro_fifo_in.len());
                    v
                }
                None => {
                    // Nothing to do yet: ask the TGP to retry this instruction
                    // rather than run on with a bogus value.
                    self.copro_stall = true;
                    0
                }
            },
            _ => 0,
        }
    }

    fn take_stall(&mut self) -> bool {
        std::mem::take(&mut self.copro_stall)
    }

    fn gpio(&mut self, index: u32) -> bool {
        // Only pin 0 is wired on Model 2 (the atan comparator).
        index == 0 && self.copro_gpio0
    }

    fn halt_requested(&self) -> bool {
        // The ninth word is accepted by the FIFO, then its synchronized
        // full callback asserts HALT.  `Mb86233::execute` checks this after
        // completing the current instruction, which is the matching boundary;
        // waiting for the next 18 kHz quantum lets several extra writes leak.
        self.copro_fifo_out.len() > crate::system::COPRO_FIFO_DEPTH
    }

    fn write_rf(&mut self, addr: u32, data: u32) {
        match addr {
            0 => {} // leds / busy flag
            2 => {
                // the FIFO accepts the overflow word, then its zero-time
                // sync callback halts the source.  `halt_requested` enforces
                // that instruction boundary in our scheduler.
                log::trace!(target: "fifo", "TGP push out={data:08X} (out {})", self.copro_fifo_out.len());
                self.copro_fifo_out.push_back(data);
            }
            3 => self.copro_bank_reg = data,
            _ => {}
        }
    }
}

impl Model2System {
    fn copro_table(&self, index: usize) -> u32 {
        self.copro_tables.get(index).copied().unwrap_or(0)
    }

    fn copro_sincos_r(&self, offset: u32) -> u32 {
        let ang = self.copro_sincos_base.wrapping_add(offset * 0x4000);
        let mut index = (ang & 0x3fff) as usize;
        if ang & 0x4000 != 0 {
            index = (0x4000usize - index).min(0x3fff);
        }
        let mut result = self.copro_table(index);
        if ang & 0x8000 != 0 {
            result ^= 0x8000_0000;
        }
        result
    }

    fn copro_inv_r(&self, offset: u32) -> u32 {
        let index = (((self.copro_inv_base >> 9) & 0x3ffe) | (offset & 1)) as usize;
        let mut result = self.copro_table(index | 0x8000);
        let base_exp = ((self.copro_inv_base >> 23) & 0xff) as u8;
        let exp = ((result >> 23) as u8).wrapping_add(0x7f_u8.wrapping_sub(base_exp));
        result = (result & 0x007f_ffff) | ((exp as u32) << 23);
        if self.copro_inv_base & 0x8000_0000 != 0 && offset != 0 {
            result |= 0x8000_0000;
        }
        result
    }

    fn copro_isqrt_r(&self, offset: u32) -> u32 {
        let index = (0x2000 ^ (((self.copro_isqrt_base >> 10) & 0x3ffe) | (offset & 1))) as usize;
        let mut result = self.copro_table(index | 0xc000);
        let base_exp = ((self.copro_isqrt_base >> 24) & 0x7f) as u8;
        let exp = ((result >> 23) as u8).wrapping_add(0x3f_u8.wrapping_sub(base_exp));
        result = (result & 0x807f_ffff) | ((exp as u32) << 23);
        if offset & 1 == 0 {
            result &= 0x7fff_ffff;
        }
        result
    }

    fn copro_atan_r(&self) -> u32 {
        let ie = 0x88_u8.wrapping_sub((self.copro_atan_base[3] >> 23) as u8);
        let s0 = self.copro_atan_base[0] & 0x8000_0000 != 0;
        let s1 = self.copro_atan_base[1] & 0x8000_0000 != 0;
        let s2 = (self.copro_atan_base[0] & 0x7fff_ffff) <= (self.copro_atan_base[1] & 0x7fff_ffff);
        let im = self.copro_atan_base[3] & 0x7f_ffff;
        let mut index = if ie <= 0x17 {
            ((im | 0x80_0000) >> ie) as usize
        } else {
            0
        };
        if index == 0x4000 {
            index = 0x3fff;
        }
        let mut result = self.copro_table(index | 0x4000);
        if s0 ^ s1 ^ s2 {
            result >>= 16;
        }
        if s2 {
            result = result.wrapping_add(0x4000);
        }
        if (s0 && !s2) || (s1 && s2) {
            result = result.wrapping_add(0x8000);
        }
        result & 0xffff
    }

    /// The TGP's external data window: the bank register supplies the high
    /// byte, so the same 16-bit offset reaches either the coprocessor data ROM
    /// or the buffer RAM it shares with the i960.
    fn copro_memory_r(&mut self, offset: u32) -> u32 {
        let adr = (self.copro_bank_reg & 0xFF_0000) | offset;
        if adr & 0x80_0000 != 0 {
            let masked = adr & (self.copro_data.len() as u32 - 1);
            return self.copro_data[masked as usize];
        }
        if adr & 0x40_0000 != 0 {
            return self.buffer_ram[(adr & 0x7FFF) as usize];
        }
        0
    }

    /// The writable half of that window. Only the buffer RAM answers; the
    /// data ROM is read-only and a write to it goes nowhere.
    fn copro_memory_w(&mut self, offset: u32, data: u32) {
        let adr = (self.copro_bank_reg & 0xFF_0000) | offset;
        if adr & 0x40_0000 != 0 {
            self.buffer_ram[(adr & 0x7FFF) as usize] = data;
        }
    }
}

// --- ADSP-21062 SHARC coprocessor bus (Model 2B) ---
//
// The SHARC reaches the same FIFOs, shared buffer RAM and copro data ROM the
// TGP does, but through its own data-space addresses:
//
//   0x0400000..0x0bfffff read  -> copro_fifo_in
//   0x0c00000..0x13fffff write -> copro_fifo_out
//   0x1400000..0x1bfffff r/w   -> shared buffer RAM (0x8000 words)
//   0x1c00000..0x1dfffff read  -> copro_data ROM
impl sharc::SharcBus for Model2System {
    fn dm_ext_read(&mut self, addr: u32) -> u32 {
        self.sharc_reads += 1;
        let bucket = match addr {
            0x0400000..=0x0bfffff => 0,
            0x1400000..=0x1bfffff => 1,
            0x1c00000..=0x1dfffff => 2,
            _ => 3,
        };
        self.sharc_read_addrs[bucket] += 1;
        match addr {
            0x0400000..=0x0bfffff => self.copro_fifo_in.pop_front().unwrap_or(0),
            0x1400000..=0x1bfffff => self
                .buffer_ram
                .get((addr & 0x7fff) as usize)
                .copied()
                .unwrap_or(0),
            0x1c00000..=0x1dfffff => self
                .copro_data
                .get((addr & 0x1f_ffff) as usize)
                .copied()
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn dm_ext_write(&mut self, addr: u32, data: u32) {
        self.sharc_writes += 1;
        let b = match addr {
            0x0c00000..=0x13fffff => 0,
            0x1400000..=0x1bfffff => 1,
            _ => 2,
        };
        self.sharc_write_addrs[b] += 1;
        if b == 2 && self.sharc_writes < 400 {
            let n = (self.sharc_writes as usize) % 8;
            self.sharc_write_samples[n] = addr;
        }
        match addr {
            0x0c00000..=0x13fffff => self.copro_fifo_out.push_back(data),
            0x1400000..=0x1bfffff => {
                if let Some(slot) = self.buffer_ram.get_mut((addr & 0x7fff) as usize) {
                    *slot = data;
                }
            }
            _ => {}
        }
    }

    fn fifo_in_empty(&self) -> bool {
        self.copro_fifo_in.is_empty()
    }

    fn fifo_out_full(&self) -> bool {
        // The reference wires FLAG1 to the *input* FIFO's full state, and both 2B FIFOs
        // are 16 deep.
        self.copro_fifo_in.len() >= 16
    }
}

// --- MB86235 "TGPx4" coprocessor bus (Model 2C) ---
//
// The reference: the buffer RAM the i960 shares sits at
// 0x00400000 with a 0x3f8000 mirror, and the coprocessor data ROM at
// 0x00800000. Command and result words move through the same FIFOs the TGP and
// SHARC use on the earlier boards.
impl mb86235::Mb86235Bus for Model2System {
    fn data_read(&mut self, addr: u32) -> u32 {
        self.tgpx4_ext_r += 1;
        let b = match addr {
            0x0040_0000..=0x007F_FFFF => 0,
            0x0080_0000..=0x009F_FFFF => 1,
            _ => 2,
        };
        self.tgpx4_rbucket[b] += 1;
        if b == 2 && self.tgpx4_rbucket[2] < 9 {
            self.tgpx4_rsample[(self.tgpx4_rbucket[2] as usize - 1) & 7] = addr;
        }
        match addr {
            0x0040_0000..=0x007F_FFFF => self
                .buffer_ram
                .get((addr & 0x7fff) as usize)
                .copied()
                .unwrap_or(0),
            0x0080_0000..=0x009F_FFFF => self
                .copro_data
                .get(((addr - 0x0080_0000) & 0x1f_ffff) as usize)
                .copied()
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn data_write(&mut self, addr: u32, data: u32) {
        self.tgpx4_ext_w += 1;
        if let 0x0040_0000..=0x007F_FFFF = addr {
            if let Some(slot) = self.buffer_ram.get_mut((addr & 0x7fff) as usize) {
                *slot = data;
            }
        }
    }

    fn fifo_in_pop(&mut self) -> Option<u32> {
        let v = self.copro_fifo_in.pop_front();
        if let Some(word) = v {
            self.tgpx4_pops += 1;
            log::trace!(target: "fifo", "pop  {word:08X} pc={:04X}", self.tgpx4.pc);
        }
        v
    }

    fn fifo_out_push(&mut self, data: u32) {
        self.tgpx4_pushes += 1;
        log::trace!(target: "fifo", "push {:08X} pc={:04X}", data, self.tgpx4.pc);
        self.copro_fifo_out.push_back(data);
    }

    fn fifo_in_empty(&self) -> bool {
        self.copro_fifo_in.is_empty()
    }

    fn fifo_in_full(&self) -> bool {
        self.copro_fifo_in.len() >= crate::system::COPRO_FIFO_DEPTH
    }

    fn fifo_out_empty(&self) -> bool {
        self.copro_fifo_out.is_empty()
    }

    fn fifo_out_full(&self) -> bool {
        self.copro_fifo_out.len() >= crate::system::COPRO_FIFO_DEPTH
    }
}
