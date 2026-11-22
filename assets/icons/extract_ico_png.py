"""Copies one PNG image out of a .ico file unchanged (all images in iat-rust.ico are PNG-compressed).

usage: py -3.13 -I assets/icons/extract_ico_png.py assets/icons/iat-rust.ico 256 assets/icons/iat-rust-256.png

The GUI window icon uses the 256x256 image; the 1254x1254 source PNG is kept as the master.
Standard library only.
"""
import struct
import sys

src, size, dst = sys.argv[1], int(sys.argv[2]), sys.argv[3]
data = open(src, "rb").read()
_, kind, count = struct.unpack("<HHH", data[:6])
if kind != 1:
    sys.exit("not an icon file")
for i in range(count):
    w, h, _, _, _, _, length, offset = struct.unpack("<BBBBHHII", data[6 + 16 * i:22 + 16 * i])
    if (w or 256) == size and (h or 256) == size:
        image = data[offset:offset + length]
        if not image.startswith(b"\x89PNG\r\n\x1a\n"):
            sys.exit("the %dx%d image is not PNG-compressed" % (size, size))
        with open(dst, "xb") as f:
            f.write(image)
        break
else:
    sys.exit("no %dx%d image" % (size, size))
