//! The I/O panel: side-port pinout, side-UART terminal, ATX power/reset control and raw GPIO.
//!
//! Protocol background (opcodes 0x78, 0x227, 0x1FF) is in `protocol.rs`. Like the vendor app,
//! the Serial & ATX settings are re-sent on every connect (the Kiwi keeps them only in RAM).
//! The GPIO tab's settings are one-shot.

use std::collections::VecDeque;
use std::time::Duration;

use gpui::{
    Context, Div, ElementId, FocusHandle, IntoElement, KeyDownEvent, Keystroke, MouseButton, ScrollHandle,
    SharedString, Stateful, Window, div, prelude::*, px, rgb,
};

use super::{ACCENT, BORDER, KiwiApp, MUTED, PANEL, TEXT, dot, tool_button};
use crate::protocol::{KiwiConfig, KiwiInfo, gpio, gpio::Setup, vport};

const UART_COLOR: u32 = 0x5a8fd9;
const ATX_COLOR: u32 = 0x7fbf5a;
const GPIO_COLOR: u32 = 0xd9a441;

/// How long "Force off" holds the power switch, like holding a PC's power button.
const FORCE_OFF: Duration = Duration::from_secs(5);
const PRESS_PRESETS: [u32; 5] = [50, 100, 200, 300, 500];
const PULSE_PRESETS: [u32; 7] = [10, 20, 50, 100, 200, 300, 500];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    SidePort,
    Serial,
    Gpio,
}

/// The side-port layout, from `KiwiInfo::gpio_count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Board {
    /// Six-pin port (GPIO 0–3). Older boards have a three-pin port (Tx, GND, Rx).
    six_pin: bool,
}

impl Board {
    pub fn from_info(info: &KiwiInfo) -> Self {
        Board { six_pin: info.gpio_count != 3 }
    }
    fn gpios(self) -> &'static [u8] {
        if self.six_pin { &[0, 1, 2, 3] } else { &[0, 1] }
    }
    /// Side-port pin number of a GPIO.
    fn side_pin(gpio: u8) -> u8 {
        [1, 3, 4, 6][gpio as usize]
    }
    // ATX pin map, from the vendor's `CycKiwiSideGpio::AtxControl`.
    fn power_switch(self) -> u8 {
        if self.six_pin { 2 } else { 0 }
    }
    fn power_led(self) -> u8 {
        if self.six_pin { 3 } else { 1 }
    }
    const RESET_SWITCH: u8 = 0;
    const HDD_LED: u8 = 1;
}

/// ATX control. The switch pins drive the motherboard's power/reset header through the Kiwi's
/// own driver: output low = released, high = pressed. The LED pins are inputs with a pull-up;
/// a lit LED pulls them low.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Atx {
    pub power: bool,
    pub reset: bool,
    pub hdd_led: bool,
}

impl Atx {
    /// Only what the board can do. Reset and HDD LED need the power switch on and a six-pin port.
    fn normalized(self, board: Board) -> Self {
        let extra = self.power && board.six_pin;
        Atx { power: self.power, reset: self.reset && extra, hdd_led: self.hdd_led && extra }
    }

    /// `(gpio, role, setup)` for each enabled function.
    fn pins(self, board: Board) -> Vec<(u8, &'static str, Setup)> {
        let a = self.normalized(board);
        let led = Setup::Input { pull_up: true, pull_down: false };
        let switch = Setup::Output { high: false };
        let mut v = Vec::new();
        if a.power {
            v.push((board.power_switch(), "ATX power switch", switch));
            v.push((board.power_led(), "ATX power LED", led));
        }
        if a.reset {
            v.push((Board::RESET_SWITCH, "ATX reset switch", switch));
        }
        if a.hdd_led {
            v.push((Board::HDD_LED, "ATX HDD LED", led));
        }
        v
    }

    fn role(self, board: Board, gpio: u8) -> Option<&'static str> {
        self.pins(board).into_iter().find(|p| p.0 == gpio).map(|p| p.1)
    }

    /// Whether this uses GPIO 0 or 1, which the side UART needs.
    fn uses_uart_pins(self, board: Board) -> bool {
        self.pins(board).iter().any(|p| p.0 <= 1)
    }
}

/// One column of the GPIO tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinDraft {
    /// 0 disabled, 1 input, 2 output, 3 pulse (the wire mode numbers).
    mode: u8,
    pull_up: bool,
    pull_down: bool,
    high: bool,
    pulse_high: bool,
    pulse_ms: u32,
}

