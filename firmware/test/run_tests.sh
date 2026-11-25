#!/bin/sh
# Host-side tests for the parts of the firmware that don't need hardware:
# framing (against the vectors shared with the Rust client), the 0x1A4 text
# typer, and the OTA receiver + stock bootloader apply step on simulated flash.
#
#   test/run_tests.sh            (from firmware)
set -eu
cd "$(dirname "$0")/.."
out=build/test
mkdir -p "$out"
CC=${CC:-cc}
CFLAGS="-std=c11 -Wall -Wextra -Werror -I src"

$CC $CFLAGS -o "$out/link_test" test/link_test.c src/link/frame.c
"$out/link_test" test/vectors/frames.txt

$CC $CFLAGS -o "$out/text_test" test/text_test.c src/usb/text.c
"$out/text_test"

# fwupdate_test needs an update image: wrap a random payload the same way the
# real build does.
$CC $CFLAGS -Wno-unused-function -o "$out/fwupdate_test" test/fwupdate_test.c
head -c 40000 /dev/urandom > "$out/fake_app.bin"
python3 tools/mkupdate.py "$out/fake_app.bin" "$out/fake.kfw" 2 12 > /dev/null
"$out/fwupdate_test" "$out/fake.kfw" > "$out/fwupdate_test.log" ||
    { cat "$out/fwupdate_test.log"; exit 1; }
tail -n 1 "$out/fwupdate_test.log"
