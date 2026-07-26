#!/usr/bin/env python3
"""Generates crates/tgpulse-core/src/roms_db.dat from MAME's Sega Model 1/2 ROM_STARTs.

The generic loader (loader.rs) consumes the emitted table to build the memory
image for any romset, instead of a hand-written function per game. Re-run when
the MAME source is updated:

    python3 tools/gen_roms_db.py /tmp/mame-src

Emits one record per game as a small line-oriented format:

    G <name> <board>              board: m1 m2o m2a m2b m2c
    R <region> <size>             ROM_REGION (hex size)
    L <region> <file> <off> <sz> <kind>   kind: p w 4w 4b 2b
    C <region> <srcoff> <dstoff> <len>    ROM_COPY within a region
"""
import re, sys, os

MAME = sys.argv[1] if len(sys.argv) > 1 else "/tmp/mame-src"
OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "tgpulse-core", "src", "roms_db.dat")

# State class -> board tag.
BOARD = {
    "model1_state": "m1", "netmerc_state": "m1",
    "model2o_state": "m2o", "model2o_maxx_state": "m2o", "model2o_gtx_state": "m2o",
    "model2a_state": "m2a", "model2a_airwlkrs_state": "m2a",
    "model2b_state": "m2b",
    "model2c_state": "m2c",
}

# String-macro region names.
REGION_MACROS = {
    "M1AUDIO_CPU_REGION": "m1audio:sndcpu",
    "M1AUDIO_MPCM1_REGION": "m1audio:pcm1",
    "M1AUDIO_MPCM2_REGION": "m1audio:pcm2",
}

NUM = r"(0x[0-9a-fA-F]+|[0-9]+)"

def num(s):
    return int(s, 16) if s.lower().startswith("0x") else int(s, 10)

def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()

def macro_body(text, name):
    """Extracts a multi-line #define body (backslash-continued)."""
    m = re.search(r"#define\s+" + name + r"\b(.*?)(?<!\\)\n", text, re.S)
    if not m:
        return ""
    return m.group(1).replace("\\\n", "\n")


# MAME's input-port set name -> the control scheme we emulate. The scheme picks
# the digital button/stick layout; the analog channel roles below say what each
# of the I/O chip's eight ADC channels is wired to, which is what actually
# differs between two games that share a scheme.
SCHEME_BY_PORTS = {
    # wheel + pedals
    "daytona": "racing", "indy500": "racing", "srallyc": "racing",
    "overrev": "racing", "sgt24h": "racing", "vr": "racing", "stcc": "racing",
    "desert": "racing", "manxtt": "bike", "motoraid": "bike",
    # stick + buttons
    "vf": "joystick", "vf2": "joystick", "doa": "joystick", "schamp": "joystick",
    "vstriker": "joystick", "dynamcop": "joystick", "pltkids": "joystick",
    "zerogun": "joystick", "airwlkrs": "joystick", "von": "joystick",
    "dynabb": "joystick", "model2crx": "joystick", "model2": "joystick",
    # light gun / mounted gun
    "vcop": "gun", "vcop2": "gun", "hotd": "gun", "gunblade": "gun",
    "rchase2": "gun", "rchase2a": "gun", "bel": "gun",
    # flight stick + throttle
    "swa": "flight", "wingwar": "flight", "wingwar360": "flight",
    "netmerc": "flight", "skytargt": "flight",
    # one-off cabinets
    "waverunr": "jetski", "topskatr": "skate", "segawski": "ski",
    "skisuprg": "ski", "powsled": "sled",
}

# MAME io-port name -> the role our input code fills that ADC channel with.
ROLE_BY_PORT = {
    "STEER": "steer", "WHEEL": "steer", "BANK": "steer", "HANDLE": "steer",
    "ACCEL": "accel", "THROTTLE": "throttle",
    "BRAKE": "brake",
    "STICKX": "stickx", "STICK1X": "stickx", "STICK2X": "stick2x",
    "STICKY": "sticky", "STICK1Y": "sticky", "STICK2Y": "stick2y",
    "P1_X": "gun1x", "P1_Y": "gun1y", "P2_X": "gun2x", "P2_Y": "gun2y",
    "ROLL": "roll", "PITCH": "pitch",
    "SLIDE": "slide", "CURVING": "curving", "SWING": "swing",
    "INCLINING": "incline",
    "BAT1": "bat1", "BAT2": "bat2",
    "P1_R": "p1r", "P1_L": "p1l", "P2_R": "p2r", "P2_L": "p2l",
}


