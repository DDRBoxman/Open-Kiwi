#include "frame.h"
#include <string.h>

const uint8_t FRAME_PREAMBLE[4] = {0x3C, 0x5A, 0xA5, 0xC3};
const uint8_t FRAME_TRAILER[4]  = {0xC9, 0x6D, 0xD6, 0x9C};

uint8_t frame_crc8(const uint8_t *p, size_t n) {
    uint8_t c = 0;
    while (n--) {
        c ^= *p++;
        for (int b = 0; b < 8; b++) c = (c & 0x80) ? (uint8_t)((c << 1) ^ 0x07) : (uint8_t)(c << 1);
    }
    return c;
}

uint8_t frame_flags(uint16_t type) {
    return (type == 0x64 || type == 0x78) ? 2 : 0;
}

void frame_header(uint8_t h[FRAME_HEADER_LEN], uint16_t type, uint16_t seq,
                  const uint8_t *body, uint16_t len) {
    h[0] = frame_flags(type);
    h[1] = frame_crc8(body, len);
    h[2] = (uint8_t)seq;  h[3] = (uint8_t)(seq >> 8);
    h[4] = (uint8_t)type; h[5] = (uint8_t)(type >> 8);
    h[6] = (uint8_t)len;  h[7] = (uint8_t)(len >> 8);
}

size_t frame_encode(uint8_t *out, size_t cap, uint16_t type, uint16_t seq,
                    const uint8_t *body, uint16_t len) {
    size_t total = FRAME_OVERHEAD + (size_t)len;
    if (len > FRAME_MAX_BODY || total > cap) return 0;
    memcpy(out, FRAME_PREAMBLE, 4);
    frame_header(out + 4, type, seq, body, len);
    if (len) memcpy(out + 4 + FRAME_HEADER_LEN, body, len);
    memcpy(out + 4 + FRAME_HEADER_LEN + len, FRAME_TRAILER, 4);
    return total;
}

void frame_decoder_init(frame_decoder_t *d) {
    memset(d, 0, sizeof *d);
}

static void drop_front(frame_decoder_t *d, size_t k) {
    if (k >= d->n) { d->n = 0; return; }
    memmove(d->buf, d->buf + k, d->n - k);
    d->n -= k;
}

void frame_decoder_push(frame_decoder_t *d, uint8_t x) {
    if (d->consume) { drop_front(d, d->consume); d->consume = 0; }
    if (d->n == sizeof d->buf) { drop_front(d, 1); d->dropped++; }   // caller skipped next()
    d->buf[d->n++] = x;
}

// Index of the first full preamble, or n if there is none.
static size_t find_preamble(const uint8_t *p, size_t n) {
    for (size_t i = 0; i + 4 <= n; i++)
        if (memcmp(p + i, FRAME_PREAMBLE, 4) == 0) return i;
    return n;
}

bool frame_decoder_next(frame_decoder_t *d, frame_t *out) {
    if (d->consume) { drop_front(d, d->consume); d->consume = 0; }
    for (;;) {
        size_t s = find_preamble(d->buf, d->n);
        if (s == d->n) {
            // No preamble yet: keep a possible partial one at the tail.
            size_t keep = d->n < 3 ? d->n : 3;
            d->dropped += (uint32_t)(d->n - keep);
            drop_front(d, d->n - keep);
            return false;
        }
        if (s) { d->dropped += (uint32_t)s; drop_front(d, s); }

        if (d->n < 4 + FRAME_HEADER_LEN) return false;
        const uint8_t *h = d->buf + 4;
        size_t len = (size_t)(h[6] | (h[7] << 8));
        if (len > FRAME_MAX_BODY) { drop_front(d, 1); d->dropped++; continue; }
        size_t total = FRAME_OVERHEAD + len;
        if (d->n < total) return false;

        const uint8_t *body = h + FRAME_HEADER_LEN;
        // A CRC byte of 0 means "not checked", as in the stock receiver
        // (fn_10006d48). The vendor app's port-detection frames rely on it.
        bool crc_ok = h[1] == 0 || frame_crc8(body, len) == h[1];
        if (memcmp(body + len, FRAME_TRAILER, 4) != 0 || !crc_ok) {
            drop_front(d, 1);
            d->dropped++;
            continue;
        }
        out->flags = h[0];
        out->seq = (uint16_t)(h[2] | (h[3] << 8));
        out->type = (uint16_t)(h[4] | (h[5] << 8));
        out->len = (uint16_t)len;
        out->body = body;
        d->consume = total;
        return true;
    }
}
