# Open Kiwi

Open source desktop application and firmware for the Cytrence Kiwi KVM

## Desktop App

A native macOS client written in Rust ([kiwi_app](kiwi_app)). It shows the target's video and audio, sends keyboard and mouse input, and controls the side port (serial, ATX power/reset, GPIO). Works without the vendor app.

<img width="1392" height="944" alt="Screenshot 2026-11-24 at 6 47 40 PM" src="https://github.com/user-attachments/assets/4e6a89f0-6b4f-41e2-a279-609c9e1983cf" />

## Open Firmware

Replacement firmware for the RP2040 ([firmware](firmware)). It speaks the same host protocol as the stock firmware, so it works with both this app and the vendor app. It installs as an over-the-air update or via BOOTSEL, and can always be restored to stock.


## Hardware

* RP2040 - Microcontroller that acts as a usb device on the target machine
* MS2130 - USB 3.0 HDMI video and audio capture chip
* WCH CH343 - USB-to-UART bridge

The RP2040 sits on the USB 2 pins of the USBC connector behind the WCH CH343 USB-to-UART bridge while the MS2130 sits on the USB 3 pins of the USBC connector. This allows the manufacturer to skip adding a usb hub chip.

<img width="4032" height="3024" alt="IMG_2593_anonymized" src="https://github.com/user-attachments/assets/4e0bc85d-fc14-4002-abec-4101e87d46bf" />
<img width="4032" height="3024" alt="IMG_2595_anonymized" src="https://github.com/user-attachments/assets/de219b49-d49b-4b45-9fcf-879ebed6ab52" />

### Headers
* P2 - BOOTSEL jumper, bridge these to get the RP2040 into boot loader mode
* P1 - Most likely debug headers for the RP2040 (untested)
