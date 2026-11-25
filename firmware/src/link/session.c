// Session state: handshake (0x32/0x14), identity and status replies to 0xC8
// (KiwiInfo 0x64, KiwiState 0x65), and KiwiConfig (0x78).
#include <string.h>
#include "pico/stdlib.h"
#include "pico/unique_id.h"
#include "link.h"
#include "info_ext.h"
#include "usb/usb.h"
#include "periph/periph.h"
#include "update/fwupdate.h"

#ifndef OPEN_KIWI_FW_MAJOR
#define OPEN_KIWI_FW_MAJOR 2
#define OPEN_KIWI_FW_MINOR 12
#endif

#define INFO_LEN (INFO_EXT_OFFSET + INFO_EXT_LEN)   // stock 132 bytes + open-kiwi extension
#define CONFIG_LEN 20

static uint8_t info[INFO_LEN];
static uint8_t config[CONFIG_LEN];
static bool active;
static bool report_watchdog;

static void put16(uint8_t *p, uint16_t v) { p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8); }
static void put32(uint8_t *p, uint32_t v) { for (int i = 0; i < 4; i++) p[i] = (uint8_t)(v >> (8 * i)); }
static uint16_t rd16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }
static uint32_t rd32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

// The firmware's power-on KiwiConfig.
static const uint8_t CONFIG_DEFAULT[CONFIG_LEN] = {
    0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xc8, 0x00, 0x00, 0x00, 0x30, 0x00,
};

static void info_init(void) {
    memset(info, 0, sizeof info);
    put16(info + 0, 1);                       // protocol 1.4, as the stock firmware
    put16(info + 2, 4);
    pico_unique_board_id_t id;                // the flash chip's 64-bit unique ID
    pico_get_unique_board_id(&id);
    memcpy(info + 4, id.id, 8);
    strncpy((char *)info + 12, "Kiwi (v1.1/B)", 31);

    uint16_t major = OPEN_KIWI_FW_MAJOR, minor = OPEN_KIWI_FW_MINOR;
    char label[21] = "open-kiwi";
    uint16_t bmajor = 0, bminor = 0;
    char blabel[21] = "none";
#ifdef KIWI_OTA
    // Behind the stock bootloader: report what it installed, and its own version.
    fwupdate_installed_version(&major, &minor, label);
    fwupdate_boot_version(&bmajor, &bminor, blabel);
#endif
    // The vendor app reads the firmware version at 101 (it gates features on
    // it and shows it in About) and the bootloader's at 76.
    put16(info + 76, bmajor);
    put16(info + 78, bminor);
    memcpy(info + 80, blabel, 21);
    put16(info + 101, major);
    put16(info + 103, minor);
    memcpy(info + 105, label, 21);
    info[126] = 2;                            // sku: Professional
    info[131] = 6;                            // side-port GPIO count

    uint32_t caps = INFO_CAP_TEXT_BATCH | INFO_CAP_SIDE_UART | INFO_CAP_CDC_BRIDGE | INFO_CAP_VID_SWITCH;
#ifdef KIWI_OTA
    caps |= INFO_CAP_OTA;
#endif
    info_ext_build(info + INFO_EXT_OFFSET, caps);
}

void session_init(bool watchdog_reset) {
    memcpy(config, CONFIG_DEFAULT, sizeof config);
    info_init();
    report_watchdog = watchdog_reset;
}

bool session_active(void) { return active; }
uint16_t session_log_level(void) { return rd16(config); }

// 0x32: the u32 is the host's frame-pacing delay. The stock firmware clamps it
// to 10000 and paces its own output by it; this firmware doesn't pace.
void session_connect(const uint8_t *b, uint16_t len) {
    active = true;
    if (len >= 4) {
        uint32_t v = rd32(b);
        if (v > 10000) link_log("Msg: the value of CommTxFrameDelay is out of the range: %u", (unsigned)v);
        else link_log("Msg: CommTxFrameRateFactor set to %u", (unsigned)v);
    }
    if (report_watchdog) {
        link_logl(LOG_WARN, "open-kiwi: the last reset was caused by the watchdog");
        report_watchdog = false;
    }
}

// 0x14: release anything held on the target, so a closed session can't leave
// a key or button stuck down. Typing from 0x1A4 carries on.
void session_disconnect(void) {
    active = false;
    usb_hid_release_all();
    link_log("Host port was closed");
}

static void send_info(void) {
    // byte 44 bit 5: target USB uses the Cytrence VID; u32 at 127: features
    // (bit 0 mass storage, bit 1 CDC serial).
    put32(info + 44, usb_uses_legacy_vid() ? 0 : 0x20);
    put32(info + 127, usb_features());
    link_send(OP_KIWIINFO, info, sizeof info);
}

static void send_state(void) {
    // states(u64) | IO | temperature | shared USB/SD | netlink
    uint8_t s[12] = {0};
    if (active) s[0] |= 0x01;
    s[8] = side_gpio_levels();
    s[9] = temp_read_c();
    link_send(OP_KIWISTATE, s, sizeof s);
}

// 0xC8: an empty body is a ping and defaults to KiwiInfo, as on stock.
void session_query(const uint8_t *b, uint16_t len) {
    uint16_t what = len >= 2 ? rd16(b) : 1;
    if (what == 1) {
        link_log("Msg: To respond with kiwi info");
        send_info();
    } else if (what == 2) {
        send_state();
    } else {
        link_log("Msg/Ping: unknown info type %u", what);
    }
}

// 0x78: copy over the live config (RAM only, as on stock), then apply it.
void session_config(const uint8_t *b, uint16_t len, uint8_t flags) {
    if (len < CONFIG_LEN) {
        link_log("Msg/Config: no enough data (%u bytes)", len);
        return;
    }
    memcpy(config, b, CONFIG_LEN);
    uint8_t vcomm = config[8], vterm = config[9];
    link_log("Msg: config: log level %u, vcomm %u, vterm %u, state interval %u ms",
             session_log_level(), vcomm, vterm, (unsigned)rd32(config + 14));
    vport_configure(vcomm, vterm, config[7] != 0);
    // Byte 19 (force legacy keyboard) only counts with flags > 1, as on stock.
    if (flags > 1 && config[19])
        link_log("Msg: legacy keyboard mode is not supported by open-kiwi yet");
}
