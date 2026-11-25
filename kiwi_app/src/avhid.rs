//! Control of the Kiwi's MS2130 video chip over its vendor HID interface
//! (`345f:2130`, usage page `0xFF00`).
//!
//! Every exchange is one 8-byte feature report out (`send_feature_report`, report ID 0) and
//! one back (`get_feature_report`). Cytrence's firmware answers `[cmd, status, data × 6]`.
//! Like the vendor app, we open the device for each request and close it afterwards. That
//! way we never hold it while the Kiwi is unplugged. Requests are serialised by a global
//! lock because a send/get pair must not interleave with another one.
//!
//! Setters only queue the change. The chip's main loop applies it a moment later, so a
//! read straight after a write can still return the old value.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use hidapi::HidApi;
use parking_lot::Mutex;

pub const VID: u16 = 0x345f;
pub const PID: u16 = 0x2130;
const USAGE_PAGE: u16 = 0xFF00;

/// Feature reports fail now and then, especially while other traffic shares the Kiwi's
/// USB link, so each exchange is retried a few times. All requests here are idempotent.
const ATTEMPTS: usize = 4;

mod cmd {
    pub const SET: u8 = 0x0A;
    pub const GET: u8 = 0x0B;
    /// MacroSilicon's stock XDATA read: `B5 addrH addrL` → `B5 addrH addrL d0 d1 d2 d3`.
    pub const XDATA_READ: u8 = 0xB5;

    pub mod set {
        pub const EDID: u8 = 0x04;
        pub const SHARPNESS: u8 = 0x07;
        pub const PICTURE: u8 = 0x08;
    }
    pub mod get {
        pub const STATUS: u8 = 0x01;
        pub const INFO: u8 = 0x02;
        pub const PICTURE: u8 = 0x06;
    }
}

/// XDATA register whose bit 0 is the sharpness filter. `0A 07` sets and clears it, and it
/// is the bit ms2130_patcher clears.
const REG_SHARPNESS: u16 = 0xF6B0;

static API: Mutex<Option<HidApi>> = Mutex::new(None);

/// Firmware status codes (`rsp[1]`).
fn status_text(code: u8) -> &'static str {
    match code {
        0 => "ok",
        1 => "rejected (bad length or buffer full)",
        2 => "checksum mismatch",
        4 => "unsupported command",
        _ => "unknown error",
    }
}

fn init(slot: &mut Option<HidApi>) -> Result<&mut HidApi> {
    if slot.is_none() {
        *slot = Some(HidApi::new().context("hidapi init")?);
    }
    Ok(slot.as_mut().unwrap())
}

fn open(api: &mut HidApi) -> Result<hidapi::HidDevice> {
    api.reset_devices()?;
    api.add_devices(VID, PID)?;
    let info = api
        .device_list()
        .find(|d| d.usage_page() == USAGE_PAGE)
        .ok_or_else(|| anyhow!("Kiwi video HID interface ({VID:04x}:{PID:04x}) not found"))?;
    Ok(info.open_device(api)?)
}

/// One raw exchange: 8 bytes out, 8 bytes back.
fn exchange(req: [u8; 8]) -> Result<[u8; 8]> {
    let mut guard = API.lock();
    let dev = open(init(&mut guard)?)?;

    let mut out = [0u8; 9];
    out[1..].copy_from_slice(&req);
    let mut last_err = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(30 * attempt as u64));
        }
        if let Err(e) = dev.send_feature_report(&out) {
            last_err = Some(anyhow!("send feature report: {e}"));
            continue;
        }
        let mut buf = [0u8; 9];
        match dev.get_feature_report(&mut buf) {
            Ok(n) if n >= 9 => {
                let mut rsp = [0u8; 8];
                rsp.copy_from_slice(&buf[1..9]);
                log::trace!("avhid {:02x?} -> {:02x?}", req, rsp);
                return Ok(rsp);
            }
            Ok(n) => last_err = Some(anyhow!("short feature report ({n} bytes)")),
            Err(e) => last_err = Some(anyhow!("get feature report: {e}")),
        }
    }
    Err(last_err.unwrap())
}

/// A Cytrence command. Returns the six data bytes and checks the echo and status.
fn command(req: [u8; 8]) -> Result<[u8; 6]> {
    let rsp = exchange(req)?;
    if rsp[0] != req[0] {
        bail!("command {:02x} {:02x}: reply is for command {:02x}", req[0], req[1], rsp[0]);
    }
    if rsp[1] != 0 {
        bail!("command {:02x} {:02x}: {} (status {})", req[0], req[1], status_text(rsp[1]), rsp[1]);
    }
    let mut data = [0u8; 6];
    data.copy_from_slice(&rsp[2..]);
    Ok(data)
}

