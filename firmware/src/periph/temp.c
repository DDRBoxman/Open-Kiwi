// RP2040 on-die temperature sensor (ADC input 4), for KiwiState byte 9.
// Approximate: assumes a 3.3 V ADC reference and the datasheet's typical
// 0.706 V at 27 °C, -1.721 mV/°C.
#include "hardware/adc.h"
#include "periph.h"

void temp_init(void) {
    adc_init();
    adc_set_temp_sensor_enabled(true);
}

uint8_t temp_read_c(void) {
    adc_select_input(4);
    int32_t mv = (int32_t)adc_read() * 3300 / 4096;
    int32_t c = 27 - (mv - 706) * 1000 / 1721;
    if (c < 0) c = 0;
    if (c > 255) c = 255;
    return (uint8_t)c;
}
