// OTA firmware-update receiver — see fwupdate.h for the flash layout and the
// contract with the stock bootloader (recovered from boot.bin, fn_10000340).
//
// Safety properties:
//  * Writes only ever land in the staging header sector (0x100000) or the
//    staging payload area (0x110000 .. +APP_MAX). The running app (0x4000),
//    the installed record (0x0FE000) and the hardware profile (0x0FF000) are
//    never touched; the bootloader does the actual install on reboot.
//  * BEGIN erases any stale staged header first, so a half-written transfer
//    can never be applied.
//  * FINISH re-CRCs the whole staged payload from flash and only writes the
//    header (the "apply me" marker) if it matches the image CRC. The
//    bootloader trusts the header, so this is the one integrity gate.

#include <string.h>
#include "fwupdate.h"
#include "link/link.h"

#ifndef FWUPDATE_HOST_TEST
#include "hardware/flash.h"
#include "hardware/sync.h"
#include "hardware/regs/addressmap.h"
#include "hardware/watchdog.h"

#define FL_XIP(off) ((const uint8_t *)(XIP_BASE + (off)))

// The app runs from XIP flash, so flash ops must run with interrupts off
// (core 1 is unused). Host-link RX is held off by UART hardware flow control.
static void fl_erase_sector(uint32_t off) {
    uint32_t s = save_and_disable_interrupts();
    flash_range_erase(off, FLASH_SECTOR_SIZE);
    restore_interrupts(s);
}
static void fl_program_page(uint32_t off, const uint8_t *data) {
    uint32_t s = save_and_disable_interrupts();
    flash_range_program(off, data, FLASH_PAGE_SIZE);
    restore_interrupts(s);
}
// A full erase is up to 250 sectors (~11 s), longer than the watchdog timeout.
static void fl_yield(void) { watchdog_update(); }
extern char __flash_binary_end;
static uint32_t running_image_end(void) {
    return (uint32_t)(uintptr_t)&__flash_binary_end - XIP_BASE;
}
#endif
// (FWUPDATE_HOST_TEST: the includer provides FL_XIP, fl_erase_sector,
//  fl_program_page, fl_yield and running_image_end over a simulated flash.)

#define SECTOR     0x1000u
#define PAGE       0x100u
#define CHUNK      256u
#define HDR_LEN    0x80u
#define INFO_LEN   0x70u        // FW_BEGIN body = header[0x10..0x80]

#define STAGE_HDR  0x100000u    // staged header: the bootloader's "apply me" marker
#define STAGE_DATA 0x110000u    // staged payload
#define INSTALLED  0x0FE000u    // installed-image record (bootloader-owned)
#define APP_OFFS   0x004000u
#define APP_ADDR   (0x10000000u + APP_OFFS)
#define APP_MAX    (INSTALLED - APP_OFFS)   // an app must not reach 0x0FE000

static const uint8_t MAGIC[16] = {0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87,
                                  0xef, 0xcd, 0xab, 0x90, 0x09, 0xba, 0xdc, 0xfe};
static const uint8_t TAIL[16]  = {0xdf, 0x9b, 0x57, 0x13, 0x31, 0x75, 0xb9, 0xfd,
                                  0xce, 0x8a, 0x46, 0x02, 0x20, 0x64, 0xa8, 0xec};

static uint16_t rd16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }
static uint32_t rd32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

// Standard reflected CRC-32 (zlib/IEEE), as used by the image header.
static uint32_t crc32(const uint8_t *p, uint32_t n) {
    uint32_t c = 0xFFFFFFFFu;
    while (n--) {
        c ^= *p++;
        for (int b = 0; b < 8; b++) c = (c >> 1) ^ (0xEDB88320u & (0u - (c & 1u)));
    }
    return ~c;
}

typedef struct {
    uint16_t major, minor;
    uint32_t size, crc, addr;
    uint8_t sku;
} hdr_t;

