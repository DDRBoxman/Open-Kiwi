// Host-side test of the 0x1A4 text typer (src/usb/text.c). Run via test/run_tests.sh.
#include <stdio.h>
#include <string.h>
#include "usb/text.h"

static int failures, checks;
#define CHECK(cond, ...) do { checks++; if (!(cond)) { failures++; \
    fprintf(stderr, "FAIL: "); fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } } while (0)

static text_typer_t t;

// Type `s` and return the presses as "mod:usage" pairs; check every press is
// followed by an all-zero release.
static void typed(const char *s, char *out, size_t cap) {
    text_init(&t);
    text_push(&t, (const uint8_t *)s, (uint16_t)strlen(s));
    out[0] = 0;
    uint8_t r[8];
    while (text_next_report(&t, r)) {
        char item[16];
        snprintf(item, sizeof item, "%s%02x:%02x", out[0] ? " " : "", r[0], r[2]);
        strncat(out, item, cap - strlen(out) - 1);
        uint8_t rel[8], zero[8] = {0};
        CHECK(text_next_report(&t, rel) && memcmp(rel, zero, 8) == 0, "no release after a press in %s", s);
    }
    CHECK(!text_busy(&t), "typer still busy after %s", s);
}

int main(void) {
    char got[512];

    typed("aA1!", got, sizeof got);
    CHECK(strcmp(got, "00:04 02:04 00:1e 02:1e") == 0, "aA1! -> %s", got);

    typed(" -=[]\\;',./`", got, sizeof got);
    CHECK(strcmp(got, "00:2c 00:2d 00:2e 00:2f 00:30 00:31 00:33 00:34 00:36 00:37 00:38 00:35") == 0,
          "unshifted punctuation -> %s", got);

    typed("_+{}|:\"<>?~@#$%^&*()", got, sizeof got);
    CHECK(strcmp(got, "02:2d 02:2e 02:2f 02:30 02:31 02:33 02:34 02:36 02:37 02:38 02:35 "
                      "02:1f 02:20 02:21 02:22 02:23 02:24 02:25 02:26 02:27") == 0,
          "shifted punctuation -> %s", got);

    // Line endings: \r\n folds into one Enter; lone \r and \n each type Enter.
    typed("a\r\nb\nc\rd\t", got, sizeof got);
    CHECK(strcmp(got, "00:04 00:28 00:05 00:28 00:06 00:28 00:07 00:2b") == 0, "line endings -> %s", got);

    // Repeated characters each get their own press (a release sits between).
    typed("oo", got, sizeof got);
    CHECK(strcmp(got, "00:12 00:12") == 0, "repeat -> %s", got);

    // Non-ASCII bytes are skipped and counted.
    typed("caf\xc3\xa9!", got, sizeof got);
    CHECK(strcmp(got, "00:06 00:04 00:09 02:1e") == 0, "utf-8 -> %s", got);
    CHECK(t.skipped == 2, "skipped %u, want 2", (unsigned)t.skipped);

    // Every printable ASCII character has a key.
    for (int c = 0x20; c <= 0x7E; c++) {
        uint8_t m, u;
        CHECK(text_keymap_us((uint8_t)c, &m, &u) && u >= 0x04 && u <= 0x38, "no key for 0x%02x", c);
    }

    // The queue holds TEXT_QUEUE_SIZE - 1 bytes and reports what it dropped.
    static uint8_t big[TEXT_QUEUE_SIZE + 10];
    memset(big, 'x', sizeof big);
    text_init(&t);
    CHECK(text_push(&t, big, sizeof big) == TEXT_QUEUE_SIZE - 1, "queue capacity");

    // Clearing mid-key still sends the release.
    text_init(&t);
    text_push(&t, (const uint8_t *)"ab", 2);
    uint8_t r[8];
    text_next_report(&t, r);
    text_clear(&t);
    CHECK(text_next_report(&t, r) && r[2] == 0, "release after clear");
    CHECK(!text_next_report(&t, r), "nothing after clear");

    printf("text_test: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