fn xdata_read(addr: u16) -> Result<u8> {
    let [hi, lo] = addr.to_be_bytes();
    let rsp = exchange([cmd::XDATA_READ, hi, lo, 0, 0, 0, 0, 0])?;
    if rsp[..3] != [cmd::XDATA_READ, hi, lo] {
        bail!("XDATA read {addr:#06x}: unexpected reply {rsp:02x?}");
    }
    Ok(rsp[3])
}

// ---- reads ---------------------------------------------------------------

/// `0B 02`: serial number and A/V firmware version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvInfo {
    pub serial: u32,
    pub version: (u8, u8),
}

impl AvInfo {
    /// The vendor app sends EDID commands only to firmware ≥ 1.7.
    pub fn supports_edid(&self) -> bool {
        self.version >= (1, 7)
    }
    /// The vendor app sends sharpness and picture commands only to firmware ≥ 1.9.
    pub fn supports_picture(&self) -> bool {
        self.version >= (1, 9)
    }
}

pub fn info() -> Result<AvInfo> {
    let d = command([cmd::GET, cmd::get::INFO, 0, 0, 0, 0, 0, 0])?;
    Ok(AvInfo { serial: u32::from_le_bytes([d[0], d[1], d[2], d[3]]), version: (d[5], d[4]) })
}

/// `0B 01`: HDMI input state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoStatus {
    /// The receiver has locked onto a signal.
    pub input: bool,
    /// The source drives +5 V on the HDMI cable, so something is plugged in and powered.
    pub power_5v: bool,
    /// Hot-plug detect is asserted towards the source.
    pub hpd: bool,
    /// Active EDID preset index (see [`EDID_PRESETS`]).
    pub edid: u8,
    /// Input resolution. Stale unless `input` is set.
    pub width: u16,
    pub height: u16,
}

impl VideoStatus {
    pub fn summary(&self) -> String {
        if self.input {
            format!("HDMI {}×{}", self.width, self.height)
        } else if self.power_5v {
            "HDMI source connected, no signal".into()
        } else {
            "No HDMI source".into()
        }
    }
}

pub fn status() -> Result<VideoStatus> {
    let d = command([cmd::GET, cmd::get::STATUS, 0, 0, 0, 0, 0, 0])?;
    Ok(VideoStatus {
        input: d[0] & 1 != 0,
        power_5v: d[0] & 2 != 0,
        hpd: d[0] & 4 != 0,
        edid: d[1],
        width: u16::from_be_bytes([d[2], d[3]]),
        height: u16::from_be_bytes([d[4], d[5]]),
    })
}

/// Picture adjustments, each 0–100. The neutral value is 50.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    pub brightness: u8,
    pub contrast: u8,
    pub saturation: u8,
    pub hue: u8,
}

impl Default for Picture {
    fn default() -> Self {
        Self { brightness: 50, contrast: 50, saturation: 50, hue: 50 }
    }
}

pub const PICTURE_MAX: u8 = 100;

/// `0B 06`: current picture adjustments.
pub fn picture() -> Result<Picture> {
    let d = command([cmd::GET, cmd::get::PICTURE, 0, 0, 0, 0, 0, 0])?;
    Ok(Picture { brightness: d[0], contrast: d[1], saturation: d[2], hue: d[3] })
}

/// Sharpness filter state, read back from its register. There's no `0B` query for it.
pub fn sharpness() -> Result<bool> {
    Ok(xdata_read(REG_SHARPNESS)? & 1 != 0)
}

/// Everything the UI shows, read in one go.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub info: AvInfo,
    pub status: VideoStatus,
    pub picture: Picture,
    pub sharpness: bool,
}

pub fn snapshot() -> Result<Snapshot> {
    Ok(Snapshot { info: info()?, status: status()?, picture: picture()?, sharpness: sharpness()? })
}

// ---- writes --------------------------------------------------------------

/// EDID presets in the order the vendor app offers them. The index is what `0A 04` takes.
/// `0x62` (a custom uploaded EDID) also exists but isn't offered here.
pub const EDID_PRESETS: &[(u8, &str)] = &[
    (0x00, "Default"),
    (0x01, "4096×2160"),
    (0x0A, "1920×1080"),
    (0x10, "1280×1024"),
    (0x0C, "1280×960"),
    (0x0B, "1280×720"),
    (0x0D, "1024×768"),
    (0x0E, "800×600"),
    (0x0F, "640×480"),
];

pub fn edid_label(idx: u8) -> String {
    match EDID_PRESETS.iter().find(|(i, _)| *i == idx) {
        Some((_, l)) => (*l).to_string(),
        None if idx == 0x62 => "Custom".into(),
        None => format!("{idx:#04x}"),
    }
}

