// Side-port GPIO / ATX control, opcode 0x227.
//   body [index, mode, args...], index 0..3 -> GP20 / GP21 / GP26 / GP27
//   mode 0 disabled (input, pull-down) · 1 input(pull_up, pull_down) ·
//        2 output(level) · 3 pulse(level, width_ms <= 500, then the opposite)
// ATX (vendor UI): GP26 = Power Switch, GP20 = Reset Switch, GP27 = Power LED,
// GP21 = HDD LED. While the side UART is on it owns GPIO 0/1, and 0x227 for
// those is ignored, as on stock.
#include "pico/stdlib.h"
#include "board/kiwi.h"
#include "link/link.h"
#include "periph.h"

static const uint8_t PIN[4] = {KIWI_SIDE_PIN1, KIWI_SIDE_PIN3, KIWI_SIDE_PIN4, KIWI_SIDE_PIN6};
static uint8_t mode[4];    // last 0x227 mode per index (0 = disabled)
static bool uart_owned;    // GPIO 0/1 are UART1 TX/RX

// Pulses run without blocking; afterwards the pin stays an output at the
// opposite level (the stock behaviour).
static uint8_t pulse_pin = 0xFF, pulse_rest_level;
static uint32_t pulse_end_ms;

static void set_disabled(uint pin) {
    gpio_init(pin);
    gpio_set_dir(pin, GPIO_IN);
    gpio_set_pulls(pin, false, true);
}

void side_gpio_init(void) {
    for (int i = 0; i < 4; i++) set_disabled(PIN[i]);
}

bool side_gpio_is_disabled(uint8_t index) { return index < 4 && mode[index] == 0; }

void side_gpio_claim_uart(bool uart) {
    uart_owned = uart;
    for (int i = 0; i < 2; i++) {
        if (pulse_pin == PIN[i]) pulse_pin = 0xFF;
        if (uart) {
            gpio_set_function(PIN[i], GPIO_FUNC_UART);
        } else {
            set_disabled(PIN[i]);
            mode[i] = 0;
        }
    }
}

// bit 2i = level, bit 2i+1 = pin is an input (host: protocol::gpio::level).
// UART-owned pins report as neither.
uint8_t side_gpio_levels(void) {
    uint8_t io = 0;
    for (int i = 0; i < 4; i++) {
        if (uart_owned && i < 2) continue;
        uint pin = PIN[i];
        if (gpio_get(pin)) io |= (uint8_t)(1u << (2 * i));
        if (!gpio_is_dir_out(pin)) io |= (uint8_t)(1u << (2 * i + 1));
    }
    return io;
}

void side_gpio_cmd(const uint8_t *b, uint16_t len) {
    if (len < 8 || b[0] > 3) { link_log("Msg/Gpio: bad setup"); return; }
    uint8_t idx = b[0], m = b[1], level = b[2];
    if (uart_owned && idx < 2) {
        link_log("Msg/Gpio: GPIO %u is in use by the side uart", idx);
        return;
    }
    uint pin = PIN[idx];
    if (pulse_pin == pin) pulse_pin = 0xFF;
    switch (m) {
    case 0:
        gpio_set_dir(pin, GPIO_IN);
        gpio_set_pulls(pin, false, true);
        break;
    case 1:
        gpio_set_dir(pin, GPIO_IN);
        gpio_set_pulls(pin, b[2] != 0, b[3] != 0);
        break;
    case 2:
        gpio_put(pin, level ? 1 : 0);
        gpio_set_dir(pin, GPIO_OUT);
        break;
    case 3: {
        uint32_t width = (uint32_t)b[3] | ((uint32_t)b[4] << 8) | ((uint32_t)b[5] << 16) | ((uint32_t)b[6] << 24);
        if (width > 500) width = 500;
        if (width < 1) width = 1;
        gpio_put(pin, level ? 1 : 0);
        gpio_set_dir(pin, GPIO_OUT);
        pulse_pin = (uint8_t)pin;
        pulse_rest_level = level ? 0 : 1;
        pulse_end_ms = to_ms_since_boot(get_absolute_time()) + width;
        break;
    }
    default:
        link_log("Unknown GPIO mode %u", m);   // modes 4/5: vendor app only
        return;
    }
    mode[idx] = m;
}

void side_gpio_task(void) {
    if (pulse_pin != 0xFF && (int32_t)(to_ms_since_boot(get_absolute_time()) - pulse_end_ms) >= 0) {
        gpio_put(pulse_pin, pulse_rest_level);
        pulse_pin = 0xFF;
    }
}
