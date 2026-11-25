// On-board peripherals: side-port GPIO/ATX, side UART + serial-stream routing,
// status LED, die temperature.
#pragma once
#include <stdbool.h>
#include <stdint.h>

// ---- side_gpio.c: 0x227 ----
void side_gpio_init(void);
void side_gpio_task(void);                    // ends pulses
void side_gpio_cmd(const uint8_t *b, uint16_t len);
uint8_t side_gpio_levels(void);               // KiwiState.IO, 2 bits per pin
bool side_gpio_is_disabled(uint8_t index);    // mode 0 (for the KiwiConfig byte-7 fallback)
// Hand GPIO 0/1 (GP20/21) to UART1, or take them back as disabled inputs.
void side_gpio_claim_uart(bool uart);

// ---- side_uart.c: 0x1FE / 0x1FF routing ----
// vcomm/vterm: 0 disabled, 1 target USB CDC, 2 side UART (115200 8N1).
#define VPORT_DISABLED 0
#define VPORT_CDC      1
#define VPORT_SIDE     2
void vport_configure(uint8_t vcomm, uint8_t vterm, bool side_uart_fallback);
void vport_from_host(uint16_t type, const uint8_t *b, uint16_t len);
void vport_from_cdc(const uint8_t *b, uint32_t n);
// The stream (OP_VCOMM/OP_TERMINAL) the target CDC is bridged to, or 0.
uint16_t vport_cdc_stream(void);
bool side_uart_enabled(void);
void vport_task(void);

// ---- led.c: GP18 status LED ----
typedef enum {
    LED_IDLE,       // no host session: slow breathe
    LED_SESSION,    // host connected: steady, flashes on input
    LED_UPDATING,   // firmware update in progress: fast blink
} led_mode_t;
void led_init(void);
void led_task(led_mode_t mode);
void led_activity(void);

// ---- temp.c: RP2040 die temperature (approximate) ----
void temp_init(void);
uint8_t temp_read_c(void);
