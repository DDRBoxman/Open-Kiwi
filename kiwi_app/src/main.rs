mod app;
mod avhid;
#[allow(dead_code)]
mod device;
mod input;
#[allow(dead_code)]
mod keyboard;
#[allow(dead_code)]
mod protocol;
mod video;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use device::DeviceEvent;

struct Args {
    port: Option<String>,
    probe: bool,
    type_text: Option<String>,
    target_config: Option<(u8, u8)>,
    mouse_test: bool,
    eeprom_dump: bool,
    eeprom_out: Option<String>,
    reboot_bootsel: bool,
    update: Option<String>,
    av: Option<AvAction>,
    /// Commands to send once connected (`--gpio`, `--side-uart`, `--text`).
    io: Vec<IoAction>,
}

enum IoAction {
    Gpio(u8, protocol::gpio::Setup),
    SideUart(bool),
    /// Text for the Kiwi to type itself (0x1A4).
    Text(String),
}

fn parse_gpio_mode(s: &str) -> Option<protocol::gpio::Setup> {
    use protocol::gpio::Setup;
    let (name, arg) = s.split_once(':').unwrap_or((s, ""));
    Some(match name {
        "off" => Setup::Disabled,
        "in" => Setup::Input { pull_up: false, pull_down: false },
        "in-up" => Setup::Input { pull_up: true, pull_down: false },
        "in-down" => Setup::Input { pull_up: false, pull_down: true },
        "low" => Setup::Output { high: false },
        "high" => Setup::Output { high: true },
        "pulse-high" | "pulse-low" => Setup::Pulse { high: name == "pulse-high", ms: arg.parse().ok()? },
        _ => return None,
    })
}

/// MS2130 video-chip commands (`--av*`).
enum AvAction {
    Show,
    Edid(u8),
    Sharpness(bool),
    Picture(avhid::Picture),
}

