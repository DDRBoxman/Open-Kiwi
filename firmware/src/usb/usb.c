// Target-facing USB: HID report queue, text typing (0x1A4), target USB config
// (0x96), and the CDC port (recovery console, or a serial bridge when
// KiwiConfig routes vcomm/vterm to it).
#include <string.h>
#include "pico/stdlib.h"
#include "pico/bootrom.h"
#include "tusb.h"
#include "link/link.h"
#include "periph/periph.h"
#include "usb.h"
#include "reports.h"
#include "text.h"

// Target USB identities, as in the stock firmware (fn_1000bc54 picks the VID).
#define VID_CYTRENCE 0x39C9
#define VID_LEGACY   0xCAFE
#define PID_KIWI     0x400F

// Time between text-batch reports (a press and its release per character), so
// ~50 characters/s. Slow enough for BIOS screens and VMs.
#define TEXT_REPORT_MS 10
// Gap between dropping off the bus and re-attaching after a 0x96 change.
#define REATTACH_MS 250

// ---------------- HID report queue ----------------
typedef struct { uint8_t rid, len, data[8]; } hidrep_t;
#define HQ_SIZE 64
static hidrep_t hq[HQ_SIZE];
static uint8_t hq_head, hq_tail;
static uint32_t hq_dropped;
static uint8_t last_abs[7];   // to release abs buttons without moving the pointer

static bool hq_empty(void) { return hq_head == hq_tail; }

void usb_hid_push(uint8_t rid, const uint8_t *d, uint8_t len) {
    if (len > sizeof hq[0].data) return;
    uint8_t n = (uint8_t)((hq_head + 1) % HQ_SIZE);
    if (n == hq_tail) { hq_dropped++; return; }
    hq[hq_head].rid = rid;
    hq[hq_head].len = len;
    memcpy(hq[hq_head].data, d, len);
    hq_head = n;
    if (rid == RID_ABS) memcpy(last_abs, d, sizeof last_abs);
}

void usb_hid_release_all(void) {
    static const uint8_t zero[8] = {0};
    usb_hid_push(RID_KB, zero, 8);
    usb_hid_push(RID_MOUSE, zero, 5);
    if (last_abs[0]) {
        uint8_t a[7];
        memcpy(a, last_abs, sizeof a);
        a[0] = 0;
        a[5] = a[6] = 0;
        usb_hid_push(RID_ABS, a, sizeof a);
    }
}

static void hq_drain(void) {
    if (hq_empty() || !tud_hid_ready()) return;
    hidrep_t *e = &hq[hq_tail];
    if (tud_hid_report(e->rid, e->data, e->len)) hq_tail = (uint8_t)((hq_tail + 1) % HQ_SIZE);
}

// ---------------- text batch (0x1A4) ----------------
static text_typer_t typer;
static uint32_t next_text_ms;

void usb_text_push(const uint8_t *b, uint16_t len) {
    uint16_t n = text_push(&typer, b, len);
    if (n < len) link_log("Text batch queue is full, dropped %u bytes", (unsigned)(len - n));
}

// Typed text waits for live input already queued, so it never reorders it.
static void text_task(uint32_t now) {
    if (!text_busy(&typer) || !hq_empty() || (int32_t)(now - next_text_ms) < 0) return;
    uint8_t r[8];
    if (text_next_report(&typer, r)) {
        usb_hid_push(RID_KB, r, 8);
        next_text_ms = now + TEXT_REPORT_MS;
    }
    if (!text_busy(&typer) && typer.skipped) {
        link_log("Text batch: skipped %u bytes with no US-layout key", (unsigned)typer.skipped);
        typer.skipped = 0;
    }
}

// ---------------- target USB config (0x96) ----------------
static bool legacy_vid;
static bool attached = true;
static uint32_t reattach_at;   // 0 = none pending

bool usb_uses_legacy_vid(void) { return legacy_vid; }
uint16_t usb_vid(void) { return legacy_vid ? VID_LEGACY : VID_CYTRENCE; }
uint16_t usb_pid(void) { return PID_KIWI; }
// The CDC interface is always present: it's this firmware's recovery console.
uint32_t usb_features(void) { return 1u << 1; }

static void detach(void) {
    tud_disconnect();
    attached = false;
}

