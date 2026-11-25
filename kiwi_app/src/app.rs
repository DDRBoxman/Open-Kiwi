//! The gpui KVM window.

mod io;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{
    App, Bounds, Context, Div, ElementId, FocusHandle, Hsla, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, Stateful, Subscription, Window, canvas, div, prelude::*, px,
    rgb, surface,
};

use crate::avhid::{self, Picture};
use crate::device::{self, DeviceEvent, DeviceHandle, LinkStatus};
use crate::input::{self, InputBridge, KeyMonitor, MouseMode};
use crate::keyboard::{self, usage};
use crate::protocol::{KiwiInfo, KiwiState, button};
use crate::video::{self, Access, Capture, Frame, FrameSink, VideoMode};

enum UiEvent {
    Device(DeviceEvent),
    Frame,
    CaptureReleased,
    AccessDecided,
    Av(avhid::Update),
}

pub struct KiwiApp {
    device: DeviceHandle,
    bridge: Arc<InputBridge>,
    _monitor: Option<KeyMonitor>,
    events: UnboundedSender<UiEvent>,

    sink: Arc<FrameSink>,
    capture: Option<Capture>,
    capture_note: Option<String>,
    frame: Option<Frame>,
    last_frame_at: Option<Instant>,
    audio_enabled: bool,
    wanted_mode: Option<VideoMode>,
    modes: Vec<VideoMode>,
    show_modes: bool,
    muted: bool,

    /// MS2130 video chip, over HID.
    av: avhid::Worker,
    av_status: Option<avhid::VideoStatus>,
    av_snapshot: Option<avhid::Snapshot>,
    av_error: Option<String>,
    /// Picture values shown in the panel. They change on click and go to the chip through
    /// the worker.
    picture: Picture,
    /// Writes sent but not yet confirmed. While any are in flight, reads don't overwrite
    /// the values the user just picked.
    av_pending: u32,
    show_picture: bool,

    /// Side port: terminal, ATX and GPIO (the I/O panel).
    io: io::IoState,

    link: LinkStatus,
    info: Option<KiwiInfo>,
    state: Option<KiwiState>,
    last_log: Option<String>,
    toast: Option<(String, Instant)>,

    focus: FocusHandle,
    video_bounds: Rc<Cell<Bounds<Pixels>>>,
    buttons: u8,
    last_pos: (u16, u16),
    scroll: (f32, f32),
    _subscriptions: Vec<Subscription>,
}