fn parse_args() -> Args {
    let mut args = Args {
        port: None,
        probe: false,
        type_text: None,
        target_config: None,
        mouse_test: false,
        eeprom_dump: false,
        eeprom_out: None,
        reboot_bootsel: false,
        update: None,
        av: None,
        io: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--probe" => args.probe = true,
            "--formats" => {
                let modes = video::list_modes();
                if modes.is_empty() {
                    println!("Kiwi capture device not found");
                }
                for m in modes {
                    println!("{}  [{}]", m.label(), m.fourcc());
                }
                std::process::exit(0);
            }
            "--port" => args.port = it.next(),
            "--target-config" => {
                args.probe = true;
                let mut num = || it.next().and_then(|v| v.parse::<u8>().ok());
                match (num(), num()) {
                    (Some(k), Some(v)) => args.target_config = Some((k, v)),
                    _ => {
                        eprintln!("--target-config needs two numbers: KIND VALUE");
                        std::process::exit(2);
                    }
                }
            }
            "--mouse-test" => {
                args.probe = true;
                args.mouse_test = true;
            }
            "--gpio" => {
                args.probe = true;
                let index = it.next().and_then(|v| v.parse::<u8>().ok()).filter(|i| *i < protocol::gpio::COUNT);
                match (index, it.next().as_deref().and_then(parse_gpio_mode)) {
                    (Some(i), Some(m)) => args.io.push(IoAction::Gpio(i, m)),
                    _ => {
                        eprintln!("--gpio needs INDEX (0-3) and MODE: off, in, in-up, in-down, low, high, pulse-high:MS, pulse-low:MS");
                        std::process::exit(2);
                    }
                }
            }
            "--side-uart" => {
                args.probe = true;
                match it.next().as_deref() {
                    Some("on" | "1") => args.io.push(IoAction::SideUart(true)),
                    Some("off" | "0") => args.io.push(IoAction::SideUart(false)),
                    _ => {
                        eprintln!("--side-uart needs on or off");
                        std::process::exit(2);
                    }
                }
            }
            "--text" => {
                args.probe = true;
                match it.next() {
                    Some(t) => args.io.push(IoAction::Text(t)),
                    None => {
                        eprintln!("--text needs the text to type");
                        std::process::exit(2);
                    }
                }
            }
            "--eeprom-dump" => args.eeprom_dump = true,
            "--eeprom-out" => args.eeprom_out = it.next(),
            "--reboot-bootsel" => args.reboot_bootsel = true,
            "--update" => args.update = it.next(),
            "--av" => args.av = Some(AvAction::Show),
            "--av-edid" => match it.next().as_deref().and_then(avhid::parse_edid) {
                Some(i) => args.av = Some(AvAction::Edid(i)),
                None => {
                    let names: Vec<&str> = avhid::EDID_PRESETS.iter().map(|(_, l)| *l).collect();
                    eprintln!("--av-edid needs a preset: {} (or its index)", names.join(", "));
                    std::process::exit(2);
                }
            },
            "--av-sharpness" => match it.next().as_deref() {
                Some("on" | "1") => args.av = Some(AvAction::Sharpness(true)),
                Some("off" | "0") => args.av = Some(AvAction::Sharpness(false)),
                _ => {
                    eprintln!("--av-sharpness needs on or off");
                    std::process::exit(2);
                }
            },
            "--av-picture" => {
                let mut num = || it.next().and_then(|v| v.parse::<u8>().ok()).filter(|v| *v <= avhid::PICTURE_MAX);
                match (num(), num(), num(), num()) {
                    (Some(brightness), Some(contrast), Some(saturation), Some(hue)) => {
                        args.av = Some(AvAction::Picture(avhid::Picture { brightness, contrast, saturation, hue }))
                    }
                    _ => {
                        eprintln!("--av-picture needs four values 0-100: BRIGHTNESS CONTRAST SATURATION HUE (neutral 50)");
                        std::process::exit(2);
                    }
                }
            }
            "--type" => {
                args.probe = true;
                args.type_text = it.next();
            }
            "-h" | "--help" => {
                println!(
                    "kiwi — Cytrence Kiwi KVM client\n\n\
                     USAGE: kiwi [--port /dev/cu.usbmodemXXXX] [--probe]\n\n  \
                     --port   serial port to use (default: auto-detect by USB id)\n  \
                     --probe  connect, print device info/state for a few seconds, exit\n  \
                     --type TEXT  probe, then type TEXT on the target\n  \
                     --text TEXT  probe, then have the Kiwi type TEXT itself (0x1A4, US layout)\n  \
                     --mouse-test  probe, then move the target mouse (relative square, then absolute corners)\n  \
                     --target-config KIND VALUE  probe, then change a target-USB setting\n           \
                     (0 usb, 1 msc, 2 cdc, 3 vga/usb 2-in-1, 4 ps2, 5 boot mouse, 6 vid: 0 cytrence / 1 legacy)\n  \
                     --eeprom-dump  read the 256-byte I2C EEPROM via the 0x11B2 CRC oracle, print a hex dump\n  \
                     --eeprom-out FILE  with --eeprom-dump, also write the raw 256 bytes to FILE\n  \
                     --reboot-bootsel  reboot the Kiwi into the RP2040 USB bootloader (BOOTSEL); ends the session\n  \
                     --gpio INDEX MODE  probe, then set up side-port GPIO 0-3 (Pin-1/3/4/6); MODE is off, in,\n           \
                     in-up, in-down, low, high, pulse-high:MS or pulse-low:MS (MS up to 500). Repeatable.\n  \
                     --side-uart on|off  probe, then route the terminal stream (0x1FF) to the side UART\n\n\
                     Video chip (MS2130, over HID; no serial session needed):\n  \
                     --av  print HDMI input status, EDID, sharpness and picture settings\n  \
                     --av-edid PRESET  set the EDID the target sees: default, 4k, 1920x1080, 1280x1024,\n           \
                     1280x960, 1280x720, 1024x768, 800x600, 640x480\n  \
                     --av-sharpness on|off  sharpness filter\n  \
                     --av-picture B C S H  brightness, contrast, saturation, hue (0-100, neutral 50)"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

/// Headless check of the serial link.
fn probe(
    port: Option<String>,
    type_text: Option<String>,
    target_config: Option<(u8, u8)>,
    mouse_test: bool,
    io: Vec<IoAction>,
) {
    let mut io = Some(io);
    let (tx, rx) = mpsc::channel();
    let handle = device::spawn(port, move |ev| {
        let _ = tx.send(ev);
    });
    let secs = if mouse_test { 25 } else if target_config.is_some() { 14 } else { 4 };
    let mut mouse_test = mouse_test;
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut pending_config = target_config;
    let mut type_text = type_text;
    let mut states = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(ev) = rx.recv_timeout(left) else { break };
        match ev {
            DeviceEvent::Status(s) => println!("status: {s:?}"),
            DeviceEvent::Info(i) => {
                println!("board:     {}", i.board);
                println!("uid:       {:016x}", i.uid);
                println!("protocol:  {}.{}", i.proto_major, i.proto_minor);
                println!("firmware:  {}.{} ({})", i.firmware.0, i.firmware.1, i.firmware_label);
                match i.open {
                    Some(o) => println!("open-kiwi: yes (extension v{}; {})", o.version, o.capability_names()),
                    None => println!("open-kiwi: no (stock firmware)"),
                }
                println!("boot:      {}.{} ({})", i.boot.0, i.boot.1, i.boot_label);
                println!(
                    "target:    vid {}, mass storage {}, serial {}",
                    if i.cytrence_vid() { "cytrence" } else { "legacy" },
                    if i.target_msc() { "on" } else { "off" },
                    if i.target_cdc() { "on" } else { "off" }
                );
                println!("side port: {} pins", i.gpio_count);
                for action in io.take().unwrap_or_default() {
                    match action {
                        IoAction::Gpio(index, setup) => {
                            println!("gpio:      {index} -> {setup:?}");
                            handle.gpio(index, setup);
                        }
                        IoAction::Text(t) => {
                            println!("text:      Kiwi types {t:?}");
                            handle.text(t.as_bytes());
                        }
                        IoAction::SideUart(on) => {
                            let vterm = if on { protocol::vport::SIDE_UART } else { protocol::vport::DISABLED };
                            println!("config:    side UART terminal {}", if on { "on" } else { "off" });
                            handle.config(&protocol::KiwiConfig { vterm, ..Default::default() });
                        }
                    }
                }
                if let Some((kind, value)) = pending_config.take() {
                    let h = handle.clone();
                    let text = type_text.take();
                    std::thread::spawn(move || {
                        println!("config:    detaching target USB, then setting [{kind}, {value}]");
                        h.send(protocol::op::TARGET_CONFIG, vec![protocol::target::USB, 0]);
                        std::thread::sleep(Duration::from_secs(1));
                        h.send(protocol::op::TARGET_CONFIG, vec![kind, value]);
                        std::thread::sleep(Duration::from_secs(4));
                        h.send(protocol::op::QUERY, protocol::query_body(protocol::query::INFO));
                        if let Some(t) = text {
                            std::thread::sleep(Duration::from_secs(2));
                            println!("typing:    {t:?}");
                            keyboard::type_text(h, t);
                        }
                    });
                } else if std::mem::take(&mut mouse_test) {
                    let h = handle.clone();
                    std::thread::spawn(move || run_mouse_test(h));
                } else if let Some(t) = type_text.take() {
                    println!("typing:    {t:?}");
                    keyboard::type_text(handle.clone(), t);
                }
            }
            DeviceEvent::State(s) => {
                states += 1;
                let levels: Vec<&str> = (0..protocol::gpio::COUNT)
                    .map(|g| match protocol::gpio::level(s.io, g) {
                        Some(true) => "H",
                        Some(false) => "L",
                        None => "-",
                    })
                    .collect();
                println!("state:     {}  (gpio 0-3: {})", protocol::hex(&s.raw), levels.join(" "));
            }
            DeviceEvent::Log(l) => println!("log:       {l}"),
            DeviceEvent::Terminal(t) => println!("terminal:  {}", String::from_utf8_lossy(&t)),
            DeviceEvent::EepromCrc(crc) => println!("eeprom:    crc {crc:#010x}"),
            DeviceEvent::FwVerifyCrc(crc) => println!("fw-verify: crc {crc:#010x}"),
            DeviceEvent::Unknown(f) => println!("frame:     {f:?}"),
        }
    }
    handle.shutdown();
    std::thread::sleep(Duration::from_millis(100));
    println!("video:     {}", if video::kiwi_video_present() { "capture device present" } else { "capture device NOT found" });
    if states == 0 {
        std::process::exit(1);
    }
}

/// Reboot the Kiwi into the RP2040 USB bootloader. Waits for the handshake so we know the
/// device is listening, sends 0xBC2, and exits. After this the Kiwi enumerates as the
/// "RPI-RP2" drive; `picotool save -a FILE` (or dragging) reads the live flash.
/// Collect device log frames for `d`, printing them; returns the log texts.
fn fw_pump(rx: &mpsc::Receiver<DeviceEvent>, d: Duration) -> Vec<String> {
    let end = Instant::now() + d;
    let mut logs = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left.min(Duration::from_millis(100))) {
            Ok(DeviceEvent::Log(l)) => {
                println!("  kiwi: {l}");
                logs.push(l);
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    logs
}

fn fw_fail(handle: &device::DeviceHandle, msg: String) -> ! {
    eprintln!("update: FAILED — {msg}");
    eprintln!("update: the device was NOT committed; it keeps its current firmware.");
    eprintln!("update: (if it misbehaves, recover with `kiwi --reboot-bootsel` + picotool).");
    handle.shutdown();
    std::thread::sleep(Duration::from_millis(120));
    std::process::exit(1);
}

/// Install a firmware image (`.kfw`) over the control channel (OTA). Safe by construction:
/// it aborts before the destructive erase if BEGIN isn't acknowledged, and aborts before
/// COMMIT if the staging CRC doesn't match — so a bad transfer leaves the device on its
/// current firmware (and BOOTSEL is always a fallback).
fn fw_update(port: Option<String>, path: String) {
    use device::LinkStatus;
    use std::io::Write;

    let file = match std::fs::read(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("update: read {path}: {e}");
            std::process::exit(1);
        }
    };
    let img = match protocol::fwupdate::Image::parse(&file) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("update: invalid image: {e}");
            std::process::exit(1);
        }
    };
    let chunks = img.data_chunks();
    println!(
        "update: {path}\n        v{}.{}, {} bytes, {} chunks, {} sectors, payload crc {:08X}",
        img.major,
        img.minor,
        img.payload.len(),
        chunks.len(),
        img.sector_count(),
        img.expected_crc()
    );

    let (tx, rx) = mpsc::channel();
    let handle = device::spawn(port, move |ev| {
        let _ = tx.send(ev);
    });

    // Handshake.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut connected = false;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left.min(Duration::from_millis(200))) {
            Ok(DeviceEvent::Info(i)) => {
                println!(
                    "update: connected to {} {} {}.{}",
                    i.board,
                    if i.open.is_some() { "open-kiwi" } else { "fw" },
                    i.firmware.0,
                    i.firmware.1
                );
                connected = true;
                break;
            }
            Ok(DeviceEvent::Status(LinkStatus::Error(e))) => eprintln!("update: {e}"),
            _ => {}
        }
    }
    if !connected {
        fw_fail(&handle, "no device / handshake timed out".into());
    }

    // [1/5] BEGIN — non-destructive; gate on the device's ack log.
    println!("update: [1/5] begin");
    handle.send(protocol::op::FW_BEGIN, img.begin_body());
    let logs = fw_pump(&rx, Duration::from_secs(2));
    let ok = logs.iter().any(|l| l.contains("to update to"));
    let bad = logs.iter().any(|l| {
        let l = l.to_lowercase();
        l.contains("downgrade") || l.contains("not valid") || l.contains("no enough") || l.contains("incompatible")
    });
    if bad || !ok {
        fw_fail(&handle, format!("BEGIN not acknowledged (logs: {logs:?})"));
    }

    // [2/5] ERASE staging (does not touch the running app).
    println!("update: [2/5] erase {} sectors", img.sector_count());
    handle.send(protocol::op::FW_ERASE, img.erase_body());
    fw_pump(&rx, Duration::from_secs(4));

    // [3/5] DATA chunks (paced; watch for CRC errors).
    print!("update: [3/5] writing {} chunks ", chunks.len());
    let _ = std::io::stdout().flush();
    for (i, c) in chunks.iter().enumerate() {
        handle.send(protocol::op::FW_DATA, c.clone());
        std::thread::sleep(Duration::from_millis(4));
        while let Ok(ev) = rx.try_recv() {
            if let DeviceEvent::Log(l) = ev {
                if l.contains("Wrong CRC") {
                    println!();
                    fw_fail(&handle, format!("chunk {i}: {l}"));
                }
                println!("\n  kiwi (chunk {i}): {l}");
            }
        }
        if i % 32 == 0 {
            print!(".");
            let _ = std::io::stdout().flush();
        }
    }
    println!(" done");

    // [4/5] VERIFY — the commit gate.
    println!("update: [4/5] verify");
    handle.send(protocol::op::FW_VERIFY, img.verify_body());
    let vend = Instant::now() + Duration::from_secs(6);
    let mut got = None;
    while let Some(left) = vend.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left.min(Duration::from_millis(100))) {
            Ok(DeviceEvent::FwVerifyCrc(c)) => {
                got = Some(c);
                break;
            }
            Ok(DeviceEvent::Log(l)) => println!("  kiwi: {l}"),
            _ => {}
        }
    }
    match got {
        Some(c) if c == img.expected_crc() => println!("update: staging CRC {c:#010x} matches"),
        Some(c) => fw_fail(
            &handle,
            format!("staging CRC {c:#010x} != expected {:#010x}", img.expected_crc()),
        ),
        None => fw_fail(&handle, "no verify reply from device".into()),
    }

    // [5/5] COMMIT, then a normal reboot so the bootloader applies the staged image.
    println!("update: [5/5] commit + reboot");
    handle.send(protocol::op::FW_FINISH, img.finish_body());
    fw_pump(&rx, Duration::from_secs(2));
    handle.send(protocol::op::FW_REBOOT, Vec::new());
    fw_pump(&rx, Duration::from_secs(1));
    handle.shutdown();
    std::thread::sleep(Duration::from_millis(200));
    println!("update: committed + rebooting. The bootloader applies v{}.{}.", img.major, img.minor);
    println!("update: run `kiwi --probe` in a few seconds to confirm.");
}