def scheme_table(text):
    """name -> (scheme, [role per ADC channel]) derived from MAME."""
    # GAME( year, name, parent, machine, inputs, class, ... )
    game = {}
    for m in re.finditer(r"^\s*GAME[BL]?\(\s*[0-9?]+\s*,\s*([a-z0-9_]+)\s*,"
                         r"\s*[a-z0-9_]+\s*,\s*([a-z0-9_]+)\s*,\s*([a-z0-9_]+)",
                         text, re.M):
        game[m.group(1)] = (m.group(2), m.group(3))

    # machine config -> ADC channel wiring
    wiring = {}
    for m in re.finditer(r"void\s+\w+::(\w+)\(machine_config &config\)\s*\{(.*?)\n\}",
                         text, re.S):
        ch = dict((int(a), b) for a, b in re.findall(
            r"an_(?:port_)?callback<(\d)>\(\)\.set_ioport\(\"(\w+)\"\)", m.group(2)))
        if ch:
            wiring[m.group(1)] = ch

    # A variant machine config usually calls its base first and only tweaks a
    # few things, so inherit the base's ADC wiring when it adds none of its own.
    calls = {}
    for m in re.finditer(r"void\s+\w+::(\w+)\(machine_config &config\)\s*\{(.*?)\n\}",
                         text, re.S):
        calls[m.group(1)] = re.findall(r"^\s*(\w+)\(config\);", m.group(2), re.M)
    for machine in list(calls):
        seen = set()
        stack = list(calls.get(machine, []))
        while stack and machine not in wiring:
            base = stack.pop(0)
            if base in seen:
                continue
            seen.add(base)
            if base in wiring:
                wiring[machine] = wiring[base]
                break
            stack.extend(calls.get(base, []))

    out = {}
    for name, (machine, ports) in game.items():
        scheme = SCHEME_BY_PORTS.get(ports, "joystick")
        ch = wiring.get(machine, {})
        roles = ["none"] * 8
        for i, port in ch.items():
            if i < 8:
                roles[i] = ROLE_BY_PORT.get(port, "none")
        # Games whose gun is read through the I/O chip's serial mux rather than
        # the ADC still need their axes named.
        if scheme == "gun" and all(r == "none" for r in roles):
            roles[:4] = ["gun1x", "gun1y", "gun2x", "gun2y"]
        while roles and roles[-1] == "none":
            roles.pop()
        out[name] = (scheme, roles)
    return out


# The I/O board's Z80 firmware lives in MAME's `model1io_device`, not in any
# game's ROM_START, so it has to be attached here. Which revision a game uses
# comes from its `set_default_bios_tag`; the boards that carry the later
# `model1io2` are left alone, since that device is not emulated.
IOBOARD_FIRMWARE = {
    "vf": "epr-14869b.25",
    "swa": "epr-14869b.25",
    "swaj": "epr-14869b.25",
}
IOBOARD_DEFAULT = "epr-14869.25"
IOBOARD_NONE = {"wingwar", "wingwar360", "wingwarj", "wingwaru", "netmerc"}


