// Host-link framing. Pure C, no Pico SDK: the same
// code is compiled into the host-side tests (test/link_test.c), which check it
// against the vectors shared with the Rust client (test/vectors/frames.txt).
//
//   preamble 3C 5A A5 C3 | flags crc8 seq:u16 type:u16 len:u16 | body | C9 6D D6 9C
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define FRAME_HEADER_LEN 8
#define FRAME_MAX_BODY   0x440   // the vendor app rejects 0x441 and up
#define FRAME_OVERHEAD   (4 + FRAME_HEADER_LEN + 4)
#define FRAME_MAX_LEN    (FRAME_OVERHEAD + FRAME_MAX_BODY)

extern const uint8_t FRAME_PREAMBLE[4];
extern const uint8_t FRAME_TRAILER[4];

// CRC-8/SMBus (poly 0x07, init 0), over the body only.
uint8_t frame_crc8(const uint8_t *p, size_t n);

// Header flags byte: 2 for 0x64 (KiwiInfo) and 0x78 (KiwiConfig), else 0.
uint8_t frame_flags(uint16_t type);

// The 8 header bytes for a frame.
void frame_header(uint8_t h[FRAME_HEADER_LEN], uint16_t type, uint16_t seq,
                  const uint8_t *body, uint16_t len);

// Whole frame into `out`. Returns its length, or 0 if it doesn't fit or the
// body is too long.
size_t frame_encode(uint8_t *out, size_t cap, uint16_t type, uint16_t seq,
                    const uint8_t *body, uint16_t len);

typedef struct {
    uint8_t flags;
    uint16_t seq, type, len;
    const uint8_t *body;   // valid until the next frame_decoder_* call
} frame_t;

// Streaming decoder with the same rules as the Rust client's Decoder. A CRC
// byte of 0 is not checked, as on stock: the vendor app sends its detection
// frames (0x32, 0xC8) with CRC 0. A frame with a bad trailer or a non-zero
// CRC that doesn't match (or an oversize length) costs one byte,
// and the search for the next preamble restarts right after it. So a good
// frame that arrives inside a truncated one is still found.
typedef struct {
    uint8_t buf[FRAME_MAX_LEN];
    size_t n;          // bytes buffered
    size_t consume;    // length of the frame last returned, dropped on the next call
    uint32_t dropped;  // bytes discarded while resyncing
} frame_decoder_t;

void frame_decoder_init(frame_decoder_t *d);

// Add one byte. Call frame_decoder_next() until it returns false before
// pushing the next byte; that keeps the buffer from overflowing.
void frame_decoder_push(frame_decoder_t *d, uint8_t x);

// The next complete, valid frame, if one is buffered.
bool frame_decoder_next(frame_decoder_t *d, frame_t *out);
