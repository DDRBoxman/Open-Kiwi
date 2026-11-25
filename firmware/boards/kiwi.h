// Pico SDK board config for the Cytrence Kiwi (v1.1/B).
// Select with -DPICO_BOARD=kiwi; the project adds this dir to
// PICO_BOARD_HEADER_DIRS. Pin assignments live in ../board/kiwi.h; this file
// only carries what the SDK/linker need (flash geometry, boot2).
#ifndef _BOARDS_KIWI_H
#define _BOARDS_KIWI_H

// 16 MB Winbond W25Q128 (confirmed via picotool). Must precede pico.h, which
// only sets a 2 MB default when this is undefined.
#define PICO_FLASH_SIZE_BYTES (16 * 1024 * 1024)

// Status LED is PWM on GP18 (our code drives it); expose it as the default LED.
#define PICO_DEFAULT_LED_PIN 18

// Inherit the rest of the generic RP2040 defaults (clocks, boot2 w25q080,
// default UART/I2C instances). The vendor firmware uses boot2_w25q080 too.
#include "boards/pico.h"

#endif