/// Parse a CLI EDID argument: a preset label (`1920x1080`, `default`, `4k`) or an index.
pub fn parse_edid(s: &str) -> Option<u8> {
    let norm = s.trim().to_ascii_lowercase().replace('×', "x");
    if norm == "4k" {
        return Some(0x01);
    }
    if let Some((i, _)) =
        EDID_PRESETS.iter().find(|(_, l)| l.to_ascii_lowercase().replace('×', "x") == norm)
    {
        return Some(*i);
    }
    let idx = match norm.strip_prefix("0x") {
        Some(h) => u8::from_str_radix(h, 16).ok()?,
        None => norm.parse().ok()?,
    };
    EDID_PRESETS.iter().any(|(i, _)| *i == idx).then_some(idx)
}

/// `0A 04 idx`: switch the EDID the target sees.
pub fn set_edid(idx: u8) -> Result<()> {
    command([cmd::SET, cmd::set::EDID, idx, 0, 0, 0, 0, 0]).map(drop)
}

/// `0A 07 on`: sharpness filter.
pub fn set_sharpness(on: bool) -> Result<()> {
    command([cmd::SET, cmd::set::SHARPNESS, on as u8, 0, 0, 0, 0, 0]).map(drop)
}

/// `0A 08 b c s h`. The firmware ignores values above 100, so they're clamped here.
pub fn set_picture(p: Picture) -> Result<()> {
    let c = |v: u8| v.min(PICTURE_MAX);
    command([cmd::SET, cmd::set::PICTURE, c(p.brightness), c(p.contrast), c(p.saturation), c(p.hue), 0, 0])
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edid_parsing() {
        assert_eq!(parse_edid("1920x1080"), Some(0x0A));
        assert_eq!(parse_edid("1920×1080"), Some(0x0A));
        assert_eq!(parse_edid("Default"), Some(0x00));
        assert_eq!(parse_edid("4k"), Some(0x01));
        assert_eq!(parse_edid("0x0f"), Some(0x0F));
        assert_eq!(parse_edid("16"), Some(0x10));
        assert_eq!(parse_edid("0x62"), None);
        assert_eq!(parse_edid("1920x1200"), None);
    }

    #[test]
    fn edid_labels() {
        assert_eq!(edid_label(0x0B), "1280×720");
        assert_eq!(edid_label(0x62), "Custom");
    }
}

// ---- worker --------------------------------------------------------------

/// A request for the background worker.
#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Read the input status only (one exchange). Cheap enough to poll.
    PollStatus,
    /// Read everything.
    Refresh,
    SetEdid(u8),
    SetSharpness(bool),
    SetPicture(Picture),
}

/// What the worker reports back.
#[derive(Clone, Debug)]
pub enum Update {
    Status(VideoStatus),
    Snapshot(Snapshot),
    /// A write finished. Sent once per write op, including picture writes that a newer
    /// one superseded. `Err` holds the message.
    Written(Result<String, String>),
    /// A read failed (device missing or not answering).
    Unavailable(String),
}

/// Runs HID requests on one thread, in order. Each write is followed by a full read so the
/// UI shows what the chip actually holds.
pub struct Worker {
    tx: std::sync::mpsc::Sender<Op>,
}

impl Worker {
    pub fn spawn(mut on_update: impl FnMut(Update) + Send + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Op>();
        std::thread::Builder::new()
            .name("kiwi-avhid".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    // Fold the queue: keep every write except picture writes, where only the
                    // newest matters, and drop polls that a refresh already covers.
                    let mut ops = vec![first];
                    ops.extend(rx.try_iter());
                    let latest_picture = ops.iter().rposition(|o| matches!(o, Op::SetPicture(_)));
                    let wants_refresh = ops.iter().any(|o| !matches!(o, Op::PollStatus));
                    let mut wrote = false;
                    for (i, op) in ops.iter().enumerate() {
                        let result = match *op {
                            Op::PollStatus | Op::Refresh => continue,
                            // Superseded by a newer picture write: report it as done so
                            // the caller's count of pending writes stays exact.
                            Op::SetPicture(_) if Some(i) != latest_picture => {
                                on_update(Update::Written(Ok(String::new())));
                                continue;
                            }
                            Op::SetEdid(idx) => set_edid(idx).map(|()| format!("EDID: {}", edid_label(idx))),
                            Op::SetSharpness(on) => set_sharpness(on)
                                .map(|()| format!("Sharpness filter {}", if on { "on" } else { "off" })),
                            Op::SetPicture(p) => set_picture(p).map(|()| "Picture updated".to_string()),
                        };
                        wrote = true;
                        on_update(Update::Written(result.map_err(|e| format!("{e:#}"))));
                    }
                    if wrote {
                        // Let the chip's main loop apply the change before reading back.
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    let update = if wants_refresh {
                        snapshot().map(Update::Snapshot)
                    } else {
                        status().map(Update::Status)
                    };
                    on_update(update.unwrap_or_else(|e| Update::Unavailable(format!("{e:#}"))));
                }
            })
            .expect("spawn avhid worker");
        Self { tx }
    }

    pub fn send(&self, op: Op) {
        let _ = self.tx.send(op);
    }
}
