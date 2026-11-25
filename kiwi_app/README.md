# kiwi

A macOS client for the Cytrence Kiwi USB KVM, written in Rust with
[gpui](https://crates.io/crates/gpui). It shows the target machine's screen and
sound in a window and sends your keyboard and mouse to it, talking to the Kiwi
directly without the vendor app.

- Video up to 1920×1080 at 60 fps, plus audio, with capture-chip picture settings
  and EDID presets.
- Keyboard capture by physical key position (Ctrl+⌘ releases), Ctrl+Alt+Del, and
  paste-as-keystrokes.
- Relative or absolute mouse, with adjustable speed.
- Side port: serial terminal, ATX power/reset, and GPIO.
- Command-line tools for probing the device, typing, firmware updates and more
  (`kiwi --help`).

Works with both the stock firmware and [open-kiwi](../firmware).

## Running

```
cargo run --release
```

Quit the vendor's Cytrence Kiwi app first, since only one app can hold the Kiwi's
control port. macOS asks for camera and microphone access for the Kiwi's video and
audio.
