// Text batch (0x1A4): the host sends raw text and the Kiwi types it as
// keyboard reports. Pure C (no Pico SDK), so test/text_test.c runs it on the host.
//
// The keymap is US ANSI, which is what the target has to be set to. Bytes with
// no US key (non-ASCII, most control characters) are skipped and counted.
#pragma once
#include <stdbool.h>
#include <stdint.h>

#define TEXT_QUEUE_SIZE 2048

typedef struct {
    uint8_t q[TEXT_QUEUE_SIZE];
    uint16_t head, tail;
    bool key_down;      // a press was sent; the next report is its release
    uint8_t prev;       // last byte taken, to fold "\r\n" into one Enter
    uint32_t skipped;   // bytes skipped since the caller last cleared this
} text_typer_t;

// HID usage + modifier for one character on a US layout. False if none.
bool text_keymap_us(uint8_t c, uint8_t *modifier, uint8_t *usage);

void text_init(text_typer_t *t);
// Queue bytes. Returns how many fit; the rest are dropped.
uint16_t text_push(text_typer_t *t, const uint8_t *b, uint16_t len);
// Forget queued text (any key still down gets its release first).
void text_clear(text_typer_t *t);
bool text_busy(const text_typer_t *t);
// The next 8-byte keyboard report: a press, then its release, per character.
// False when there is nothing left to send.
bool text_next_report(text_typer_t *t, uint8_t report[8]);
