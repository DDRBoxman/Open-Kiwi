#!/usr/bin/env python3
"""Wrap an app .bin (linked at flash 0x4000) in the Cytrence Kiwi update-image
header, producing a file the vendor bootloader / updater accepts.

Header format, 0x80 bytes, little-endian:
  0x00 16   magic
  0x10 u32  header format (1)
  0x14 u16  fw major
  0x16 u16  fw minor
  0x18 u32  payload size
  0x1C u32  CRC-32 (zlib/IEEE) of the payload
  0x20 c20  build label: "open-kiwi YYMMDDHHMM" (UTC), so KiwiInfo shows the
            installed image as open firmware (stock uses an ISO timestamp)
  0x35 u8   sku (2 = professional)
  0x36 u16  flash offset (0x4000)
  0x38 u32  erase sector size (0x1000)
  0x70 16   fixed constant (copied from a stock image)
  0x80 ..   payload

Usage: mkupdate.py app.bin out.kfw [major] [minor]
"""
import struct
import sys
import time
import zlib

MAGIC = bytes.fromhex("7856341221436587efcdab9009badcfe")
# Fixed 16 bytes at 0x70, identical across stock boot/standard/pro images.
TAIL = bytes.fromhex("df9b57133175b9fdce8a46022064a8ec")
SKU_PRO = 2
FLASH_OFFSET = 0x4000
SECTOR = 0x1000

def main():
    app = open(sys.argv[1], "rb").read()
    out = sys.argv[2]
    major = int(sys.argv[3]) if len(sys.argv) > 3 else 2
    minor = int(sys.argv[4]) if len(sys.argv) > 4 else 11

    hdr = bytearray(0x80)
    hdr[0x00:0x10] = MAGIC
    struct.pack_into("<I", hdr, 0x10, 1)                       # header format
    struct.pack_into("<HH", hdr, 0x14, major, minor)          # version
    struct.pack_into("<I", hdr, 0x18, len(app))               # payload size
    struct.pack_into("<I", hdr, 0x1C, zlib.crc32(app))        # payload CRC-32
    label = time.strftime("open-kiwi %y%m%d%H%M", time.gmtime()).encode()
    assert len(label) <= 20
    hdr[0x20:0x20 + len(label)] = label
    hdr[0x35] = SKU_PRO
    struct.pack_into("<H", hdr, 0x36, FLASH_OFFSET)
    struct.pack_into("<I", hdr, 0x38, SECTOR)
    hdr[0x70:0x80] = TAIL

    open(out, "wb").write(bytes(hdr) + app)
    print(f"{out}: v{major}.{minor} \"{label.decode()}\", payload {len(app)} bytes, "
          f"crc {zlib.crc32(app):08X}, sku {SKU_PRO}, flash +0x{FLASH_OFFSET:04X}")

if __name__ == "__main__":
    main()