impl KiwiApp {
    pub fn new(port: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<UiEvent>();

        let dev_tx = tx.clone();
        let device = device::spawn(port, move |ev| {
            let _ = dev_tx.unbounded_send(UiEvent::Device(ev));
        });

        let rel_tx = tx.clone();
        let bridge = InputBridge::new(device.clone(), move || {
            let _ = rel_tx.unbounded_send(UiEvent::CaptureReleased);
        });
        let monitor = input::install(bridge.clone());
        if monitor.is_none() {
            log::error!("failed to install key monitor; keyboard capture unavailable");
        }

        let av_tx = tx.clone();
        let av = avhid::Worker::spawn(move |u| {
            let _ = av_tx.unbounded_send(UiEvent::Av(u));
        });
        av.send(avhid::Op::Refresh);

        let frame_tx = tx.clone();
        let sink = FrameSink::new(move || {
            let _ = frame_tx.unbounded_send(UiEvent::Frame);
        });

        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                if this.update(cx, |app, cx| app.handle(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Retry video until the capture device shows up (hotplug) and refresh the "no signal" state.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update(cx, |app, cx| {
                        if app.capture.is_none() {
                            app.start_capture(cx);
                        }
                        app.av.send(if app.show_picture { avhid::Op::Refresh } else { avhid::Op::PollStatus });
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut subs = Vec::new();
        subs.push(cx.observe_window_activation(window, |app, window, cx| {
            if !window.is_window_active() && app.bridge.is_captured() {
                app.set_captured(false, cx);
            }
        }));

        let mut app = Self {
            device,
            bridge,
            _monitor: monitor,
            events: tx,
            sink,
            capture: None,
            capture_note: None,
            frame: None,
            last_frame_at: None,
            audio_enabled: true,
            wanted_mode: None,
            modes: Vec::new(),
            show_modes: false,
            muted: false,
            av,
            av_status: None,
            av_snapshot: None,
            av_error: None,
            picture: Picture::default(),
            av_pending: 0,
            show_picture: false,
            io: io::IoState::new(cx),
            link: LinkStatus::Searching,
            info: None,
            state: None,
            last_log: None,
            toast: None,
            focus: cx.focus_handle(),
            video_bounds: Rc::new(Cell::new(Bounds::default())),
            buttons: 0,
            last_pos: (0x4000, 0x4000),
            scroll: (0.0, 0.0),
            _subscriptions: subs,
        };
        app.start_capture(cx);
        app
    }

    pub fn shutdown(&mut self) {
        self.bridge.set_captured(false);
        self.capture = None;
        self.release_power_hold();
        self.device.shutdown();
    }

    fn handle(&mut self, ev: UiEvent, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Frame => {
                if let Some(f) = self.sink.take() {
                    self.sink.record_display(&f);
                    self.frame = Some(f);
                    self.last_frame_at = Some(Instant::now());
                }
            }
            UiEvent::Device(DeviceEvent::Status(s)) => {
                if !matches!(s, LinkStatus::Connected(_)) {
                    self.info = None;
                    self.state = None;
                }
                self.link = s;
            }
            UiEvent::Device(DeviceEvent::Info(info)) => {
                self.info = Some(info);
                self.io_on_connect();
            }
            UiEvent::Device(DeviceEvent::State(s)) => self.state = Some(s),
            UiEvent::Device(DeviceEvent::Log(l)) => self.last_log = Some(l),
            UiEvent::Device(DeviceEvent::Terminal(bytes)) => self.on_terminal_bytes(&bytes),
            UiEvent::Device(
                DeviceEvent::Unknown(_) | DeviceEvent::EepromCrc(_) | DeviceEvent::FwVerifyCrc(_),
            ) => {}
            UiEvent::CaptureReleased => self.buttons = 0,
            UiEvent::AccessDecided => {
                self.capture = None;
                self.start_capture(cx);
            }
            UiEvent::Av(u) => self.handle_av(u),
        }
        cx.notify();
    }

    fn handle_av(&mut self, u: avhid::Update) {
        match u {
            avhid::Update::Status(s) => {
                self.av_status = Some(s);
                self.av_error = None;
            }
            avhid::Update::Snapshot(snap) => {
                self.av_status = Some(snap.status);
                self.av_error = None;
                if self.av_pending == 0 {
                    self.picture = snap.picture;
                    self.av_snapshot = Some(snap);
                } else if let Some(cur) = &mut self.av_snapshot {
                    // Keep the user's pending choices; take the fresh input status.
                    cur.status = snap.status;
                    cur.info = snap.info;
                } else {
                    self.av_snapshot = Some(snap);
                }
            }
            avhid::Update::Written(r) => {
                self.av_pending = self.av_pending.saturating_sub(1);
                if let Err(e) = r {
                    self.av_error = Some(e.clone());
                    self.flash(format!("Video chip: {e}"));
                }
            }
            avhid::Update::Unavailable(e) => {
                log::debug!("avhid: {e}");
                self.av_status = None;
                self.av_snapshot = None;
                self.av_error = Some(e);
            }
        }
    }

    /// Queue a write. The worker reports each one back, so `av_pending` returns to zero
    /// once the chip has taken them all.
    fn av_write(&mut self, op: avhid::Op, cx: &mut Context<Self>) {
        self.av_pending += 1;
        // Mirror the change locally so the panel responds at once.
        if let Some(snap) = &mut self.av_snapshot {
            match op {
                avhid::Op::SetEdid(i) => snap.status.edid = i,
                avhid::Op::SetSharpness(on) => snap.sharpness = on,
                avhid::Op::SetPicture(p) => snap.picture = p,
                _ => {}
            }
        }
        if let Some(s) = &mut self.av_status {
            if let avhid::Op::SetEdid(i) = op {
                s.edid = i;
            }
        }
        self.av.send(op);
        cx.notify();
    }

    fn step_picture(&mut self, field: fn(&mut Picture) -> &mut u8, delta: i16, cx: &mut Context<Self>) {
        let mut p = self.picture;
        let v = field(&mut p);
        *v = (*v as i16 + delta).clamp(0, avhid::PICTURE_MAX as i16) as u8;
        if p != self.picture {
            self.picture = p;
            self.av_write(avhid::Op::SetPicture(p), cx);
        }
    }

    fn toggle_picture(&mut self, cx: &mut Context<Self>) {
        self.show_picture = !self.show_picture;
        if self.show_picture {
            self.show_modes = false;
            self.io.show = false;
            self.av.send(avhid::Op::Refresh);
        }
        cx.notify();
    }

    fn start_capture(&mut self, _cx: &mut Context<Self>) {
        let tx = self.events.clone();
        match video::ensure_access(false, move |_| {
            let _ = tx.unbounded_send(UiEvent::AccessDecided);
        }) {
            Access::Granted => {}
            Access::Pending => {
                self.capture_note = Some("Waiting for camera permission…".into());
                return;
            }
            Access::Denied => {
                self.capture_note = Some(
                    "Camera access denied. Allow it in System Settings › Privacy & Security › Camera."
                        .into(),
                );
                return;
            }
        }
        let with_audio = self.audio_enabled && {
            let tx = self.events.clone();
            video::ensure_access(true, move |_| {
                let _ = tx.unbounded_send(UiEvent::AccessDecided);
            }) == Access::Granted
        };
        if !video::kiwi_video_present() {
            self.capture_note = Some("Kiwi capture device not found.".into());
            return;
        }
        match Capture::start(self.sink.clone(), with_audio, self.wanted_mode.as_ref()) {
            Ok(c) => {
                c.set_volume(if self.muted { 0.0 } else { 1.0 });
                self.capture = Some(c);
                self.capture_note = None;
            }
            Err(e) => self.capture_note = Some(format!("Video: {e:#}")),
        }
    }

    fn set_captured(&mut self, on: bool, cx: &mut Context<Self>) {
        self.bridge.set_captured(on);
        if !on && self.buttons != 0 {
            self.buttons = 0;
            self.send_abs(0, 0);
        }
        cx.notify();
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    // ---- mouse ---------------------------------------------------------

    /// Map a window position into the target's 0..=32767 coordinate space, using the
    /// letterboxed video rectangle.
    fn to_target(&self, p: Point<Pixels>) -> Option<(u16, u16)> {
        let area = self.video_bounds.get();
        let (fw, fh) = self
            .frame
            .as_ref()
            .map(|f| (f.0.get_width() as f32, f.0.get_height() as f32))
            .unwrap_or((16.0, 9.0));
        let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
        if aw <= 0.0 || ah <= 0.0 {
            return None;
        }
        let scale = (aw / fw).min(ah / fh);
        let (vw, vh) = (fw * scale, fh * scale);
        let ox = f32::from(area.origin.x) + (aw - vw) / 2.0;
        let oy = f32::from(area.origin.y) + (ah - vh) / 2.0;
        let nx = ((f32::from(p.x) - ox) / vw).clamp(0.0, 1.0);
        let ny = ((f32::from(p.y) - oy) / vh).clamp(0.0, 1.0);
        Some(((nx * 32767.0).round() as u16, (ny * 32767.0).round() as u16))
    }

    fn send_abs(&mut self, wheel: i8, pan: i8) {
        let (x, y) = self.last_pos;
        self.device.mouse_abs(self.buttons, x, y, wheel, pan);
    }

    fn button_bit(b: MouseButton) -> u8 {
        match b {
            MouseButton::Left => button::LEFT,
            MouseButton::Right => button::RIGHT,
            MouseButton::Middle => button::MIDDLE,
            MouseButton::Navigate(gpui::NavigationDirection::Back) => button::BACK,
            MouseButton::Navigate(gpui::NavigationDirection::Forward) => button::FORWARD,
        }
    }

    fn on_video_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.bridge.is_captured() {
            window.focus(&self.focus);
            self.set_captured(true, cx);
            self.flash("Input captured — press Ctrl+⌘ to release");
            return;
        }
        if !self.absolute() {
            return;
        }
        if let Some(pos) = self.to_target(ev.position) {
            self.last_pos = pos;
        }
        self.buttons |= Self::button_bit(ev.button);
        self.send_abs(0, 0);
    }

    fn absolute(&self) -> bool {
        self.bridge.mouse_mode() == MouseMode::Absolute
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, _cx: &mut Context<Self>) {
        if !self.absolute() {
            return;
        }
        let bit = Self::button_bit(ev.button);
        if self.buttons & bit != 0 {
            self.buttons &= !bit;
            self.send_abs(0, 0);
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, _cx: &mut Context<Self>) {
        if !self.bridge.is_captured() || !self.absolute() {
            return;
        }
        if let Some(pos) = self.to_target(ev.position) {
            if pos != self.last_pos {
                self.last_pos = pos;
                self.send_abs(0, 0);
            }
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, _cx: &mut Context<Self>) {
        if !self.bridge.is_captured() || !self.absolute() {
            return;
        }
        let (dx, dy) = match ev.delta {
            ScrollDelta::Lines(p) => (p.x, p.y),
            ScrollDelta::Pixels(p) => (f32::from(p.x) / 24.0, f32::from(p.y) / 24.0),
        };
        self.scroll.0 += dx;
        self.scroll.1 += dy;
        let wheel = self.scroll.1.trunc().clamp(-127.0, 127.0);
        let pan = (-self.scroll.0).trunc().clamp(-127.0, 127.0);
        if wheel != 0.0 || pan != 0.0 {
            self.scroll.1 -= wheel;
            self.scroll.0 += pan;
            self.send_abs(wheel as i8, pan as i8);
        }
    }

    // ---- actions -------------------------------------------------------

    fn ctrl_alt_del(&mut self, cx: &mut Context<Self>) {
        let bridge = self.bridge.clone();
        std::thread::spawn(move || bridge.send_chord(&[usage::L_CTRL, usage::L_ALT, usage::DELETE]));
        self.flash("Sent Ctrl+Alt+Del");
        cx.notify();
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            self.flash("Clipboard has no text");
            cx.notify();
            return;
        };
        let n = text.chars().count();
        let skipped = keyboard::type_text(self.device.clone(), text);
        self.flash(if skipped > 0 {
            format!("Typing {n} characters ({skipped} unsupported skipped)")
        } else {
            format!("Typing {n} characters")
        });
        cx.notify();
    }

    fn toggle_mouse_mode(&mut self, cx: &mut Context<Self>) {
        let next = if self.absolute() { MouseMode::Relative } else { MouseMode::Absolute };
        if self.buttons != 0 {
            self.buttons = 0;
            self.send_abs(0, 0);
        }
        self.bridge.set_mouse_mode(next);
        self.flash(match next {
            MouseMode::Relative => "Mouse: relative (pointer locked while captured)",
            MouseMode::Absolute => "Mouse: absolute (some targets, e.g. SteamOS Game Mode, ignore it)",
        });
        cx.notify();
    }

    fn change_speed(&mut self, up: bool, cx: &mut Context<Self>) {
        const STEPS: [f32; 10] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0];
        let cur = self.bridge.mouse_speed();
        let idx = STEPS.iter().position(|&s| (s - cur).abs() < 0.01).unwrap_or(3);
        let next = if up { (idx + 1).min(STEPS.len() - 1) } else { idx.saturating_sub(1) };
        self.bridge.set_mouse_speed(STEPS[next]);
        cx.notify();
    }

    fn toggle_modes(&mut self, cx: &mut Context<Self>) {
        self.show_modes = !self.show_modes;
        if self.show_modes {
            self.show_picture = false;
            self.io.show = false;
        }
        if self.show_modes && self.modes.is_empty() {
            self.modes = video::list_modes();
        }
        cx.notify();
    }

    fn select_mode(&mut self, mode: VideoMode, cx: &mut Context<Self>) {
        self.show_modes = false;
        self.wanted_mode = Some(mode);
        self.capture = None;
        self.frame = None;
        self.last_frame_at = None;
        self.start_capture(cx);
        cx.notify();
    }

    /// One row per resolution; each row offers that resolution's frame rates.
    /// Where a mode exists as both NV12 and YUY2, NV12 wins (it needs the least conversion).
    fn mode_rows(&self) -> Vec<((i32, i32), Vec<VideoMode>)> {
        let mut rows: Vec<((i32, i32), Vec<VideoMode>)> = Vec::new();
        for m in &self.modes {
            let key = (m.width, m.height);
            let row = match rows.iter_mut().find(|(k, _)| *k == key) {
                Some(r) => r,
                None => {
                    rows.push((key, Vec::new()));
                    rows.last_mut().unwrap()
                }
            };
            match row.1.iter_mut().find(|e| (e.fps - m.fps).abs() < 0.5) {
                Some(existing) => {
                    if m.codec() == "NV12" && existing.codec() != "NV12" {
                        *existing = m.clone();
                    }
                }
                None => row.1.push(m.clone()),
            }
        }
        rows
    }

    fn render_mode_picker(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.capture.as_ref().and_then(|c| c.mode.clone());
        let rows = self.mode_rows();
        div()
            .id("mode-picker")
            .occlude()
            .absolute()
            .top(px(46.))
            .right(px(12.))
            .w(px(380.))
            .max_h(px(520.))
            .overflow_y_scroll()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_md()
            .shadow_lg()
            .child(
                div()
                    .px_1()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("All modes are uncompressed. Lower resolutions move less data per frame."),
            )
            .when(rows.is_empty(), |el| el.child(div().p_2().text_sm().child("No capture modes found")))
            .children(rows.into_iter().map(|((w, h), modes)| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().w(px(96.)).text_sm().child(format!("{w}×{h}")))
                    .children(modes.into_iter().map(|m| {
                        let active = current.as_ref() == Some(&m);
                        let id = SharedString::from(format!("mode-{w}x{h}-{:.0}", m.fps));
                        tool_button(id, format!("{:.0}", m.fps), active)
                            .px_2()
                            .on_click(cx.listener(move |app, _, _, cx| app.select_mode(m.clone(), cx)))
                    }))
            }))
    }

    fn render_picture_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div()
            .id("picture-panel")
            .occlude()
            .absolute()
            .top(px(46.))
            .right(px(12.))
            .w(px(400.))
            .max_h(px(560.))
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_md()
            .shadow_lg()
            .text_sm();

        let Some(snap) = self.av_snapshot else {
            let msg = match &self.av_error {
                Some(e) => format!("Video chip not reachable: {e}"),
                None => "Reading the video chip…".into(),
            };
            return panel.child(div().text_color(rgb(MUTED)).child(msg));
        };

        let s = snap.status;
        let heading = |t: &'static str| div().text_xs().text_color(rgb(MUTED)).child(t);
        let note = |t: String| div().text_xs().text_color(rgb(MUTED)).child(t);

        // HDMI input
        let input = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(heading("HDMI INPUT"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(dot(rgb(if s.input { ACCENT } else if s.power_5v { 0xd9a441 } else { MUTED }).into()))
                    .child(s.summary()),
            )
            .child(note(format!(
                "+5V {} · hot-plug {} · A/V firmware {}.{}",
                if s.power_5v { "yes" } else { "no" },
                if s.hpd { "on" } else { "off" },
                snap.info.version.0,
                snap.info.version.1
            )));

        // EDID presets
        let edid_ok = snap.info.supports_edid();
        let edid = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(heading("EDID (RESOLUTIONS OFFERED TO THE TARGET)"))
            .child(div().flex().flex_wrap().gap_1().children(avhid::EDID_PRESETS.iter().map(|&(idx, label)| {
                tool_button(SharedString::from(format!("edid-{idx}")), label, s.edid == idx)
                    .px_2()
                    .when(edid_ok, |b| {
                        b.on_click(cx.listener(move |app, _, _, cx| app.av_write(avhid::Op::SetEdid(idx), cx)))
                    })
            })))
            .child(note(if edid_ok {
                "The target picks up a new EDID when it next checks the display. Some need a replug.".into()
            } else {
                "Needs A/V firmware 1.7 or newer.".into()
            }));

        // Sharpness + picture
        let pic_ok = snap.info.supports_picture();
        let sharp = snap.sharpness;
        let sharpness = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().child("Sharpness filter"))
            .child(
                tool_button("sharp-off", "Off", !sharp)
                    .px_2()
                    .when(pic_ok, |b| {
                        b.on_click(cx.listener(|app, _, _, cx| app.av_write(avhid::Op::SetSharpness(false), cx)))
                    }),
            )
            .child(
                tool_button("sharp-on", "On", sharp)
                    .px_2()
                    .when(pic_ok, |b| {
                        b.on_click(cx.listener(|app, _, _, cx| app.av_write(avhid::Op::SetSharpness(true), cx)))
                    }),
            );

        type Field = fn(&mut Picture) -> &mut u8;
        let fields: [(&'static str, Field); 4] = [
            ("Brightness", |p| &mut p.brightness),
            ("Contrast", |p| &mut p.contrast),
            ("Saturation", |p| &mut p.saturation),
            ("Hue", |p| &mut p.hue),
        ];
        let mut current = self.picture;
        let rows = fields.into_iter().map(|(name, field)| {
            let value = *field(&mut current);
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().flex_1().child(name))
                .child(
                    tool_button(SharedString::from(format!("{name}-down")), "−", false).when(pic_ok, |b| {
                        b.on_click(cx.listener(move |app, _, _, cx| app.step_picture(field, -5, cx)))
                    }),
                )
                .child(div().w(px(40.)).text_center().child(value.to_string()))
                .child(
                    tool_button(SharedString::from(format!("{name}-up")), "+", false).when(pic_ok, |b| {
                        b.on_click(cx.listener(move |app, _, _, cx| app.step_picture(field, 5, cx)))
                    }),
                )
        });
        let neutral = self.picture == Picture::default();
        let picture = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().child(heading("PICTURE")))
                    .child(tool_button("picture-reset", "Reset", false).px_2().when(pic_ok && !neutral, |b| {
                        b.on_click(cx.listener(|app, _, _, cx| {
                            app.picture = Picture::default();
                            app.av_write(avhid::Op::SetPicture(Picture::default()), cx);
                        }))
                    })),
            )
            .child(sharpness)
            .children(rows)
            .child(note(if pic_ok {
                "Applied by the Kiwi's video chip, so they affect every app that uses the capture device."
                    .into()
            } else {
                "Needs A/V firmware 1.9 or newer.".into()
            }));

        panel
            .child(input)
            .child(edid)
            .child(picture)
            .when_some(self.av_error.clone(), |el, e| el.child(div().text_xs().text_color(rgb(0xd9534f)).child(e)))
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        if let Some(c) = &self.capture {
            c.set_volume(if self.muted { 0.0 } else { 1.0 });
        }
        cx.notify();
    }
}

