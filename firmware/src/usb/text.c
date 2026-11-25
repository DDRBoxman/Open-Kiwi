#include "text.h"
#include <string.h>

#define SHIFT 0x02   // left shift

// US layout, printable ASCII 0x20..0x7E: usage, with bit 7 = needs shift.
#define S(u) ((uint8_t)(0x80 | (u)))
static const uint8_t US[95] = {
    0x2C,    S(0x1E), S(0x34), S(0x20), S(0x21), S(0x22), S(0x24), 0x34,     //  !"#$%&'
    S(0x26), S(0x27), S(0x25), S(0x2E), 0x36,    0x2D,    0x37,    0x38,     // ()*+,-./
    0x27,    0x1E,    0x1F,    0x20,    0x21,    0x22,    0x23,    0x24,     // 01234567
    0x25,    0x26,    S(0x33), 0x33,    S(0x36), 0x2E,    S(0x37), S(0x38),  // 89:;<=>?
    S(0x1F),                                                                 // @
    S(0x04), S(0x05), S(0x06), S(0x07), S(0x08), S(0x09), S(0x0A), S(0x0B),  // A-H
    S(0x0C), S(0x0D), S(0x0E), S(0x0F), S(0x10), S(0x11), S(0x12), S(0x13),  // I-P
    S(0x14), S(0x15), S(0x16), S(0x17), S(0x18), S(0x19), S(0x1A), S(0x1B),  // Q-X
    S(0x1C), S(0x1D),                                                        // YZ
    0x2F,    0x31,    0x30,    S(0x23), S(0x2D), 0x35,                       // [\]^_`
    0x04,    0x05,    0x06,    0x07,    0x08,    0x09,    0x0A,    0x0B,     // a-h
    0x0C,    0x0D,    0x0E,    0x0F,    0x10,    0x11,    0x12,    0x13,     // i-p
    0x14,    0x15,    0x16,    0x17,    0x18,    0x19,    0x1A,    0x1B,     // q-x
    0x1C,    0x1D,                                                           // yz
    S(0x2F), S(0x31), S(0x30), S(0x35),                                      // {|}~
};

bool text_keymap_us(uint8_t c, uint8_t *modifier, uint8_t *usage) {
    *modifier = 0;
    switch (c) {
    case '\n': case '\r': *usage = 0x28; return true;   // Enter
    case '\t': *usage = 0x2B; return true;              // Tab
    case '\b': *usage = 0x2A; return true;              // Backspace
    case 0x1B: *usage = 0x29; return true;              // Escape
    default: break;
    }
    if (c < 0x20 || c > 0x7E) return false;
    uint8_t e = US[c - 0x20];
    *usage = e & 0x7F;
    if (e & 0x80) *modifier = SHIFT;
    return true;
}

void text_init(text_typer_t *t) { memset(t, 0, sizeof *t); }

static uint16_t used(const text_typer_t *t) {
    return (uint16_t)((t->head + TEXT_QUEUE_SIZE - t->tail) % TEXT_QUEUE_SIZE);
}

uint16_t text_push(text_typer_t *t, const uint8_t *b, uint16_t len) {
    uint16_t room = (uint16_t)(TEXT_QUEUE_SIZE - 1 - used(t));
    uint16_t n = len < room ? len : room;
    for (uint16_t i = 0; i < n; i++) {
        t->q[t->head] = b[i];
        t->head = (uint16_t)((t->head + 1) % TEXT_QUEUE_SIZE);
    }
    return n;
}

void text_clear(text_typer_t *t) {
    t->tail = t->head;
    t->prev = 0;
}

bool text_busy(const text_typer_t *t) { return t->key_down || t->head != t->tail; }

bool text_next_report(text_typer_t *t, uint8_t report[8]) {
    memset(report, 0, 8);
    if (t->key_down) {          // release the last key; repeats need a gap
        t->key_down = false;
        return true;
    }
    while (t->head != t->tail) {
        uint8_t c = t->q[t->tail];
        t->tail = (uint16_t)((t->tail + 1) % TEXT_QUEUE_SIZE);
        uint8_t prev = t->prev;
        t->prev = c;
        if (c == '\n' && prev == '\r') continue;
        uint8_t mod, usage;
        if (!text_keymap_us(c, &mod, &usage)) { t->skipped++; continue; }
        report[0] = mod;
        report[2] = usage;
        t->key_down = true;
        return true;
    }
    return false;
}
