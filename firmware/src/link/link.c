// Host-link transport over UART0.
#include <stdarg.h>
#include <stdio.h>
#include "pico/stdlib.h"
#include "hardware/uart.h"
#include "board/kiwi.h"
#include "link.h"

// A frame that can't be written within this budget is abandoned. At 2 Mbaud a
// maximum-size frame takes ~5.6 ms, so this only trips when the host has
// stopped reading and CTS stays deasserted.
#define TX_BUDGET_US 20000
// Bytes handled per link_task() call, so USB keeps getting serviced.
#define RX_BATCH 256

static frame_decoder_t dec;
static uint32_t last_rx_ms;
static bool tx_stalled;

void link_init(void) {
    uart_init(KIWI_HOST_UART, KIWI_HOST_UART_BAUD);
    gpio_set_function(KIWI_HOST_UART_TX, GPIO_FUNC_UART);
    gpio_set_function(KIWI_HOST_UART_RX, GPIO_FUNC_UART);
    gpio_set_function(KIWI_HOST_UART_CTS, GPIO_FUNC_UART);
    gpio_set_function(KIWI_HOST_UART_RTS, GPIO_FUNC_UART);
    uart_set_hw_flow(KIWI_HOST_UART, true, true);
    uart_set_format(KIWI_HOST_UART, 8, 1, UART_PARITY_NONE);
    uart_set_fifo_enabled(KIWI_HOST_UART, true);
    frame_decoder_init(&dec);
}

void link_task(void) {
    for (int i = 0; i < RX_BATCH && uart_is_readable(KIWI_HOST_UART); i++) {
        frame_decoder_push(&dec, (uint8_t)uart_getc(KIWI_HOST_UART));
        frame_t f;
        while (frame_decoder_next(&dec, &f)) {
            last_rx_ms = to_ms_since_boot(get_absolute_time());
            if (!last_rx_ms) last_rx_ms = 1;
            dispatch_frame(&f);
        }
    }
}

uint32_t link_last_rx_ms(void) { return last_rx_ms; }

static bool tx_bytes(const uint8_t *p, size_t n, uint32_t t0) {
    for (size_t i = 0; i < n; i++) {
        while (!uart_is_writable(KIWI_HOST_UART))
            if (time_us_32() - t0 > TX_BUDGET_US) return false;
        uart_putc_raw(KIWI_HOST_UART, (char)p[i]);
    }
    return true;
}

void link_send(uint16_t type, const uint8_t *body, uint16_t len) {
    static uint16_t seq = 0;
    if (len > FRAME_MAX_BODY) return;
    // While the host isn't reading, drop frames at once instead of waiting
    // out the budget on every one.
    if (tx_stalled && !uart_is_writable(KIWI_HOST_UART)) return;
    if (++seq == 0) seq = 1;
    uint8_t h[FRAME_HEADER_LEN];
    frame_header(h, type, seq, body, len);
    uint32_t t0 = time_us_32();
    tx_stalled = !(tx_bytes(FRAME_PREAMBLE, 4, t0) && tx_bytes(h, sizeof h, t0) &&
                   tx_bytes(body, len, t0) && tx_bytes(FRAME_TRAILER, 4, t0));
}

static void vlog(uint16_t level, const char *fmt, va_list ap) {
    if (level < session_log_level()) return;
    uint8_t b[2 + 160] = {(uint8_t)level, (uint8_t)(level >> 8)};
    int n = vsnprintf((char *)b + 2, sizeof b - 2, fmt, ap);
    if (n < 0) return;
    if (n > (int)sizeof b - 3) n = (int)sizeof b - 3;
    link_send(OP_LOG, b, (uint16_t)(2 + n + 1));
}

void link_log(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vlog(LOG_INFO, fmt, ap);
    va_end(ap);
}

void link_logl(uint16_t level, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vlog(level, fmt, ap);
    va_end(ap);
}
