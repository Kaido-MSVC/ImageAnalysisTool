"""Generates the TIFF variants used to check the Rust TIFF front end against C++ iat 1.0.0 (libtiff 4.7.2).

usage: py -3.13 -I tests/fixtures/tiff_variants/gen_tiff_variants.py [--out <dir>]

Standard library only. Most variants are copies of fixtures in ../generated with one field or byte
changed (see README.md in this folder for the list); six are small files written from scratch.
The expected results are whatever C++ iat 1.0.0 reports for them; tests/compare/compare_iat.py runs
both programs on these files. Output is deterministic for a given zlib (Python 3.13.x bundles zlib
1.3.1); the SHA-256 of the committed files is in ../SHA256SUMS.
"""
import argparse
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "..", "generated")

ap = argparse.ArgumentParser()
ap.add_argument("--out", default=HERE)
OUT = ap.parse_args().out
os.makedirs(OUT, exist_ok=True)

TYPE_SIZE = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8, 16: 8}


def first_ifd(b):
    """Tags of the first IFD: {tag: (type, count, value offset, values or None)}."""
    e = "<" if b[:2] == b"II" else ">"
    big = struct.unpack(e + "H", b[2:4])[0] == 43
    off = struct.unpack(e + ("Q" if big else "I"), b[8:16] if big else b[4:8])[0]
    n = struct.unpack(e + ("Q" if big else "H"), b[off:off + (8 if big else 2)])[0]
    p, es = off + (8 if big else 2), 20 if big else 12
    tags = {}
    for i in range(n):
        q = p + i * es
        tag, typ = struct.unpack(e + "HH", b[q:q + 4])
        cnt = struct.unpack(e + ("Q" if big else "I"), b[q + 4:q + 12] if big else b[q + 4:q + 8])[0]
        vo, size = q + (12 if big else 8), TYPE_SIZE.get(typ, 0) * cnt
        if size > (8 if big else 4):
            vo = struct.unpack(e + ("Q" if big else "I"), b[vo:vo + (8 if big else 4)])[0]
        fmt = {1: "B", 3: "H", 4: "I", 16: "Q"}.get(typ)
        vals = list(struct.unpack(e + fmt * cnt, b[vo:vo + size])) if fmt and cnt < 4096 else None
        tags[tag] = (typ, cnt, vo, vals)
    return e, tags


def patch(src, name, counts=None, flips=()):
    """Copy of ../generated/<src>; counts: {block: f(old byte count)}; flips: [(block, offset in block)]."""
    b = bytearray(open(os.path.join(FIX, src), "rb").read())
    e, t = first_ifd(b)
    tag = 325 if 325 in t else 279
    typ, _, vo, vals = t[tag]
    fmt = {3: "H", 4: "I", 16: "Q"}[typ]
    for i, f in (counts or {}).items():
        struct.pack_into(e + fmt, b, vo + i * struct.calcsize(fmt), f(vals[i]))
    offs = t[324 if 324 in t else 273][3]
    for i, rel in flips:
        b[offs[i] + rel] ^= 0xFF
    open(os.path.join(OUT, name), "wb").write(b)


def write_tiff(name, w, h, spp, bits, photo, rows, comp=1, rps=None, fill=None, extrasamples=None, predictor=None,
               endian="<"):
    """Classic TIFF, one IFD, strips; rows are the stored (already differenced) row bytes."""
    rps = rps or h
    strips = [b"".join(rows[y:y + rps]) for y in range(0, h, rps)]
    if comp == 8:
        strips = [zlib.compress(s) for s in strips]
    tags = [(256, 4, [w]), (257, 4, [h]), (258, 3, [bits] * spp), (259, 3, [comp]), (262, 3, [photo]),
            (273, 4, [0] * len(strips)), (277, 3, [spp]), (278, 4, [rps]), (279, 4, [len(s) for s in strips]),
            (284, 3, [1])]
    if fill:
        tags.append((266, 3, [fill]))
    if predictor:
        tags.append((317, 3, [predictor]))
    if extrasamples is not None:
        tags.append((338, 3, extrasamples))
    tags.sort()
    blob, offs = b"", []
    for s in strips:
        offs.append(8 + len(blob))
        blob += s + (b"\0" if len(s) % 2 else b"")
    tags = [(t, ty, offs if t == 273 else v) for t, ty, v in tags]
    ifd_off = 8 + len(blob)
    ext, ext_base, ent = b"", ifd_off + 2 + 12 * len(tags) + 4, b""
    for t, ty, v in tags:
        raw = struct.pack(endian + {3: "H", 4: "I"}[ty] * len(v), *v)
        if len(raw) <= 4:
            field = raw.ljust(4, b"\0")
        else:
            field = struct.pack(endian + "I", ext_base + len(ext))
            ext += raw
        ent += struct.pack(endian + "HHI", t, ty, len(v)) + field
    head = (b"II*\0" if endian == "<" else b"MM\0*") + struct.pack(endian + "I", ifd_off)
    data = head + blob + struct.pack(endian + "H", len(tags)) + ent + struct.pack(endian + "I", 0) + ext
    open(os.path.join(OUT, name), "wb").write(data)


