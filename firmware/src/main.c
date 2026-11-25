// open-kiwi: open firmware for the Cytrence Kiwi's RP2040.
// See ../README.md for an overview.
//
//   src/link/    host link: framing, session (handshake/info/state/config), dispatcher
//   src/usb/     target USB: descriptors, HID queue, text typing, 0x96 config, CDC
//   src/periph/  side-port GPIO/ATX, side UART + serial routing, LED, temperature
//   src/update/  OTA update receiver (open_kiwi_ota only)
//
// Everything runs from one cooperative loop on core 0; nothing blocks for
// long, and a hardware watchdog resets the chip if the loop ever stalls.
#include "pico/stdlib.h"
#include "hardware/watchdog.h"
#include "tusb.h"
#include "link/link.h"
#include "usb/usb.h"
#include "periph/periph.h"
#include "update/fwupdate.h"

// Longest legitimate stall is a FW_VERIFY/FW_FINISH CRC over a 1 MB image
// (well under 1 s); sector erases kick the watchdog themselves.
#define WATCHDOG_MS 4000
// A host that has sent nothing for this long counts as gone (it polls KiwiState every ~1 s).
#define HOST_TIMEOUT_MS 3000

static led_mode_t led_mode(void) {
    uint32_t last = link_last_rx_ms();
    bool host_recent = last && to_ms_since_boot(get_absolute_time()) - last < HOST_TIMEOUT_MS;
#ifdef KIWI_OTA
    if (fwupdate_active() && host_recent) return LED_UPDATING;
#endif
    return (session_active() && host_recent) ? LED_SESSION : LED_IDLE;
}

int main(void) {
    bool watchdog_reset = watchdog_enable_caused_reboot();

    led_init();
    side_gpio_init();
    temp_init();
    session_init(watchdog_reset);
    link_init();
    usb_init();
    tusb_init();
    watchdog_enable(WATCHDOG_MS, true);

    while (true) {
        watchdog_update();
        tud_task();
        link_task();
        usb_task();
        side_gpio_task();
        vport_task();
        led_task(led_mode());
    }
}
