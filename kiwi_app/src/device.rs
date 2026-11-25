//! Serial link to the Kiwi: discovery, handshake, keepalive, reconnect.

use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};

use crate::protocol::{self, Decoder, Frame, KiwiInfo, KiwiState, op, query};

/// WCH CH343-based "Kiwi Serial" bridge.
pub const SERIAL_VID: u16 = 0x1A86;
pub const SERIAL_PID: u16 = 0x55D3;
/// Vendor app default; it also allows 921600 and 1500000.
pub const BAUD: u32 = 2_000_000;
pub const TX_FRAME_FACTOR: u32 = 500;

const INFO_RETRY: Duration = Duration::from_secs(2);
const KEEPALIVE: Duration = Duration::from_secs(1);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Searching,
    Connecting(String),
    Connected(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum DeviceEvent {
    Status(LinkStatus),
    Info(KiwiInfo),
    State(KiwiState),
    Log(String),
    Terminal(Vec<u8>),
    /// CRC-32 from an EEPROM verify/read reply (opcode 0x11B2).
    EepromCrc(u32),
    /// CRC-32 of the firmware staging area, from a FW_VERIFY reply (opcode 0x1022).
    FwVerifyCrc(u32),
    Unknown(Frame),
}

enum Msg {
    Send(u16, Vec<u8>),
    Rx(Frame),
    RxClosed(String),
    Shutdown,
}

/// Cheap, cloneable handle used by the UI and input code to talk to the device.
#[derive(Clone)]
pub struct DeviceHandle {
    tx: mpsc::Sender<Msg>,
}

impl DeviceHandle {
    pub fn send(&self, ty: u16, body: Vec<u8>) {
        let _ = self.tx.send(Msg::Send(ty, body));
    }

    pub fn keyboard(&self, report: [u8; 8]) {
        self.send(op::KEYBOARD, report.to_vec());
    }

    pub fn mouse_abs(&self, buttons: u8, x: u16, y: u16, wheel: i8, pan: i8) {
        self.send(op::MOUSE_ABS, protocol::mouse_abs_body(buttons, x, y, wheel, pan));
    }

    pub fn mouse_rel(&self, buttons: u8, dx: i8, dy: i8, wheel: i8, pan: i8) {
        self.send(op::MOUSE_REL, protocol::mouse_rel_body(buttons, dx, dy, wheel, pan));
    }

    pub fn gpio(&self, index: u8, setup: protocol::gpio::Setup) {
        self.send(op::SIDE_GPIO, setup.body(index));
    }

    pub fn config(&self, cfg: &protocol::KiwiConfig) {
        self.send(op::KIWI_CONFIG, cfg.body());
    }

    /// Bytes for the terminal stream (0x1FF). Split to stay under the frame size limit.
    pub fn terminal(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(256) {
            self.send(op::TERMINAL, chunk.to_vec());
        }
    }

    /// Text for the Kiwi to type itself (0x1A4), on a US layout. Split to stay under the
    /// frame size limit.
    pub fn text(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(256) {
            self.send(op::TEXT, chunk.to_vec());
        }
    }

    /// Request the one-byte EEPROM window at `addr`; the reply arrives as `DeviceEvent::EepromCrc`.
    pub fn eeprom_read_byte(&self, addr: u16) {
        self.send(op::EEPROM_VERIFY, protocol::eeprom::read_byte_body(addr));
    }

    /// Reboot the Kiwi into the RP2040 USB bootloader (BOOTSEL). The session ends after this.
    pub fn reboot_bootsel(&self) {
        self.send(op::REBOOT_BOOTSEL, Vec::new());
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

pub fn find_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    let mut candidates: Vec<String> = ports
        .into_iter()
        .filter(|p| match &p.port_type {
            serialport::SerialPortType::UsbPort(u) => {
                (u.vid == SERIAL_VID && u.pid == SERIAL_PID)
                    || u.product.as_deref().is_some_and(|s| s.contains("Kiwi"))
            }
            _ => false,
        })
        .map(|p| p.port_name)
        .collect();
    // On macOS prefer the callout device.
    candidates.sort_by_key(|n| !n.starts_with("/dev/cu."));
    candidates.into_iter().next()
}

/// Start the device manager thread. `on_event` is called from background threads.
pub fn spawn(
    port_override: Option<String>,
    on_event: impl Fn(DeviceEvent) + Send + Sync + 'static,
) -> DeviceHandle {
    let (tx, rx) = mpsc::channel();
    let handle = DeviceHandle { tx: tx.clone() };
    thread::Builder::new()
        .name("kiwi-device".into())
        .spawn(move || manager(port_override, tx, rx, on_event))
        .expect("spawn device thread");
    handle
}

fn manager(
    port_override: Option<String>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    on_event: impl Fn(DeviceEvent) + Send + Sync + 'static,
) {
    let on_event = std::sync::Arc::new(on_event);
    let mut last_status = None;
    let mut status = |s: LinkStatus, on_event: &dyn Fn(DeviceEvent)| {
        if last_status.as_ref() != Some(&s) {
            last_status = Some(s.clone());
            on_event(DeviceEvent::Status(s));
        }
    };
    loop {
        let path = port_override.clone().or_else(find_port);
        let Some(path) = path else {
            status(LinkStatus::Searching, &*on_event);
            if wait_or_shutdown(&rx, RECONNECT_DELAY) {
                return;
            }
            continue;
        };
        status(LinkStatus::Connecting(path.clone()), &*on_event);
        match session(&path, &tx, &rx, &*on_event, &mut |s| status(s, &*on_event)) {
            Ok(()) => return, // shutdown requested
            Err(e) => {
                log::warn!("kiwi session ended: {e:#}");
                status(LinkStatus::Error(format!("{e:#}")), &*on_event);
                if wait_or_shutdown(&rx, RECONNECT_DELAY) {
                    return;
                }
            }
        }
    }
}

/// Drain (and drop) messages for `d`. Returns true if a shutdown was requested.
fn wait_or_shutdown(rx: &mpsc::Receiver<Msg>, d: Duration) -> bool {
    let deadline = Instant::now() + d;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return true,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
}

fn session(
    path: &str,
    tx: &mpsc::Sender<Msg>,
    rx: &mpsc::Receiver<Msg>,
    on_event: &dyn Fn(DeviceEvent),
    status: &mut dyn FnMut(LinkStatus),
) -> Result<()> {
    let mut port = serialport::new(path, BAUD)
        .timeout(Duration::from_millis(50))
        .open()
        .with_context(|| format!("open {path}"))?;
    let _ = port.write_data_terminal_ready(true);
    let _ = port.write_request_to_send(true);
    let _ = port.clear(serialport::ClearBuffer::All);

    let mut reader = port.try_clone().context("clone port")?;
    let rx_tx = tx.clone();
    let reader_thread = thread::Builder::new().name("kiwi-rx".into()).spawn(move || {
        let mut dec = Decoder::default();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    dec.push(&buf[..n]);
                    while let Some(f) = dec.next_frame() {
                        if rx_tx.send(Msg::Rx(f)).is_err() {
                            return;
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    let _ = rx_tx.send(Msg::RxClosed(e.to_string()));
                    return;
                }
            }
        }
    })?;

    let mut seq: u16 = 0;
    let mut write = |port: &mut Box<dyn serialport::SerialPort>, ty: u16, body: &[u8]| -> Result<()> {
        seq = seq.wrapping_add(1).max(1);
        let frame = protocol::encode(ty, seq, body);
        log::trace!("tx {ty:#x} {}", protocol::hex(body));
        port.write_all(&frame)?;
        Ok(())
    };

    let hello = |port: &mut Box<dyn serialport::SerialPort>, write: &mut dyn FnMut(&mut Box<dyn serialport::SerialPort>, u16, &[u8]) -> Result<()>| -> Result<()> {
        write(port, op::CONNECT, &protocol::connect_body(TX_FRAME_FACTOR))?;
        write(port, op::QUERY, &protocol::query_body(query::INFO))
    };

    hello(&mut port, &mut write)?;
    let mut have_info = false;
    let mut last_hello = Instant::now();
    let mut last_tx = Instant::now();

    let result = loop {
        let msg = rx.recv_timeout(Duration::from_millis(100));
        match msg {
            Ok(Msg::Send(ty, body)) => {
                if have_info {
                    if let Err(e) = write(&mut port, ty, &body) {
                        break Err(e);
                    }
                    last_tx = Instant::now();
                }
            }
            Ok(Msg::Rx(f)) => handle_frame(f, on_event, &mut have_info, path, status),
            Ok(Msg::RxClosed(e)) => break Err(anyhow!("read: {e}")),
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                let _ = write(&mut port, op::DISCONNECT, &[]);
                let _ = port.flush();
                break Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if !have_info && now.duration_since(last_hello) >= INFO_RETRY {
            log::debug!("no KiwiInfo yet, re-sending hello");
            if let Err(e) = hello(&mut port, &mut write) {
                break Err(e);
            }
            last_hello = now;
        }
        if have_info && now.duration_since(last_tx) >= KEEPALIVE {
            if let Err(e) = write(&mut port, op::QUERY, &protocol::query_body(query::STATE)) {
                break Err(e);
            }
            last_tx = now;
        }
    };
    drop(port);
    // The reader exits once its port handle errors or the channel closes; don't block on it.
    drop(reader_thread);
    result
}

fn handle_frame(
    f: Frame,
    on_event: &dyn Fn(DeviceEvent),
    have_info: &mut bool,
    path: &str,
    status: &mut dyn FnMut(LinkStatus),
) {
    match f.ty {
        op::KIWI_INFO => {
            if let Some(info) = KiwiInfo::parse(&f.body) {
                log::info!(
                    "Kiwi: {} uid {:016x} protocol {}.{} fw {}.{} ({})",
                    info.board,
                    info.uid,
                    info.proto_major,
                    info.proto_minor,
                    info.firmware.0,
                    info.firmware.1,
                    info.firmware_label
                );
                if info.proto_major != 1 {
                    log::warn!("unexpected protocol major version {}", info.proto_major);
                }
                *have_info = true;
                status(LinkStatus::Connected(path.to_string()));
                on_event(DeviceEvent::Info(info));
            }
        }
        op::KIWI_STATE => on_event(DeviceEvent::State(KiwiState::parse(&f.body))),
        op::LOG => {
            let (_level, text) = protocol::parse_log(&f.body);
            log::debug!("kiwi log: {text}");
            on_event(DeviceEvent::Log(text));
        }
        op::TERMINAL => on_event(DeviceEvent::Terminal(f.body)),
        op::EEPROM_VERIFY => {
            if let Some(crc) = protocol::eeprom::reply_crc(&f.body) {
                on_event(DeviceEvent::EepromCrc(crc));
            } else {
                log::warn!("short EEPROM reply: {}", protocol::hex(&f.body));
            }
        }
        op::FW_VERIFY => {
            if let Some(crc) = protocol::fwupdate::verify_reply_crc(&f.body) {
                on_event(DeviceEvent::FwVerifyCrc(crc));
            }
        }
        _ => {
            log::debug!("unhandled {f:?}");
            on_event(DeviceEvent::Unknown(f));
        }
    }
}
