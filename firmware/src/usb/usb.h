// Target-facing USB (TinyUSB composite: CDC + HID keyboard/mouse/abs mouse).
#pragma once
#include <stdbool.h>
#include <stdint.h>

void usb_init(void);
// HID queue drain, text typing, delayed re-attach, CDC console. Main loop.
void usb_task(void);

// Queue one HID report (UART input arrives faster than USB polls).
void usb_hid_push(uint8_t report_id, const uint8_t *data, uint8_t len);
// Release every key and button (keeps the absolute pointer where it is).
void usb_hid_release_all(void);

// 0x1A4: queue text for the Kiwi to type.
void usb_text_push(const uint8_t *b, uint16_t len);

// 0x96: target USB config [kind, value].
void usb_target_config(const uint8_t *b, uint16_t len);

// For KiwiInfo: which VID the target sees, and the features bitmask
// (bit 0 mass storage, bit 1 CDC serial).
bool usb_uses_legacy_vid(void);
uint32_t usb_features(void);

// Device-descriptor IDs in use (descriptors.c reads these).
uint16_t usb_vid(void);
uint16_t usb_pid(void);

// CDC helpers for the serial bridge (periph/side_uart.c).
bool usb_cdc_ready(void);
uint32_t usb_cdc_write(const uint8_t *b, uint32_t n);
