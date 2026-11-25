// Host-link opcodes. → host-to-Kiwi, ← Kiwi-to-host.
#pragma once

#define OP_DISCONNECT     0x14    // →  end of session, empty body
#define OP_CONNECT        0x32    // →  session open, u32 frame-delay factor
#define OP_KIWIINFO       0x64    // ←  device identity, 132 bytes
#define OP_KIWISTATE      0x65    // ←  status, 12 bytes
#define OP_KIWICONFIG     0x78    // →  features & logging, 20 bytes
#define OP_TARGET_USB     0x96    // →  target USB config [kind, value]
#define OP_QUERY          0xC8    // →  u16: 1 = KiwiInfo, 2 = KiwiState
#define OP_LOG            0x12C   // ←  u16 level + NUL-terminated text (→: debug tab, ignored)
#define OP_KEYBOARD       0x190   // →  8-byte boot-keyboard report
#define OP_MOUSE_REL      0x19A   // →  [buttons, dx, dy, wheel, pan]
#define OP_MOUSE_ABS      0x19B   // →  [buttons, x:u16, y:u16, wheel, pan]
#define OP_TEXT           0x1A4   // →  raw text for the Kiwi to type
#define OP_VCOMM          0x1FE   // ←→ virtual-serial-port stream, routed by KiwiConfig.vcomm
#define OP_TERMINAL       0x1FF   // ←→ terminal stream, routed by KiwiConfig.vterm
#define OP_SIDE_GPIO      0x227   // →  side-port GPIO setup, 8 bytes
#define OP_REBOOT         0xBB8   // →  normal reboot (the bootloader applies staged firmware)
#define OP_REBOOT_BOOTSEL 0xBC2   // →  reboot into the RP2040 USB bootloader

#define OP_FW_BEGIN  0x100E
#define OP_FW_ERASE  0x1013
#define OP_FW_DATA   0x1018
#define OP_FW_VERIFY 0x1022
#define OP_FW_FINISH 0x1036

// 0x12C log levels. The host's KiwiConfig sets the lowest level sent (default
// 2). The vendor firmware sends its normal messages at 2.
#define LOG_DEBUG 1
#define LOG_INFO  2
#define LOG_WARN  3