impl Default for PinDraft {
    fn default() -> Self {
        PinDraft { mode: 0, pull_up: false, pull_down: false, high: false, pulse_high: true, pulse_ms: 100 }
    }
}

impl PinDraft {
    fn setup(&self) -> Setup {
        match self.mode {
            1 => Setup::Input { pull_up: self.pull_up, pull_down: self.pull_down },
            2 => Setup::Output { high: self.high },
            3 => Setup::Pulse { high: self.pulse_high, ms: self.pulse_ms },
            _ => Setup::Disabled,
        }
    }
}

/// What a GPIO is being used for right now.
enum PinUse {
    Uart(&'static str),
    Atx(&'static str),
    Gpio(Setup),
}

/// A minimal line-oriented terminal: keeps printable text, handles newline, tab and
/// backspace, and drops ANSI escape sequences.
#[derive(Default)]
pub struct Terminal {
    lines: VecDeque<String>,
    esc: Esc,
    utf8: Vec<u8>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Esc {
    #[default]
    None,
    Start,
    Csi,
    Osc,
}

impl Terminal {
    const MAX_LINES: usize = 2000;

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.lines.is_empty() {
            self.lines.push_back(String::new());
        }
        for &b in bytes {
            match self.esc {
                Esc::Start => {
                    self.esc = match b {
                        b'[' => Esc::Csi,
                        b']' => Esc::Osc,
                        _ => Esc::None,
                    };
                    continue;
                }
                Esc::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = Esc::None;
                    }
                    continue;
                }
                Esc::Osc => {
                    if b == 0x07 || b == 0x1b {
                        self.esc = Esc::None;
                    }
                    continue;
                }
                Esc::None => {}
            }
            let line = self.lines.back_mut().unwrap();
            match b {
                b'\n' => {
                    self.lines.push_back(String::new());
                    if self.lines.len() > Self::MAX_LINES {
                        self.lines.pop_front();
                    }
                }
                0x08 => {
                    line.pop();
                }
                b'\t' => {
                    let col = line.chars().count();
                    line.extend(std::iter::repeat_n(' ', 8 - col % 8));
                }
                0x1b => self.esc = Esc::Start,
                0x80.. => {
                    self.utf8.push(b);
                    match std::str::from_utf8(&self.utf8) {
                        Ok(s) => {
                            line.push_str(s);
                            self.utf8.clear();
                        }
                        Err(e) if e.error_len().is_some() || self.utf8.len() >= 4 => {
                            line.push('\u{fffd}');
                            self.utf8.clear();
                        }
                        Err(_) => {}
                    }
                }
                0x20..=0x7e => line.push(b as char),
                _ => {} // \r, bell and other controls
            }
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// Bytes a key press sends to a serial console.
fn key_bytes(k: &Keystroke) -> Option<Vec<u8>> {
    let m = &k.modifiers;
    if m.platform {
        return None;
    }
    let seq: &[u8] = match k.key.as_str() {
        "enter" => b"\r",
        "backspace" => b"\x7f",
        "tab" => b"\t",
        "escape" => b"\x1b",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "delete" => b"\x1b[3~",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        _ => b"",
    };
    if !seq.is_empty() {
        return Some(seq.to_vec());
    }
    if m.control && k.key.len() == 1 {
        let c = k.key.as_bytes()[0].to_ascii_lowercase();
        if c.is_ascii_lowercase() || b"@[\\]^_".contains(&c) {
            return Some(vec![c & 0x1f]);
        }
    }
    k.key_char.as_ref().filter(|s| !s.is_empty()).map(|s| s.as_bytes().to_vec())
}

pub struct IoState {
    pub show: bool,
    tab: Tab,

    /// Side UART terminal (KiwiConfig vterm = side UART): chosen in the panel / last applied.
    term_draft: bool,
    term_applied: bool,
    atx_draft: Atx,
    atx_applied: Atx,
    press_ms: u32,
    /// GPIO whose power switch "Force off" is holding, if any.
    holding: Option<u8>,

    pins: [PinDraft; gpio::COUNT as usize],
    /// Last setup sent from the GPIO tab, per GPIO.
    pins_applied: [Setup; gpio::COUNT as usize],

    pub terminal: Terminal,
    pub show_terminal: bool,
    term_focus: FocusHandle,
    term_scroll: ScrollHandle,
}

impl IoState {
    pub fn new(cx: &mut Context<KiwiApp>) -> Self {
        IoState {
            show: false,
            tab: Tab::default(),
            term_draft: false,
            term_applied: false,
            atx_draft: Atx::default(),
            atx_applied: Atx::default(),
            press_ms: 200,
            holding: None,
            pins: Default::default(),
            pins_applied: [Setup::Disabled; gpio::COUNT as usize],
            terminal: Terminal::default(),
            show_terminal: false,
            term_focus: cx.focus_handle(),
            term_scroll: ScrollHandle::new(),
        }
    }

    pub fn terminal_enabled(&self) -> bool {
        self.term_applied
    }

    fn config(&self) -> KiwiConfig {
        KiwiConfig {
            vterm: if self.term_applied { vport::SIDE_UART } else { vport::DISABLED },
            ..KiwiConfig::default()
        }
    }

    fn pin_use(&self, board: Board, g: u8) -> PinUse {
        if self.term_applied && g <= 1 {
            return PinUse::Uart(if g == 0 { "Side UART Tx" } else { "Side UART Rx" });
        }
        if let Some(role) = self.atx_applied.role(board, g) {
            return PinUse::Atx(role);
        }
        PinUse::Gpio(self.pins_applied[g as usize])
    }
}

// ---- small widgets ---------------------------------------------------------

fn heading(t: impl Into<SharedString>) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(t.into())
}

fn note(t: impl Into<SharedString>) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(t.into())
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool, enabled: bool) -> Stateful<Div> {
    tool_button(id, label, active).px_2().when(!enabled, |b| b.opacity(0.4))
}

fn checkbox(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.4))
        .child(
            div()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .border_1()
                .border_color(rgb(if checked { ACCENT } else { BORDER }))
                .bg(rgb(if checked { 0x2a3a22 } else { PANEL }))
                .text_xs()
                .child(if checked { "✓" } else { "" }),
        )
        .child(label.into())
}

