// GP18 status LED (PWM slice 1A). What it shows:
//   slow breathe  no host session
//   steady dim    host connected; a bright flash on each input event
//   fast blink    firmware update in progress
#include "pico/stdlib.h"
#include "hardware/pwm.h"
#include "board/kiwi.h"
#include "periph.h"

#define SESSION_LEVEL  0x2000
#define FLASH_MS       40

static uint slice, chan;
static uint32_t activity_until;
static int breathe = 0, dir = 1;
static uint32_t next_step;

void led_init(void) {
    gpio_set_function(KIWI_LED, GPIO_FUNC_PWM);
    slice = pwm_gpio_to_slice_num(KIWI_LED);
    chan = pwm_gpio_to_channel(KIWI_LED);
    pwm_set_wrap(slice, 0xFFFF);
    pwm_set_chan_level(slice, chan, 0);
    pwm_set_enabled(slice, true);
}

void led_activity(void) {
    activity_until = to_ms_since_boot(get_absolute_time()) + FLASH_MS;
}

void led_task(led_mode_t mode) {
    uint32_t now = to_ms_since_boot(get_absolute_time());
    uint16_t level;
    switch (mode) {
    case LED_UPDATING:
        level = ((now / 100) & 1) ? 0xFFFF : 0;
        break;
    case LED_SESSION:
        level = (int32_t)(activity_until - now) > 0 ? 0xFFFF : SESSION_LEVEL;
        break;
    default:
        if ((int32_t)(now - next_step) >= 0) {
            breathe += dir * 1200;
            if (breathe >= 0xFFFF) { breathe = 0xFFFF; dir = -1; }
            if (breathe <= 0)      { breathe = 0;      dir = 1; }
            next_step = now + 8;
        }
        level = (uint16_t)breathe;
        break;
    }
    pwm_set_chan_level(slice, chan, level);
}