fn reboot_bootsel(port: Option<String>) {
    let (tx, rx) = mpsc::channel();
    let handle = device::spawn(port, move |ev| {
        let _ = tx.send(ev);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            eprintln!("reboot-bootsel: no device / handshake timed out");
            handle.shutdown();
            std::thread::sleep(Duration::from_millis(80));
            std::process::exit(1);
        }
        match rx.recv_timeout(left) {
            Ok(DeviceEvent::Info(_)) => {
                println!("reboot-bootsel: sending 0xBC2; the Kiwi will reboot into BOOTSEL (RPI-RP2)");
                handle.reboot_bootsel();
                std::thread::sleep(Duration::from_millis(200));
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    handle.shutdown();
    std::thread::sleep(Duration::from_millis(120));
    println!("reboot-bootsel: done. Read the flash with:  picotool save -a kiwi_live.bin");
}

/// Dump the 256-byte I2C EEPROM by abusing the 0x11B2 verify opcode as a read oracle:
/// request a one-byte window at each address and map the returned CRC-32 back to the byte.
/// Requests are strictly serialised (one in flight) since the reply carries no address or seq
/// we can match on.
fn eeprom_dump(port: Option<String>, out: Option<String>) {
    use device::LinkStatus;
    const SIZE: usize = protocol::eeprom::SIZE as usize;
    const STEP_TIMEOUT: Duration = Duration::from_secs(3);
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

    let (tx, rx) = mpsc::channel();
    let handle = device::spawn(port, move |ev| {
        let _ = tx.send(ev);
    });

    let mut data = vec![0u8; SIZE];
    let mut next: usize = 0; // address awaiting a reply
    let mut received = 0usize;
    let mut started = false;
    let mut deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    let fail = |handle: &device::DeviceHandle, msg: String| -> ! {
        eprintln!("eeprom: {msg}");
        handle.shutdown();
        std::thread::sleep(Duration::from_millis(80));
        std::process::exit(1);
    };

    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            if !started {
                fail(&handle, "no device / handshake timed out".into());
            }
            fail(&handle, format!("timed out awaiting reply for address {next:#04x}"));
        }
        let Ok(ev) = rx.recv_timeout(left) else { continue };
        match ev {
            DeviceEvent::Info(_) if !started => {
                started = true;
                eprintln!("eeprom: reading {SIZE} bytes via 0x11B2 one-byte CRC oracle…");
                handle.eeprom_read_byte(next as u16);
                deadline = Instant::now() + STEP_TIMEOUT;
            }
            DeviceEvent::EepromCrc(crc) if started => {
                match protocol::eeprom::byte_from_single_crc(crc) {
                    Some(b) => {
                        data[next] = b;
                        received += 1;
                    }
                    None => fail(
                        &handle,
                        format!("address {next:#04x}: reply CRC {crc:#010x} is not any single byte's CRC"),
                    ),
                }
                if received >= SIZE {
                    break;
                }
                next += 1;
                handle.eeprom_read_byte(next as u16);
                deadline = Instant::now() + STEP_TIMEOUT;
            }
            DeviceEvent::Status(LinkStatus::Error(e)) => fail(&handle, e),
            DeviceEvent::Log(l) => log::debug!("kiwi log: {l}"),
            _ => {}
        }
    }

    handle.shutdown();
    std::thread::sleep(Duration::from_millis(80));
    print_hex_dump(&data);
    if let Some(path) = out {
        match std::fs::write(&path, &data) {
            Ok(()) => eprintln!("eeprom: wrote {SIZE} bytes to {path}"),
            Err(e) => fail(&handle, format!("writing {path}: {e}")),
        }
    }
}

/// `xxd`-style 16-byte rows with an ASCII gutter.
fn print_hex_dump(data: &[u8]) {
    for (i, row) in data.chunks(16).enumerate() {
        let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
        let (left, right) = hex.split_at(row.len().min(8));
        let ascii: String =
            row.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        println!("{:04x}  {:<23}  {:<23}  |{}|", i * 16, left.join(" "), right.join(" "), ascii);
    }
}

fn av_command(action: AvAction) {
    let fail = |e: anyhow::Error| -> ! {
        eprintln!("av: {e:#}");
        std::process::exit(1);
    };
    let result = match action {
        AvAction::Show => Ok(()),
        AvAction::Edid(i) => avhid::set_edid(i).map(|()| println!("av: EDID set to {}", avhid::edid_label(i))),
        AvAction::Sharpness(on) => {
            avhid::set_sharpness(on).map(|()| println!("av: sharpness {}", if on { "on" } else { "off" }))
        }
        AvAction::Picture(p) => avhid::set_picture(p).map(|()| println!("av: picture set to {p:?}")),
    };
    if let Err(e) = result {
        fail(e);
    }
    // The chip applies settings from its main loop, so give it a moment before reading back.
    std::thread::sleep(Duration::from_millis(150));
    let snap = avhid::snapshot().unwrap_or_else(|e| fail(e));
    let s = snap.status;
    println!("firmware:  {}.{} (serial {:08X})", snap.info.version.0, snap.info.version.1, snap.info.serial);
    println!(
        "input:     {} (signal {}, +5V {}, HPD {})",
        s.summary(),
        if s.input { "locked" } else { "none" },
        if s.power_5v { "yes" } else { "no" },
        if s.hpd { "on" } else { "off" }
    );
    println!("edid:      {}", avhid::edid_label(s.edid));
    println!("sharpness: {}", if snap.sharpness { "on" } else { "off" });
    let p = snap.picture;
    println!(
        "picture:   brightness {}, contrast {}, saturation {}, hue {}",
        p.brightness, p.contrast, p.saturation, p.hue
    );
}

fn run_mouse_test(h: device::DeviceHandle) {
    let shift = 1u8 << (keyboard::usage::L_SHIFT - 0xE0);
    println!("mouse:     tapping Shift to wake the target screen");
    h.keyboard([shift, 0, 0, 0, 0, 0, 0, 0]);
    std::thread::sleep(Duration::from_millis(60));
    h.keyboard([0; 8]);
    std::thread::sleep(Duration::from_secs(2));

    println!("mouse:     relative (0x19A): three large squares");
    for _ in 0..3 {
        for (dx, dy) in [(30i8, 0i8), (0, 30), (-30, 0), (0, -30)] {
            for _ in 0..15 {
                h.mouse_rel(0, dx, dy, 0, 0);
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    std::thread::sleep(Duration::from_secs(1));
    println!("mouse:     absolute (0x19B): centre, then the four corners, 1.5 s each");
    for (x, y) in [(16384u16, 16384u16), (2000, 2000), (30767, 2000), (30767, 30767), (2000, 30767), (16384, 16384)] {
        h.mouse_abs(0, x, y, 0, 0);
        std::thread::sleep(Duration::from_millis(1500));
    }
    println!("mouse:     done");
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("kiwi=info")).init();
    let args = parse_args();
    if let Some(action) = args.av {
        av_command(action);
        return;
    }
    if args.reboot_bootsel {
        reboot_bootsel(args.port);
        return;
    }
    if let Some(file) = args.update {
        fw_update(args.port, file);
        return;
    }
    if args.eeprom_dump {
        eeprom_dump(args.port, args.eeprom_out);
        return;
    }
    if args.probe {
        probe(args.port, args.type_text, args.target_config, args.mouse_test, args.io);
        return;
    }

    Application::new().run(move |cx| {
        app::quit_on_close(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let port = args.port.clone();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions { title: Some("Kiwi KVM".into()), ..Default::default() }),
                    window_min_size: Some(size(px(640.), px(420.))),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| app::KiwiApp::new(port, window, cx)),
            )
            .expect("open window");
        cx.on_app_quit(move |cx| {
            let _ = window.update(cx, |app, _, _| app.shutdown());
            async {
                // Let the device thread flush its DISCONNECT frame.
                std::thread::sleep(Duration::from_millis(60));
            }
        })
        .detach();
        cx.activate(true);
    });
}