fn use_color(u: &PinUse) -> u32 {
    match u {
        PinUse::Uart(_) => UART_COLOR,
        PinUse::Atx(_) => ATX_COLOR,
        PinUse::Gpio(Setup::Disabled) => MUTED,
        PinUse::Gpio(_) => GPIO_COLOR,
    }
}

fn use_label(u: &PinUse) -> String {
    match u {
        PinUse::Uart(s) | PinUse::Atx(s) => (*s).into(),
        PinUse::Gpio(Setup::Disabled) => "not used".into(),
        PinUse::Gpio(Setup::Input { pull_up, pull_down }) => format!(
            "GPIO input{}",
            match (pull_up, pull_down) {
                (true, false) => ", pull-up",
                (false, true) => ", pull-down",
                (true, true) => ", pull-up + pull-down",
                (false, false) => "",
            }
        ),
        PinUse::Gpio(Setup::Output { high }) => format!("GPIO output {}", if *high { "high" } else { "low" }),
        PinUse::Gpio(Setup::Pulse { high, .. }) => format!("GPIO output {} (after pulse)", if *high { "low" } else { "high" }),
    }
}

fn level_text(level: Option<bool>) -> &'static str {
    match level {
        Some(true) => "High",
        Some(false) => "Low",
        None => "–",
    }
}

fn next_preset(presets: &[u32], cur: u32, up: bool) -> u32 {
    if up {
        presets.iter().copied().find(|&p| p > cur).unwrap_or(*presets.last().unwrap())
    } else {
        presets.iter().rev().copied().find(|&p| p < cur).unwrap_or(presets[0])
    }
}

// ---- behaviour -------------------------------------------------------------

impl KiwiApp {
    fn board(&self) -> Option<Board> {
        self.info.as_ref().map(Board::from_info)
    }

    fn io_level(&self, g: u8) -> Option<bool> {
        self.state.as_ref().and_then(|s| gpio::level(s.io, g))
    }

    pub(super) fn toggle_io(&mut self, cx: &mut Context<Self>) {
        self.io.show = !self.io.show;
        if self.io.show {
            self.show_modes = false;
            self.show_picture = false;
            // Start from what's applied.
            self.io.term_draft = self.io.term_applied;
            self.io.atx_draft = self.io.atx_applied;
        }
        cx.notify();
    }

    /// Called on every (re)connect: the Kiwi forgets these settings when it resets.
    pub(super) fn io_on_connect(&mut self) {
        self.device.config(&self.io.config());
        if let Some(board) = self.board() {
            for (g, _, setup) in self.io.atx_applied.pins(board) {
                self.device.gpio(g, setup);
            }
        }
    }

