# open-kiwi firmware

Open replacement firmware for the RP2040 in the Cytrence Kiwi KVM. It speaks the
same host protocol as the stock firmware, so both [kiwi_app](../kiwi_app) and the
vendor app can drive it.

The RP2040 handles everything except video and audio: it talks to the host over
UART0 (through the CH343 bridge), presents keyboard/mouse/serial to the target over
its native USB, and drives the side port. The MS2130 capture chip is a separate USB
device and is not touched by this firmware.

## Status

Working:

- Host session: handshake, device info/state, config, and log messages to the host.
- Target USB: keyboard, relative and absolute mouse, typing text (US layout),
  detach/attach and VID switching.
- Side port: GPIO and ATX power/reset, side UART (115200 8N1) for the terminal and
  virtual serial streams.
- Maintenance: reboot, reboot to BOOTSEL, and over-the-air updates.

Not yet: virtual disk, legacy keyboard mode, other target-USB kinds, EEPROM access
(no EEPROM is fitted on v1.1/B boards).

The firmware still reports itself as `Kiwi (v1.1/B)` so the vendor app accepts it,
labels its version `open-kiwi`, and appends an `OKIW` extension to the device info
with a capability bitmask ([src/link/info_ext.h](src/link/info_ext.h)).

## Hardware (v1.1/B)

Pin map in [board/kiwi.h](board/kiwi.h), recovered from the stock firmware:

| Signal | Pins |
|--------|------|
| Host UART0 (CH343, 2 Mbaud, flow control) | TX GP16, RX GP17, CTS GP14, RTS GP15 |
| Status LED | GP18 (PWM) |
| Side-port header | Pin-1 GP20, Pin-3 GP21, Pin-4 GP26, Pin-6 GP27 (Pins 2 and 5 are GND) |

On the side port, GP20/GP21 double as the side UART (TX/RX). For ATX control,
Pin-4 is the power switch, Pin-6 the power LED, Pin-1 the reset switch and Pin-3
the HDD LED.

## Layout

```
src/main.c     init and the main loop (watchdog-guarded, nothing blocks for long)
src/link/      host link: framing, session, opcode dispatch
src/usb/       target USB: descriptors, HID, CDC console, text typer
src/periph/    side GPIO/ATX, side UART, LED, temperature
src/update/    OTA update receiver
board/         pin map
test/          host-side tests (framing vectors are shared with kiwi_app)
tools/         mkupdate.py: wraps a build in the vendor update header
```

## Build

Needs CMake, Ninja, the Pico SDK 1.5.1 and Arm's full GNU toolchain (Homebrew's
`arm-none-eabi-gcc` lacks newlib and won't link).

```
export PICO_SDK_PATH=~/pico/pico-sdk
cmake -S . -B build -G Ninja
cmake --build build
```

| Output | What it is |
|--------|------------|
| `build/open_kiwi.kfw` | app at 0x4000 behind the stock bootloader, in the vendor update format |
| `build/open_kiwi.uf2` | standalone image at 0x10000000 (replaces the stock bootloader) |

Host-side tests (no hardware needed): `test/run_tests.sh`.

## Install

**Back up the stock firmware first.** In BOOTSEL mode:

```
picotool save -r 0x10000000 0x10100000 kiwi_stock.bin
```

Over the host link, from stock firmware or a previous open-kiwi build:

```
kiwi --update build/open_kiwi.kfw
```

Or via BOOTSEL. The RP2040's USB is on the **target** port, so move that cable to
the machine running picotool:

```
kiwi --reboot-bootsel
picotool load build/open_kiwi.uf2
picotool reboot
```

## Recovery

BOOTSEL is in ROM, so a bad flash can always be undone. Every build keeps three
software routes into BOOTSEL: `kiwi --reboot-bootsel`, a 1200-baud touch on the
target-side CDC port, or sending `!` to that port. If none of those work, bridge the
**P2** pads on the board while plugging in power.

To go back to stock:

```
picotool load -o 0x10000000 kiwi_stock.bin
picotool reboot
```