# ---- decoder errors inside the file (the container scan accepts these; the decoder must fail like libtiff)
patch("s2_tif_rgb16_pred2_le_deflate.tif", "v_deflate_short_mid.tif", {2: lambda c: c - 20})
patch("s2_tif_rgb16_pred2_le_deflate.tif", "v_deflate_no_adler.tif", {2: lambda c: c - 4})
patch("s2_tif_rgb16_pred2_le_deflate.tif", "v_deflate_corrupt.tif", flips=[(2, 30)])
patch("s2_tif_rgb16_pred2_le_deflate.tif", "v_deflate_last_short.tif", {4: lambda c: c - 20})
patch("s2_tif_rgb16_be_strip_lzw.tif", "v_lzw_short_mid.tif", {3: lambda c: c - 100})
patch("s2_tif_rgb16_be_strip_lzw.tif", "v_lzw_corrupt.tif", flips=[(3, 400)])
patch("s2_tif_rgb16_be_strip_lzw.tif", "v_lzw_no_eoi.tif", {3: lambda c: c - 2})
patch("s2_tif_rgba8_tile_lzw.tif", "v_lzw_tile_short.tif", {1: lambda c: c - 50})
patch("s2_btf_rgb16_le_strip_packbits.tif", "v_packbits_short_mid.tif", {1: lambda c: c - 100})
patch("tif_rgb8_le_strip_packbits.tif", "v_packbits_short_last.tif", {5: lambda c: c - 3})
patch("s2_tif_rgb16_le_strip_none.tif", "v_none_short_count.tif", {2: lambda c: c // 2})
patch("s2_tif_rgb16_le_strip_none.tif", "v_none_short_count_last.tif", {5: lambda c: c - 100})

# ---- sample layouts the fixtures do not cover
W, H = 13, 7
ga = [bytes((x * 17 + y * 5) % 256 if c == 0 else (x * 31 + y) % 256 for x in range(W) for c in range(2)) for y in range(H)]
write_tiff("v_gray8a_miniswhite.tif", W, H, 2, 8, 0, ga, extrasamples=[2])
write_tiff("v_gray8a_miniswhite_expected.tif", W, H, 2, 8, 1,
           [bytes(255 - v if i % 2 == 0 else v for i, v in enumerate(r)) for r in ga], extrasamples=[2])
g16 = [b"".join(struct.pack("<H", (x * 4099 + y * 37) % 65536) for x in range(W)) for y in range(H)]
write_tiff("v_gray16_miniswhite_deflate.tif", W, H, 1, 16, 0, g16, comp=8, rps=3)
rgb = [bytes((x * 7 + y * 3 + c * 50) % 256 for x in range(W) for c in range(3)) for y in range(H)]
write_tiff("v_rgb8_fill2.tif", W, H, 3, 8, 2, rgb, fill=2)
write_tiff("v_rgb8_fill2_deflate.tif", W, H, 3, 8, 2, rgb, fill=2, comp=8, rps=2)
rev = lambda v: int("{:08b}".format(v)[::-1], 2)
write_tiff("v_rgb8_fill2_expected.tif", W, H, 3, 8, 2, [bytes(rev(v) for v in r) for r in rgb])
write_tiff("v_rgb8_none_ref.tif", W, H, 3, 8, 2, rgb)


def diff8(row, spp):
    r = bytearray(row)
    for i in range(len(r) - 1, spp - 1, -1):
        r[i] = (r[i] - r[i - spp]) % 256
    return bytes(r)


write_tiff("v_rgb8_pred2_deflate_be.tif", W, H, 3, 8, 2, [diff8(r, 3) for r in rgb], comp=8, rps=4, endian=">",
           predictor=2)
print("written to", OUT)
