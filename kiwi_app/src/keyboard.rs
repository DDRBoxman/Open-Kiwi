//! HID keyboard state, macOS keycode -> HID usage mapping, and text typing.

use std::thread;
use std::time::Duration;

use crate::device::DeviceHandle;

pub mod usage {
    pub const ENTER: u8 = 0x28;
    pub const ESCAPE: u8 = 0x29;
    pub const BACKSPACE: u8 = 0x2A;
    pub const TAB: u8 = 0x2B;
    pub const SPACE: u8 = 0x2C;
    pub const CAPS_LOCK: u8 = 0x39;
    pub const DELETE: u8 = 0x4C;
    pub const L_CTRL: u8 = 0xE0;
    pub const L_SHIFT: u8 = 0xE1;
    pub const L_ALT: u8 = 0xE2;
    pub const L_GUI: u8 = 0xE3;
    pub const R_CTRL: u8 = 0xE4;
    pub const R_SHIFT: u8 = 0xE5;
    pub const R_ALT: u8 = 0xE6;
    pub const R_GUI: u8 = 0xE7;
}

pub fn is_modifier(u: u8) -> bool {
    (0xE0..=0xE7).contains(&u)
}

/// Boot-keyboard state: modifier byte plus up to six held keys.
#[derive(Default, Debug, Clone)]
pub struct KeyboardState {
    mods: u8,
    keys: Vec<u8>,
}

impl KeyboardState {
    /// Returns true if the report changed.
    pub fn press(&mut self, u: u8) -> bool {
        if is_modifier(u) {
            let bit = 1 << (u - 0xE0);
            let changed = self.mods & bit == 0;
            self.mods |= bit;
            changed
        } else if self.keys.contains(&u) {
            false
        } else {
            if self.keys.len() == 6 {
                self.keys.remove(0);
            }
            self.keys.push(u);
            true
        }
    }

    pub fn release(&mut self, u: u8) -> bool {
        if is_modifier(u) {
            let bit = 1 << (u - 0xE0);
            let changed = self.mods & bit != 0;
            self.mods &= !bit;
            changed
        } else {
            let before = self.keys.len();
            self.keys.retain(|&k| k != u);
            before != self.keys.len()
        }
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.mods != 0 || !self.keys.is_empty();
        self.mods = 0;
        self.keys.clear();
        changed
    }

    pub fn report(&self) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[0] = self.mods;
        for (i, k) in self.keys.iter().enumerate() {
            r[2 + i] = *k;
        }
        r
    }
}

/// macOS virtual keycode (kVK_*) -> USB HID usage (page 0x07).
pub fn mac_keycode_to_hid(code: u16) -> Option<u8> {
    Some(match code {
        0x00 => 0x04, // A
        0x01 => 0x16, // S
        0x02 => 0x07, // D
        0x03 => 0x09, // F
        0x04 => 0x0B, // H
        0x05 => 0x0A, // G
        0x06 => 0x1D, // Z
        0x07 => 0x1B, // X
        0x08 => 0x06, // C
        0x09 => 0x19, // V
        0x0A => 0x64, // ISO section (non-US \)
        0x0B => 0x05, // B
        0x0C => 0x14, // Q
        0x0D => 0x1A, // W
        0x0E => 0x08, // E
        0x0F => 0x15, // R
        0x10 => 0x1C, // Y
        0x11 => 0x17, // T
        0x12 => 0x1E, // 1
        0x13 => 0x1F, // 2
        0x14 => 0x20, // 3
        0x15 => 0x21, // 4
        0x16 => 0x23, // 6
        0x17 => 0x22, // 5
        0x18 => 0x2E, // =
        0x19 => 0x26, // 9
        0x1A => 0x24, // 7
        0x1B => 0x2D, // -
        0x1C => 0x25, // 8
        0x1D => 0x27, // 0
        0x1E => 0x30, // ]
        0x1F => 0x12, // O
        0x20 => 0x18, // U
        0x21 => 0x2F, // [
        0x22 => 0x0C, // I
        0x23 => 0x13, // P
        0x24 => 0x28, // Return
        0x25 => 0x0F, // L
        0x26 => 0x0D, // J
        0x27 => 0x34, // '
        0x28 => 0x0E, // K
        0x29 => 0x33, // ;
        0x2A => 0x31, // backslash
        0x2B => 0x36, // ,
        0x2C => 0x38, // /
        0x2D => 0x11, // N
        0x2E => 0x10, // M
        0x2F => 0x37, // .
        0x30 => 0x2B, // Tab
        0x31 => 0x2C, // Space
        0x32 => 0x35, // `
        0x33 => 0x2A, // Backspace
        0x35 => 0x29, // Escape
        0x36 => usage::R_GUI,
        0x37 => usage::L_GUI,
        0x38 => usage::L_SHIFT,
        0x39 => usage::CAPS_LOCK,
        0x3A => usage::L_ALT,
        0x3B => usage::L_CTRL,
        0x3C => usage::R_SHIFT,
        0x3D => usage::R_ALT,
        0x3E => usage::R_CTRL,
        0x40 => 0x6C, // F17
        0x41 => 0x63, // Keypad .
        0x43 => 0x55, // Keypad *
        0x45 => 0x57, // Keypad +
        0x47 => 0x53, // Keypad Clear -> Num Lock
        0x48 => 0x80, // Volume up
        0x49 => 0x81, // Volume down
        0x4A => 0x7F, // Mute
        0x4B => 0x54, // Keypad /
        0x4C => 0x58, // Keypad Enter
        0x4E => 0x56, // Keypad -
        0x4F => 0x6D, // F18
        0x50 => 0x6E, // F19
        0x51 => 0x67, // Keypad =
        0x52 => 0x62, // Keypad 0
        0x53 => 0x59, // Keypad 1
        0x54 => 0x5A, // Keypad 2
        0x55 => 0x5B, // Keypad 3
        0x56 => 0x5C, // Keypad 4
        0x57 => 0x5D, // Keypad 5
        0x58 => 0x5E, // Keypad 6
        0x59 => 0x5F, // Keypad 7
        0x5A => 0x6F, // F20
        0x5B => 0x60, // Keypad 8
        0x5C => 0x61, // Keypad 9
        0x5D => 0x89, // JIS Yen
        0x5E => 0x87, // JIS Underscore
        0x5F => 0x85, // JIS Keypad comma
        0x60 => 0x3E, // F5
        0x61 => 0x3F, // F6
        0x62 => 0x40, // F7
        0x63 => 0x3C, // F3
        0x64 => 0x41, // F8
        0x65 => 0x42, // F9
        0x66 => 0x91, // JIS Eisu
        0x67 => 0x44, // F11
        0x68 => 0x90, // JIS Kana
        0x69 => 0x68, // F13
        0x6A => 0x6B, // F16
        0x6B => 0x69, // F14
        0x6D => 0x43, // F10
        0x6E => 0x65, // Context menu
        0x6F => 0x45, // F12
        0x71 => 0x6A, // F15
        0x72 => 0x49, // Help -> Insert
        0x73 => 0x4A, // Home
        0x74 => 0x4B, // Page up
        0x75 => 0x4C, // Forward delete
        0x76 => 0x3D, // F4
        0x77 => 0x4D, // End
        0x78 => 0x3B, // F2
        0x79 => 0x4E, // Page down
        0x7A => 0x3A, // F1
        0x7B => 0x50, // Left
        0x7C => 0x4F, // Right
        0x7D => 0x51, // Down
        0x7E => 0x52, // Up
        _ => return None,
    })
}

