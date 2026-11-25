//! Keyboard and (relative) mouse capture via an `NSEvent` local monitor.
//!
//! We read hardware keycodes (layout independent, left/right modifiers distinct) and
//! swallow the events while captured so host shortcuts like Cmd+Q go to the target instead.
//!
//! In relative mouse mode the host cursor is hidden and frozen (pointer lock) and raw
//! movement deltas are forwarded as HID relative reports. This works everywhere (BIOS,
//! SteamOS Game Mode), unlike absolute positioning which some targets ignore.

use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::Weak;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
use parking_lot::Mutex;

use crate::device::DeviceHandle;
use crate::keyboard::{KeyboardState, mac_keycode_to_hid, usage};
use crate::protocol::button;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    /// Pointer lock + relative deltas (works on every target).
    Relative,
    /// Absolute position over the video (seamless, but not every target accepts it).
    Absolute,
}

#[derive(Default)]
struct RelMouse {
    buttons: u8,
    /// Sub-pixel remainders.
    fx: f64,
    fy: f64,
    wheel: f64,
    pan: f64,
}

/// Device-dependent modifier bits in `NSEvent.modifierFlags` (IOKit `NX_DEVICE*KEYMASK`).
const NX_LCTL: usize = 0x0000_0001;
const NX_LSHIFT: usize = 0x0000_0002;
const NX_RSHIFT: usize = 0x0000_0004;
const NX_LCMD: usize = 0x0000_0008;
const NX_RCMD: usize = 0x0000_0010;
const NX_LALT: usize = 0x0000_0020;
const NX_RALT: usize = 0x0000_0040;
const NX_RCTL: usize = 0x0000_2000;

pub struct InputBridge {
    captured: AtomicBool,
    mode: AtomicU8,
    pointer_locked: AtomicBool,
    rel: Mutex<RelMouse>,
    /// Relative-mouse multiplier, f32 bits.
    speed: AtomicU32,
    keyboard: Mutex<KeyboardState>,
    device: DeviceHandle,
    on_release: Box<dyn Fn() + Send + Sync>,
}

impl InputBridge {
    pub fn new(device: DeviceHandle, on_release: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let bridge = Arc::new(Self {
            captured: AtomicBool::new(false),
            mode: AtomicU8::new(0),
            pointer_locked: AtomicBool::new(false),
            rel: Mutex::new(RelMouse::default()),
            speed: AtomicU32::new(1.0f32.to_bits()),
            keyboard: Mutex::new(KeyboardState::default()),
            device,
            on_release: Box::new(on_release),
        });
        spawn_flusher(Arc::downgrade(&bridge));
        bridge
    }

    pub fn is_captured(&self) -> bool {
        self.captured.load(Ordering::SeqCst)
    }

    pub fn mouse_mode(&self) -> MouseMode {
        if self.mode.load(Ordering::SeqCst) == 0 { MouseMode::Relative } else { MouseMode::Absolute }
    }

    /// Main thread only (touches the cursor).
    pub fn set_mouse_mode(&self, mode: MouseMode) {
        self.mode.store(if mode == MouseMode::Relative { 0 } else { 1 }, Ordering::SeqCst);
        self.update_pointer_lock();
    }

    /// Main thread only (touches the cursor).
    pub fn set_captured(&self, on: bool) {
        let was = self.captured.swap(on, Ordering::SeqCst);
        if was && !on {
            self.release_all();
        }
        self.update_pointer_lock();
    }

    fn update_pointer_lock(&self) {
        let want = self.is_captured() && self.mouse_mode() == MouseMode::Relative;
        if self.pointer_locked.swap(want, Ordering::SeqCst) == want {
            return;
        }
        unsafe {
            if want {
                CGDisplayHideCursor(CGMainDisplayID());
                CGAssociateMouseAndMouseCursorPosition(0);
            } else {
                CGAssociateMouseAndMouseCursorPosition(1);
                CGDisplayShowCursor(CGMainDisplayID());
            }
        }
    }

