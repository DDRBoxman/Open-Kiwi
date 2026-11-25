// Kiwi (v1.1/B) board pin map — recovered from the vendor firmware's
// CyfKiwiBoardDefs table (professional.bin, struct at flash 0x1001f2ac).
// See ../README.md for the side-port header.
//
// Confidence:
//   [confirmed] read from the peripheral-init call sites in the disassembly
//               (gpio_set_function with the named function number).
//   [board-data] present in the board-defs struct; role inferred.
// On this unit the flash hardware-profile sector (0xFF000) is blank, so the
// vendor firmware uses its compiled-in CyfKiwiBoardDefs table (the struct at
// professional.bin 0x1001f2ac) — i.e. these are fixed defaults, not stored per
// unit. Hard-coding them for v1.1/B is therefore faithful; a multi-board build
// would compile a small defs table keyed by variant. The ATX/side roles below
// are additionally confirmed by the vendor app's I/O Configuration dialog.

#ifndef KIWI_BOARD_H
#define KIWI_BOARD_H

// ---------------------------------------------------------------------------
// Host control link — UART0 to the WCH CH343 bridge (1a86:55d3), 2 Mbaud, 8N1,
// hardware RTS/CTS flow control.  [confirmed: gpio func 2 = UART]
#define KIWI_HOST_UART        uart0
#define KIWI_HOST_UART_TX     16   // GP16, UART0 TX
#define KIWI_HOST_UART_RX     17   // GP17, UART0 RX  (defs TX pin + 1)
#define KIWI_HOST_UART_CTS    14   // GP14, UART0 CTS
#define KIWI_HOST_UART_RTS    15   // GP15, UART0 RTS (defs CTS pin + 1)
#define KIWI_HOST_UART_BAUD   2000000
#define KIWI_HOST_UART_FLOW   1    // enable RTS/CTS

// ---------------------------------------------------------------------------
// Side port — a 6-pin header. Pins 2 and 5 are GND; the four signal pins each
// multiplex UART / plain GPIO / a dedicated ATX function. Roles below are from
// the vendor app's I/O Configuration dialog (authoritative), and the GPIO pins
// match the recovered board-defs order (side-GPIO index 0..3 = defs
// +0x14/+0x1c/+0x24/+0x2c = GP20/21/26/27). The 0x227 opcode addresses them by
// index 0..3. [confirmed]
//
//   Header   GPIO   UART        ATX function
//   Pin-1    GP20   UART1 TX     Reset Switch
//   Pin-2    GND
//   Pin-3    GP21   UART1 RX     HDD LED (input)
//   Pin-4    GP26   -            Power Switch
//   Pin-5    GND
//   Pin-6    GP27   -            Power LED (input)

// Side UART1 (target serial / Kiwi Terminal, opcode 0x1FF) — Pin-1/Pin-3.
#define KIWI_SIDE_UART        uart1
#define KIWI_SIDE_UART_TX     20   // GP20, UART1 TX  (Pin-1)
#define KIWI_SIDE_UART_RX     21   // GP21, UART1 RX  (Pin-3)

// Side-port GPIO by connector pin (0x227 index 0..3), driven as SIO by
// fn_100078b0. ATX switches are open-drain pulses to momentary front-panel
// headers; the two LED lines are inputs that sense the target's state.
#define KIWI_SIDE_PIN1        20   // idx 0 — ATX Reset Switch (shared UART TX)
#define KIWI_SIDE_PIN3        21   // idx 1 — ATX HDD LED in   (shared UART RX)
#define KIWI_SIDE_PIN4        26   // idx 2 — ATX Power Switch
#define KIWI_SIDE_PIN6        27   // idx 3 — ATX Power LED in
#define KIWI_ATX_POWER_SW     26   // GP26, pulse low to toggle target power
#define KIWI_ATX_RESET_SW     20   // GP20, pulse low to reset the target
#define KIWI_ATX_POWER_LED    27   // GP27, input: target power state
#define KIWI_ATX_HDD_LED      21   // GP21, input: target HDD activity

// ---------------------------------------------------------------------------
// Status LED — PWM brightness.  [confirmed: gpio func 4 = PWM, defs byte 0x02]
#define KIWI_LED              18   // GP18, PWM slice 1 channel A — the green LED on the board
#define KIWI_LED_PWM          1    // PWM uses brightness, not on/off

// ---------------------------------------------------------------------------
// I2C EEPROM (bus addr 0x54) — NOT fitted on v1.1/B: defs +0x4C is 0xFF, so the
// vendor firmware never sets the I2C pin functions on this board. The 0x11A8 /
// 0x11B2 opcodes exist in the Professional image but target hardware this
// variant lacks. Other variants populate defs +0x4C (SDA) and +0x4C+1 (SCL)
// with gpio func 3 (I2C1). Leave undefined here.
// #define KIWI_EEPROM_SDA    0xFF
// #define KIWI_EEPROM_SCL    0xFF
#define KIWI_HAS_EEPROM       0

// ---------------------------------------------------------------------------
// Target-facing USB is the RP2040's dedicated USB PHY (fixed pins) via TinyUSB.
// Flash: 16 MB (W25Q128), boot2 = boot2_w25q080. Hardware profile at 0xFF000.

#endif // KIWI_BOARD_H
