// Target-facing USB descriptors: composite CDC (console / serial bridge) +
// one HID interface with keyboard, relative mouse and absolute mouse reports,
// so 0x190/0x19A/0x19B bodies forward to the target verbatim.
//
// VID/PID come from usb.c (0x96 kind 6 switches them); the serial number is
// the flash chip's unique ID, so each unit has its own.
#include <string.h>
#include "tusb.h"
#include "pico/unique_id.h"
#include "usb.h"
#include "reports.h"

static tusb_desc_device_t desc_device = {
    .bLength = sizeof(tusb_desc_device_t),
    .bDescriptorType = TUSB_DESC_DEVICE,
    .bcdUSB = 0x0200,
    .bDeviceClass = TUSB_CLASS_MISC,
    .bDeviceSubClass = MISC_SUBCLASS_COMMON,
    .bDeviceProtocol = MISC_PROTOCOL_IAD,
    .bMaxPacketSize0 = CFG_TUD_ENDPOINT0_SIZE,
    .idVendor = 0,     // filled in from usb_vid()/usb_pid() on every request
    .idProduct = 0,
    .bcdDevice = 0x0100,
    .iManufacturer = 0x01,
    .iProduct = 0x02,
    .iSerialNumber = 0x03,
    .bNumConfigurations = 0x01,
};

uint8_t const *tud_descriptor_device_cb(void) {
    desc_device.idVendor = usb_vid();
    desc_device.idProduct = usb_pid();
    return (uint8_t const *)&desc_device;
}

// Absolute-mouse report descriptor (not in pico-sdk 1.5.1's TinyUSB). Payload:
// buttons(1) + X(16, 0..0x7FFF) + Y(16) + wheel(8) + pan(8) = 7 bytes, matching
// the 0x19B body [buttons, x:u16, y:u16, wheel, pan].
#define TUD_HID_REPORT_DESC_ABSMOUSE_K(...) \
  HID_USAGE_PAGE ( HID_USAGE_PAGE_DESKTOP     )                   ,\
  HID_USAGE      ( HID_USAGE_DESKTOP_MOUSE    )                   ,\
  HID_COLLECTION ( HID_COLLECTION_APPLICATION )                   ,\
    __VA_ARGS__ \
    HID_USAGE      ( HID_USAGE_DESKTOP_POINTER )                  ,\
    HID_COLLECTION ( HID_COLLECTION_PHYSICAL   )                  ,\
      HID_USAGE_PAGE  ( HID_USAGE_PAGE_BUTTON )                   ,\
        HID_USAGE_MIN   ( 1                                      ) ,\
        HID_USAGE_MAX   ( 5                                      ) ,\
        HID_LOGICAL_MIN ( 0                                      ) ,\
        HID_LOGICAL_MAX ( 1                                      ) ,\
        HID_REPORT_COUNT( 5                                      ) ,\
        HID_REPORT_SIZE ( 1                                      ) ,\
        HID_INPUT       ( HID_DATA | HID_VARIABLE | HID_ABSOLUTE ) ,\
        HID_REPORT_COUNT( 1                                      ) ,\
        HID_REPORT_SIZE ( 3                                      ) ,\
        HID_INPUT       ( HID_CONSTANT                           ) ,\
      HID_USAGE_PAGE  ( HID_USAGE_PAGE_DESKTOP )                  ,\
        HID_USAGE       ( HID_USAGE_DESKTOP_X                    ) ,\
        HID_USAGE       ( HID_USAGE_DESKTOP_Y                    ) ,\
        HID_LOGICAL_MIN  ( 0x00                                 ) ,\
        HID_LOGICAL_MAX_N( 0x7FFF, 2                            ) ,\
        HID_REPORT_SIZE ( 16                                     ) ,\
        HID_REPORT_COUNT( 2                                      ) ,\
        HID_INPUT       ( HID_DATA | HID_VARIABLE | HID_ABSOLUTE ) ,\
        HID_USAGE       ( HID_USAGE_DESKTOP_WHEEL                ) ,\
        HID_LOGICAL_MIN ( 0x81                                  ) ,\
        HID_LOGICAL_MAX ( 0x7f                                  ) ,\
        HID_REPORT_SIZE ( 8                                      ) ,\
        HID_REPORT_COUNT( 1                                      ) ,\
        HID_INPUT       ( HID_DATA | HID_VARIABLE | HID_RELATIVE ) ,\
      HID_USAGE_PAGE  ( HID_USAGE_PAGE_CONSUMER )                ,\
        HID_USAGE_N     ( HID_USAGE_CONSUMER_AC_PAN, 2          ) ,\
        HID_LOGICAL_MIN ( 0x81                                  ) ,\
        HID_LOGICAL_MAX ( 0x7f                                  ) ,\
        HID_REPORT_SIZE ( 8                                      ) ,\
        HID_REPORT_COUNT( 1                                      ) ,\
        HID_INPUT       ( HID_DATA | HID_VARIABLE | HID_RELATIVE ) ,\
    HID_COLLECTION_END                                            ,\
  HID_COLLECTION_END