// ---- view ----------------------------------------------------------------

const BG: u32 = 0x16181d;
const PANEL: u32 = 0x1f2228;
const BORDER: u32 = 0x2c3038;
const TEXT: u32 = 0xe6e8eb;
const MUTED: u32 = 0x8b919c;
const ACCENT: u32 = 0x7fbf5a;

fn dot(color: Hsla) -> Div {
    div().size(px(8.)).rounded_full().bg(color)
}

fn tool_button(id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(if active { ACCENT } else { BORDER }))
        .bg(rgb(if active { 0x2a3a22 } else { PANEL }))
        .text_color(rgb(TEXT))
        .text_sm()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0x2a2e36)))
        .child(label.into())
}

impl KiwiApp {
    fn link_summary(&self) -> (Hsla, String) {
        match &self.link {
            LinkStatus::Searching => (rgb(MUTED).into(), "Kiwi not found".into()),
            LinkStatus::Connecting(p) => (rgb(0xd9a441).into(), format!("Connecting {p}…")),
            LinkStatus::Connected(_) => {
                let label = match &self.info {
                    Some(i) => format!(
                        "{} · {} {}.{} · protocol {}.{}",
                        i.board,
                        if i.open.is_some() { "open-kiwi" } else { "fw" },
                        i.firmware.0,
                        i.firmware.1,
                        i.proto_major,
                        i.proto_minor
                    ),
                    None => "Connected".into(),
                };
                (rgb(ACCENT).into(), label)
            }
            LinkStatus::Error(e) => (rgb(0xd9534f).into(), format!("Error: {e}")),
        }
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (color, label) = self.link_summary();
        let captured = self.bridge.is_captured();
        let has_audio = self.capture.as_ref().is_some_and(|c| c.has_audio());
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .child(dot(color))
                    .child(div().text_sm().text_color(rgb(TEXT)).truncate().child(label)),
            )
            .child(
                tool_button("capture", if captured { "Captured" } else { "Capture" }, captured)
                    .on_click(cx.listener(|app, _, window, cx| {
                        let on = !app.bridge.is_captured();
                        if on {
                            window.focus(&app.focus);
                        }
                        app.set_captured(on, cx);
                    })),
            )
            .child(
                tool_button(
                    "mouse-mode",
                    if self.absolute() { "Mouse: absolute" } else { "Mouse: relative" },
                    false,
                )
                .on_click(cx.listener(|app, _, _, cx| app.toggle_mouse_mode(cx))),
            )
            .when(!self.absolute(), |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            tool_button("speed-down", "−", false)
                                .on_click(cx.listener(|app, _, _, cx| app.change_speed(false, cx))),
                        )
                        .child(
                            div()
                                .w(px(64.))
                                .text_center()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child(format!("speed {}×", self.bridge.mouse_speed())),
                        )
                        .child(
                            tool_button("speed-up", "+", false)
                                .on_click(cx.listener(|app, _, _, cx| app.change_speed(true, cx))),
                        ),
                )
            })
            .child(
                tool_button("cad", "Ctrl+Alt+Del", false)
                    .on_click(cx.listener(|app, _, _, cx| app.ctrl_alt_del(cx))),
            )
            .child(
                tool_button("paste", "Paste as keys", false)
                    .on_click(cx.listener(|app, _, _, cx| app.paste(cx))),
            )
            .when(self.io.terminal_enabled(), |el| {
                el.child(
                    tool_button("terminal", "Terminal", self.io.show_terminal).on_click(cx.listener(|app, _, _, cx| {
                        app.io.show_terminal = !app.io.show_terminal;
                        cx.notify();
                    })),
                )
            })
            .child(tool_button("io", "I/O ▾", self.io.show).on_click(cx.listener(|app, _, _, cx| app.toggle_io(cx))))
            .child(
                tool_button("picture", "Picture ▾", self.show_picture)
                    .on_click(cx.listener(|app, _, _, cx| app.toggle_picture(cx))),
            )
            .child({
                let label = match self.capture.as_ref().and_then(|c| c.mode.as_ref()) {
                    Some(m) => format!("{}×{} @ {:.0} ▾", m.width, m.height, m.fps),
                    None => "Video ▾".into(),
                };
                tool_button("video-mode", label, self.show_modes)
                    .on_click(cx.listener(|app, _, _, cx| app.toggle_modes(cx)))
            })
            .when(has_audio, |el| {
                el.child(
                    tool_button("mute", if self.muted { "Unmute" } else { "Mute" }, false)
                        .on_click(cx.listener(|app, _, _, cx| app.toggle_mute(cx))),
                )
            })
    }

    fn render_video(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds_cell = self.video_bounds.clone();
        let captured = self.bridge.is_captured();
        let stale = self.last_frame_at.is_none_or(|t| t.elapsed() > Duration::from_secs(3));

        let content = match (&self.frame, stale) {
            (Some(f), false) => surface(f.0.clone())
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element(),
            _ => {
                let msg = self.capture_note.clone().unwrap_or_else(|| {
                    match (&self.capture, &self.av_status) {
                        (None, _) => "Starting video…".into(),
                        (Some(_), Some(s)) if !s.input => s.summary(),
                        (Some(_), _) => "No video signal".into(),
                    }
                });
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(MUTED))
                    .child(msg)
                    .into_any_element()
            }
        };

        div()
            .id("video")
            .track_focus(&self.focus)
            .relative()
            .flex_1()
            .min_h_0()
            .bg(gpui::black())
            .border_2()
            .border_color(if captured { rgb(ACCENT) } else { rgb(BG) })
            .child(
                canvas(move |b, _, _| bounds_cell.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(content)
            .on_any_mouse_down(cx.listener(Self::on_video_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
    }

    fn render_status(&mut self) -> impl IntoElement {
        let captured = self.bridge.is_captured();
        let hint = if captured {
            "Keyboard & mouse go to the target · Ctrl+⌘ to release"
        } else {
            "Click the video to capture keyboard & mouse"
        };
        let toast = self
            .toast
            .as_ref()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(4))
            .map(|(m, _)| m.clone());
        let right = toast.or_else(|| self.last_log.clone()).unwrap_or_default();
        let video = self.capture.as_ref().map(|c| c.description.clone()).unwrap_or_default();
        div()
            .flex()
            .gap_4()
            .px_3()
            .py_1()
            .bg(rgb(PANEL))
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(div().child(hint))
            .child(div().child(video))
            .when_some(self.av_status.filter(|s| s.input), |el, s| el.child(div().child(s.summary())))
            .when_some(self.state.as_ref(), |el, s| {
                el.child(div().child(format!(
                    "{} °C{}",
                    s.temperature_c,
                    if s.boot_mode() { " · target in boot mode" } else { "" }
                )))
            })
            .child(div().flex_1().min_w_0().truncate().text_right().child(right))
    }
}

impl Render for KiwiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .capture_any_mouse_up(cx.listener(Self::on_mouse_up))
            .relative()
            .child(self.render_toolbar(cx))
            .child(self.render_video(cx))
            .when(self.io.terminal_enabled() && self.io.show_terminal, |el| {
                el.child(self.render_terminal(window, cx))
            })
            .child(self.render_status())
            .when(self.show_modes, |el| el.child(self.render_mode_picker(cx)))
            .when(self.show_picture, |el| el.child(self.render_picture_panel(cx)))
            .when(self.io.show, |el| el.child(self.render_io_panel(cx)))
    }
}

pub fn quit_on_close(cx: &mut App) {
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}