/// US-layout mapping of a character to `(usage, shift)`.
pub fn char_to_hid(c: char) -> Option<(u8, bool)> {
    Some(match c {
        'a'..='z' => (0x04 + (c as u8 - b'a'), false),
        'A'..='Z' => (0x04 + (c as u8 - b'A'), true),
        '1'..='9' => (0x1E + (c as u8 - b'1'), false),
        '0' => (0x27, false),
        '\n' => (usage::ENTER, false),
        '\t' => (usage::TAB, false),
        ' ' => (usage::SPACE, false),
        '!' => (0x1E, true),
        '@' => (0x1F, true),
        '#' => (0x20, true),
        '$' => (0x21, true),
        '%' => (0x22, true),
        '^' => (0x23, true),
        '&' => (0x24, true),
        '*' => (0x25, true),
        '(' => (0x26, true),
        ')' => (0x27, true),
        '-' => (0x2D, false),
        '_' => (0x2D, true),
        '=' => (0x2E, false),
        '+' => (0x2E, true),
        '[' => (0x2F, false),
        '{' => (0x2F, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        _ => return None,
    })
}

/// Type `text` on the target by synthesising key presses (US layout) on a worker thread.
/// Returns the number of characters that could not be mapped.
pub fn type_text(device: DeviceHandle, text: String) -> usize {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let skipped = text.chars().filter(|&c| char_to_hid(c).is_none()).count();
    thread::spawn(move || {
        const STEP: Duration = Duration::from_millis(40);
        let shift = 1u8 << (usage::L_SHIFT - 0xE0);
        let mut last: Option<u8> = None;
        for c in text.chars() {
            let Some((u, shifted)) = char_to_hid(c) else { continue };
            if last == Some(u) {
                // Repeated key needs an explicit gap so the target sees two presses.
                thread::sleep(STEP);
            }
            let mods = if shifted { shift } else { 0 };
            device.keyboard([mods, 0, u, 0, 0, 0, 0, 0]);
            thread::sleep(STEP);
            device.keyboard([0; 8]);
            thread::sleep(STEP);
            last = Some(u);
        }
    });
    skipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollover_and_modifiers() {
        let mut k = KeyboardState::default();
        assert!(k.press(usage::L_SHIFT));
        assert!(k.press(0x04));
        assert!(!k.press(0x04));
        assert_eq!(k.report(), [0x02, 0, 0x04, 0, 0, 0, 0, 0]);
        for u in 0x05..0x0B {
            k.press(u);
        }
        // Oldest key dropped once more than six are held.
        assert_eq!(&k.report()[2..], &[0x05, 0x06, 0x07, 0x08, 0x09, 0x0A]);
        assert!(k.release(0x07));
        assert!(k.release(usage::L_SHIFT));
        assert_eq!(k.report(), [0, 0, 0x05, 0x06, 0x08, 0x09, 0x0A, 0]);
        assert!(k.clear());
        assert_eq!(k.report(), [0; 8]);
    }

    #[test]
    fn chars() {
        assert_eq!(char_to_hid('a'), Some((0x04, false)));
        assert_eq!(char_to_hid('Z'), Some((0x1D, true)));
        assert_eq!(char_to_hid('?'), Some((0x38, true)));
        assert_eq!(char_to_hid('é'), None);
    }
}