    /// Add movement; it is sent by the flusher thread (or immediately on button changes).
    fn accumulate(&self, rel: &mut RelMouse, dx: f64, dy: f64, wheel: f64, pan: f64) {
        let speed = self.mouse_speed() as f64;
        rel.fx += dx * speed;
        rel.fy += dy * speed;
        rel.wheel += wheel;
        rel.pan += pan;
    }

    /// Send whatever whole counts have accumulated. Returns true if anything was sent.
    fn flush(&self, rel: &mut RelMouse, force: bool) -> bool {
        let mut x = rel.fx.trunc() as i64;
        let mut y = rel.fy.trunc() as i64;
        let w = rel.wheel.trunc().clamp(-127.0, 127.0) as i8;
        let p = rel.pan.trunc().clamp(-127.0, 127.0) as i8;
        if !force && x == 0 && y == 0 && w == 0 && p == 0 {
            return false;
        }
        rel.fx -= x as f64;
        rel.fy -= y as f64;
        rel.wheel -= w as f64;
        rel.pan -= p as f64;
        // Split large movements into several i8 reports.
        let mut first = true;
        loop {
            let sx = x.clamp(-127, 127);
            let sy = y.clamp(-127, 127);
            let (sw, sp) = if first { (w, p) } else { (0, 0) };
            self.device.mouse_rel(rel.buttons, sx as i8, sy as i8, sw, sp);
            x -= sx;
            y -= sy;
            first = false;
            if x == 0 && y == 0 {
                break;
            }
        }
        true
    }

    pub fn mouse_speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }

    pub fn set_mouse_speed(&self, speed: f32) {
        self.speed.store(speed.clamp(0.1, 10.0).to_bits(), Ordering::Relaxed);
    }

    fn on_mouse(&self, ev: &NSEvent) {
        let ty = ev.r#type();
        let mut rel = self.rel.lock();
        let bit = |n: isize| match n {
            0 => button::LEFT,
            1 => button::RIGHT,
            2 => button::MIDDLE,
            3 => button::BACK,
            4 => button::FORWARD,
            _ => 0,
        };
        if ty == NSEventType::LeftMouseDown || ty == NSEventType::RightMouseDown || ty == NSEventType::OtherMouseDown {
            // Flush pending motion first so the click lands where the cursor is.
            self.flush(&mut rel, false);
            rel.buttons |= bit(ev.buttonNumber());
            self.flush(&mut rel, true);
        } else if ty == NSEventType::LeftMouseUp || ty == NSEventType::RightMouseUp || ty == NSEventType::OtherMouseUp {
            let b = bit(ev.buttonNumber());
            if rel.buttons & b != 0 {
                self.flush(&mut rel, false);
                rel.buttons &= !b;
                self.flush(&mut rel, true);
            }
        } else if ty == NSEventType::ScrollWheel {
            let (dx, dy) = (ev.scrollingDeltaX(), ev.scrollingDeltaY());
            let scale = if ev.hasPreciseScrollingDeltas() { 1.0 / 12.0 } else { 1.0 };
            rel.wheel += dy * scale;
            rel.pan += -dx * scale;
        } else {
            // Moved / dragged.
            let (dx, dy) = (ev.deltaX(), ev.deltaY());
            self.accumulate(&mut rel, dx, dy, 0.0, 0.0);
        }
    }

    /// Send all-keys-up and all-buttons-up reports.
    pub fn release_all(&self) {
        self.keyboard.lock().clear();
        self.device.keyboard([0; 8]);
        let mut rel = self.rel.lock();
        if rel.buttons != 0 {
            rel.buttons = 0;
            self.device.mouse_rel(0, 0, 0, 0, 0);
        }
    }

    fn update(&self, f: impl FnOnce(&mut KeyboardState) -> bool) {
        let mut kb = self.keyboard.lock();
        if f(&mut kb) {
            self.device.keyboard(kb.report());
        }
    }

    /// Press and release a chord, e.g. Ctrl+Alt+Del.
    pub fn send_chord(&self, usages: &[u8]) {
        let mut kb = KeyboardState::default();
        for &u in usages {
            kb.press(u);
        }
        self.device.keyboard(kb.report());
        std::thread::sleep(std::time::Duration::from_millis(40));
        self.device.keyboard([0; 8]);
    }

    fn on_key(&self, code: u16, down: bool) {
        let Some(u) = mac_keycode_to_hid(code) else {
            log::debug!("unmapped keycode {code:#x}");
            return;
        };
        self.update(|kb| if down { kb.press(u) } else { kb.release(u) });
    }

    fn on_flags(&self, code: u16, flags: NSEventModifierFlags) {
        let bits = flags.0;
        let (u, mask) = match code {
            0x38 => (usage::L_SHIFT, NX_LSHIFT),
            0x3C => (usage::R_SHIFT, NX_RSHIFT),
            0x3B => (usage::L_CTRL, NX_LCTL),
            0x3E => (usage::R_CTRL, NX_RCTL),
            0x3A => (usage::L_ALT, NX_LALT),
            0x3D => (usage::R_ALT, NX_RALT),
            0x37 => (usage::L_GUI, NX_LCMD),
            0x36 => (usage::R_GUI, NX_RCMD),
            0x39 => {
                // Caps Lock: the host toggles a latched state; the target wants a tap.
                self.update(|kb| kb.press(usage::CAPS_LOCK));
                self.update(|kb| kb.release(usage::CAPS_LOCK));
                return;
            }
            _ => return,
        };
        let down = bits & mask != 0;
        self.update(|kb| if down { kb.press(u) } else { kb.release(u) });

        // Release chord: Ctrl + Cmd.
        if flags.contains(NSEventModifierFlags::Control) && flags.contains(NSEventModifierFlags::Command) {
            self.set_captured(false);
            (self.on_release)();
        }
    }
}

