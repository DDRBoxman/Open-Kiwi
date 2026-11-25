// Serial streams: KiwiConfig routes the terminal
// stream (0x1FF, vterm) and the virtual-serial-port stream (0x1FE, vcomm) to
// the side UART (UART1 on side-port Pin-1 TX / Pin-3 RX, 115200 8N1) or the
// target's USB CDC port. The side UART is on while either stream uses it.
//
// Host -> device bytes go through a TX ring so a 256-byte chunk at 115200
// baud (~22 ms) never blocks the main loop. Device -> host bytes are batched
// into frames of up to 64 bytes, flushed after 2 ms without new input.
#include "pico/stdlib.h"
#include "hardware/uart.h"
#include "board/kiwi.h"
#include "link/link.h"
#include "usb/usb.h"
#include "periph.h"

#define SIDE_BAUD   115200
#define TX_RING     1024
#define RX_BATCH    64
#define RX_IDLE_MS  2

static uint8_t vcomm, vterm;
static bool side_on;

static uint8_t tx[TX_RING];
static uint16_t tx_head, tx_tail;
static uint32_t tx_dropped;

typedef struct {
    uint8_t b[RX_BATCH];
    uint8_t n;
    uint32_t last_ms;
} batch_t;
static batch_t from_side, from_cdc;

bool side_uart_enabled(void) { return side_on; }

// Which stream a sink carries, preferring the terminal if both point at it.
static uint16_t stream_for(uint8_t sink) {
    if (vterm == sink) return OP_TERMINAL;
    if (vcomm == sink) return OP_VCOMM;
    return 0;
}

uint16_t vport_cdc_stream(void) { return stream_for(VPORT_CDC); }

static void side_enable(bool on) {
    if (on == side_on) return;
    side_on = on;
    if (on) {
        uart_init(KIWI_SIDE_UART, SIDE_BAUD);
        uart_set_format(KIWI_SIDE_UART, 8, 1, UART_PARITY_NONE);
        uart_set_hw_flow(KIWI_SIDE_UART, false, false);
        uart_set_fifo_enabled(KIWI_SIDE_UART, true);
        side_gpio_claim_uart(true);
        link_log("Comm: Side uart enabled");
    } else {
        side_gpio_claim_uart(false);
        uart_deinit(KIWI_SIDE_UART);
        tx_head = tx_tail = 0;
        from_side.n = 0;
        link_log("Comm:: Side uart disabled");
    }
}

// Byte 7 of KiwiConfig also turns the side UART on, but only while GPIO 0
// and 1 are both disabled (the stock fallback; the vendor app never sets it).
void vport_configure(uint8_t new_vcomm, uint8_t new_vterm, bool fallback) {
    vcomm = new_vcomm > VPORT_SIDE ? VPORT_DISABLED : new_vcomm;
    vterm = new_vterm > VPORT_SIDE ? VPORT_DISABLED : new_vterm;
    bool on = vcomm == VPORT_SIDE || vterm == VPORT_SIDE ||
              (fallback && side_gpio_is_disabled(0) && side_gpio_is_disabled(1));
    side_enable(on);
}

void vport_from_host(uint16_t type, const uint8_t *b, uint16_t len) {
    uint8_t sink = type == OP_TERMINAL ? vterm : vcomm;
    if (sink == VPORT_SIDE && side_on) {
        for (uint16_t i = 0; i < len; i++) {
            uint16_t n = (uint16_t)((tx_head + 1) % TX_RING);
            if (n == tx_tail) { tx_dropped += (uint32_t)(len - i); break; }
            tx[tx_head] = b[i];
            tx_head = n;
        }
    } else if (sink == VPORT_CDC) {
        if (usb_cdc_ready()) usb_cdc_write(b, len);   // nobody listening: drop
    }
    // VPORT_DISABLED: dropped, as on stock.
}

static void batch_add(batch_t *q, uint16_t stream, uint8_t x, uint32_t now) {
    q->b[q->n++] = x;
    q->last_ms = now;
    if (q->n == RX_BATCH) { link_send(stream, q->b, q->n); q->n = 0; }
}

static void batch_flush_idle(batch_t *q, uint16_t stream, uint32_t now) {
    if (q->n && (now - q->last_ms) >= RX_IDLE_MS) {
        if (stream) link_send(stream, q->b, q->n);
        q->n = 0;
    }
}

void vport_from_cdc(const uint8_t *b, uint32_t n) {
    uint16_t stream = vport_cdc_stream();
    if (!stream) return;
    uint32_t now = to_ms_since_boot(get_absolute_time());
    for (uint32_t i = 0; i < n; i++) batch_add(&from_cdc, stream, b[i], now);
}

void vport_task(void) {
    uint32_t now = to_ms_since_boot(get_absolute_time());
    if (side_on) {
        while (tx_tail != tx_head && uart_is_writable(KIWI_SIDE_UART)) {
            uart_putc_raw(KIWI_SIDE_UART, (char)tx[tx_tail]);
            tx_tail = (uint16_t)((tx_tail + 1) % TX_RING);
        }
        uint16_t stream = stream_for(VPORT_SIDE);
        while (uart_is_readable(KIWI_SIDE_UART)) {
            uint8_t x = (uint8_t)uart_getc(KIWI_SIDE_UART);
            if (stream) batch_add(&from_side, stream, x, now);
        }
        batch_flush_idle(&from_side, stream, now);
    }
    batch_flush_idle(&from_cdc, vport_cdc_stream(), now);
    if (tx_dropped) {
        link_log("Comm: side uart tx buffer full, dropped %u bytes", (unsigned)tx_dropped);
        tx_dropped = 0;
    }
}
