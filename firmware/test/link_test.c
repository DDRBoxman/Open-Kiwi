// Host-side test of src/link/frame.c against the vectors shared with the Rust
// client (test/vectors/frames.txt). Run via test/run_tests.sh.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "link/frame.h"
#include "link/info_ext.h"

static int failures, checks;

#define CHECK(cond, ...) do { checks++; if (!(cond)) { failures++; \
    fprintf(stderr, "FAIL line %d: ", lineno); fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } } while (0)

static size_t unhex(const char *s, uint8_t *out, size_t cap) {
    if (strcmp(s, "-") == 0) return 0;
    size_t n = strlen(s) / 2;
    if (n > cap) { fprintf(stderr, "hex field too long\n"); exit(2); }
    for (size_t i = 0; i < n; i++) {
        unsigned v;
        sscanf(s + 2 * i, "%2x", &v);
        out[i] = (uint8_t)v;
    }
    return n;
}

// Feed bytes one at a time, as the UART does; collect decoded frames.
static int decode_all(const uint8_t *b, size_t n, frame_t *frames, uint8_t bodies[][FRAME_MAX_BODY], int max) {
    static frame_decoder_t d;
    frame_decoder_init(&d);
    int got = 0;
    for (size_t i = 0; i < n; i++) {
        frame_decoder_push(&d, b[i]);
        frame_t f;
        while (frame_decoder_next(&d, &f)) {
            if (got < max) {
                frames[got] = f;
                memcpy(bodies[got], f.body, f.len);
                frames[got].body = bodies[got];
            }
            got++;
        }
    }
    return got;
}

static uint8_t bodies[16][FRAME_MAX_BODY];

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "test/vectors/frames.txt";
    FILE *fp = fopen(path, "r");
    if (!fp) { perror(path); return 2; }

    static char line[16384];
    static uint8_t a[8192], b[8192];
    int lineno = 0;
    while (fgets(line, sizeof line, fp)) {
        lineno++;
        line[strcspn(line, "\r\n")] = 0;
        if (!line[0] || line[0] == '#') continue;
        char *kind = strtok(line, " ");
        if (strcmp(kind, "crc8") == 0) {
            size_t n = unhex(strtok(NULL, " "), a, sizeof a);
            unsigned want = (unsigned)strtoul(strtok(NULL, " "), NULL, 16);
            CHECK(frame_crc8(a, n) == want, "crc8 = %02x, want %02x", frame_crc8(a, n), want);
        } else if (strcmp(kind, "frame") == 0) {
            uint16_t type = (uint16_t)strtoul(strtok(NULL, " "), NULL, 16);
            uint16_t seq = (uint16_t)strtoul(strtok(NULL, " "), NULL, 10);
            size_t blen = unhex(strtok(NULL, " "), a, sizeof a);
            size_t wlen = unhex(strtok(NULL, " "), b, sizeof b);
            uint8_t enc[FRAME_MAX_LEN];
            size_t n = frame_encode(enc, sizeof enc, type, seq, a, (uint16_t)blen);
            CHECK(n == wlen && memcmp(enc, b, n) == 0, "encode of type %x differs", type);
            frame_t f[2];
            int got = decode_all(b, wlen, f, bodies, 2);
            CHECK(got == 1, "decode of type %x yielded %d frames", type, got);
            if (got == 1) {
                CHECK(f[0].type == type && f[0].seq == seq && f[0].len == blen &&
                      memcmp(f[0].body, a, blen) == 0 && f[0].flags == frame_flags(type),
                      "decode of type %x doesn't round-trip", type);
            }
        } else if (strcmp(kind, "stream") == 0) {
            size_t n = unhex(strtok(NULL, " "), a, sizeof a);
            char *types = strtok(NULL, " ");
            uint16_t want[16];
            int nwant = 0;
            for (char *t = strtok(types, ","); t && nwant < 16; t = strtok(NULL, ","))
                want[nwant++] = (uint16_t)strtoul(t, NULL, 16);
            frame_t f[16];
            int got = decode_all(a, n, f, bodies, 16);
            CHECK(got == nwant, "stream yielded %d frames, want %d", got, nwant);
            for (int i = 0; i < got && i < nwant; i++)
                CHECK(f[i].type == want[i], "stream frame %d is type %x, want %x", i, f[i].type, want[i]);
        } else if (strcmp(kind, "infoext") == 0) {
            uint32_t caps = (uint32_t)strtoul(strtok(NULL, " "), NULL, 16);
            size_t n = unhex(strtok(NULL, " "), a, sizeof a);
            uint8_t ext[INFO_EXT_LEN];
            info_ext_build(ext, caps);
            CHECK(n == INFO_EXT_LEN && memcmp(ext, a, n) == 0, "KiwiInfo extension for caps %x differs", caps);
        } else {
            fprintf(stderr, "line %d: unknown vector kind '%s'\n", lineno, kind);
            return 2;
        }
    }
    fclose(fp);

    // Not expressible as a vector: a body of exactly FRAME_MAX_BODY decodes,
    // and the encoder refuses one byte more.
    {
        static uint8_t body[FRAME_MAX_BODY + 1], enc[FRAME_MAX_LEN + 1];
        memset(body, 0xA5, sizeof body);
        size_t n = frame_encode(enc, sizeof enc, 0x1FF, 1, body, FRAME_MAX_BODY);
        frame_t f[1];
        lineno = 0;
        CHECK(n == FRAME_MAX_LEN && decode_all(enc, n, f, bodies, 1) == 1, "max-size frame");
        CHECK(frame_encode(enc, sizeof enc, 0x1FF, 1, body, FRAME_MAX_BODY + 1) == 0, "oversize encode");
    }

    printf("link_test: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
