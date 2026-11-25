// Host-side test for src/update/fwupdate.c against a simulated 16 MB NOR flash.
//
// Drives the receiver with exactly the frame bodies `kiwi --update` sends
// (kiwi_app/src/protocol.rs `fwupdate`), then replays the STOCK bootloader's
// apply step (boot.bin fn_10000340) and checks flash 0x4000 == the payload.
//
//   test/run_tests.sh   (or: cc -std=c11 -I src -o build/fwupdate_test test/fwupdate_test.c
//                             build/fwupdate_test build/open_kiwi.kfw)

#define FWUPDATE_HOST_TEST 1
#include <assert.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include "link/link.h"

#define FLASH_SIZE (16u << 20)
static uint8_t flash[FLASH_SIZE];
static unsigned n_erase_outside, n_prog_outside;

// Writes outside the staging areas would be a safety bug.
static bool in_staging(uint32_t off) {
    return (off >= 0x100000 && off < 0x101000) || (off >= 0x110000 && off < 0x110000 + 0xFA000);
}
#define FL_XIP(off) ((const uint8_t *)(flash + (off)))
static void fl_erase_sector(uint32_t off) {
    assert(off % 0x1000 == 0);
    if (!in_staging(off)) n_erase_outside++;
    memset(flash + off, 0xFF, 0x1000);
}
static void fl_program_page(uint32_t off, const uint8_t *d) {
    assert(off % 0x100 == 0);
    if (!in_staging(off)) n_prog_outside++;
    for (int i = 0; i < 0x100; i++) flash[off + i] &= d[i];   // NOR: 1 -> 0 only
}
static uint32_t fake_image_end = 0x4000 + 40000;
static uint32_t running_image_end(void) { return fake_image_end; }
static void fl_yield(void) {}

// link.h implementation: capture logs and the last 0x1022 reply.
static char last_logs[4096];
static uint32_t verify_reply;
static bool got_verify;
void link_send(uint16_t type, const uint8_t *b, uint16_t len) {
    if (type == 0x1022 && len == 4) {
        verify_reply = b[0] | (b[1] << 8) | (b[2] << 16) | ((uint32_t)b[3] << 24);
        got_verify = true;
    }
}
void link_log(const char *fmt, ...) {
    char line[200];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    printf("    kiwi: %s\n", line);
    strncat(last_logs, line, sizeof last_logs - strlen(last_logs) - 2);
    strcat(last_logs, "\n");
}

#include "../src/update/fwupdate.c"

// ---- host side (mirrors protocol.rs fwupdate) ----
static uint8_t *img; static size_t img_len;
static uint32_t pay_size(void) { return rd32(img + 0x18); }

static void send(uint16_t t, const uint8_t *b, int n) { fwupdate_on_frame(t, b, n); }

static void host_update(int corrupt_chunk) {
    last_logs[0] = 0; got_verify = false;
    send(OP_FW_BEGIN, img + 0x10, 0x70);
    uint32_t size = pay_size();
    uint16_t sectors = (uint16_t)((size + 4095) / 4096);
    uint8_t eb[2] = {(uint8_t)sectors, (uint8_t)(sectors >> 8)};
    send(OP_FW_ERASE, eb, 2);
    for (uint32_t off = 0, idx = 0; off < size; off += 256, idx++) {
        uint8_t b[262];
        memset(b + 6, 0xFF, 256);
        uint32_t n = size - off < 256 ? size - off : 256;
        memcpy(b + 6, img + 0x80 + off, n);
        uint32_t c = crc32(b + 6, 256);
        b[0] = (uint8_t)idx; b[1] = (uint8_t)(idx >> 8);
        b[2] = (uint8_t)c; b[3] = (uint8_t)(c >> 8); b[4] = (uint8_t)(c >> 16); b[5] = (uint8_t)(c >> 24);
        if ((int)idx == corrupt_chunk) { b[6 + 17] ^= 0x40; c = crc32(b + 6, 256);   // bad data, "valid" chunk CRC
            b[2] = (uint8_t)c; b[3] = (uint8_t)(c >> 8); b[4] = (uint8_t)(c >> 16); b[5] = (uint8_t)(c >> 24); }
        send(OP_FW_DATA, b, sizeof b);
    }
    uint8_t vb[4] = {(uint8_t)size, (uint8_t)(size >> 8), (uint8_t)(size >> 16), (uint8_t)(size >> 24)};
    send(OP_FW_VERIFY, vb, 4);
    send(OP_FW_FINISH, img, 0x80);
}

