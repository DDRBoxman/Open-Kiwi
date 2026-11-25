//! Wire protocol for the Cytrence Kiwi control channel (the "Kiwi Serial" CDC port).
//!
//! Frame layout (all integers little-endian):
//!
//! ```text
//!  preamble  3C 5A A5 C3
//!  flags     u8   2 for opcodes 0x64 / 0x78, else 0
//!  crc8      u8   CRC-8 (poly 0x07, init 0) over the body only
//!  seq       u16
//!  type      u16
//!  length    u16  body length
//!  body      [length]
//!  trailer   C9 6D D6 9C
//! ```

use std::fmt;

pub const PREAMBLE: [u8; 4] = [0x3C, 0x5A, 0xA5, 0xC3];
pub const TRAILER: [u8; 4] = [0xC9, 0x6D, 0xD6, 0x9C];
pub const HEADER_LEN: usize = 8;
/// The host app rejects bodies of 0x441 bytes or more.
pub const MAX_BODY: usize = 0x440;

/// Message opcodes (`CyMsg::Type`).
pub mod op {
    /// Host -> Kiwi: end the session.
    pub const DISCONNECT: u16 = 0x14;
    /// Host -> Kiwi: open session; body is `u32` TxFrameFactor (default 500).
    pub const CONNECT: u16 = 0x32;
    /// Kiwi -> host: device identity.
    pub const KIWI_INFO: u16 = 0x64;
    /// Kiwi -> host: periodic status.
    pub const KIWI_STATE: u16 = 0x65;
    /// Host -> Kiwi: feature/logging config, 20-byte body (see [`KiwiConfig`](super::KiwiConfig)).
    pub const KIWI_CONFIG: u16 = 0x78;
    /// Host -> Kiwi: target USB config (2-byte body).
    pub const TARGET_CONFIG: u16 = 0x96;
    /// Host -> Kiwi: query; body `u16` 1 = info, 2 = state.
    pub const QUERY: u16 = 0xC8;
    /// Kiwi -> host: log text, body `u16` level + NUL-terminated string.
    pub const LOG: u16 = 0x12C;
    /// Host -> Kiwi: 8-byte HID boot-keyboard report.
    pub const KEYBOARD: u16 = 0x190;
    /// Host -> Kiwi: relative mouse `[buttons, dx, dy, wheel, pan]`.
    pub const MOUSE_REL: u16 = 0x19A;
    /// Host -> Kiwi: absolute mouse `[buttons, x:u16, y:u16, wheel, pan]`, x/y in 0..=32767.
    pub const MOUSE_ABS: u16 = 0x19B;
    /// Host -> Kiwi: text for the Kiwi to type itself (raw bytes).
    pub const TEXT: u16 = 0x1A4;
    /// Both directions: the virtual-serial-port byte stream, raw bytes. Routed by
    /// `KiwiConfig::vcomm` (1 = target USB serial, 2 = side UART).
    pub const VCOMM: u16 = 0x1FE;
    /// Both directions: the terminal byte stream, raw bytes. Routed by `KiwiConfig::vterm`
    /// (1 = target USB serial, 2 = side UART).
    pub const TERMINAL: u16 = 0x1FF;
    /// Host -> Kiwi: set up one side-port GPIO, 8-byte body (see [`gpio`](super::gpio)).
    pub const SIDE_GPIO: u16 = 0x227;
    /// Host -> Kiwi: write a span to the I2C EEPROM at bus address 0x54.
    /// Body `[addr:u16, len:u16, crc32:u32, data[len]]`; device verifies the CRC-32.
    pub const EEPROM_WRITE: u16 = 0x11A8;
    /// Host <-> Kiwi: verify/read the EEPROM. Request body `[addr:u16, len:u16, expected_crc32:u32]`;
    /// the device CRC-32s the `len`-byte window at `addr` and replies (same opcode) with that
    /// CRC-32 as a 4-byte LE body. See `eeprom` below for the read-by-oracle trick.
    pub const EEPROM_VERIFY: u16 = 0x11B2;
    /// Host -> Kiwi: reboot into the RP2040 USB bootloader (BOOTSEL / "RPI-RP2" drive), empty body.
    /// The firmware calls the bootrom `reset_to_usb_boot`. The KVM session ends; replug to recover.
    pub const REBOOT_BOOTSEL: u16 = 0xBC2;

    // Firmware update (OTA over the control channel). See the `fwupdate` module.
    /// Begin: body = image header[0x10..0x80] (CyFirmwareInfo).
    pub const FW_BEGIN: u16 = 0x100E;
    /// Erase staging: body = `u16` sector count.
    pub const FW_ERASE: u16 = 0x1013;
    /// Flash data chunk: body = `[idx:u16, crc32(chunk):u32, chunk:256]` (262 bytes).
    pub const FW_DATA: u16 = 0x1018;
    /// Verify: body = `u32` length; the device replies (same opcode) with the staging CRC-32.
    pub const FW_VERIFY: u16 = 0x1022;
    /// Finish/commit: body = the full 0x80 image header.
    pub const FW_FINISH: u16 = 0x1036;
    /// Normal reboot (runs the bootloader, which applies any staged firmware), empty body.
    /// Distinct from REBOOT_BOOTSEL (0xBC2); this reboots into the application path.
    pub const FW_REBOOT: u16 = 0xBB8;
}