    fn apply_serial_atx(&mut self, cx: &mut Context<Self>) {
        let Some(board) = self.board() else { return };
        let old = self.io.atx_applied.normalized(board);
        let new = self.io.atx_draft.normalized(board);
        self.io.term_applied = self.io.term_draft;
        self.io.atx_applied = new;
        self.io.atx_draft = new;
        // Config first: turning the side UART off frees GPIO 0/1 for ATX.
        self.device.config(&self.io.config());
        // Release pins ATX no longer uses (unless the UART now owns them).
        let new_pins = new.pins(board);
        for (g, _, _) in old.pins(board) {
            if !new_pins.iter().any(|p| p.0 == g) && !(self.io.term_applied && g <= 1) {
                self.device.gpio(g, Setup::Disabled);
                self.io.pins_applied[g as usize] = Setup::Disabled;
            }
        }
        for (g, _, setup) in new_pins {
            self.device.gpio(g, setup);
        }
        if self.io.term_applied {
            self.io.show_terminal = true;
        }
        self.flash("I/O settings sent to the Kiwi");
        cx.notify();
    }

    fn atx_press(&mut self, gpio: u8, what: &str, cx: &mut Context<Self>) {
        self.device.gpio(gpio, Setup::Pulse { high: true, ms: self.io.press_ms });
        self.flash(format!("Pressed {what} for {} ms", self.io.press_ms));
        cx.notify();
    }