// ---- stock bootloader apply (boot.bin fn_10000340, ≥1.25 layout) ----
static bool hdr_magic_ok(const uint8_t *h) { return !memcmp(h, MAGIC, 16) && !memcmp(h + 0x70, TAIL, 16); }
static bool bootloader_apply(void) {
    const uint8_t *st = flash + 0x100000, *in = flash + 0x0FE000;
    if (!hdr_magic_ok(st)) return false;                       // "No new firmware found"
    if (hdr_magic_ok(in) && rd32(st + 0x18) == rd32(in + 0x18) && rd32(st + 0x36) == rd32(in + 0x36) &&
        rd32(st + 0x1C) == rd32(in + 0x1C) && !memcmp(st + 0x20, in + 0x20, 0x15) &&
        rd32(st + 0x10) == rd32(in + 0x10) && rd16(st + 0x14) == rd16(in + 0x14) &&
        rd16(st + 0x16) == rd16(in + 0x16) && st[0x35] == in[0x35])
        return false;                                          // identical -> skip
    memset(flash + 0x0FE000, 0xFF, 0x1000);
    uint32_t dst = rd32(st + 0x36) - 0x10000000, size = rd32(st + 0x18);
    for (uint32_t o = 0; o < size; o += 0x1000) {
        memset(flash + dst + o, 0xFF, 0x1000);
        memcpy(flash + dst + o, flash + 0x110000 + o, 0x1000);
    }
    memcpy(flash + 0x0FE000, st, 0x100);
    memset(flash + 0x100000, 0xFF, 0x1000);
    return true;
}

static int failures;
#define CHECK(cond, msg) do { if (cond) printf("  ok   %s\n", msg); else { printf("  FAIL %s\n", msg); failures++; } } while (0)

