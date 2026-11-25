// open-kiwi's KiwiInfo extension. Pure C, shared with the host-side tests.
//
// KiwiInfo is designed to grow by appending fields, so
// open-kiwi appends this block after the stock 132 bytes. Hosts that don't know
// it ignore it; the Rust client (protocol::OpenFirmware) uses it to tell open
// firmware from stock and to see which open-only features are present.
//
//   +0  char[4]  "OKIW"
//   +4  u16      extension format version (1)
//   +6  u16      extension length in bytes, this header included (12 for v1).
//                Later versions append fields; hosts skip what they don't know.
//   +8  u32      capabilities (INFO_CAP_*)
#pragma once
#include <stdint.h>
#include <string.h>

#define INFO_EXT_OFFSET  132
#define INFO_EXT_VERSION 1
#define INFO_EXT_LEN     12

#define INFO_CAP_TEXT_BATCH (1u << 0)   // 0x1A4: the firmware types text itself
#define INFO_CAP_SIDE_UART  (1u << 1)   // 0x1FE/0x1FF routed to the side UART
#define INFO_CAP_CDC_BRIDGE (1u << 2)   // 0x1FE/0x1FF routed to the target's USB serial port
#define INFO_CAP_VID_SWITCH (1u << 3)   // 0x96 kind 6
#define INFO_CAP_OTA        (1u << 4)   // receives OTA updates (runs behind the stock bootloader)

static inline void info_ext_build(uint8_t out[INFO_EXT_LEN], uint32_t caps) {
    memcpy(out, "OKIW", 4);
    out[4] = INFO_EXT_VERSION; out[5] = 0;
    out[6] = INFO_EXT_LEN;     out[7] = 0;
    for (int i = 0; i < 4; i++) out[8 + i] = (uint8_t)(caps >> (8 * i));
}
