// Host link: UART0 to the CH343 bridge (2 Mbaud, RTS/CTS), the session state
// (handshake, KiwiInfo/KiwiState, KiwiConfig) and the opcode dispatcher.
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "frame.h"
#include "opcodes.h"

// ---- link.c: transport ----
void link_init(void);
// Read the UART and dispatch complete frames. Call from the main loop.
void link_task(void);
// Send one frame. Never blocks for long: if the host stops reading (CTS held
// off), the frame is dropped and the host's decoder resyncs on the next one.
void link_send(uint16_t type, const uint8_t *body, uint16_t len);
// 0x12C log line at LOG_INFO (printf-style). `kiwi --update` gates on some of
// these, e.g. "to update to".
void link_log(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void link_logl(uint16_t level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
// ms timestamp of the last valid frame from the host (0 = never).
uint32_t link_last_rx_ms(void);

// ---- dispatch.c ----
void dispatch_frame(const frame_t *f);

// ---- session.c: handshake, identity, config ----
void session_init(bool watchdog_reset);
void session_connect(const uint8_t *b, uint16_t len);      // 0x32
void session_disconnect(void);                             // 0x14
void session_query(const uint8_t *b, uint16_t len);        // 0xC8
void session_config(const uint8_t *b, uint16_t len, uint8_t flags);   // 0x78
bool session_active(void);
uint16_t session_log_level(void);