static void reset_flash(void) {
    memset(flash, 0xFF, sizeof flash);
    memset(flash + 0x4000, 0xA5, 40000);                       // "old app"
    memset(flash + 0x0FF000, 0x3C, 0x100);                     // hw profile sentinel
    n_erase_outside = n_prog_outside = 0;
    memset(&up, 0, sizeof up);
}

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s image.kfw\n", argv[0]); return 2; }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror(argv[1]); return 2; }
    img = malloc(1 << 21);
    img_len = fread(img, 1, 1 << 21, f);
    fclose(f);
    printf("image %s: %zu bytes, v%u.%u, payload %u, crc %08X\n", argv[1], img_len,
           rd16(img + 0x14), rd16(img + 0x16), pay_size(), rd32(img + 0x1C));

    printf("[1] full update, then bootloader apply\n");
    reset_flash();
    host_update(-1);
    CHECK(strstr(last_logs, "to update to"), "BEGIN acked with 'to update to'");
    CHECK(got_verify && verify_reply == rd32(img + 0x1C), "VERIFY reply == image CRC");
    CHECK(hdr_magic_ok(flash + 0x100000), "FINISH staged header at 0x100000");
    CHECK(!n_erase_outside && !n_prog_outside, "no flash writes outside staging");
    CHECK(bootloader_apply(), "bootloader applies the staged image");
    CHECK(!memcmp(flash + 0x4000, img + 0x80, pay_size()), "flash 0x4000 == payload");
    uint16_t ma, mi; char lab[21];
    CHECK(fwupdate_installed_version(&ma, &mi, lab) && ma == rd16(img + 0x14) && mi == rd16(img + 0x16),
          "installed record reports the new version");
    CHECK(flash[0x0FF000] == 0x3C, "hardware profile sector untouched");
    CHECK(!bootloader_apply(), "second boot: nothing staged");

    printf("[2] same image again -> bootloader skips (identical)\n");
    host_update(-1);
    CHECK(strstr(last_logs, "identical"), "FINISH warns identical");
    CHECK(!bootloader_apply(), "bootloader skips identical image");

    printf("[3] corrupted chunk (data flipped, chunk CRC recomputed) -> no commit\n");
    reset_flash();
    host_update(3);
    CHECK(got_verify && verify_reply != rd32(img + 0x1C), "VERIFY reply exposes mismatch");
    CHECK(strstr(last_logs, "not committing"), "FINISH refuses");
    CHECK(!hdr_magic_ok(flash + 0x100000), "no staged header");
    CHECK(!bootloader_apply() && flash[0x4000] == 0xA5, "old app still in place");

    printf("[4] stale staged header from an earlier run is cleared by BEGIN\n");
    reset_flash();
    host_update(-1);                                           // leaves a valid staged header
    send(OP_FW_BEGIN, img + 0x10, 0x70);                       // new attempt, host dies after BEGIN
    CHECK(!hdr_magic_ok(flash + 0x100000), "staged header erased on BEGIN");

    printf("[5] header rejections\n");
    uint8_t bad[0x80];
    memcpy(bad, img, 0x80); bad[0x14] = 1; bad[0x15] = 0; bad[0x16] = 24; bad[0x17] = 0;
    last_logs[0] = 0; send(OP_FW_BEGIN, bad + 0x10, 0x70);
    CHECK(strstr(last_logs, "downgrade") && !up.active, "v1.24 refused (downgrade)");
    memcpy(bad, img, 0x80); bad[0x36] = 0; bad[0x37] = 0; bad[0x38] = 0; bad[0x39] = 0x10;
    last_logs[0] = 0; send(OP_FW_BEGIN, bad + 0x10, 0x70);
    CHECK(strstr(last_logs, "not valid") && !up.active, "bootloader-address image refused");
    memcpy(bad, img, 0x80); bad[0x35] = 0;
    last_logs[0] = 0; send(OP_FW_BEGIN, bad + 0x10, 0x70);
    CHECK(strstr(last_logs, "not valid") && !up.active, "sku 0 (bootloader) refused");
    memcpy(bad, img, 0x80); bad[0x18] = 0; bad[0x19] = 0; bad[0x1A] = 0x10; bad[0x1B] = 0;
    last_logs[0] = 0; send(OP_FW_BEGIN, bad + 0x10, 0x70);
    CHECK(strstr(last_logs, "not valid") && !up.active, "oversized payload refused");
    fake_image_end = 0x100100;
    last_logs[0] = 0; send(OP_FW_BEGIN, img + 0x10, 0x70);
    CHECK(strstr(last_logs, "overlaps") && !up.active, "refused when running image overlaps staging");
    fake_image_end = 0x4000 + 40000;

    printf("[6] DATA out of range / without BEGIN\n");
    reset_flash();
    uint8_t d[262] = {0};
    last_logs[0] = 0; uint32_t c = crc32(d + 6, 256);
    d[2] = (uint8_t)c; d[3] = (uint8_t)(c >> 8); d[4] = (uint8_t)(c >> 16); d[5] = (uint8_t)(c >> 24);
    send(OP_FW_DATA, d, sizeof d);
    CHECK(strstr(last_logs, "not valid") && !n_prog_outside, "DATA without BEGIN refused");
    send(OP_FW_BEGIN, img + 0x10, 0x70);
    uint16_t sectors = (uint16_t)((pay_size() + 4095) / 4096);
    uint8_t eb[2] = {(uint8_t)sectors, (uint8_t)(sectors >> 8)};
    send(OP_FW_ERASE, eb, 2);
    d[0] = 0xFF; d[1] = 0xFF; last_logs[0] = 0;
    send(OP_FW_DATA, d, sizeof d);
    CHECK(strstr(last_logs, "out of range") && !n_prog_outside, "DATA idx past erased area refused");

    printf("[7] bootloader version from the header it carries (0x2C08 on v1.1/B)\n");
    reset_flash();
    uint16_t bmaj = 0, bmin = 0;
    char blabel[21];
    CHECK(!fwupdate_boot_version(&bmaj, &bmin, blabel), "no bootloader header -> false");
    memcpy(flash + 0x2C08, img, 0x80);
    CHECK(fwupdate_boot_version(&bmaj, &bmin, blabel) && bmaj == rd16(img + 0x14) &&
          bmin == rd16(img + 0x16) && memcmp(blabel, img + 0x20, 20) == 0, "found at 0x2C08");
    CHECK(fwupdate_active() == false, "no update in progress");

    printf(failures ? "\n%d FAILED\n" : "\nall passed\n", failures);
    return failures ? 1 : 0;
}
