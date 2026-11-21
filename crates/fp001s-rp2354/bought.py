#!/usr/bin/env python3
"""Copy an image (ELF or flat .bin) with its IMAGE_DEF's TBYB flag cleared: a "bought" image.

The firmware's IMAGE_DEF is try-before-you-buy (src/main.rs) so a serial OTA boots it on trial. The boot ROM only
picks an unbought TBYB image on the flash-update reboot that follows an OTA, never on a normal boot, so an image
written by picotool (the factory, `cargo run`) must be the bought form. There is no hash or signature in the block,
so the one bit is all that differs (a buy by `OTA CONFIRM` makes the same change in flash).

    python3 bought.py <in> <out>
"""
import struct
import sys

START = struct.pack("<I", 0xFFFFDED3)  # PICOBIN_BLOCK_MARKER_START
TBYB = 1 << 31  # IMAGE_TYPE_TBYB (0x8000) in the image-type item's flags (<< 16)

src, dst = sys.argv[1], sys.argv[2]
b = bytearray(open(src, "rb").read())
i = b.find(START)
while i >= 0:
    w = struct.unpack_from("<I", b, i + 4)[0]
    if w & 0xFF == 0x42:  # PICOBIN_BLOCK_ITEM_1BS_IMAGE_TYPE, size 1
        break
    i = b.find(START, i + 4)
if i < 0:
    sys.exit("no IMAGE_DEF image-type item")
struct.pack_into("<I", b, i + 4, w & ~TBYB)
open(dst, "wb").write(b)
print(f"{dst}: TBYB {'cleared' if w & TBYB else 'already clear'}")
