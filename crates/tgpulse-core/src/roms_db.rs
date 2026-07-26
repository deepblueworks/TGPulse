//! Data-driven ROM loading for every Sega Model 1 / Model 2 game.
//!
//! `roms_db.dat` is generated from the arcade driver definitions by
//! `tools/gen_roms_db.py`; this module parses it and builds the region images
//! for whatever romset is supplied, so no per-game loader is needed. The board
//! that fully emulates a game (Model 1, Model 2 original, Model 2A) boots from
//! the result; the SHARC (2B) and TGPx4 (2C) boards load but cannot render 3D
//! until those coprocessors exist.

use std::collections::HashMap;
use std::fs::File;
use std::sync::LazyLock;
use zip::ZipArchive;

use crate::loader::{load16_byte, load16_word_swap, load32_byte, load32_word, read_chip};

const DB: &str = include_str!("roms_db.dat");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Board {
    Model1,
    Model2o,
    Model2a,
    Model2b,
    Model2c,
}

impl Board {
    fn parse(tag: &str) -> Option<Board> {
        Some(match tag {
            "m1" => Board::Model1,
            "m2o" => Board::Model2o,
            "m2a" => Board::Model2a,
            "m2b" => Board::Model2b,
            "m2c" => Board::Model2c,
            _ => return None,
        })
    }
    /// Short name for the board, for a list view.
    pub fn label(self) -> &'static str {
        match self {
            Board::Model1 => "Model 1",
            Board::Model2o => "Model 2",
            Board::Model2a => "Model 2A",
            Board::Model2b => "Model 2B",
            Board::Model2c => "Model 2C",
        }
    }

    pub fn is_model1(self) -> bool {
        self == Board::Model1
    }
}

struct Load {
    region: String,
    file: String,
    off: usize,
    kind: [u8; 2],
}

struct Copy {
    region: String,
    src: usize,
    dst: usize,
    len: usize,
}

/// The control layout a cabinet presents, as classified from the reference input
/// port sets. The scheme decides the digital button/stick mapping; which
/// physical axis reaches which ADC channel is described by `analog_roles`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scheme {
    /// Wheel, pedals and view buttons.
    Racing,
    /// Handlebars: throttle and brake levers plus a lean axis.
    Bike,
    /// Eight-way stick and three or four buttons.
    #[default]
    Joystick,
    /// Light gun or mounted gun, aimed with the mouse.
    Gun,
    /// Two-axis flight stick with a throttle lever.
    Flight,
    /// Wave Runner's jet ski.
    Jetski,
    /// Top Skater's board: a curving axis and a slide axis.
    Skate,
    /// Ski cabinets: swing/slide left-right plus an incline axis.
    Ski,
    /// Power Sled's four foot pedals.
    Sled,
}

impl Scheme {
    fn parse(tag: &str) -> Scheme {
        match tag {
            "racing" => Scheme::Racing,
            "bike" => Scheme::Bike,
            "gun" => Scheme::Gun,
            "flight" => Scheme::Flight,
            "jetski" => Scheme::Jetski,
            "skate" => Scheme::Skate,
            "ski" => Scheme::Ski,
            "sled" => Scheme::Sled,
            _ => Scheme::Joystick,
        }
    }
}

/// What one of the I/O chip's eight ADC channels is wired to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AnalogRole {
    #[default]
    None,
    Steer,
    Accel,
    Brake,
    Throttle,
    StickX,
    StickY,
    Stick2X,
    Stick2Y,
    Gun1X,
    Gun1Y,
    Gun2X,
    Gun2Y,
    Roll,
    Pitch,
    Slide,
    Curving,
    Swing,
    Incline,
    Bat1,
    Bat2,
    P1R,
    P1L,
    P2R,
    P2L,
}

impl AnalogRole {
    fn parse(tag: &str) -> AnalogRole {
        use AnalogRole::*;
        match tag {
            "steer" => Steer,
            "accel" => Accel,
            "brake" => Brake,
            "throttle" => Throttle,
            "stickx" => StickX,
            "sticky" => StickY,
            "stick2x" => Stick2X,
            "stick2y" => Stick2Y,
            "gun1x" => Gun1X,
            "gun1y" => Gun1Y,
            "gun2x" => Gun2X,
            "gun2y" => Gun2Y,
            "roll" => Roll,
            "pitch" => Pitch,
            "slide" => Slide,
            "curving" => Curving,
            "swing" => Swing,
            "incline" => Incline,
            "bat1" => Bat1,
            "bat2" => Bat2,
            "p1r" => P1R,
            "p1l" => P1L,
            "p2r" => P2R,
            "p2l" => P2L,
            _ => None,
        }
    }
}

pub struct GameDef {
    /// The short set name, `vf2`, `waverunr`.
    pub name: String,
    /// The title on the cabinet, for showing to a player.
    pub title: String,
    /// Year of release, as printed in the driver; "????" when unknown.
    pub year: String,
    pub manufacturer: String,
    pub board: Board,
    pub scheme: Scheme,
    /// Role of each of the eight ADC channels, in channel order.
    pub analog_roles: [AnalogRole; 8],
    regions: Vec<(String, usize, u8)>,
    loads: Vec<Load>,
    copies: Vec<Copy>,
}