/// Coalesce relative motion: one report per interval instead of one per host event, so
/// the Kiwi never queues more reports than the target polls (which shows up as lag).
const MOUSE_INTERVAL: Duration = Duration::from_millis(8);

fn spawn_flusher(bridge: Weak<InputBridge>) {
    std::thread::Builder::new()
        .name("kiwi-mouse".into())
        .spawn(move || {
            loop {
                std::thread::sleep(MOUSE_INTERVAL);
                let Some(b) = bridge.upgrade() else { return };
                if b.is_captured() && b.mouse_mode() == MouseMode::Relative {
                    let mut rel = b.rel.lock();
                    b.flush(&mut rel, false);
                }
            }
        })
        .expect("spawn mouse thread");
}

/// Keeps the monitor installed for as long as it is alive.
pub struct KeyMonitor(Retained<AnyObject>);

impl Drop for KeyMonitor {
    fn drop(&mut self) {
        unsafe { NSEvent::removeMonitor(&self.0) };
    }
}

/// Must be called on the main thread.
pub fn install(bridge: Arc<InputBridge>) -> Option<KeyMonitor> {
    let block = RcBlock::new(move |ev: NonNull<NSEvent>| -> *mut NSEvent {
        let ev_ref = unsafe { ev.as_ref() };
        if !bridge.is_captured() {
            return ev.as_ptr();
        }
        let ty = ev_ref.r#type();
        if ty == NSEventType::KeyDown {
            if !ev_ref.isARepeat() {
                bridge.on_key(ev_ref.keyCode(), true);
            }
        } else if ty == NSEventType::KeyUp {
            bridge.on_key(ev_ref.keyCode(), false);
        } else if ty == NSEventType::FlagsChanged {
            bridge.on_flags(ev_ref.keyCode(), ev_ref.modifierFlags());
        } else if bridge.mouse_mode() == MouseMode::Relative {
            bridge.on_mouse(ev_ref);
        } else {
            return ev.as_ptr();
        }
        ptr::null_mut()
    });
    let mask = NSEventMask::KeyDown
        | NSEventMask::KeyUp
        | NSEventMask::FlagsChanged
        | NSEventMask::MouseMoved
        | NSEventMask::LeftMouseDragged
        | NSEventMask::RightMouseDragged
        | NSEventMask::OtherMouseDragged
        | NSEventMask::LeftMouseDown
        | NSEventMask::LeftMouseUp
        | NSEventMask::RightMouseDown
        | NSEventMask::RightMouseUp
        | NSEventMask::OtherMouseDown
        | NSEventMask::OtherMouseUp
        | NSEventMask::ScrollWheel;
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) }.map(KeyMonitor)
}