static void attach(void) {
    tud_connect();
    attached = true;
    reattach_at = 0;
}

// The host sends [0, 0], waits 1 s, then [kind, value]; the device then
// re-attaches with the new setting.
void usb_target_config(const uint8_t *b, uint16_t len) {
    if (len < 2) { link_log("Msg/TargetUsb: no enough data"); return; }
    uint8_t kind = b[0], value = b[1];
    uint32_t now = to_ms_since_boot(get_absolute_time());
    bool reenumerate = false;
    switch (kind) {
    case 0:
        if (value) { attach(); link_log("Msg: enabled usb connection"); }
        else { detach(); reattach_at = 0; link_log("Msg: disabled usb connection"); }
        return;
    case 1:
        if (value) link_log("Msg: target mass storage is not supported by open-kiwi yet");
        break;
    case 2:
        if (!value) link_log("Msg: the target serial port stays on in open-kiwi (recovery console)");
        break;
    case 3: case 4: case 5:
        link_log("Msg: target USB setting %u is not supported by open-kiwi", kind);
        break;
    case 6:
        legacy_vid = value != 0;
        link_log("Msg: target USB VID %04X:%04X", usb_vid(), usb_pid());
        reenumerate = true;
        break;
    default:
        link_log("Msg/TargetUsb: unknown setting %u", kind);
        return;
    }
    if (reenumerate && attached) detach();
    if (!attached) reattach_at = now + REATTACH_MS;   // re-attach, as stock does
}

static void reattach_task(uint32_t now) {
    if (reattach_at && (int32_t)(now - reattach_at) >= 0) {
        attach();
        link_log("Msg: device configured and enabled usb connection");
    }
}

// TinyUSB callbacks, logged with the stock firmware's wording.
void tud_mount_cb(void)   { link_log("USB: was mounted"); }
void tud_umount_cb(void)  { link_log("USB: was un-mounted"); }
void tud_suspend_cb(bool remote_wakeup_en) { (void)remote_wakeup_en; link_log("USB: was suspended"); }
void tud_resume_cb(void)  { link_log("USB: was resumed"); }

// ---------------- CDC: recovery console / serial bridge ----------------
// BOOTSEL safety trigger (see ../../README.md, Recovery): a 1200-baud touch always
// works; sending '!' works while the CDC isn't bridged to the host.
static volatile bool want_bootsel;

void tud_cdc_line_coding_cb(uint8_t itf, cdc_line_coding_t const *coding) {
    (void)itf;
    if (coding->bit_rate == 1200) want_bootsel = true;
}

bool usb_cdc_ready(void) { return tud_cdc_connected(); }

uint32_t usb_cdc_write(const uint8_t *b, uint32_t n) {
    uint32_t w = tud_cdc_write(b, n);
    tud_cdc_write_flush();
    return w;
}

static void cdc_task(void) {
    static bool banner;
    bool bridged = vport_cdc_stream() != 0;
    if (tud_cdc_available()) {
        uint8_t buf[64];
        uint32_t n = tud_cdc_read(buf, sizeof buf);
        if (bridged) vport_from_cdc(buf, n);
        else for (uint32_t i = 0; i < n; i++) if (buf[i] == '!') want_bootsel = true;
    }
    if (want_bootsel) {
        if (!bridged) usb_cdc_write((const uint8_t *)"\nopen-kiwi: rebooting to BOOTSEL...\n", 36);
        sleep_ms(50);
        reset_usb_boot(0, 0);
    }
    if (!tud_cdc_connected()) { banner = false; return; }
    if (!banner && !bridged) {
        static const char msg[] = "open-kiwi: host link on UART0. '!' or a 1200-baud touch -> BOOTSEL.\n";
        usb_cdc_write((const uint8_t *)msg, sizeof msg - 1);
    }
    banner = true;
}

// ---------------- main-loop hooks ----------------
void usb_init(void) {
    text_init(&typer);
}

void usb_task(void) {
    uint32_t now = to_ms_since_boot(get_absolute_time());
    reattach_task(now);
    text_task(now);
    hq_drain();
    cdc_task();
    if (hq_dropped) {
        link_log("Keyboard/Mouse event queue is full, dropped %u", (unsigned)hq_dropped);
        hq_dropped = 0;
    }
}