def main():
    files = [os.path.join(MAME, "src/mame/sega/model1.cpp"),
             os.path.join(MAME, "src/mame/sega/model2.cpp")]
    text = "\n".join(read(p) for p in files)

    macros = {n: macro_body(text, n) for n in
              ("MODEL1_CPU_BOARD", "MODEL2_CPU_BOARD", "MODEL2A_VID_BOARD")}

    # name -> board and title, from
    # GAME( year, name, parent, machine, input, class, init, rot, maker, title, flags )
    board, title = {}, {}
    for m in re.finditer(r"^\s*GAME[BL]?\(\s*([0-9?]+)\s*,\s*([a-z0-9_]+)\s*,"
                         r"\s*[a-z0-9_]+\s*,\s*[a-z0-9_]+\s*,\s*[a-z0-9_]+\s*,"
                         r"\s*([a-z0-9_]+)\s*,[^,]+,[^,]+,\s*\"([^\"]*)\"\s*,\s*\"([^\"]*)\"",
                         text, re.M | re.S):
        year, name, cls, maker, desc = m.groups()
        if cls in BOARD:
            board[name] = BOARD[cls]
            title[name] = (desc, year, maker)

    schemes = scheme_table(text)

    records = []
    for m in re.finditer(r"ROM_START\(\s*([a-z0-9_]+)\s*\)(.*?)ROM_END", text, re.S):
        name, body = m.group(1), m.group(2)
        if name not in board:
            continue  # a set whose GAME line we could not classify
        # Expand board macros and string-macro region names.
        for mac, val in macros.items():
            body = body.replace(mac, val)
        for tok, val in REGION_MACROS.items():
            body = body.replace(tok, '"%s"' % val)

        regions, loads, copies = [], [], []
        region = None
        last_file = None  # for ROM_RELOAD
        for line in body.splitlines():
            line = line.strip()
            if line.startswith("//"):
                continue

            rm = re.match(r'ROM_REGION(16_BE|16_LE|32_LE|32_BE)?\(\s*(0x[0-9a-fA-F]+)\s*,\s*"([^"]+)"([^)]*)', line)
            if rm:
                region = rm.group(3)
                fill = "ff" if "ERASEFF" in rm.group(4) else "00"
                regions.append((region, int(rm.group(2), 16), fill))
                continue

            # ROM_LOAD / ROMX_LOAD / ROM_LOAD16_WORD_SWAP / ROM_LOAD32_WORD / ...
            lm = re.match(r'ROMX?_(LOAD(32_WORD|16_WORD_SWAP|32_BYTE|16_BYTE)?)\(\s*"([^"]+)"\s*,\s*' + NUM + r'\s*,\s*' + NUM, line)
            if lm and region:
                variant = lm.group(2) or ""
                kind = {"": "p", "16_WORD_SWAP": "w", "32_WORD": "4w",
                        "32_BYTE": "4b", "16_BYTE": "2b"}[variant]
                off = num(lm.group(4))
                sz = num(lm.group(5))
                file = lm.group(3)
                loads.append((region, file, off, sz, kind))
                last_file = (file, sz, kind)
                continue

            cm = re.match(r'ROM_COPY\(\s*"([^"]+)"\s*,\s*' + NUM + r'\s*,\s*' + NUM + r'\s*,\s*' + NUM, line)
            if cm and region:
                copies.append((region, num(cm.group(2)),
                               num(cm.group(3)), num(cm.group(4))))
                continue

            rl = re.match(r'ROM_RELOAD\(\s*' + NUM + r'\s*,\s*' + NUM, line)
            if rl and region and last_file:
                loads.append((region, last_file[0], num(rl.group(1)),
                              num(rl.group(2)), last_file[2]))
                continue

        if board[name] == "m1" and name not in IOBOARD_NONE:
            regions.append(("ioboard:iocpu", 0x10000, "00"))
            loads.append(("ioboard:iocpu",
                          IOBOARD_FIRMWARE.get(name, IOBOARD_DEFAULT),
                          0, 0x10000, "p"))

        scheme, roles = schemes.get(name, ("joystick", []))
        records.append((name, board[name], scheme, roles,
                        title.get(name, (name, "????", "Sega")),
                        regions, loads, copies))

    lines = []
    for name, brd, scheme, roles, (desc, year, maker), regions, loads, copies in sorted(records):
        lines.append(" ".join(["G", name, brd, scheme] + roles))
        lines.append("T %s\t%s\t%s" % (desc, year, maker))
        for r, sz, fill in regions:
            lines.append("R %s %x %s" % (r, sz, fill))
        for r, f, off, sz, kind in loads:
            lines.append("L %s %s %x %x %s" % (r, f, off, sz, kind))
        for r, so, do, ln in copies:
            lines.append("C %s %x %x %x" % (r, so, do, ln))

    with open(OUT, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("wrote %d games -> %s" % (len(records), OUT))

if __name__ == "__main__":
    main()