/// `TARGET_CONFIG` body is `[kind, value]`. The vendor app sends `[USB, 0]`, waits 1 s,
/// then the setting it wants to change.
pub mod target {
    pub const USB: u8 = 0;
    pub const MSC: u8 = 1;
    pub const CDC: u8 = 2;
    pub const VGA_USB_2IN1: u8 = 3;
    pub const PS2: u8 = 4;
    pub const BOOT_MOUSE: u8 = 5;
    /// 0 = Cytrence VID, 1 = legacy VID.
    pub const USB_VID: u8 = 6;
}

pub mod query {
    pub const INFO: u16 = 1;
    pub const STATE: u16 = 2;
}

const CRC8_TABLE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
            b += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| CRC8_TABLE[(c ^ b) as usize])
}

/// Reflected CRC-32/ISO-HDLC (the zlib/IEEE variant): poly 0xEDB88320, init/xorout 0xFFFFFFFF.
/// This is the CRC the Kiwi firmware uses for EEPROM and firmware-image payloads.
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut b = 0;
        while b < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            b += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(0xFFFF_FFFFu32, |c, &b| (c >> 8) ^ CRC32_TABLE[((c ^ b as u32) & 0xFF) as usize])
}

fn flags_for(ty: u16) -> u8 {
    if ty == op::KIWI_CONFIG || ty == op::KIWI_INFO { 2 } else { 0 }
}

pub fn encode(ty: u16, seq: u16, body: &[u8]) -> Vec<u8> {
    assert!(body.len() <= MAX_BODY, "body too large: {}", body.len());
    let mut out = Vec::with_capacity(4 + HEADER_LEN + body.len() + 4);
    out.extend_from_slice(&PREAMBLE);
    out.push(flags_for(ty));
    out.push(crc8(body));
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(&TRAILER);
    out
}