static bool hdr_parse(const uint8_t *h, hdr_t *o, const char **why) {
    if (memcmp(h, MAGIC, 16) != 0)          { *why = "bad magic"; return false; }
    if (memcmp(h + 0x70, TAIL, 16) != 0)    { *why = "bad header tail"; return false; }
    if (rd32(h + 0x10) != 1)                { *why = "unknown header format"; return false; }
    o->major = rd16(h + 0x14);
    o->minor = rd16(h + 0x16);
    o->size  = rd32(h + 0x18);
    o->crc   = rd32(h + 0x1C);
    o->sku   = h[0x35];
    o->addr  = rd32(h + 0x36);
    if (o->sku != 1 && o->sku != 2)         { *why = "not an application image"; return false; }
    if (o->addr != APP_ADDR)                { *why = "flash address is not 0x10004000"; return false; }
    if (o->size == 0 || o->size > APP_MAX)  { *why = "payload size out of range"; return false; }
    return true;
}

static bool is_erased(const uint8_t *p, uint32_t n) {
    while (n--) if (*p++ != 0xFF) return false;
    return true;
}

static struct {
    bool active;              // BEGIN accepted, not yet finished
    uint8_t hdr[HDR_LEN];     // full header reconstructed from BEGIN
    hdr_t h;
    uint32_t erased;          // bytes of staging erased by ERASE (0 = none)
} up;

static void on_begin(const uint8_t *b, int len) {
    if (len < (int)INFO_LEN) { link_log("FwUpdateBegin: no enough data"); return; }
    memset(&up, 0, sizeof up);
    memcpy(up.hdr, MAGIC, 16);
    memcpy(up.hdr + 16, b, INFO_LEN);

    hdr_t h;
    const char *why = "";
    if (!hdr_parse(up.hdr, &h, &why)) { link_log("FwUpdateBegin: image not valid: %s", why); return; }
    // Same guard as the vendor firmware: ≤1.24 images use the legacy staging
    // layout (0x200000) that this bootloader generation doesn't read.
    if (h.major < 1 || (h.major == 1 && h.minor <= 24)) {
        link_log("FwUpdateBegin: Block attempting downgrade to incompatible version (1.24-)");
        return;
    }
    if (running_image_end() > STAGE_HDR) {
        link_log("FwUpdateBegin: not valid: running image overlaps the staging area");
        return;
    }
    fl_erase_sector(STAGE_HDR);   // no stale "apply me" marker while data changes
    up.h = h;
    up.active = true;
    link_log("FwUpdateBegin: to update to %u.%u", h.major, h.minor);
}

static void on_erase(const uint8_t *b, int len) {
    if (!up.active) { link_log("FwErase: not valid: no update in progress"); return; }
    if (len < 2)    { link_log("FwErase: no enough data"); return; }
    uint32_t n = rd16(b);
    uint32_t need = (up.h.size + SECTOR - 1) / SECTOR;
    if (n < need || n * SECTOR > APP_MAX) {
        link_log("FwErase: not valid: %u sectors (image needs %u)", (unsigned)n, (unsigned)need);
        return;
    }
    for (uint32_t i = 0; i < n; i++) {
        fl_erase_sector(STAGE_DATA + i * SECTOR);
        fl_yield();
    }
    up.erased = n * SECTOR;
    link_log("FwErase: %u sectors erased", (unsigned)n);
}

static void on_data(const uint8_t *b, int len) {
    if (len < (int)(6 + CHUNK)) {
        link_log("Payload size (%u) is not match to flash data", (unsigned)len);
        return;
    }
    if (!up.active || !up.erased) { link_log("FwData: not valid: staging not erased"); return; }
    uint16_t idx = rd16(b);
    uint32_t want = rd32(b + 2);
    const uint8_t *chunk = b + 6;
    uint32_t got = crc32(chunk, CHUNK);
    if (got != want) {
        link_log("Wrong CRC in received firmware data, idx %u, crc 0x%X, expected crc 0x%X",
                 (unsigned)idx, (unsigned)got, (unsigned)want);
        return;
    }
    uint32_t off = (uint32_t)idx * CHUNK;
    if (off + CHUNK > up.erased) { link_log("FwData: not valid: idx %u out of range", (unsigned)idx); return; }

    const uint8_t *dst = FL_XIP(STAGE_DATA + off);
    if (memcmp(dst, chunk, CHUNK) == 0) return;            // resent chunk, already there
    if (!is_erased(dst, CHUNK)) {
        link_log("FwData: idx %u already written with different data", (unsigned)idx);
        return;                                            // FINISH's CRC check will refuse
    }
    fl_program_page(STAGE_DATA + off, chunk);
    if (memcmp(dst, chunk, CHUNK) != 0) link_log("FwData: idx %u readback mismatch", (unsigned)idx);
}