static uint8_t const desc_hid_report[] = {
    TUD_HID_REPORT_DESC_KEYBOARD(HID_REPORT_ID(RID_KB)),
    TUD_HID_REPORT_DESC_MOUSE(HID_REPORT_ID(RID_MOUSE)),
    TUD_HID_REPORT_DESC_ABSMOUSE_K(HID_REPORT_ID(RID_ABS)),
};

uint8_t const *tud_hid_descriptor_report_cb(uint8_t instance) {
    (void)instance;
    return desc_hid_report;
}

enum { ITF_NUM_CDC = 0, ITF_NUM_CDC_DATA, ITF_NUM_HID, ITF_NUM_TOTAL };
#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_CDC_DESC_LEN + TUD_HID_DESC_LEN)
#define EPNUM_CDC_NOTIF 0x81
#define EPNUM_CDC_OUT   0x02
#define EPNUM_CDC_IN    0x82
#define EPNUM_HID       0x83

static uint8_t const desc_configuration[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_NUM_TOTAL, 0, CONFIG_TOTAL_LEN, 0x00, 100),
    TUD_CDC_DESCRIPTOR(ITF_NUM_CDC, 4, EPNUM_CDC_NOTIF, 8, EPNUM_CDC_OUT, EPNUM_CDC_IN, 64),
    // HID: report protocol (report IDs), 1 ms polling for responsive input.
    TUD_HID_DESCRIPTOR(ITF_NUM_HID, 5, HID_ITF_PROTOCOL_NONE, sizeof(desc_hid_report),
                       EPNUM_HID, CFG_TUD_HID_EP_BUFSIZE, 1),
};

uint8_t const *tud_descriptor_configuration_cb(uint8_t index) {
    (void)index;
    return desc_configuration;
}

static char serial[2 * PICO_UNIQUE_BOARD_ID_SIZE_BYTES + 1];

static char const *string_desc_arr[] = {
    (const char[]){0x09, 0x04},
    "open-kiwi",
    "open-kiwi KVM",
    serial,
    "open-kiwi CDC",
    "open-kiwi HID",
};

static uint16_t _desc_str[32];

uint16_t const *tud_descriptor_string_cb(uint8_t index, uint16_t langid) {
    (void)langid;
    uint8_t chr_count;
    if (!serial[0]) pico_get_unique_board_id_string(serial, sizeof serial);
    if (index == 0) {
        memcpy(&_desc_str[1], string_desc_arr[0], 2);
        chr_count = 1;
    } else {
        if (index >= sizeof(string_desc_arr) / sizeof(string_desc_arr[0])) return NULL;
        const char *str = string_desc_arr[index];
        chr_count = (uint8_t)strlen(str);
        if (chr_count > 31) chr_count = 31;
        for (uint8_t i = 0; i < chr_count; i++) _desc_str[1 + i] = str[i];
    }
    _desc_str[0] = (uint16_t)((TUSB_DESC_STRING << 8) | (2 * chr_count + 2));
    return _desc_str;
}

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t type,
                               uint8_t *buffer, uint16_t reqlen) {
    (void)instance; (void)report_id; (void)type; (void)buffer; (void)reqlen;
    return 0;
}

void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t type,
                           uint8_t const *buffer, uint16_t bufsize) {
    (void)instance; (void)report_id; (void)type; (void)buffer; (void)bufsize;
}