#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub flags: u8,
    pub seq: u16,
    pub ty: u16,
    pub body: Vec<u8>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Frame {{ ty: {:#x}, seq: {}, flags: {}, body: {} }}",
            self.ty,
            self.seq,
            self.flags,
            hex(&self.body)
        )
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

/// Incremental frame decoder. Resynchronises on the preamble after garbage or bad frames.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    pub dropped: usize,
}

impl Decoder {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn next_frame(&mut self) -> Option<Frame> {
        loop {
            let start = find(&self.buf, &PREAMBLE);
            match start {
                None => {
                    // Keep a possible partial preamble at the tail.
                    let keep = self.buf.len().min(PREAMBLE.len() - 1);
                    let drop = self.buf.len() - keep;
                    self.dropped += drop;
                    self.buf.drain(..drop);
                    return None;
                }
                Some(s) if s > 0 => {
                    self.dropped += s;
                    self.buf.drain(..s);
                }
                _ => {}
            }
            let hdr_end = PREAMBLE.len() + HEADER_LEN;
            if self.buf.len() < hdr_end {
                return None;
            }
            let h = &self.buf[PREAMBLE.len()..hdr_end];
            let flags = h[0];
            let crc = h[1];
            let seq = u16::from_le_bytes([h[2], h[3]]);
            let ty = u16::from_le_bytes([h[4], h[5]]);
            let len = u16::from_le_bytes([h[6], h[7]]) as usize;
            if len > MAX_BODY {
                self.skip_preamble();
                continue;
            }
            let total = hdr_end + len + TRAILER.len();
            if self.buf.len() < total {
                return None;
            }
            let body = &self.buf[hdr_end..hdr_end + len];
            let trailer = &self.buf[hdr_end + len..total];
            // A CRC byte of 0 means "not checked", as in the stock firmware's receiver.
            // The vendor app sends its port-detection frames that way.
            if trailer != TRAILER || (crc != 0 && crc8(body) != crc) {
                log::warn!("bad frame (ty {ty:#x}, len {len}): trailer/crc mismatch");
                self.skip_preamble();
                continue;
            }
            let frame = Frame { flags, seq, ty, body: body.to_vec() };
            self.buf.drain(..total);
            return Some(frame);
        }
    }

    fn skip_preamble(&mut self) {
        self.dropped += 1;
        self.buf.drain(..1);
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Message bodies

pub fn connect_body(tx_frame_factor: u32) -> Vec<u8> {
    tx_frame_factor.to_le_bytes().to_vec()
}

pub fn query_body(what: u16) -> Vec<u8> {
    what.to_le_bytes().to_vec()
}

/// HID mouse button bits.
pub mod button {
    pub const LEFT: u8 = 0x01;
    pub const RIGHT: u8 = 0x02;
    pub const MIDDLE: u8 = 0x04;
    pub const BACK: u8 = 0x08;
    pub const FORWARD: u8 = 0x10;
}

pub fn mouse_rel_body(buttons: u8, dx: i8, dy: i8, wheel: i8, pan: i8) -> Vec<u8> {
    vec![buttons, dx as u8, dy as u8, wheel as u8, pan as u8]
}

/// `x`, `y` are normalised to 0..=32767 across the target screen.
pub fn mouse_abs_body(buttons: u8, x: u16, y: u16, wheel: i8, pan: i8) -> Vec<u8> {
    let mut b = Vec::with_capacity(7);
    b.push(buttons);
    b.extend_from_slice(&x.min(0x7FFF).to_le_bytes());
    b.extend_from_slice(&y.min(0x7FFF).to_le_bytes());
    b.push(wheel as u8);
    b.push(pan as u8);
    b
}

/// Side-port GPIO over opcode 0x227 (`CycKiwiSideGpio::SetupPin`).
///
/// Body is 8 bytes, `[index, mode, args…]`. The firmware handler (`fn_100078b0`) drives
/// the RP2040 SIO block directly:
///
/// | mode | name     | args                          | effect |
/// |------|----------|-------------------------------|--------|
/// | 0    | disabled | —                             | input with pull-down |
/// | 1    | input    | `pull_up:u8, pull_down:u8`    | input, pulls as given |
/// | 2    | output   | `level:u8`                    | drive the level |
/// | 3    | pulse    | `level:u8, width_ms:u32`      | drive `level` for `width_ms` (≤ 500), then the opposite |
///
/// The vendor app also knows modes 4 and 5. This firmware rejects them ("Unknown GPIO mode").
/// GPIO 0 and 1 double as the side UART's Tx and Rx. While the side UART is on, the firmware
/// ignores setup requests for them.
pub mod gpio {
    /// GPIO indexes the firmware accepts (0..=3).
    pub const COUNT: u8 = 4;
    /// The firmware clamps pulses to this.
    pub const MAX_PULSE_MS: u32 = 500;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Setup {
        Disabled,
        Input { pull_up: bool, pull_down: bool },
        Output { high: bool },
        Pulse { high: bool, ms: u32 },
    }

    impl Setup {
        pub fn body(self, index: u8) -> Vec<u8> {
            let mut b = vec![0u8; 8];
            b[0] = index;
            match self {
                Setup::Disabled => b[1] = 0,
                Setup::Input { pull_up, pull_down } => {
                    b[1] = 1;
                    b[2] = pull_up as u8;
                    b[3] = pull_down as u8;
                }
                Setup::Output { high } => {
                    b[1] = 2;
                    b[2] = high as u8;
                }
                Setup::Pulse { high, ms } => {
                    b[1] = 3;
                    b[2] = high as u8;
                    b[3..7].copy_from_slice(&ms.clamp(1, MAX_PULSE_MS).to_le_bytes());
                }
            }
            b
        }
    }

    /// Level of GPIO `index` from `KiwiState::io`: bit `2i+1` is set while the pin is an
    /// input, bit `2i` is its level. `None` when the pin isn't reading (not an input).
    pub fn level(io: u8, index: u8) -> Option<bool> {
        let bits = io >> (2 * index);
        (bits & 2 != 0).then_some(bits & 1 != 0)
    }
}

/// `KiwiConfig::vcomm` / `vterm` values (`CyConfig::VPortMode`).
pub mod vport {
    pub const DISABLED: u8 = 0;
    /// The target-facing USB CDC serial port ("Kiwi Serial Port" on the target).
    pub const TARGET_USB: u8 = 1;
    /// The side port's UART (GPIO 0 = Tx, GPIO 1 = Rx), 115200 8N1.
    pub const SIDE_UART: u8 = 2;
}

/// Body of opcode 0x78. The firmware copies all 20 bytes over its live config (it doesn't
/// persist them), then turns the side UART on or off to match. The vendor app sends it on
/// every connect and whenever the Serial settings change.
///
/// ```text
///  0  u16  log level: the Kiwi sends log lines at this level and above (firmware default 2)
///  2  [6]  unused (the firmware reads byte 7 only in a side-UART fallback the app never sets)
///  8  u8   vcomm: where the 0x1FE stream goes (`vport`)
///  9  u8   vterm: where the 0x1FF stream goes (`vport`)
/// 10  [4]  unused
/// 14  u32  state report interval, ms (firmware default 200)
/// 18  u8   unknown; firmware default 0x30, the vendor app sends 0
/// 19  u8   force legacy keyboard (only honoured when the frame's flags byte is > 1)
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KiwiConfig {
    pub log_level: u16,
    pub vcomm: u8,
    pub vterm: u8,
    pub state_interval_ms: u32,
    pub legacy_keyboard: bool,
}

impl Default for KiwiConfig {
    /// The firmware's power-on values.
    fn default() -> Self {
        KiwiConfig { log_level: 2, vcomm: vport::DISABLED, vterm: vport::DISABLED, state_interval_ms: 200, legacy_keyboard: false }
    }
}

impl KiwiConfig {
    pub const LEN: usize = 20;

    pub fn body(&self) -> Vec<u8> {
        let mut b = vec![0u8; Self::LEN];
        b[0..2].copy_from_slice(&self.log_level.to_le_bytes());
        b[8] = self.vcomm;
        b[9] = self.vterm;
        b[14..18].copy_from_slice(&self.state_interval_ms.to_le_bytes());
        b[18] = 0x30;
        b[19] = self.legacy_keyboard as u8;
        b
    }

    /// Whether the firmware turns the side UART on for this config.
    pub fn side_uart(&self) -> bool {
        self.vcomm == vport::SIDE_UART || self.vterm == vport::SIDE_UART
    }
}

/// EEPROM access over opcodes 0x11A8 / 0x11B2.
///
/// The device is a 256-byte I2C EEPROM at bus address 0x54. The verify opcode (0x11B2) never
/// returns data — only the CRC-32 of the requested window. But because the window is arbitrary
/// and CRC-32 of a single byte is a bijection over 0..=255, a one-byte window turns the verify
/// into a read oracle: `byte_from_single_crc(reply_crc)` recovers the byte at that address, so
/// 256 one-byte queries dump the whole device.
pub mod eeprom {
    use super::crc32;

    /// Device size (`addr + len` must be `<= SIZE`, enforced by the firmware).
    pub const SIZE: u16 = 256;

    /// Build a 0x11B2 verify/read request body `[addr:u16, len:u16, expected_crc:u32]` (8 bytes).
    /// `expected_crc` only feeds the device's log line; the reply carries the computed CRC either way.
    pub fn verify_body(addr: u16, len: u16, expected_crc: u32) -> Vec<u8> {
        let mut b = Vec::with_capacity(8);
        b.extend_from_slice(&addr.to_le_bytes());
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(&expected_crc.to_le_bytes());
        b
    }

    /// Request the one-byte window at `addr`, so the reply CRC identifies the byte there.
    pub fn read_byte_body(addr: u16) -> Vec<u8> {
        verify_body(addr, 1, 0)
    }

    /// Build a 0x11A8 write request body `[addr:u16, len:u16, crc32:u32, data]`.
    pub fn write_body(addr: u16, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(8 + data.len());
        b.extend_from_slice(&addr.to_le_bytes());
        b.extend_from_slice(&(data.len() as u16).to_le_bytes());
        b.extend_from_slice(&crc32(data).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    /// Parse the CRC-32 out of a 0x11B2 reply body (4 bytes, little-endian).
    pub fn reply_crc(body: &[u8]) -> Option<u32> {
        body.get(..4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }

    /// Reverse of `crc32(&[b])`: recover a single byte from the CRC-32 of a one-byte window.
    /// Returns `None` only if `crc` is not the CRC of any single byte (e.g. a corrupted reply,
    /// or a reply to a multi-byte window).
    pub fn byte_from_single_crc(crc: u32) -> Option<u8> {
        (0u16..256).find(|&b| crc32(&[b as u8]) == crc).map(|b| b as u8)
    }
}

/// Firmware OTA update (opcodes FW_* / `CyUf` image format).
///
/// Sequence: FW_BEGIN → FW_ERASE → FW_DATA×N → FW_VERIFY (compare CRC) → FW_FINISH.
/// A `.kfw` file is a 0x80-byte header (see `mkupdate.py`) followed by the payload
/// that maps to flash `0x4000`.
pub mod fwupdate {
    use super::crc32;

    pub const HEADER_LEN: usize = 0x80;
    pub const CHUNK: usize = 256;

    #[derive(Clone, Debug)]
    pub struct Image {
        pub header: Vec<u8>, // the 0x80 header
        pub payload: Vec<u8>,
        pub major: u16,
        pub minor: u16,
    }

    const MAGIC: [u8; 16] = [
        0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87, 0xef, 0xcd, 0xab, 0x90, 0x09, 0xba, 0xdc, 0xfe,
    ];

    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
    }

    impl Image {
        /// Parse and validate a `.kfw` file (checks magic, size, and the header CRC-32).
        pub fn parse(file: &[u8]) -> Result<Image, String> {
            if file.len() < HEADER_LEN {
                return Err("file smaller than header".into());
            }
            if file[0..16] != MAGIC {
                return Err("bad magic (not a Kiwi update image)".into());
            }
            let size = u32_at(file, 0x18) as usize;
            let crc = u32_at(file, 0x1C);
            if file.len() < HEADER_LEN + size {
                return Err(format!("truncated: need {} bytes", HEADER_LEN + size));
            }
            let payload = file[HEADER_LEN..HEADER_LEN + size].to_vec();
            if crc32(&payload) != crc {
                return Err(format!("payload CRC {:08X} != header {:08X}", crc32(&payload), crc));
            }
            Ok(Image {
                header: file[..HEADER_LEN].to_vec(),
                payload,
                major: u16_at(file, 0x14),
                minor: u16_at(file, 0x16),
            })
        }

        /// FW_BEGIN body: the header minus the 16-byte magic (CyFirmwareInfo).
        pub fn begin_body(&self) -> Vec<u8> {
            self.header[0x10..HEADER_LEN].to_vec()
        }

        /// Number of 4 KiB flash sectors the payload spans.
        pub fn sector_count(&self) -> u16 {
            self.payload.len().div_ceil(4096) as u16
        }

        /// FW_ERASE body: `u16` sector count.
        pub fn erase_body(&self) -> Vec<u8> {
            self.sector_count().to_le_bytes().to_vec()
        }

        /// FW_DATA chunks, each `[idx:u16, crc32(chunk):u32, chunk:256]`. The final chunk is
        /// padded with 0xFF (matching erased flash) so every chunk is 256 bytes.
        pub fn data_chunks(&self) -> Vec<Vec<u8>> {
            self.payload
                .chunks(CHUNK)
                .enumerate()
                .map(|(idx, part)| {
                    let mut chunk = [0xFFu8; CHUNK];
                    chunk[..part.len()].copy_from_slice(part);
                    let mut b = Vec::with_capacity(6 + CHUNK);
                    b.extend_from_slice(&(idx as u16).to_le_bytes());
                    b.extend_from_slice(&crc32(&chunk).to_le_bytes());
                    b.extend_from_slice(&chunk);
                    b
                })
                .collect()
        }

        /// FW_VERIFY body: `u32` length to CRC in staging (the exact payload length).
        pub fn verify_body(&self) -> Vec<u8> {
            (self.payload.len() as u32).to_le_bytes().to_vec()
        }

        /// Expected staging CRC-32 (over the exact payload) to compare against the FW_VERIFY reply.
        pub fn expected_crc(&self) -> u32 {
            crc32(&self.payload)
        }

        /// FW_FINISH body: the full 0x80 header.
        pub fn finish_body(&self) -> Vec<u8> {
            self.header.clone()
        }
    }

    /// Parse the CRC-32 out of a FW_VERIFY reply (4-byte LE body), like the EEPROM reply.
    pub fn verify_reply_crc(body: &[u8]) -> Option<u32> {
        body.get(..4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KiwiInfo {
    pub proto_major: u16,
    pub proto_minor: u16,
    pub uid: u64,
    pub board: String,
    pub firmware: (u16, u16),
    pub firmware_label: String,
    pub boot: (u16, u16),
    pub boot_label: String,
    /// Byte 44. Bit 5: target USB uses Cytrence's VID (clear = legacy VID).
    pub flags: u8,
    /// u32 at byte 127. Bit 0: target mass storage enabled, bit 1: target CDC serial enabled.
    pub usb_functions: u32,
    /// Byte 131: side-port pin count. 6 = the six-pin port (GPIO 0–3); 3 = the older
    /// three-pin port (Tx, GND, Rx: GPIO 0–1). The vendor app picks its ATX pin map from this.
    pub gpio_count: u8,
    /// Present when the device runs open-kiwi firmware (see [`OpenFirmware`]).
    pub open: Option<OpenFirmware>,
    pub raw: Vec<u8>,
}

/// open-kiwi's KiwiInfo extension, appended after the stock 132 bytes
/// (`firmware/src/link/info_ext.h`):
///
/// ```text
/// 132  char[4]  "OKIW"
/// 136  u16      extension format version
/// 138  u16      extension length in bytes, header included (12 for v1)
/// 140  u32      capabilities (`open_caps`)
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenFirmware {
    pub version: u16,
    pub capabilities: u32,
}

/// `OpenFirmware::capabilities` bits.
pub mod open_caps {
    /// 0x1A4: the firmware types text itself.
    pub const TEXT_BATCH: u32 = 1 << 0;
    /// 0x1FE/0x1FF routed to the side UART.
    pub const SIDE_UART: u32 = 1 << 1;
    /// 0x1FE/0x1FF routed to the target's USB serial port.
    pub const CDC_BRIDGE: u32 = 1 << 2;
    /// 0x96 kind 6 (VID switch).
    pub const VID_SWITCH: u32 = 1 << 3;
    /// Receives OTA updates (runs behind the stock bootloader).
    pub const OTA: u32 = 1 << 4;

    pub const NAMES: [(u32, &str); 5] =
        [(TEXT_BATCH, "text"), (SIDE_UART, "side-uart"), (CDC_BRIDGE, "cdc-bridge"), (VID_SWITCH, "vid-switch"), (OTA, "ota")];
}

impl OpenFirmware {
    pub const OFFSET: usize = 132;
    const MAGIC: &'static [u8; 4] = b"OKIW";
    const MIN_LEN: usize = 12;

    /// The extension in a KiwiInfo body, if there is one.
    pub fn parse(info: &[u8]) -> Option<Self> {
        let ext = info.get(Self::OFFSET..)?;
        if ext.len() < Self::MIN_LEN || &ext[..4] != Self::MAGIC {
            return None;
        }
        let len = u16_at(ext, 6) as usize;
        if len < Self::MIN_LEN || len > ext.len() {
            return None;
        }
        Some(OpenFirmware { version: u16_at(ext, 4), capabilities: u32::from_le_bytes(ext[8..12].try_into().ok()?) })
    }

    pub fn has(&self, cap: u32) -> bool {
        self.capabilities & cap != 0
    }

    /// Capability names, e.g. "text, side-uart, ota".
    pub fn capability_names(&self) -> String {
        open_caps::NAMES.iter().filter(|(bit, _)| self.has(*bit)).map(|(_, n)| *n).collect::<Vec<_>>().join(", ")
    }
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    b.get(off..off + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

impl KiwiInfo {
    /// Layout observed from firmware "Kiwi (v1.1/B)" speaking protocol 1.4:
    /// `0` ver major, `2` ver minor, `4` uid u64, `12` board[32],
    /// `76` boot major/minor + `80` label[21], `101` fw major/minor + `105` label[21]
    /// (the vendor app reads the firmware version at 101; stock reports 2.10 in both).
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 12 {
            return None;
        }
        let mut info = KiwiInfo {
            proto_major: u16_at(b, 0),
            proto_minor: u16_at(b, 2),
            uid: u64::from_le_bytes(b[4..12].try_into().ok()?),
            raw: b.to_vec(),
            ..Default::default()
        };
        if b.len() >= 44 {
            info.board = cstr(&b[12..44]);
        }
        if b.len() >= 45 {
            info.flags = b[44];
        }
        if b.len() >= 131 {
            info.usb_functions = u32::from_le_bytes(b[127..131].try_into().unwrap());
        }
        if b.len() >= 132 {
            info.gpio_count = b[131];
        }
        info.open = OpenFirmware::parse(b);
        if b.len() >= 101 {
            info.boot = (u16_at(b, 76), u16_at(b, 78));
            info.boot_label = cstr(&b[80..101]);
        }
        if b.len() >= 126 {
            info.firmware = (u16_at(b, 101), u16_at(b, 103));
            info.firmware_label = cstr(&b[105..126]);
        }
        Some(info)
    }

    pub fn cytrence_vid(&self) -> bool {
        self.flags & 0x20 != 0
    }
    pub fn target_msc(&self) -> bool {
        self.usb_functions & 1 != 0
    }
    pub fn target_cdc(&self) -> bool {
        self.usb_functions & 2 != 0
    }
    pub fn firmware_at_least(&self, major: u16, minor: u16) -> bool {
        self.firmware >= (major, minor)
    }
}

/// Periodic device status (12 bytes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KiwiState {
    /// Bit 1: target host is in USB boot mode. Bit 0: session active (observed).
    pub states: u64,
    /// Side-port GPIO levels.
    pub io: u8,
    pub temperature_c: u8,
    pub shared_usb: u8,
    pub netlink: u8,
    pub raw: Vec<u8>,
}

impl KiwiState {
    pub fn parse(b: &[u8]) -> Self {
        let at = |i: usize| b.get(i).copied().unwrap_or(0);
        KiwiState {
            states: b.get(0..8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap())),
            io: at(8),
            temperature_c: at(9),
            shared_usb: at(10),
            netlink: at(11),
            raw: b.to_vec(),
        }
    }

    pub fn boot_mode(&self) -> bool {
        self.states & 2 != 0
    }
}

/// `(level, text)` from an 0x12C log frame.
pub fn parse_log(b: &[u8]) -> (u16, String) {
    (u16_at(b, 0), cstr(b.get(2..).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn crc_matches_binary_constants() {
        // CRCs hard-coded in the vendor app for the two query bodies.
        assert_eq!(crc8(&[1, 0]), 0x15);
        assert_eq!(crc8(&[2, 0]), 0x2a);
    }

    #[test]
    fn encode_matches_wire() {
        assert_eq!(
            encode(op::QUERY, 2, &query_body(query::INFO)),
            unhex("3c5aa5c300150200c80002000100c96dd69c")
        );
        assert_eq!(
            encode(op::CONNECT, 1, &connect_body(500)),
            unhex("3c5aa5c30030010032000400f4010000c96dd69c")
        );
    }

    // Bytes captured from a real Kiwi in response to CONNECT + QUERY(INFO).
    const CAPTURE: &str = "3c5aa5c3002b02002c01210002004d73673a20546f20726573706f6e642077697468206b69776920696e666f00c96dd69c3c5aa5c302c402006400840001000400e4656014c33f18324b697769202876312e312f422900000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000002000a00323032362e30372e31375431373a35363a32305a0002000a00323032362e30372e31375431373a35363a32305a00020000000006c96dd69c";

    #[test]
    fn decode_capture() {
        let bytes = unhex(CAPTURE);
        let mut d = Decoder::default();
        // Feed in awkward chunks to exercise reassembly.
        for chunk in bytes.chunks(7) {
            d.push(chunk);
        }
        let log = d.next_frame().unwrap();
        assert_eq!(log.ty, op::LOG);
        assert_eq!(parse_log(&log.body), (2, "Msg: To respond with kiwi info".into()));

        let info = d.next_frame().unwrap();
        assert_eq!(info.ty, op::KIWI_INFO);
        assert_eq!(info.flags, 2);
        let info = KiwiInfo::parse(&info.body).unwrap();
        assert_eq!((info.proto_major, info.proto_minor), (1, 4));
        assert_eq!(info.uid, 0x32183fc3146065e4);
        assert_eq!(info.board, "Kiwi (v1.1/B)");
        assert_eq!(info.firmware, (2, 10));
        assert_eq!(info.firmware_label, "2026.07.17T17:56:20Z");
        assert_eq!(info.boot, (2, 10));
        assert!(info.cytrence_vid());
        assert!(!info.target_cdc() && !info.target_msc());
        assert_eq!(info.open, None, "stock firmware has no open-kiwi extension");
        assert!(d.next_frame().is_none());
        assert_eq!(d.dropped, 0);
    }

    #[test]
    fn decoder_resyncs_after_garbage() {
        let mut bytes = vec![0xde, 0xad, 0x3c, 0x5a];
        bytes.extend(encode(op::KIWI_STATE, 3, &[0; 12]));
        let mut d = Decoder::default();
        d.push(&bytes);
        let f = d.next_frame().unwrap();
        assert_eq!(f.ty, op::KIWI_STATE);
        assert_eq!(d.dropped, 4);
    }

    #[test]
    fn state_fields() {
        let s = KiwiState::parse(&unhex("010000000000000000250000"));
        assert_eq!(s.states, 1);
        assert_eq!(s.temperature_c, 37);
        assert!(!s.boot_mode());
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(&[0x00]), 0xD202_EF8D);
        assert_eq!(crc32(&[0xA5]), 0x74BE_B8EA);
    }

    #[test]
    fn eeprom_single_byte_crc_is_invertible() {
        // Every byte value must round-trip through the read oracle.
        for b in 0u16..256 {
            let crc = crc32(&[b as u8]);
            assert_eq!(eeprom::byte_from_single_crc(crc), Some(b as u8));
        }
        // A multi-byte window's CRC is (almost always) not any single byte's CRC.
        assert_eq!(eeprom::byte_from_single_crc(crc32(&[1, 2, 3])), None);
    }

    #[test]
    fn fwupdate_image() {
        // Build a minimal valid .kfw: 0x80 header + payload, header CRC at 0x1C.
        let payload: Vec<u8> = (0..600u32).map(|i| i as u8).collect(); // 600 bytes -> 3 chunks, 2 sectors
        let mut file = vec![0u8; 0x80];
        file[0..16].copy_from_slice(&[
            0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87, 0xef, 0xcd, 0xab, 0x90, 0x09, 0xba, 0xdc, 0xfe,
        ]);
        file[0x14..0x16].copy_from_slice(&2u16.to_le_bytes()); // major
        file[0x16..0x18].copy_from_slice(&11u16.to_le_bytes()); // minor
        file[0x18..0x1C].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        file[0x1C..0x20].copy_from_slice(&crc32(&payload).to_le_bytes());
        file.extend_from_slice(&payload);

        let img = fwupdate::Image::parse(&file).unwrap();
        assert_eq!((img.major, img.minor), (2, 11));
        assert_eq!(img.begin_body(), file[0x10..0x80]);
        assert_eq!(img.sector_count(), 1); // ceil(600/4096)
        assert_eq!(img.verify_body(), 600u32.to_le_bytes());
        assert_eq!(img.expected_crc(), crc32(&payload));

        let chunks = img.data_chunks();
        assert_eq!(chunks.len(), 3); // ceil(600/256)
        // chunk layout: idx(2) + crc(4) + 256 data
        assert_eq!(chunks[0].len(), 262);
        assert_eq!(&chunks[0][0..2], &0u16.to_le_bytes());
        assert_eq!(&chunks[2][0..2], &2u16.to_le_bytes());
        // last chunk padded with 0xFF beyond the 88 real bytes (600 - 512)
        assert_eq!(chunks[2][6 + 88], 0xFF);
        // its embedded CRC matches a CRC over the padded 256 bytes
        let mut last = [0xFFu8; 256];
        last[..88].copy_from_slice(&payload[512..]);
        assert_eq!(&chunks[2][2..6], &crc32(&last).to_le_bytes());

        // a corrupt CRC is rejected
        let mut bad = file.clone();
        bad[0x1C] ^= 1;
        assert!(fwupdate::Image::parse(&bad).is_err());
    }

    #[test]
    fn eeprom_bodies() {
        assert_eq!(eeprom::read_byte_body(0x12), vec![0x12, 0, 1, 0, 0, 0, 0, 0]);
        // write: addr 0, data [0xAA,0xBB] -> len 2, crc32 prefixed LE
        let w = eeprom::write_body(0, &[0xAA, 0xBB]);
        assert_eq!(&w[..4], &[0, 0, 2, 0]);
        assert_eq!(&w[4..8], &crc32(&[0xAA, 0xBB]).to_le_bytes());
        assert_eq!(&w[8..], &[0xAA, 0xBB]);
        assert_eq!(eeprom::reply_crc(&0x74BE_B8EAu32.to_le_bytes()), Some(0x74BE_B8EA));
    }

    #[test]
    fn gpio_bodies() {
        use gpio::Setup;
        assert_eq!(Setup::Disabled.body(1), vec![1, 0, 0, 0, 0, 0, 0, 0]);
        // Vendor ATX setup: power LED pin as input with pull-up, switch pin as output low.
        assert_eq!(Setup::Input { pull_up: true, pull_down: false }.body(3), vec![3, 1, 1, 0, 0, 0, 0, 0]);
        assert_eq!(Setup::Output { high: false }.body(2), vec![2, 2, 0, 0, 0, 0, 0, 0]);
        // Vendor PushPowerButton: [pin, 3, 1, width:u32].
        assert_eq!(Setup::Pulse { high: true, ms: 200 }.body(2), vec![2, 3, 1, 200, 0, 0, 0, 0]);
        assert_eq!(Setup::Pulse { high: false, ms: 9999 }.body(0), vec![0, 3, 0, 0xf4, 0x01, 0, 0, 0]);
    }

    #[test]
    fn gpio_levels() {
        // Pin 3 input high, pin 2 input low, pin 1 not an input, pin 0 input high.
        let io = 0b11_10_00_11;
        assert_eq!(gpio::level(io, 0), Some(true));
        assert_eq!(gpio::level(io, 1), None);
        assert_eq!(gpio::level(io, 2), Some(false));
        assert_eq!(gpio::level(io, 3), Some(true));
    }

    #[test]
    fn kiwi_config_body() {
        let d = KiwiConfig::default();
        // Matches the firmware's power-on config (fn_10005ab0).
        assert_eq!(d.body(), unhex("0200000000000000000000000000c80000003000"));
        assert!(!d.side_uart());
        let term = KiwiConfig { vterm: vport::SIDE_UART, ..d };
        assert_eq!(term.body()[9], 2);
        assert!(term.side_uart());
    }

    #[test]
    fn info_gpio_count() {
        let bytes = unhex(CAPTURE);
        let mut d = Decoder::default();
        d.push(&bytes);
        d.next_frame();
        let info = KiwiInfo::parse(&d.next_frame().unwrap().body).unwrap();
        assert_eq!(info.gpio_count, 6);
        assert!(info.firmware_at_least(1, 16));
    }

    #[test]
    fn mouse_bodies() {
        assert_eq!(mouse_rel_body(1, -1, 2, 0, 0), vec![1, 0xff, 2, 0, 0]);
        assert_eq!(mouse_abs_body(0, 0x7fff, 0x4000, -1, 0), vec![0, 0xff, 0x7f, 0x00, 0x40, 0xff, 0]);
    }

    /// The wire-format vectors shared with the open firmware's C tests
    /// (`firmware/test/link_test.c`), so both ends agree byte for byte.
    #[test]
    fn shared_vectors() {
        const VECTORS: &str = include_str!("../../firmware/test/vectors/frames.txt");
        let field = |s: &str| if s == "-" { Vec::new() } else { unhex(s) };
        let mut checked = 0;
        for (n, line) in VECTORS.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let at = format!("frames.txt line {}", n + 1);
            match f[0] {
                "crc8" => assert_eq!(crc8(&field(f[1])), u8::from_str_radix(f[2], 16).unwrap(), "{at}"),
                "frame" => {
                    let ty = u16::from_str_radix(f[1], 16).unwrap();
                    let seq: u16 = f[2].parse().unwrap();
                    let body = field(f[3]);
                    let wire = unhex(f[4]);
                    assert_eq!(encode(ty, seq, &body), wire, "{at}: encode");
                    let mut d = Decoder::default();
                    d.push(&wire);
                    assert_eq!(d.next_frame(), Some(Frame { flags: flags_for(ty), seq, ty, body }), "{at}: decode");
                    assert!(d.next_frame().is_none(), "{at}: extra frame");
                }
                "stream" => {
                    let want: Vec<u16> = f[2].split(',').map(|t| u16::from_str_radix(t, 16).unwrap()).collect();
                    let mut d = Decoder::default();
                    let mut got = Vec::new();
                    // Byte at a time, like the firmware's UART.
                    for b in unhex(f[1]) {
                        d.push(&[b]);
                        while let Some(fr) = d.next_frame() {
                            got.push(fr.ty);
                        }
                    }
                    assert_eq!(got, want, "{at}: stream");
                }
                "infoext" => {
                    let caps = u32::from_str_radix(f[1], 16).unwrap();
                    let mut body = vec![0u8; OpenFirmware::OFFSET];
                    body.extend(unhex(f[2]));
                    let info = KiwiInfo::parse(&body).unwrap();
                    assert_eq!(info.open, Some(OpenFirmware { version: 1, capabilities: caps }), "{at}: infoext");
                    // Truncated or wrong-magic extensions are ignored.
                    assert_eq!(OpenFirmware::parse(&body[..body.len() - 1]), None, "{at}: truncated");
                    body[OpenFirmware::OFFSET] = b'X';
                    assert_eq!(OpenFirmware::parse(&body), None, "{at}: bad magic");
                }
                other => panic!("{at}: unknown vector kind {other}"),
            }
            checked += 1;
        }
        assert!(checked > 20, "only {checked} vectors found");
    }
}