impl GameDef {
    /// Every distinct filename this romset expects, for matching a zip to a game.
    pub(crate) fn files(&self) -> impl Iterator<Item = &str> {
        self.loads.iter().map(|l| l.file.as_str())
    }
}

/// The whole database, parsed once.
static GAMES: LazyLock<Vec<GameDef>> = LazyLock::new(parse_db);

fn parse_num(s: &str) -> usize {
    usize::from_str_radix(s, 16).unwrap_or(0)
}

fn parse_db() -> Vec<GameDef> {
    let mut games: Vec<GameDef> = Vec::new();
    for line in DB.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("G") => {
                let name = it.next().unwrap_or("").to_string();
                let board = it.next().and_then(Board::parse).unwrap_or(Board::Model2a);
                let scheme = Scheme::parse(it.next().unwrap_or(""));
                let mut analog_roles = [AnalogRole::None; 8];
                for slot in analog_roles.iter_mut() {
                    match it.next() {
                        Some(tag) => *slot = AnalogRole::parse(tag),
                        None => break,
                    }
                }
                games.push(GameDef {
                    title: String::new(),
                    year: String::new(),
                    manufacturer: String::new(),
                    name,
                    board,
                    scheme,
                    analog_roles,
                    regions: Vec::new(),
                    loads: Vec::new(),
                    copies: Vec::new(),
                });
            }
            Some("T") => {
                // `T <title>\t<year>\t<manufacturer>`, attached to the game
                // record just opened.
                if let Some(g) = games.last_mut() {
                    let rest = line.get(2..).unwrap_or("");
                    let mut f = rest.split('\t');
                    g.title = f.next().unwrap_or("").to_string();
                    g.year = f.next().unwrap_or("").to_string();
                    g.manufacturer = f.next().unwrap_or("").to_string();
                }
            }
            Some("R") => {
                if let Some(g) = games.last_mut() {
                    let region = it.next().unwrap_or("").to_string();
                    let size = parse_num(it.next().unwrap_or("0"));
                    let fill = if it.next() == Some("ff") { 0xff } else { 0x00 };
                    g.regions.push((region, size, fill));
                }
            }
            Some("L") => {
                if let Some(g) = games.last_mut() {
                    let region = it.next().unwrap_or("").to_string();
                    let file = it.next().unwrap_or("").to_string();
                    let off = parse_num(it.next().unwrap_or("0"));
                    let _sz = it.next();
                    let k = it.next().unwrap_or("p").as_bytes();
                    let kind = [k[0], if k.len() > 1 { k[1] } else { 0 }];
                    g.loads.push(Load {
                        region,
                        file,
                        off,
                        kind,
                    });
                }
            }
            Some("C") => {
                if let Some(g) = games.last_mut() {
                    let region = it.next().unwrap_or("").to_string();
                    let src = parse_num(it.next().unwrap_or("0"));
                    let dst = parse_num(it.next().unwrap_or("0"));
                    let len = parse_num(it.next().unwrap_or("0"));
                    g.copies.push(Copy {
                        region,
                        src,
                        dst,
                        len,
                    });
                }
            }
            _ => {}
        }
    }
    games
}

/// Identifies which game an archive is by matching its files against the
/// database. The best match by shared-filename count wins, so a merged or
/// slightly-off set still resolves to the right game.
pub fn identify(names: &[String]) -> Option<&'static GameDef> {
    let present: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut best: Option<(&GameDef, usize)> = None;
    for g in GAMES.iter() {
        let hits = g.files().filter(|f| present.contains(f)).count();
        if hits == 0 {
            continue;
        }
        if best.is_none_or(|(_, b)| hits > b) {
            best = Some((g, hits));
        }
    }
    best.map(|(g, _)| g)
}

/// Builds every ROM region for a game by applying its load and copy directives
/// to the files in the archive. Missing files are warned about and skipped, so
/// a split or incomplete set still produces as much as it can.
pub fn build_regions(
    def: &GameDef,
    archive: &mut ZipArchive<File>,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut regions: HashMap<String, Vec<u8>> = HashMap::new();
    for (name, size, fill) in &def.regions {
        regions.insert(name.clone(), vec![*fill; *size]);
    }
    let mut missing = 0;
    for load in &def.loads {
        let data = match read_chip(archive, &load.file) {
            Ok(d) => d,
            Err(_) => {
                missing += 1;
                continue;
            }
        };
        let Some(dest) = regions.get_mut(&load.region) else {
            continue;
        };
        let r = match &load.kind {
            b"p\0" => crate::loader::copy_at(dest, load.off, &data),
            b"w\0" => load16_word_swap(dest, load.off, &data),
            b"4w" => load32_word(dest, load.off, &data),
            b"4b" => load32_byte(dest, load.off & !3, load.off & 3, &data),
            b"2b" => load16_byte(dest, load.off & !1, load.off & 1, &data),
            _ => Ok(()),
        };
        r?;
    }
    for c in &def.copies {
        if let Some(reg) = regions.get_mut(&c.region) {
            if c.src + c.len <= reg.len() && c.dst + c.len <= reg.len() {
                reg.copy_within(c.src..c.src + c.len, c.dst);
            }
        }
    }
    if missing > 0 {
        log::info!(target: "loader", "warning: {missing} ROM file(s) missing from the set");
    }
    Ok(regions)
}
