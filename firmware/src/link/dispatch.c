// Opcode dispatcher: the open counterpart of the stock fn_10007240.
// Every valid inbound frame lands here.
#include "pico/stdlib.h"
#include "pico/bootrom.h"
#include "hardware/uart.h"
#include "hardware/watchdog.h"
#include "board/kiwi.h"
#include "link.h"
#include "usb/usb.h"
#include "usb/reports.h"
#include "periph/periph.h"
#include "update/fwupdate.h"

static void reboot_now(bool bootsel) {
    uart_tx_wait_blocking(KIWI_HOST_UART);   // let the log/ack frame drain
    if (bootsel) reset_usb_boot(0, 0);
    watchdog_reboot(0, 0, 10);               // normal boot: the bootloader applies staged fw
    while (true) tight_loop_contents();
}

void dispatch_frame(const frame_t *f) {
    const uint8_t *b = f->body;
    uint16_t len = f->len;
    switch (f->type) {
    case OP_CONNECT:    session_connect(b, len); break;
    case OP_DISCONNECT: session_disconnect(); break;
    case OP_QUERY:      session_query(b, len); break;
    case OP_KIWICONFIG: session_config(b, len, f->flags); break;
    case OP_LOG:        break;   // host's debug-tab ping, nothing to do

    // HID input is forwarded verbatim, as on stock.
    case OP_KEYBOARD:  if (len >= 8) usb_hid_push(RID_KB, b, 8); led_activity(); break;
    case OP_MOUSE_REL: if (len >= 5) usb_hid_push(RID_MOUSE, b, 5); led_activity(); break;
    case OP_MOUSE_ABS: if (len >= 7) usb_hid_push(RID_ABS, b, 7); led_activity(); break;
    case OP_TEXT:      usb_text_push(b, len); led_activity(); break;
    case OP_TARGET_USB: usb_target_config(b, len); break;

    case OP_VCOMM:
    case OP_TERMINAL:  vport_from_host(f->type, b, len); break;
    case OP_SIDE_GPIO: side_gpio_cmd(b, len); break;

    case OP_REBOOT:
        link_log("To reboot...");
        link_send(OP_REBOOT, NULL, 0);
        reboot_now(false);
        break;
    case OP_REBOOT_BOOTSEL:
        link_log("Msg: To reboot to mass storage mode!");
        reboot_now(true);
        break;

    case OP_FW_BEGIN: case OP_FW_ERASE: case OP_FW_DATA: case OP_FW_VERIFY: case OP_FW_FINISH:
#ifdef KIWI_OTA
        fwupdate_on_frame(f->type, b, len);
#else
        if (f->type == OP_FW_BEGIN)
            link_log("FwUpdateBegin: not valid: this build has no bootloader (use open_kiwi_ota)");
#endif
        break;

    default:
        link_log("Msg/?: Unknown event type: %u", f->type);
        break;
    }
}