    fn atx_force_off(&mut self, board: Board, cx: &mut Context<Self>) {
        if self.io.holding.is_some() {
            return;
        }
        let pin = board.power_switch();
        self.device.gpio(pin, Setup::Output { high: true });
        self.io.holding = Some(pin);
        self.flash(format!("Holding the power button for {} s…", FORCE_OFF.as_secs()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FORCE_OFF).await;
            let _ = this.update(cx, |app, cx| {
                app.release_power_hold();
                app.flash("Released the power button");
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Let go of a "Force off" hold. Also called on shutdown so the button isn't left pressed.
    pub(super) fn release_power_hold(&mut self) {
        if let Some(pin) = self.io.holding.take() {
            self.device.gpio(pin, Setup::Output { high: false });
        }
    }

    fn apply_pin(&mut self, g: u8, cx: &mut Context<Self>) {
        let setup = self.io.pins[g as usize].setup();
        self.device.gpio(g, setup);
        self.io.pins_applied[g as usize] = match setup {
            Setup::Pulse { high, .. } => Setup::Output { high: !high },
            s => s,
        };
        self.flash(format!("Pin-{}: {}", Board::side_pin(g), use_label(&PinUse::Gpio(setup))));
        // Ask for the new input level straight away.
        self.device.send(
            crate::protocol::op::QUERY,
            crate::protocol::query_body(crate::protocol::query::STATE),
        );
        cx.notify();
    }

    pub(super) fn on_terminal_bytes(&mut self, bytes: &[u8]) {
        self.io.terminal.feed(bytes);
        self.io.term_scroll.scroll_to_bottom();
    }

    fn terminal_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.platform {
            match k.key.as_str() {
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        let text = text.replace("\r\n", "\r").replace('\n', "\r");
                        self.device.terminal(text.as_bytes());
                    }
                    cx.stop_propagation();
                }
                "k" => {
                    self.io.terminal.clear();
                    cx.stop_propagation();
                    cx.notify();
                }
                _ => {}
            }
            return;
        }
        if let Some(bytes) = key_bytes(k) {
            self.device.terminal(&bytes);
            cx.stop_propagation();
        }
    }

    // ---- rendering ---------------------------------------------------------

    pub(super) fn render_io_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div()
            .id("io-panel")
            .occlude()
            .absolute()
            .top(px(46.))
            .right(px(12.))
            .w(px(620.))
            .max_h(px(640.))
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

        let tabs = div().flex().gap_1().children(
            [(Tab::SidePort, "Side Port"), (Tab::Serial, "Serial & ATX"), (Tab::Gpio, "GPIO")].map(|(tab, label)| {
                tool_button(SharedString::from(format!("io-tab-{label}")), label, self.io.tab == tab)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.io.tab = tab;
                        cx.notify();
                    }))
            }),
        );

        let Some(board) = self.board() else {
            return panel.child(tabs).child(note("Connect a Kiwi to configure its side port."));
        };
        let body = match self.io.tab {
            Tab::SidePort => self.render_side_port(board).into_any_element(),
            Tab::Serial => self.render_serial(board, cx).into_any_element(),
            Tab::Gpio => self.render_gpio(board, cx).into_any_element(),
        };
        panel.child(tabs).child(body)
    }

    fn render_side_port(&self, board: Board) -> impl IntoElement {
        // (side pin, label above, label below, gpio)
        let pins: &[(u8, &str, &str, Option<u8>)] = if board.six_pin {
            &[
                (1, "Tx/IO", "", Some(0)),
                (2, "", "GND", None),
                (3, "Rx/IO", "", Some(1)),
                (4, "IO", "", Some(2)),
                (5, "", "GND", None),
                (6, "IO", "", Some(3)),
            ]
        } else {
            &[(1, "Tx/IO", "", Some(0)), (2, "", "GND", None), (3, "Rx/IO", "", Some(1))]
        };

        let diagram = div()
            .flex()
            .justify_center()
            .py_3()
            .bg(rgb(0x2a2e36))
            .rounded_lg()
            .child(div().flex().gap_2().px_3().py_1().bg(rgb(0x17191d)).rounded_lg().children(pins.iter().map(
                |&(n, above, below, g)| {
                    let color = g.map_or(MUTED, |g| use_color(&self.io.pin_use(board, g)));
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .w(px(52.))
                        .child(div().h(px(16.)).text_xs().text_color(rgb(MUTED)).child(above))
                        .child(
                            div()
                                .size(px(34.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(rgb(0x3a3f48))
                                .border_2()
                                .border_color(rgb(color))
                                .text_color(rgb(0xe8d44d))
                                .child(n.to_string()),
                        )
                        .child(div().h(px(16.)).text_xs().text_color(rgb(MUTED)).child(below))
                },
            )));

        let describe = |n: u8, g: Option<u8>| -> (String, Option<PinUse>) {
            let what = match (board.six_pin, n) {
                (true, 1) => "UART Tx · GPIO · ATX reset switch",
                (true, 3) => "UART Rx · GPIO · ATX HDD LED",
                (true, 4) => "GPIO · ATX power switch",
                (true, 6) => "GPIO · ATX power LED",
                (false, 1) => "UART Tx · GPIO · ATX power switch",
                (false, 3) => "UART Rx · GPIO · ATX power LED",
                _ => "Ground",
            };
            (format!("Pin-{n}: {what}"), g.map(|g| self.io.pin_use(board, g)))
        };
        let rows = pins.iter().map(|&(n, _, _, g)| {
            let (text, now) = describe(n, g);
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(text))
                .when_some(now, |el, u| {
                    let level = g.and_then(|g| self.io_level(g));
                    el.child(dot(rgb(use_color(&u)).into())).child(
                        div()
                            .w(px(200.))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(match level {
                                Some(l) => format!("{} · {}", use_label(&u), level_text(Some(l))),
                                None => use_label(&u),
                            }),
                    )
                })
        });

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("SIDE VIEW"))
            .child(diagram)
            .child(div().flex().flex_col().gap_1().children(rows))
            .child(note("3.3 V logic. The ATX functions need the Kiwi ATX adapter or equivalent wiring to the motherboard's front-panel header."))
    }

    fn render_serial(&self, board: Board, cx: &mut Context<Self>) -> impl IntoElement {
        let io = &self.io;
        let fw_ok = self.info.as_ref().is_some_and(|i| i.firmware_at_least(1, 16));
        let atx = io.atx_draft.normalized(board);
        let uart_blocked = atx.uses_uart_pins(board);
        let term = io.term_draft;

        // UART terminal
        let terminal = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("UART TERMINAL"))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(button("term-off", "Disabled", !term, true).on_click(cx.listener(|app, _, _, cx| {
                        app.io.term_draft = false;
                        cx.notify();
                    })))
                    .child(button("term-side", "Side UART", term, !uart_blocked).when(!uart_blocked, |b| {
                        b.on_click(cx.listener(|app, _, _, cx| {
                            app.io.term_draft = true;
                            cx.notify();
                        }))
                    })),
            )
            .child(note(if uart_blocked {
                "Pins 1 and 3 are used by ATX control. Turn off the reset switch and HDD LED to use the side UART."
            } else {
                "Pin-1 Tx, Pin-3 Rx, Pin-2 ground · 115200 baud, 8N1, 3.3 V. Output shows in a terminal pane under the video."
            }));

        // ATX
        let led_on = |g: u8| self.io_level(g) == Some(false);
        let reset_ok = board.six_pin && atx.power && !term;
        let applied = io.atx_applied.normalized(board);
        let holding = io.holding.is_some();
        let power_checkbox_ok = fw_ok && (board.six_pin || !term);

        let mut atx_box = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("ATX CONTROL"))
            .child(
                checkbox("atx-power", format!("Power switch & LED (Pin-{} / Pin-{})", Board::side_pin(board.power_switch()), Board::side_pin(board.power_led())), atx.power, power_checkbox_ok)
                    .when(power_checkbox_ok, |c| {
                        c.on_click(cx.listener(|app, _, _, cx| {
                            app.io.atx_draft.power = !app.io.atx_draft.power;
                            cx.notify();
                        }))
                    }),
            );
        if board.six_pin {
            atx_box = atx_box
                .child(checkbox("atx-reset", "Reset switch (Pin-1)", atx.reset, reset_ok).when(reset_ok, |c| {
                    c.on_click(cx.listener(|app, _, _, cx| {
                        app.io.atx_draft.reset = !app.io.atx_draft.reset;
                        cx.notify();
                    }))
                }))
                .child(checkbox("atx-hdd", "HDD LED (Pin-3)", atx.hdd_led, reset_ok).when(reset_ok, |c| {
                    c.on_click(cx.listener(|app, _, _, cx| {
                        app.io.atx_draft.hdd_led = !app.io.atx_draft.hdd_led;
                        cx.notify();
                    }))
                }));
        }
        atx_box = atx_box.child(note(if !fw_ok {
            "Needs Kiwi firmware 1.16 or newer."
        } else if term && board.six_pin {
            "The side UART uses Pin-1 and Pin-3, so the reset switch and HDD LED are unavailable."
        } else if term {
            "The side UART uses the ATX pins on this board."
        } else {
            "The switches are pulsed through the Kiwi's driver; the LEDs are read back as inputs."
        }));

        // Live controls for what's applied.
        if applied.power {
            let led = |label: &'static str, on: bool| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(dot(rgb(if on { ACCENT } else { 0x3a3f48 }).into()))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!("{label} {}", if on { "on" } else { "off" })))
            };
            let power_pin = board.power_switch();
            atx_box = atx_box.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x17191d))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(led("Power", led_on(board.power_led())))
                            .when(applied.hdd_led, |el| el.child(led("HDD", led_on(Board::HDD_LED)))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(button("atx-press-power", "Power", false, !holding).when(!holding, |b| {
                                b.on_click(cx.listener(move |app, _, _, cx| app.atx_press(power_pin, "power", cx)))
                            }))
                            .when(applied.reset, |el| {
                                el.child(button("atx-press-reset", "Reset", false, !holding).when(!holding, |b| {
                                    b.on_click(cx.listener(|app, _, _, cx| app.atx_press(Board::RESET_SWITCH, "reset", cx)))
                                }))
                            })
                            .child(
                                button("atx-force-off", if holding { "Holding…" } else { "Force off (hold 5 s)" }, holding, !holding)
                                    .when(!holding, |b| {
                                        b.on_click(cx.listener(move |app, _, _, cx| app.atx_force_off(board, cx)))
                                    }),
                            )
                            .child(div().flex_1())
                            .child(note("press"))
                            .child(tool_button("press-down", "−", false).on_click(cx.listener(|app, _, _, cx| {
                                app.io.press_ms = next_preset(&PRESS_PRESETS, app.io.press_ms, false);
                                cx.notify();
                            })))
                            .child(div().w(px(56.)).text_center().child(format!("{} ms", io.press_ms)))
                            .child(tool_button("press-up", "+", false).on_click(cx.listener(|app, _, _, cx| {
                                app.io.press_ms = next_preset(&PRESS_PRESETS, app.io.press_ms, true);
                                cx.notify();
                            }))),
                    ),
            );
        }

        let dirty = io.term_draft != io.term_applied || atx != applied;
        let apply = div()
            .flex()
            .items_center()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(button("io-apply", "Apply settings", dirty, dirty).when(dirty, |b| {
                b.on_click(cx.listener(|app, _, _, cx| app.apply_serial_atx(cx)))
            }))
            .child(note(if dirty {
                "Not applied yet."
            } else {
                "Re-sent to the Kiwi on every connect while the app is open."
            }));

        div().flex().flex_col().gap_4().child(terminal).child(atx_box).child(apply)
    }

    fn render_gpio(&self, board: Board, cx: &mut Context<Self>) -> impl IntoElement {
        let cx: &Context<Self> = cx;
        let gpios = board.gpios();

        // One row per property, one cell per pin, so the labels line up whatever the content.
        let row = |label: &'static str, cells: Vec<gpui::AnyElement>| {
            div()
                .flex()
                .items_start()
                .gap_2()
                .py_1()
                .border_b_1()
                .border_color(rgb(BORDER))
                .child(div().w(px(84.)).flex_none().pt_1().text_xs().text_color(rgb(MUTED)).child(label))
                .children(cells.into_iter().map(|c| div().flex_1().min_w_0().child(c)))
        };
        let used = |g: u8| match self.io.pin_use(board, g) {
            PinUse::Uart(s) | PinUse::Atx(s) => Some(s),
            PinUse::Gpio(_) => None,
        };
        let id = |g: u8, s: &str| SharedString::from(format!("gpio-{g}-{s}"));
        let set = move |g: u8, f: fn(&mut PinDraft)| {
            cx.listener(move |app: &mut KiwiApp, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<KiwiApp>| {
                f(&mut app.io.pins[g as usize]);
                cx.notify();
            })
        };

        let header = gpios
            .iter()
            .map(|&g| div().text_color(rgb(TEXT)).child(format!("Pin-{}", Board::side_pin(g))).into_any_element())
            .collect();

        let modes = gpios
            .iter()
            .map(|&g| {
                let d = self.io.pins[g as usize];
                let free = used(g).is_none();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children([(0u8, "Disabled"), (1, "Input"), (2, "Output"), (3, "Pulse")].map(|(m, name)| {
                        button(id(g, name), name, d.mode == m, free).when(free, |b| {
                            b.on_click(cx.listener(move |app, _, _, cx| {
                                app.io.pins[g as usize].mode = m;
                                cx.notify();
                            }))
                        })
                    }))
                    .into_any_element()
            })
            .collect();

        let options = gpios
            .iter()
            .map(|&g| {
                let d = self.io.pins[g as usize];
                let free = used(g).is_none();
                match d.mode {
                    1 => div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            checkbox(id(g, "pu"), "Pull up", d.pull_up, free)
                                .when(free, |c| c.on_click(set(g, |p| p.pull_up = !p.pull_up))),
                        )
                        .child(
                            checkbox(id(g, "pd"), "Pull down", d.pull_down, free)
                                .when(free, |c| c.on_click(set(g, |p| p.pull_down = !p.pull_down))),
                        ),
                    2 => div()
                        .flex()
                        .gap_1()
                        .child(button(id(g, "lo"), "Low", !d.high, free).when(free, |b| b.on_click(set(g, |p| p.high = false))))
                        .child(button(id(g, "hi"), "High", d.high, free).when(free, |b| b.on_click(set(g, |p| p.high = true)))),
                    3 => div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            checkbox(id(g, "th"), "To high", d.pulse_high, free)
                                .when(free, |c| c.on_click(set(g, |p| p.pulse_high = !p.pulse_high))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(button(id(g, "w-"), "−", false, free).when(free, |b| {
                                    b.on_click(set(g, |p| p.pulse_ms = next_preset(&PULSE_PRESETS, p.pulse_ms, false)))
                                }))
                                .child(div().flex_1().text_center().text_xs().child(format!("{} ms", d.pulse_ms)))
                                .child(button(id(g, "w+"), "+", false, free).when(free, |b| {
                                    b.on_click(set(g, |p| p.pulse_ms = next_preset(&PULSE_PRESETS, p.pulse_ms, true)))
                                })),
                        ),
                    _ => div().pt_1().child(note("—")),
                }
                .into_any_element()
            })
            .collect();

        let levels = gpios
            .iter()
            .map(|&g| {
                let level = self.io_level(g);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pt_1()
                    .child(dot(rgb(match level {
                        Some(true) => ACCENT,
                        Some(false) => 0x3a3f48,
                        None => PANEL,
                    })
                    .into()))
                    .child(div().text_xs().child(level_text(level)))
                    .into_any_element()
            })
            .collect();

        let actions = gpios
            .iter()
            .map(|&g| {
                let d = self.io.pins[g as usize];
                let dirty = d.setup() != self.io.pins_applied[g as usize] || d.mode == 3;
                match used(g) {
                    Some(s) => note(format!("Used by {s}")).into_any_element(),
                    None => button(id(g, "apply"), if d.mode == 3 { "Pulse" } else { "Apply" }, dirty, true)
                        .on_click(cx.listener(move |app, _, _, cx| app.apply_pin(g, cx)))
                        .into_any_element(),
                }
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .child(row("", header))
            .child(row("Mode", modes))
            .child(row("Options", options))
            .child(row("Input level", levels))
            .child(row("", actions))
            .child(div().pt_2().child(note(
                "Input level reads back as High/Low while a pin is an input. Pulse drives the pin for the given width \
                 (up to 500 ms), then to the opposite level. These settings aren't re-sent on reconnect.",
            )))
            .when(self.io.term_applied || self.io.atx_applied.power, |el| {
                el.child(div().pt_1().child(note("Pins used by the side UART or ATX control are managed on the Serial & ATX tab.")))
            })
    }

    pub(super) fn render_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.io.term_focus.is_focused(window);
        let lines = &self.io.terminal.lines;
        let skip = lines.len().saturating_sub(400);
        div()
            .h(px(240.))
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(0x0f1114))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .bg(rgb(PANEL))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(dot(rgb(UART_COLOR).into()))
                    .child(div().flex_1().child(if focused {
                        "Side UART · 115200 8N1 · typing goes to the UART · ⌘V paste · ⌘K clear"
                    } else {
                        "Side UART · 115200 8N1 · click here to type"
                    }))
                    .child(tool_button("term-clear", "Clear", false).px_2().on_click(cx.listener(|app, _, _, cx| {
                        app.io.terminal.clear();
                        cx.notify();
                    })))
                    .child(tool_button("term-hide", "Hide", false).px_2().on_click(cx.listener(|app, _, _, cx| {
                        app.io.show_terminal = false;
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .id("terminal")
                    .track_focus(&self.io.term_focus)
                    .track_scroll(&self.io.term_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .py_2()
                    .border_1()
                    .border_color(rgb(if focused { UART_COLOR } else { 0x0f1114 }))
                    .font_family("Menlo")
                    .text_xs()
                    .text_color(rgb(0xd0d4da))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|app, _, window, cx| {
                            if app.bridge.is_captured() {
                                app.set_captured(false, cx);
                            }
                            window.focus(&app.io.term_focus);
                            cx.notify();
                        }),
                    )
                    .on_key_down(cx.listener(|app, ev: &KeyDownEvent, _, cx| app.terminal_key(ev, cx)))
                    .children(lines.iter().skip(skip).map(|l| {
                        div().whitespace_nowrap().child(if l.is_empty() { " ".to_string() } else { l.clone() })
                    }))
                    .when(lines.iter().all(|l| l.is_empty()), |el| {
                        el.child(div().text_color(rgb(MUTED)).child("No output yet."))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &Terminal) -> Vec<&str> {
        t.lines.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    fn terminal_basics() {
        let mut t = Terminal::default();
        t.feed(b"login: \x1b[1;32mroot\x1b[0m\r\nab\x08c\tx\n");
        assert_eq!(text(&t), vec!["login: root", "ac      x", ""]);
    }

    #[test]
    fn terminal_utf8_split() {
        let mut t = Terminal::default();
        let s = "°C".as_bytes();
        t.feed(&s[..1]);
        t.feed(&s[1..]);
        assert_eq!(text(&t), vec!["°C"]);
    }

    #[test]
    fn atx_pin_map() {
        let six = Board { six_pin: true };
        let all = Atx { power: true, reset: true, hdd_led: true };
        let pins: Vec<u8> = all.pins(six).iter().map(|p| p.0).collect();
        assert_eq!(pins, vec![2, 3, 0, 1]);
        assert!(all.uses_uart_pins(six));
        assert!(!Atx { power: true, ..Default::default() }.uses_uart_pins(six));
        // Reset needs power.
        assert!(Atx { reset: true, ..Default::default() }.pins(six).is_empty());

        let three = Board { six_pin: false };
        let pins: Vec<u8> = all.pins(three).iter().map(|p| p.0).collect();
        assert_eq!(pins, vec![0, 1]);
        assert!(all.uses_uart_pins(three));
    }

    #[test]
    fn presets_step() {
        assert_eq!(next_preset(&PRESS_PRESETS, 200, true), 300);
        assert_eq!(next_preset(&PRESS_PRESETS, 500, true), 500);
        assert_eq!(next_preset(&PRESS_PRESETS, 50, false), 50);
        assert_eq!(next_preset(&PRESS_PRESETS, 120, false), 100);
    }
}
