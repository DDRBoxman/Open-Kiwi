cmake_minimum_required(VERSION 3.13)

# Board config (16 MB flash, boot2 w25q080) — set before the SDK loads it.
set(PICO_BOARD kiwi)
list(APPEND PICO_BOARD_HEADER_DIRS ${CMAKE_CURRENT_LIST_DIR}/boards)

# Pull in the Pico SDK (uses PICO_SDK_PATH, e.g. ~/pico/pico-sdk).
include(pico_sdk_import.cmake)

project(open_kiwi C CXX ASM)
set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)

pico_sdk_init()

# Firmware version reported in KiwiInfo and stamped into the OTA image.
# Keep it above 1.24: the stock updater refuses anything at or below that.
set(OPEN_KIWI_FW_MAJOR 2 CACHE STRING "open-kiwi firmware major version")
set(OPEN_KIWI_FW_MINOR 12 CACHE STRING "open-kiwi firmware minor version")

set(OPEN_KIWI_SOURCES
    src/main.c
    src/link/frame.c
    src/link/link.c
    src/link/session.c
    src/link/dispatch.c
    src/usb/usb.c
    src/usb/descriptors.c
    src/usb/text.c
    src/periph/side_gpio.c
    src/periph/side_uart.c
    src/periph/led.c
    src/periph/temp.c
)

function(open_kiwi_firmware target)
    target_include_directories(${target} PRIVATE
        ${CMAKE_CURRENT_LIST_DIR}          # board/kiwi.h (pin map)
        ${CMAKE_CURRENT_LIST_DIR}/src)     # module headers + tusb_config.h
    target_compile_definitions(${target} PRIVATE
        OPEN_KIWI_FW_MAJOR=${OPEN_KIWI_FW_MAJOR}
        OPEN_KIWI_FW_MINOR=${OPEN_KIWI_FW_MINOR})
    target_compile_options(${target} PRIVATE -Wall -Wextra -Wno-unused-parameter)
    target_link_libraries(${target} pico_stdlib pico_unique_id tinyusb_device
        hardware_uart hardware_pwm hardware_adc hardware_watchdog)
    pico_add_extra_outputs(${target})
endfunction()

# --- open_kiwi: standalone image at 0x10000000 -------------------------------
# Replaces the stock bootloader too; flash it as a UF2 from BOOTSEL.
add_executable(open_kiwi ${OPEN_KIWI_SOURCES})
open_kiwi_firmware(open_kiwi)

# --- open_kiwi_ota: app at flash 0x4000 behind the STOCK bootloader ----------
# Includes the OTA receiver (src/update/fwupdate.c), so a unit running it can be
# updated again with `kiwi --update` (no BOOTSEL).
add_executable(open_kiwi_ota ${OPEN_KIWI_SOURCES} src/update/fwupdate.c)
open_kiwi_firmware(open_kiwi_ota)
target_compile_definitions(open_kiwi_ota PRIVATE KIWI_OTA=1)
target_link_libraries(open_kiwi_ota hardware_flash hardware_sync)
pico_set_linker_script(open_kiwi_ota ${CMAKE_CURRENT_LIST_DIR}/memmap_app_0x4000.ld)

# open_kiwi.kfw: the OTA image wrapped in the vendor update header, for
# `kiwi --update build/open_kiwi.kfw`.
find_package(Python3 COMPONENTS Interpreter REQUIRED)
add_custom_command(
    OUTPUT ${CMAKE_CURRENT_BINARY_DIR}/open_kiwi.kfw
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_LIST_DIR}/tools/mkupdate.py
            ${CMAKE_CURRENT_BINARY_DIR}/open_kiwi_ota.bin
            ${CMAKE_CURRENT_BINARY_DIR}/open_kiwi.kfw
            ${OPEN_KIWI_FW_MAJOR} ${OPEN_KIWI_FW_MINOR}
    DEPENDS open_kiwi_ota ${CMAKE_CURRENT_LIST_DIR}/tools/mkupdate.py
    COMMENT "Wrapping open_kiwi_ota.bin in the vendor update header")
add_custom_target(open_kiwi_kfw ALL DEPENDS ${CMAKE_CURRENT_BINARY_DIR}/open_kiwi.kfw)