// Like the vendor: CRC-32 of the first `len` staged bytes, replied on 0x1022.
static void on_verify(const uint8_t *b, int len) {
    if (len < 4) { link_log("Msg/FwCrc: no enough data"); return; }
    uint32_t n = rd32(b);
    if (n > APP_MAX) n = APP_MAX;
    uint32_t c = crc32(FL_XIP(STAGE_DATA), n);
    uint8_t r[4] = {(uint8_t)c, (uint8_t)(c >> 8), (uint8_t)(c >> 16), (uint8_t)(c >> 24)};
    link_send(OP_FW_VERIFY, r, sizeof r);
}

static void on_finish(const uint8_t *b, int len) {
    if (len < (int)HDR_LEN)         { link_log("FwUpdateFinish: no enough data"); return; }
    if (!up.active || !up.erased)   { link_log("FwUpdateFinish: not valid: no update in progress"); return; }
    if (memcmp(b, up.hdr, HDR_LEN) != 0) {
        link_log("FwUpdateFinish: not valid: header differs from FwUpdateBegin, not committing");
        return;
    }
    uint32_t c = crc32(FL_XIP(STAGE_DATA), up.h.size);
    if (c != up.h.crc) {
        link_log("FwUpdateFinish: staged CRC 0x%08X != image CRC 0x%08X, not committing",
                 (unsigned)c, (unsigned)up.h.crc);
        return;
    }
    uint8_t page[PAGE];
    memset(page, 0xFF, sizeof page);
    memcpy(page, b, HDR_LEN);
    fl_erase_sector(STAGE_HDR);
    fl_program_page(STAGE_HDR, page);
    if (memcmp(FL_XIP(STAGE_HDR), page, PAGE) != 0) {
        fl_erase_sector(STAGE_HDR);
        link_log("FwUpdateFinish: header readback mismatch, not committing");
        return;
    }
    up.active = false;
    link_log("FwUpdateFinish: %u.%u staged (%u bytes, crc 0x%08X), reboot to apply",
             up.h.major, up.h.minor, (unsigned)up.h.size, (unsigned)up.h.crc);
    // The bootloader skips a staged image identical to the installed one.
    if (memcmp(FL_XIP(INSTALLED), page, HDR_LEN) == 0)
        link_log("FwUpdateFinish: note: identical to the installed image, bootloader will skip it");
}

bool fwupdate_on_frame(uint16_t type, const uint8_t *body, int len) {
    switch (type) {
    case OP_FW_BEGIN:  on_begin(body, len);  return true;
    case OP_FW_ERASE:  on_erase(body, len);  return true;
    case OP_FW_DATA:   on_data(body, len);   return true;
    case OP_FW_VERIFY: on_verify(body, len); return true;
    case OP_FW_FINISH: on_finish(body, len); return true;
    default: return false;
    }
}

static bool read_version(const uint8_t *h, uint16_t *major, uint16_t *minor, char label[21]) {
    if (memcmp(h, MAGIC, 16) != 0 || memcmp(h + 0x70, TAIL, 16) != 0) return false;
    *major = rd16(h + 0x14);
    *minor = rd16(h + 0x16);
    memcpy(label, h + 0x20, 20);
    label[20] = 0;
    return true;
}

bool fwupdate_installed_version(uint16_t *major, uint16_t *minor, char label[21]) {
    return read_version(FL_XIP(INSTALLED), major, minor, label);
}

bool fwupdate_boot_version(uint16_t *major, uint16_t *minor, char label[21]) {
    for (uint32_t off = 0; off + HDR_LEN <= APP_OFFS; off += 4)
        if (read_version(FL_XIP(off), major, minor, label)) return true;
    return false;
}

bool fwupdate_active(void) { return up.active; }
