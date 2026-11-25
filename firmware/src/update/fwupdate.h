// OTA firmware-update receiver.
//
// Implements the device side of FW_BEGIN(0x100E) → FW_ERASE(0x1013) →
// FW_DATA(0x1018)×N → FW_VERIFY(0x1022) → FW_FINISH(0x1036), staging the image
// exactly where the STOCK vendor bootloader looks for it (≥1.25 layout):
//
//   flash 0x100000  staged 0x80 header (+0xFF pad to 256) — written by FINISH
//   flash 0x110000  staged payload                         — written by DATA
//
// On the next reboot the bootloader sees a valid header at 0x100000 that
// differs from the installed record at 0x0FE000, copies `size` bytes from
// 0x110000 to the header's flash address (0x4000), writes the header to
// 0x0FE000 and erases 0x100000. The bootloader does NOT re-check the payload
// CRC, so FINISH refuses to commit unless the staged bytes match the header CRC.
//
// Only meaningful in a build linked at 0x4000 behind the vendor bootloader
// (open_kiwi_ota, KIWI_OTA=1).
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "link/opcodes.h"

// Handle a FW_* frame. Returns true if `type` was an update opcode.
bool fwupdate_on_frame(uint16_t type, const uint8_t *body, int len);

// The installed-firmware record the bootloader keeps at flash 0x0FE000.
// Returns false if there is no valid record (e.g. flashed via picotool/UF2).
// `label` receives up to 20 chars + NUL (needs 21 bytes).
bool fwupdate_installed_version(uint16_t *major, uint16_t *minor, char label[21]);

// The bootloader's own version, from the image header it carries in flash
// 0x0000..0x4000 (at 0x2C08 on the v1.1/B unit). False if none is found.
bool fwupdate_boot_version(uint16_t *major, uint16_t *minor, char label[21]);

// True between an accepted FW_BEGIN and FW_FINISH (drives the status LED).
bool fwupdate_active(void);
