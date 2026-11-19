"""Generate PNG/TIFF fixtures with independent ground truth.

Only the Python standard library is used (struct, zlib). Every image is built
byte by byte from a sample array chosen here, and that same array is written
as a .truth file. The decoders under test never take part in producing truth.

Truth file layout (little endian):
    b"IATT" u32 version=1, u32 width, u32 height, u32 channels, u32 bits
    then width*height*channels samples, u8 when bits <= 8 else u16.
Channel order is canonical: Gray or RGB, alpha last.

Usage: python -I tools/gen_fixtures.py <out-dir> [--codec-bin <bin-dir>]
Writes <out-dir>/<name>.{png,tif} + <name>.truth and manifest.json.
With --codec-bin (the build's bin directory holding gen_codec_fixture.exe) the
stage 3 JPEG XL / WebP fixtures are generated too; without it they are skipped.
"""

import json
import os
import struct
import sys
import zlib

OUT = None
MANIFEST = []


# --------------------------------------------------------------------------
# helpers


def write_truth(name, w, h, ch, bits, samples):
    assert len(samples) == w * h * ch
    hdr = b"IATT" + struct.pack("<5I", 1, w, h, ch, bits)
    if bits <= 8:
        body = bytes(samples)
    else:
        body = struct.pack("<%dH" % len(samples), *samples)
    with open(os.path.join(OUT, name + ".truth"), "wb") as f:
        f.write(hdr + body)


def add(name, ext, data, expect, truth=None, note=""):
    path = os.path.join(OUT, name + "." + ext)
    with open(path, "wb") as f:
        f.write(data)
    entry = {"name": name, "file": name + "." + ext, "expect": expect, "note": note}
    if truth is not None:
        w, h, ch, bits, samples = truth
        write_truth(name, w, h, ch, bits, samples)
        entry["truth"] = name + ".truth"
    MANIFEST.append(entry)


# --------------------------------------------------------------------------
# PNG


def chunk(typ, payload, bad_crc=False):
    crc = zlib.crc32(typ + payload) & 0xFFFFFFFF
    if bad_crc:
        crc ^= 0x00000001
    return struct.pack(">I", len(payload)) + typ + payload + struct.pack(">I", crc)


SIG = b"\x89PNG\r\n\x1a\n"


def ihdr(w, h, depth, ctype, interlace=0):
    return chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, ctype, 0, 0, interlace))


def pack_row(values, depth):
    """Pack native codes of one row (already channel-interleaved)."""
    if depth == 16:
        return b"".join(struct.pack(">H", v) for v in values)
    if depth == 8:
        return bytes(values)
    out = bytearray()
    per = 8 // depth
    for i in range(0, len(values), per):
        byte = 0
        group = values[i:i + per]
        for j, v in enumerate(group):
            byte |= v << (8 - depth * (j + 1))
        out.append(byte)
    return bytes(out)


def idat_stream(rows, depth):
    raw = b"".join(b"\x00" + pack_row(r, depth) for r in rows)
    return zlib.compress(raw, 9)


# Adam7 pass geometry from the PNG specification (8.2), written out here
# independently of any decoder: (x start, x step, y start, y step).
ADAM7 = [(0, 8, 0, 8), (4, 8, 0, 8), (0, 4, 4, 8), (2, 4, 0, 4), (0, 2, 2, 4), (1, 2, 0, 2), (0, 1, 1, 2)]


def adam7_stream(rows, w, depth):
    """Interlaced IDAT data: each non-empty pass is a reduced image whose rows
    start with filter byte 0; rows are channel-interleaved native codes."""
    ch = len(rows[0]) // w
    raw = b""
    for xs, xi, ys, yi in ADAM7:
        cols = range(xs, w, xi)
        if not cols:
            continue
        for y in range(ys, len(rows), yi):
            vals = []
            for x in cols:
                vals += rows[y][x * ch:(x + 1) * ch]
            raw += b"\x00" + pack_row(vals, depth)
    return zlib.compress(raw, 9)


def png(w, h, depth, ctype, rows, pre=(), post=(), interlace=0, idat=None, split=0, adam7=False):
    """adam7=True writes real Adam7 data (and sets IHDR interlace=1); plain
    interlace=1 keeps non-interlaced data under the flag (malformed on purpose)."""
    if adam7:
        interlace = 1
    data = idat if idat is not None else (adam7_stream(rows, w, depth) if adam7 else idat_stream(rows, depth))
    body = SIG + ihdr(w, h, depth, ctype, interlace) + b"".join(pre)
    if split:
        for i in range(0, len(data), split):
            body += chunk(b"IDAT", data[i:i + split])
    else:
        body += chunk(b"IDAT", data)
    return body + b"".join(post) + chunk(b"IEND", b"")


def rows_of(flat, w, ch):
    return [flat[y * w * ch:(y + 1) * w * ch] for y in range(len(flat) // (w * ch))]


def gen_png():
    # 16-bit RGB: high bytes repeat, low bytes all distinct -> catches 16->8 loss.
    w, h = 5, 3
    rgb16 = []
    for y in range(h):
        for x in range(w):
            for c in range(3):
                hi = (x * 40 + c * 80) & 0xFF
                lo = (y * 61 + x * 17 + c * 5 + 1) & 0xFF
                rgb16.append((hi << 8) | lo)
    good16 = png(w, h, 16, 2, rows_of(rgb16, w, 3))
    add("png_rgb16", "png", good16, "ok", (w, h, 3, 16, rgb16),
        "16-bit RGB, low byte varies independently of high byte")

    # Same image, one sample changes only in its low bit (last pixel, last channel).
    rgb16b = list(rgb16)
    rgb16b[-1] ^= 0x0001
    add("png_rgb16_lsb", "png", png(w, h, 16, 2, rows_of(rgb16b, w, 3)), "ok",
        (w, h, 3, 16, rgb16b), "png_rgb16 with 1 LSB changed at last pixel/channel")

    # 8-bit RGBA with fully transparent pixels carrying distinct hidden RGB.
    w8, h8 = 4, 2
    rgba = []
    for y in range(h8):
        for x in range(w8):
            a = 0 if (x + y) % 2 == 0 else 255
            rgba += [(x * 50 + 7) & 0xFF, (y * 90 + 11) & 0xFF, (x * y * 30 + 200) & 0xFF, a]
    add("png_rgba8_hidden", "png", png(w8, h8, 8, 6, rows_of(rgba, w8, 4)), "ok",
        (w8, h8, 4, 8, rgba), "A=0 pixels keep distinct RGB")

    # Palette 4-bit with tRNS, odd width (row padding bits).
    pal = [(10 * i, 255 - 10 * i, (i * 37) & 0xFF) for i in range(16)]
    trns = [255, 0, 128] + [255] * 3   # entries 0..5 have alpha; others implicit 255
    w4, h4 = 7, 3
    idx = [(x + y * 3) % 16 for y in range(h4) for x in range(w4)]
    exp = []
    for i in idx:
        exp += list(pal[i]) + [trns[i] if i < len(trns) else 255]
    plte = chunk(b"PLTE", bytes(b for p in pal for b in p))
    add("png_pal4_trns", "png",
        png(w4, h4, 4, 3, [idx[y * w4:(y + 1) * w4] for y in range(h4)],
            pre=[plte, chunk(b"tRNS", bytes(trns))]),
        "ok", (w4, h4, 4, 8, exp), "4-bit palette + tRNS expanded to RGBA8 by explicit rule")

    # 2-bit gray, native codes kept (no scaling), width 13 -> padding.
    w2, h2 = 13, 2
    g2 = [(x + y) % 4 for y in range(h2) for x in range(w2)]
    add("png_gray2", "png", png(w2, h2, 2, 0, rows_of(g2, w2, 1)), "ok",
        (w2, h2, 1, 2, g2), "2-bit gray native codes 0..3")

    # 1-bit gray, width 13.
    g1 = [(x * 3 + y) % 2 for y in range(h2) for x in range(w2)]
    add("png_gray1", "png", png(w2, h2, 1, 0, rows_of(g1, w2, 1)), "ok",
        (w2, h2, 1, 1, g1), "1-bit gray native codes 0..1")

    # 16-bit gray with tRNS key -> GA16 (alpha 0 where sample == key, else 65535).
    key = 0x1234
    wg, hg = 3, 2
    gg = [0x1234, 0x1235, 0xFFFF, 0x0000, 0x1234, 0x3412]
    ga = []
    for v in gg:
        ga += [v, 0 if v == key else 0xFFFF]
    add("png_gray16_trns", "png",
        png(wg, hg, 16, 0, rows_of(gg, wg, 1), pre=[chunk(b"tRNS", struct.pack(">H", key))]),
        "ok", (wg, hg, 2, 16, ga), "16-bit gray + tRNS key expanded to GA16")

    # 8-bit RGB with tRNS key -> RGBA8.
    k = (1, 2, 3)
    wr, hr = 2, 2
    px = [(1, 2, 3), (1, 2, 4), (255, 255, 255), (1, 2, 3)]
    flat = [c for p in px for c in p]
    exp = []
    for p in px:
        exp += list(p) + [0 if p == k else 255]
    add("png_rgb8_trns", "png",
        png(wr, hr, 8, 2, rows_of(flat, wr, 3), pre=[chunk(b"tRNS", struct.pack(">HHH", *k))]),
        "ok", (wr, hr, 4, 8, exp), "RGB8 + tRNS key expanded to RGBA8")

    # --- structural errors (truth = the intact image, decode must not succeed)
    t16 = (w, h, 3, 16, rgb16)
    idat_off = good16.index(b"IDAT") - 4
    idat_len = struct.unpack(">I", good16[idat_off:idat_off + 4])[0]
    add("png_trunc_idat", "png", good16[:idat_off + 8 + idat_len // 2], "decode_error", t16,
        "file cut in the middle of IDAT")
    add("png_no_iend", "png", good16[:-12], "decode_error", t16,
        "IEND chunk missing (file ends after IDAT)")
    add("png_trunc_iend", "png", good16[:-6], "decode_error", t16,
        "file cut inside IEND")
    bad = bytearray(good16)
    crc_pos = idat_off + 8 + idat_len
    bad[crc_pos + 3] ^= 0x01
    add("png_crc_idat", "png", bytes(bad), "decode_error", t16, "IDAT CRC wrong")
    rows16 = rows_of(rgb16, w, 3)
    add("png_crc_text_after_idat", "png",
        png(w, h, 16, 2, rows16, post=[chunk(b"tEXt", b"Comment\x00hello", bad_crc=True)]),
        "decode_error", t16, "ancillary tEXt after IDAT with wrong CRC")
    add("png_trailing", "png", good16 + b"TRAILING", "ok_trailing", t16,
        "8 bytes after IEND")
    zt = zlib.compress(b"x" * 400, 9)
    add("png_ztxt_trunc_after_idat", "png",
        png(w, h, 16, 2, rows16, post=[chunk(b"zTXt", b"Comment\x00\x00" + zt[:len(zt) // 2])]),
        "metadata_error", t16,
        "zTXt after IDAT with truncated deflate stream (CVE-2026-46675 path)")
    zs = bytearray(zlib.compress(b"".join(b"\x00" + pack_row(r, 16) for r in rows16), 9))
    zs[-1] ^= 0xFF
    add("png_bad_adler", "png", png(w, h, 16, 2, rows16, idat=bytes(zs)), "decode_error", t16,
        "zlib Adler-32 wrong")
    add("png_split_idat", "png", png(w, h, 16, 2, rows16, split=7), "ok", t16,
        "IDAT split into 7-byte chunks")
    add("png_interlaced", "png", png(w, h, 16, 2, rows16, interlace=1, idat=b""),
        "unsupported", None, "Adam7 flag set (stage 1 rejects interlaced)")


# --------------------------------------------------------------------------
# TIFF


def lzw_encode(data):
    """TIFF LZW (MSB first). Clears before 9-bit codes run out, so all codes stay 9 bits."""
    out = []
    bitbuf = 0
    nbits = 0

    def emit(code):
        nonlocal bitbuf, nbits
        bitbuf = (bitbuf << 9) | code
        nbits += 9
        while nbits >= 8:
            nbits -= 8
            out.append((bitbuf >> nbits) & 0xFF)

    table = {bytes([i]): i for i in range(256)}
    nxt = 258
    emit(256)
    w = b""
    for b in data:
        wc = w + bytes([b])
        if wc in table:
            w = wc
            continue
        emit(table[w])
        table[wc] = nxt
        nxt += 1
        w = bytes([b])
        if nxt >= 510:
            emit(256)
            table = {bytes([i]): i for i in range(256)}
            nxt = 258
    if w:
        emit(table[w])
    emit(257)
    if nbits:
        out.append((bitbuf << (8 - nbits)) & 0xFF)
    return bytes(out)


def packbits_encode(data):
    out = bytearray()
    i = 0
    while i < len(data):
        run = 1
        while i + run < len(data) and run < 128 and data[i + run] == data[i]:
            run += 1
        if run >= 3:
            out.append((257 - run) & 0xFF)
            out.append(data[i])
            i += run
            continue
        j = i
        while j < len(data) and j - i < 128:
            if j + 2 < len(data) and data[j] == data[j + 1] == data[j + 2]:
                break
            j += 1
        out.append(j - i - 1)
        out += data[i:j]
        i = j
    return bytes(out)


COMP = {"none": 1, "lzw": 5, "deflate": 8, "packbits": 32773}


def compress(kind, raw):
    if kind == "none":
        return raw
    if kind == "lzw":
        return lzw_encode(raw)
    if kind == "deflate":
        return zlib.compress(raw, 9)
    return packbits_encode(raw)


def sample_bytes(vals, bits, bo):
    if bits == 8:
        return bytes(vals)
    return struct.pack(bo + "%dH" % len(vals), *vals)


def tiff(w, h, ch, bits, samples, bo="<", comp="none", tile=None, rows_per_strip=None,
         big=False, bad_offset=False, ifd_first=False, tags=(), photometric=None, extra_samples=2,
         comp_code=None, next_ifd=None, planar=1, sample_format=None, predictor=1):
    """Build a contiguous TIFF. samples is canonical (row-major, interleaved).

    ifd_first=True places the IFD right after the header and the image data at the
    end of the file, so cutting the file truncates image data but keeps the IFD.
    tags: extra (tag, type, values) entries; type 7 (UNDEFINED) / 2 (ASCII) take bytes.
    photometric: override (default RGB for 3/4 channels, MinIsBlack otherwise).
    extra_samples: ExtraSamples value for 2/4-channel images (2 unassociated, 1 associated, 0 unspecified).
    comp_code: declared Compression value (data still written as `comp`), to fake codecs.
    next_ifd: "second" appends a second IFD (multi-page), "dangling" points past the end.
    planar / sample_format: declared only (for unsupported-layout cases).
    predictor: 2 applies horizontal differencing (per row, per channel, modulo 2^bits) to
    contiguous strips before compression, as TIFF 6.0 section 14 specifies; strips only.
    """
    assert predictor == 1 or (predictor == 2 and not tile and planar == 1)
    mask = (1 << bits) - 1
    blocks = []
    if tile:
        tw, tl = tile
        pad = [0xAB if bits == 8 else 0xABCD]   # padding garbage
        for ty in range(0, h, tl):
            for tx in range(0, w, tw):
                vals = []
                n = min(tw, w - tx)              # valid columns in this tile
                for y in range(ty, ty + tl):
                    if y < h:
                        base = (y * w + tx) * ch
                        vals += samples[base:base + n * ch]
                        vals += pad * ((tw - n) * ch)
                    else:
                        vals += pad * (tw * ch)
                blocks.append(compress(comp, sample_bytes(vals, bits, bo)))
    else:
        rps = rows_per_strip or h
        for plane in (range(ch) if planar == 2 else [None]):   # planar 2: one strip set per channel
            for y0 in range(0, h, rps):
                y1 = min(h, y0 + rps)
                vals = samples[y0 * w * ch:y1 * w * ch]
                if plane is not None:
                    vals = vals[plane::ch]
                if predictor == 2:
                    vals = list(vals)
                    for r0 in range(0, len(vals), w * ch):
                        for i in range(r0 + w * ch - 1, r0 + ch - 1, -1):   # right to left: uses original left neighbour
                            vals[i] = (vals[i] - vals[i - ch]) & mask
                blocks.append(compress(comp, sample_bytes(vals, bits, bo)))

    hdr_len = 16 if big else 8
    counts = [len(b) for b in blocks]

    def layout(data_start):
        data = bytearray()
        offsets = []
        for b in blocks:
            offsets.append(data_start + len(data))
            data += b
            if len(data) % 2:
                data += b"\x00"
        return offsets, bytes(data)

    def build_ifd(ifd_pos, offsets, next_ptr=0):
        entries = [
            (256, 4, [w]), (257, 4, [h]), (258, 3, [bits] * ch), (259, 3, [comp_code or COMP[comp]]),
            (262, 3, [photometric if photometric is not None else (2 if ch >= 3 else 1)]), (277, 3, [ch]),
            (284, 3, [planar]),
        ]
        if ch in (2, 4) and extra_samples is not None:
            entries.append((338, 3, [extra_samples]))  # ExtraSamples (2 = unassociated alpha)
        if sample_format is not None:
            entries.append((339, 3, [sample_format] * ch))
        if predictor != 1:
            entries.append((317, 3, [predictor]))
        entries += list(tags)
        if bad_offset:
            offsets = [v + 10_000_000 for v in offsets]
        num_type = 16 if big else 4
        if tile:
            entries += [(322, 4, [tile[0]]), (323, 4, [tile[1]]),
                        (324, num_type, offsets), (325, num_type, counts)]
        else:
            entries += [(273, num_type, offsets), (278, 4, [rows_per_strip or h]),
                        (279, num_type, counts)]
        entries.sort()
        fmt = {3: "H", 4: "I", 16: "Q"}
        inline = 8 if big else 4
        ifd_size = (8 if big else 2) + (20 if big else 12) * len(entries) + (8 if big else 4)
        extra = bytearray()
        extra_pos = ifd_pos + ifd_size
        ifd = struct.pack(bo + ("Q" if big else "H"), len(entries))
        for tag, typ, vals in entries:
            payload = bytes(vals) if typ in (2, 7) else struct.pack(bo + fmt[typ] * len(vals), *vals)
            if len(payload) <= inline:
                val = payload + b"\x00" * (inline - len(payload))
            else:
                val = struct.pack(bo + ("Q" if big else "I"), extra_pos + len(extra))
                extra += payload
                if len(extra) % 2:
                    extra += b"\x00"
            ifd += struct.pack(bo + ("HHQ" if big else "HHI"), tag, typ, len(vals)) + val
        ifd += struct.pack(bo + ("Q" if big else "I"), next_ptr)
        return bytes(ifd + extra)

    if ifd_first:
        ifd_pos = hdr_len
        ifd_len = len(build_ifd(ifd_pos, [0] * len(blocks)))
        offsets, data = layout(hdr_len + ifd_len)
        body = build_ifd(ifd_pos, offsets) + data
    else:
        offsets, data = layout(hdr_len)
        ifd_pos = hdr_len + len(data)
        first = build_ifd(ifd_pos, offsets)
        if next_ifd == "second":
            end = ifd_pos + len(first)
            second_pos = end + (end % 2)
            first = build_ifd(ifd_pos, offsets, second_pos)   # same length, new next pointer
            first += b"\x00" * (second_pos - end) + build_ifd(second_pos, offsets)
        elif next_ifd == "dangling":
            first = build_ifd(ifd_pos, offsets, 0x7FFFFFF0)
        body = data + first
    magic = b"II" if bo == "<" else b"MM"
    if big:
        header = magic + struct.pack(bo + "HHHQ", 43, 8, 0, ifd_pos)
    else:
        header = magic + struct.pack(bo + "HI", 42, ifd_pos)
    return header + body


def tiff_samples(w, h, ch, bits, seed):
    m = (1 << bits) - 1
    return [((x * 37 + y * 101 + c * 7919 + seed) * 2654435761 >> 7) & m
            for y in range(h) for x in range(w) for c in range(ch)]


def gen_tiff():
    w, h = 20, 18
    cases = [
        ("tif_rgb16_le_strip_none", dict(bo="<", comp="none", rows_per_strip=5), 3, 16),
        ("tif_rgb16_be_strip_none", dict(bo=">", comp="none", rows_per_strip=5), 3, 16),
        ("tif_rgb16_le_strip_lzw", dict(bo="<", comp="lzw", rows_per_strip=4), 3, 16),
        ("tif_rgb16_be_strip_deflate", dict(bo=">", comp="deflate", rows_per_strip=7), 3, 16),
        ("tif_rgb8_le_strip_packbits", dict(bo="<", comp="packbits", rows_per_strip=3), 3, 8),
        ("tif_gray16_be_tile_none", dict(bo=">", comp="none", tile=(16, 16)), 1, 16),
        ("tif_rgb16_le_tile_deflate", dict(bo="<", comp="deflate", tile=(16, 16)), 3, 16),
        ("tif_rgba8_le_tile_lzw", dict(bo="<", comp="lzw", tile=(16, 16)), 4, 8),
        ("tif_gray8_le_strip_none", dict(bo="<", comp="none", rows_per_strip=18), 1, 8),
        ("btf_rgb16_le_strip_deflate", dict(bo="<", comp="deflate", rows_per_strip=6, big=True), 3, 16),
        ("btf_rgb16_be_tile_none", dict(bo=">", comp="none", tile=(16, 16), big=True), 3, 16),
    ]
    for i, (name, kw, ch, bits) in enumerate(cases):
        s = tiff_samples(w, h, ch, bits, i * 13 + 1)
        add(name, "tif", tiff(w, h, ch, bits, s, **kw), "ok", (w, h, ch, bits, s),
            ", ".join("%s=%s" % kv for kv in kw.items()))

    s = tiff_samples(w, h, 3, 16, 99)
    full = tiff(w, h, 3, 16, s, bo="<", comp="none", rows_per_strip=5)
    head = tiff(w, h, 3, 16, s, bo="<", comp="none", rows_per_strip=5, ifd_first=True)
    add("tif_trunc_strip_data", "tif", head[:-100], "decode_error", (w, h, 3, 16, s),
        "IFD intact, last strip cut by 100 bytes")
    sd = tiff(w, h, 3, 16, s, bo="<", comp="deflate", rows_per_strip=5, ifd_first=True)
    add("tif_trunc_deflate_strip", "tif", sd[:-40], "decode_error", (w, h, 3, 16, s),
        "IFD intact, last deflate strip cut by 40 bytes")
    st = tiff(w, h, 3, 16, s, bo=">", comp="none", tile=(16, 16), ifd_first=True)
    add("tif_trunc_tile", "tif", st[:-64], "decode_error", (w, h, 3, 16, s),
        "IFD intact, last tile cut by 64 bytes")
    add("tif_bad_strip_offset", "tif",
        tiff(w, h, 3, 16, s, bo="<", comp="none", rows_per_strip=5, bad_offset=True),
        "decode_error", (w, h, 3, 16, s), "strip offsets point past EOF")
    add("tif_trunc_no_ifd", "tif", full[:len(full) // 2], "open_error", None,
        "file cut before the IFD")
    big_s = tiff_samples(64, 64, 3, 16, 7)
    add("tif_deflate_big_strip", "tif",
        tiff(64, 64, 3, 16, big_s, bo="<", comp="deflate"), "ok", (64, 64, 3, 16, big_s),
        "one 24 KiB deflate strip; used to trigger libtiff memory limits")


# --------------------------------------------------------------------------
# Stage 1: compare pairs with hand-specified expectations

PAIRS = []
SINGLES = []


def icc_profile(white_x, space=b"RGB "):
    """Minimal ICC v2 display profile built from scratch (header + wtpt)."""
    def s15(v):
        return struct.pack(">i", int(round(v * 65536)))
    tag_data = b"XYZ " + b"\0" * 4 + s15(white_x) + s15(1.0) + s15(0.8249)
    tag_off = 128 + 4 + 12
    size = tag_off + len(tag_data)
    hdr = bytearray(128)
    struct.pack_into(">I", hdr, 0, size)
    hdr[8:12] = b"\x02\x10\x00\x00"
    hdr[12:16] = b"mntr"
    hdr[16:20] = space
    hdr[20:24] = b"XYZ "
    hdr[36:40] = b"acsp"
    hdr[68:80] = s15(0.9642) + s15(1.0) + s15(0.8249)  # D50 illuminant
    table = struct.pack(">I", 1) + b"wtpt" + struct.pack(">II", tag_off, len(tag_data))
    return bytes(hdr) + table + tag_data


def iccp(profile):
    # zlib level 0 (stored): libpng 1.6.59 rejects iCCP payloads shorter than
    # 81 + 11 bytes ("too short"), which a compressed 164-byte profile would be.
    return chunk(b"iCCP", b"synthetic\x00\x00" + zlib.compress(profile, 0))


SRGB = chunk(b"sRGB", b"\x00")


def exif_orientation_chunk(value):
    ifd = struct.pack("<H", 1) + struct.pack("<HHIHH", 0x0112, 3, 1, value, 0) + struct.pack("<I", 0)
    return chunk(b"eXIf", b"II*\x00" + struct.pack("<I", 8) + ifd)


def save(name, data):
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)
    return name


def pair(name, a, b, exit_code, strict, codes=(), note="", check=None, args=()):
    PAIRS.append({"name": name, "original": a, "output": b, "exit": exit_code, "strict": strict,
                  "codes": list(codes), "note": note, "check": check or {}, "args": list(args)})


def big_exif(orientation, total):
    """Little-endian TIFF with one Orientation entry, zero-padded to `total` bytes."""
    e = b"II*\x00" + struct.pack("<I", 8) + struct.pack("<H", 1) + struct.pack("<HHIHH", 0x0112, 3, 1, orientation, 0) + struct.pack("<I", 0)
    return e + b"\x00" * (total - len(e))


OVER_1MIB = (1 << 20) + 64   # just over --limits metadata_item_mib=1


def gen_pairs():
    w, h = 5, 3
    rgb16 = [((x * 40 + c * 80) & 0xFF) << 8 | ((y * 61 + x * 17 + c * 5 + 1) & 0xFF)
             for y in range(h) for x in range(w) for c in range(3)]
    rows16 = rows_of(rgb16, w, 3)
    a = save("s1_rgb16_srgb.png", png(w, h, 16, 2, rows16, pre=[SRGB]))
    b = save("s1_rgb16_srgb_split.png", png(w, h, 16, 2, rows16, pre=[SRGB], split=5))
    pair("equal_other_container", a, b, 0, "match", note="same samples, IDAT split differently")

    lsb = list(rgb16); lsb[-1] ^= 1
    pair("one_lsb_last_pixel", a, save("s1_rgb16_srgb_lsb.png", png(w, h, 16, 2, rows_of(lsb, w, 3), pre=[SRGB])),
         1, "mismatch", ["samples_differ"], "last pixel, channel B, 1 LSB",
         {"pixels_differing": "1", "first": [4, 2], "channel": "B", "max_abs": 1})

    low = [v ^ 0x00A5 if i % 7 == 0 else v for i, v in enumerate(rgb16)]
    pair("low_byte_only_16bit", a, save("s1_rgb16_srgb_lowbyte.png", png(w, h, 16, 2, rows_of(low, w, 3), pre=[SRGB])),
         1, "mismatch", ["samples_differ"], "only low bytes differ; an 8-bit path would miss this",
         {"max_abs_below": 256})

    w8, h8 = 4, 2
    rgba = []
    for y in range(h8):
        for x in range(w8):
            al = 0 if (x + y) % 2 == 0 else 255
            rgba += [(x * 50 + 7) & 0xFF, (y * 90 + 11) & 0xFF, (x * y * 30 + 200) & 0xFF, al]
    ra = save("s1_rgba8_srgb.png", png(w8, h8, 8, 6, rows_of(rgba, w8, 4), pre=[SRGB]))
    hid = list(rgba); hid[0] ^= 0x01   # pixel (0,0) has A=0; change hidden R by 1
    pair("hidden_rgb_under_alpha0", ra, save("s1_rgba8_srgb_hidden.png", png(w8, h8, 8, 6, rows_of(hid, w8, 4), pre=[SRGB])),
         1, "mismatch", ["samples_differ"], "RGB under A=0 changed", {"first": [0, 0], "channel": "R"})
    alp = list(rgba); alp[7] = 254
    pair("alpha_changed", ra, save("s1_rgba8_srgb_alpha.png", png(w8, h8, 8, 6, rows_of(alp, w8, 4), pre=[SRGB])),
         1, "mismatch", ["samples_differ"], "alpha 255->254 at (1,0)", {"first": [1, 0], "channel": "A"})

    icc1, icc2 = icc_profile(0.9642), icc_profile(0.9505)
    ia = save("s1_rgb16_icc1.png", png(w, h, 16, 2, rows16, pre=[iccp(icc1)]))
    pair("icc_equal", ia, save("s1_rgb16_icc1_b.png", png(w, h, 16, 2, rows16, pre=[iccp(icc1)], split=9)), 0, "match")
    pair("icc_missing_on_output", ia, save("s1_rgb16_noicc.png", png(w, h, 16, 2, rows16)),
         2, "indeterminate", ["icc_missing_on_output"])
    pair("icc_differs", ia, save("s1_rgb16_icc2.png", png(w, h, 16, 2, rows16, pre=[iccp(icc2)])),
         2, "indeterminate", ["icc_differs"])
    pair("icc_plus_low_priority_chrm", ia,
         save("s1_rgb16_icc1_chrm.png", png(w, h, 16, 2, rows16,
                                           pre=[iccp(icc1), chunk(b"cHRM", struct.pack(">8I", 31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000))])),
         0, "match", ["low_priority_color_signal_differs"], "D4: iCCP outranks cHRM")
    pair("no_color_both", "png_rgb16.png", save("s1_rgb16_plain_copy.png", png(w, h, 16, 2, rows16, split=4)),
         2, "indeterminate", ["color_unknown"])
    cicp = chunk(b"cICP", bytes([1, 13, 0, 1]))
    pair("cicp_equal", save("s1_rgb16_cicp.png", png(w, h, 16, 2, rows16, pre=[cicp])),
         save("s1_rgb16_cicp_b.png", png(w, h, 16, 2, rows16, pre=[cicp], split=3)), 0, "match")

    rgb8 = [(v >> 8) for v in rgb16]
    r8 = save("s1_rgb8_srgb.png", png(w, h, 8, 2, rows_of(rgb8, w, 3), pre=[SRGB]))
    opaque = []
    for i in range(0, len(rgb8), 3):
        opaque += rgb8[i:i + 3] + [255]
    pair("rgb_vs_rgba_opaque", r8, save("s1_rgba8_opaque.png", png(w, h, 8, 6, rows_of(opaque, w, 4), pre=[SRGB])),
         0, "match", ["alpha_channel_added"], "D3")
    trans = list(opaque); trans[-1] = 254
    pair("rgb_vs_rgba_not_opaque", r8, save("s1_rgba8_254.png", png(w, h, 8, 6, rows_of(trans, w, 4), pre=[SRGB])),
         1, "mismatch", ["samples_differ"], "D3: one alpha below max", {"channel": "A"})
    pair("bit_depth_16_to_8", a, r8, 1, "mismatch", ["bit_depth_changed"])
    gray = [rgb8[i] for i in range(0, len(rgb8), 3)]
    pair("gray_vs_rgb", save("s1_gray8_srgb.png", png(w, h, 8, 0, rows_of(gray, w, 1), pre=[SRGB])),
         save("s1_rgb8_from_gray.png", png(w, h, 8, 2, rows_of([g for g in gray for _ in range(3)], w, 3), pre=[SRGB])),
         1, "mismatch", ["sample_model_changed"])
    pair("geometry_differs", a, save("s1_rgb16_3x5.png", png(h, w, 16, 2, rows_of(rgb16, h, 3), pre=[SRGB])),
         1, "mismatch", ["geometry_differs"])
    pair("orientation_differs", a, save("s1_rgb16_exif6.png", png(w, h, 16, 2, rows16, pre=[SRGB, exif_orientation_chunk(6)])),
         1, "mismatch", ["orientation_differs"])

    full = png(w, h, 16, 2, rows16, pre=[SRGB])
    off = full.index(b"IDAT") - 4
    ln = struct.unpack(">I", full[off:off + 4])[0]
    pair("truncated_output", a, save("s1_trunc.png", full[:off + 8 + ln // 2]), 3, "not_evaluated", ["truncated"])
    bad = bytearray(full); bad[off + 8 + ln + 3] ^= 1
    pair("crc_error_output", a, save("s1_crc.png", bytes(bad)), 3, "not_evaluated", ["crc_error"])
    pair("missing_iend_output", a, save("s1_noiend.png", full[:-12]), 3, "not_evaluated", ["missing_end_marker"])
    # Stage 2: Adam7 is supported, so a file that only sets the flag over
    # non-interlaced data is now a decode failure (stage 1 expected exit 2).
    pair("interlace_flag_without_adam7_data", a, save("s1_interlaced.png", png(w, h, 16, 2, rows16, pre=[SRGB], interlace=1)),
         3, "not_evaluated", ["decode_failed"])
    actl = chunk(b"acTL", struct.pack(">II", 1, 0))
    pair("apng_output", a, save("s1_apng.png", png(w, h, 16, 2, rows16, pre=[SRGB, actl])),
         2, "not_evaluated", ["animation_not_supported"])
    pair("trailing_data_output", a, save("s1_trailing.png", full + b"EXTRA"), 2, "indeterminate", ["trailing_data"])
    zt = zlib.compress(b"x" * 400, 9)
    pair("broken_ztxt_output", a,
         save("s1_ztxt_broken.png", png(w, h, 16, 2, rows16, pre=[SRGB], post=[chunk(b"zTXt", b"Comment\x00\x00" + zt[:len(zt) // 2])])),
         0, "match", ["metadata_parse_failed"], "metadata failure is reported but does not change sample/strict result")
    gry = [(x + y) % 4 for y in range(2) for x in range(13)]
    pair("gray2_vs_gray8", save("s1_gray2.png", png(13, 2, 2, 0, rows_of(gry, 13, 1), pre=[SRGB])),
         save("s1_gray8_codes.png", png(13, 2, 8, 0, rows_of(gry, 13, 1), pre=[SRGB])), 1, "mismatch", ["bit_depth_changed"],
         "native low-bit codes are not rescaled")

    # palette + tRNS vs explicit RGBA8 with the same expanded samples
    pal = [(10 * i, 255 - 10 * i, (i * 37) & 0xFF) for i in range(16)]
    trns = [255, 0, 128] + [255] * 3
    w4, h4 = 7, 3
    idx = [(x + y * 3) % 16 for y in range(h4) for x in range(w4)]
    exp = []
    for i in idx:
        exp += list(pal[i]) + [trns[i] if i < len(trns) else 255]
    plte = chunk(b"PLTE", bytes(v for p in pal for v in p))
    pair("palette_trns_vs_rgba",
         save("s1_pal4_trns_srgb.png", png(w4, h4, 4, 3, [idx[y * w4:(y + 1) * w4] for y in range(h4)], pre=[SRGB, plte, chunk(b"tRNS", bytes(trns))])),
         save("s1_rgba8_from_pal.png", png(w4, h4, 8, 6, rows_of(exp, w4, 4), pre=[SRGB])), 0, "match")
    key = 0x1234
    gg = [0x1234, 0x1235, 0xFFFF, 0x0000, 0x1234, 0x3412]
    ga = []
    for v in gg:
        ga += [v, 0 if v == key else 0xFFFF]
    pair("gray16_trns_vs_ga16",
         save("s1_gray16_trns_srgb.png", png(3, 2, 16, 0, rows_of(gg, 3, 1), pre=[SRGB, chunk(b"tRNS", struct.pack(">H", key))])),
         save("s1_ga16_srgb.png", png(3, 2, 16, 4, rows_of(ga, 3, 2), pre=[SRGB])), 0, "match")

    # ---- Stage 4 gaps (capability matrix rows without an e2e case before stage 4)
    # Gray 1/4-bit: native codes are compared, so one changed code differs by exactly 1
    # (a path rescaling to 8 bits would report 255 or 17).
    for depth, wg in ((1, 11), (4, 9)):
        codes = [(x * 3 + y * 5) % (1 << depth) for y in range(3) for x in range(wg)]
        chg = list(codes); chg[-1] ^= 1
        pair("gray%d_one_code_differs" % depth,
             save("s4_gray%d.png" % depth, png(wg, 3, depth, 0, rows_of(codes, wg, 1), pre=[SRGB])),
             save("s4_gray%d_changed.png" % depth, png(wg, 3, depth, 0, rows_of(chg, wg, 1), pre=[SRGB])),
             1, "mismatch", ["samples_differ"], "last pixel, native code +-1",
             {"pixels_differing": "1", "first": [wg - 1, 2], "max_abs": 1})
    # RGB8 + tRNS colour key -> RGBA8 with A=0 exactly where RGB equals the key.
    key8 = (0x12, 0x34, 0x56)
    px8 = [key8 if (x + y) % 3 == 0 else ((x * 40) & 0xFF, (y * 70 + 3) & 0xFF, (x * y * 9) & 0xFF) for y in range(3) for x in range(5)]
    rgbk, rgbak = [], []
    for p_ in px8:
        rgbk += list(p_)
        rgbak += list(p_) + [0 if p_ == key8 else 255]
    pair("rgb8_trns_vs_rgba8",
         save("s4_rgb8_trns.png", png(5, 3, 8, 2, rows_of(rgbk, 5, 3), pre=[SRGB, chunk(b"tRNS", struct.pack(">3H", *key8))])),
         save("s4_rgba8_from_key.png", png(5, 3, 8, 6, rows_of(rgbak, 5, 4), pre=[SRGB])), 0, "match")
    # zlib Adler-32 of the IDAT stream is wrong (last byte flipped).
    zs = bytearray(idat_stream(rows16, 16)); zs[-1] ^= 0xFF   # last byte belongs to the Adler-32 trailer
    pair("adler32_error_output", a, save("s4_bad_adler.png", png(w, h, 16, 2, rows16, pre=[SRGB], idat=bytes(zs))),
         3, "not_evaluated", ["decode_failed"], "zlib Adler-32 mismatch (deflate data itself intact)",
         {"message_contains": "incorrect data check"})
    # CRC error in an ancillary chunk after IDAT (libpng would only warn for some ancillary chunks).
    pair("ancillary_crc_after_idat_output", a,
         save("s4_text_crc_after_idat.png", png(w, h, 16, 2, rows16, pre=[SRGB], post=[chunk(b"tEXt", b"Comment\x00x", bad_crc=True)])),
         3, "not_evaluated", ["crc_error"])
    # Conflicting colour signals in one file: cICP outranks a contradicting iCCP (D4).
    pair("cicp_outranks_conflicting_iccp", save("s1_rgb16_cicp.png", png(w, h, 16, 2, rows16, pre=[cicp])),
         save("s4_rgb16_cicp_plus_other_iccp.png", png(w, h, 16, 2, rows16, pre=[cicp, iccp(icc1)])),
         0, "match", ["low_priority_color_signal_differs"], "D4: cICP > iCCP; the iCCP difference is a warning only")

    # eXIf over the metadata budget on both sides, orientations 1 and 6: orientation is unknown,
    # so strict must not report a match (D21).
    pair("png_exif_over_limit_orientation_differs",
         save("s4_rgb16_exif1_over_1mib.png", png(w, h, 16, 2, rows16, pre=[SRGB, chunk(b"eXIf", big_exif(1, OVER_1MIB))])),
         save("s4_rgb16_exif6_over_1mib.png", png(w, h, 16, 2, rows16, pre=[SRGB, chunk(b"eXIf", big_exif(6, OVER_1MIB))])),
         2, "indeterminate", ["orientation_unknown"], "D21", {"orientation_source": "unknown"}, ["--limits", "metadata_item_mib=1"])
    # Same with a VP8L WebP (hand-written bitstream) carrying an over-budget EXIF chunk.
    vl = vp8l_flat(5, 3, (255, 1, 2, 3), False)
    pair("webp_exif_over_limit_orientation_differs",
         save("s4_webp_exif1_over_1mib.webp", riff([vp8x(5, 3, 0x08), (b"VP8L", vl), (b"EXIF", big_exif(1, OVER_1MIB))])),
         save("s4_webp_exif6_over_1mib.webp", riff([vp8x(5, 3, 0x08), (b"VP8L", vl), (b"EXIF", big_exif(6, OVER_1MIB))])),
         2, "indeterminate", ["orientation_unknown"], "D21", {"orientation_source": "unknown"}, ["--limits", "metadata_item_mib=1"])

    # Single-file inspect expectations
    SINGLES.extend([
        {"name": "inspect_ok", "file": a, "exit": 0},
        {"name": "inspect_truncated", "file": "s1_trunc.png", "exit": 3},
        {"name": "inspect_interlace_flag_only", "file": "s1_interlaced.png", "exit": 3},
        {"name": "inspect_trailing", "file": "s1_trailing.png", "exit": 2},
        {"name": "inspect_broken_ztxt", "file": "s1_ztxt_broken.png", "exit": 2},
        # Stage 3 decodes WebP, so a RIFF/WEBP header without any chunk is a structure error.
        {"name": "inspect_webp_no_image_chunk",
         "file": save("s2_webp_magic_only.webp", b"RIFF" + struct.pack("<I", 4) + b"WEBP"), "exit": 3},
        {"name": "inspect_unknown_format", "file": save("s2_unknown_format.bin", b"not an image\n"), "exit": 2},
    ])

    # PNG keywords are Latin-1 (0xE9 = e-acute): shown as UTF-8, hashed as the raw bytes.
    # values_sha256 = first 16 hex of SHA-256(raw keyword + hex SHA-256 of the value).
    kw, val = b"Caf\xe9", b"latte"
    vh = hashlib_sha(kw + hashlib_sha(val).encode())[:16]
    SINGLES.extend([
        {"name": "png_text_keyword_latin1",
         "file": save("s4_text_keyword_latin1.png", png(w, h, 16, 2, rows16, pre=[SRGB, chunk(b"tEXt", kw + b"\x00" + val)])),
         "exit": 0, "meta": {"text": "entries=1 keys=[tEXt:Café] values_sha256=" + vh}},
        {"name": "png_ztxt_broken_keyword_latin1",
         "file": save("s4_ztxt_broken_keyword_latin1.png",
                      png(w, h, 16, 2, rows16, pre=[SRGB], post=[chunk(b"zTXt", b"R\xe9sum\xe9\x00\x00" + zt[:len(zt) // 2])])),
         "exit": 2, "codes": ["metadata_parse_failed"], "message_contains": "'Résumé'"},
    ])

    # One very wide row (2,000,000 x 1 RGB16 = 12 MB per row) for budget tests.
    wide = 2_000_000
    raw = b"\x00" + b"\x00" * (wide * 6)
    save("s1_wide_rgb16_srgb.png", SIG + ihdr(wide, 1, 16, 2) + SRGB + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


# --------------------------------------------------------------------------
# Stage 2: Adam7


def gen_adam7_pairs():
    w, h = 5, 3
    rgb16 = [((x * 40 + c * 80) & 0xFF) << 8 | ((y * 61 + x * 17 + c * 5 + 1) & 0xFF)
             for y in range(h) for x in range(w) for c in range(3)]
    plain = "s1_rgb16_srgb.png"
    a7 = save("s2_adam7_rgb16_srgb.png", png(w, h, 16, 2, rows_of(rgb16, w, 3), pre=[SRGB], adam7=True))
    pair("adam7_vs_plain", plain, a7, 0, "match", ["interlace_differs"], "5x3: passes 3 and 7 empty")
    lsb = list(rgb16); lsb[-1] ^= 1
    pair("adam7_one_lsb_last_pixel", plain,
         save("s2_adam7_rgb16_lsb.png", png(w, h, 16, 2, rows_of(lsb, w, 3), pre=[SRGB], adam7=True)),
         1, "mismatch", ["samples_differ"], "last pixel (pass 5), channel B, 1 LSB",
         {"pixels_differing": "1", "first": [4, 2], "channel": "B", "max_abs": 1})

    # 37x29 RGBA16: every pass non-empty, A=0 pixels keep hidden RGB.
    W, H = 37, 29
    base = []
    for y in range(H):
        for x in range(W):
            rgb = [((x * 131 + y * 17 + c * 4099) * 2654435761 >> 9) & 0xFFFF for c in range(3)]
            base += rgb + [0 if (x * 3 + y) % 5 == 0 else 0xFFFF - x]
    rows = rows_of(base, W, 4)
    pa = save("s2_rgba16_37x29_plain.png", png(W, H, 16, 6, rows, pre=[SRGB]))
    pb = save("s2_rgba16_37x29_adam7.png", png(W, H, 16, 6, rows, pre=[SRGB], adam7=True))
    pc = save("s2_rgba16_37x29_adam7_split.png", png(W, H, 16, 6, rows, pre=[SRGB], adam7=True, split=97))
    pair("adam7_all_passes_vs_plain", pa, pb, 0, "match", ["interlace_differs"])
    pair("adam7_vs_adam7", pb, pc, 0, "match", note="both sides through temp storage")
    # One change in each of four different passes; expected in raster order.
    #   (4,0) pass 2, (1,2) pass 6, (3,5) pass 7, (36,28) pass 3   (1-based pass numbers)
    d = list(base)
    for (x, y, c, delta) in ((4, 0, 0, 1), (1, 2, 3, 0x0100), (3, 5, 2, 0x0001), (36, 28, 1, 0x00FF)):
        i = (y * W + x) * 4 + c
        d[i] ^= delta
    pair("adam7_diffs_in_four_passes", pa,
         save("s2_rgba16_37x29_adam7_diff.png", png(W, H, 16, 6, rows_of(d, W, 4), pre=[SRGB], adam7=True)),
         1, "mismatch", ["samples_differ"], "coordinates must map back from each pass",
         {"pixels_differing": "4", "coords": [[4, 0], [1, 2], [3, 5], [36, 28]]})

    # 1x1: passes 2..7 are empty.
    pair("adam7_1x1", save("s2_gray8_1x1_plain.png", png(1, 1, 8, 0, [[77]], pre=[SRGB])),
         save("s2_gray8_1x1_adam7.png", png(1, 1, 8, 0, [[77]], pre=[SRGB], adam7=True)), 0, "match")
    # 2-bit gray: pass rows are packed at the pass width.
    gry = [(x + y) % 4 for y in range(2) for x in range(13)]
    pair("adam7_gray2", "s1_gray2.png",
         save("s2_gray2_adam7.png", png(13, 2, 2, 0, rows_of(gry, 13, 1), pre=[SRGB], adam7=True)), 0, "match")
    # 4-bit palette + tRNS, interlaced, against explicit RGBA8.
    pal = [(10 * i, 255 - 10 * i, (i * 37) & 0xFF) for i in range(16)]
    trns = [255, 0, 128] + [255] * 3
    w4, h4 = 7, 3
    idx = [(x + y * 3) % 16 for y in range(h4) for x in range(w4)]
    plte = chunk(b"PLTE", bytes(v for p in pal for v in p))
    pair("adam7_palette_trns_vs_rgba",
         save("s2_pal4_trns_adam7.png", png(w4, h4, 4, 3, [idx[y * w4:(y + 1) * w4] for y in range(h4)],
                                            pre=[SRGB, plte, chunk(b"tRNS", bytes(trns))], adam7=True)),
         "s1_rgba8_from_pal.png", 0, "match")
    # Truncated inside the interlaced data.
    full = png(W, H, 16, 6, rows, pre=[SRGB], adam7=True)
    off = full.index(b"IDAT") - 4
    ln = struct.unpack(">I", full[off:off + 4])[0]
    pair("adam7_truncated_output", pa, save("s2_adam7_trunc.png", full[:off + 8 + ln // 2]), 3, "not_evaluated", ["truncated"])
    SINGLES.extend([
        {"name": "inspect_adam7", "file": pb, "exit": 0},
        {"name": "inspect_adam7_truncated", "file": "s2_adam7_trunc.png", "exit": 3},
    ])


# --------------------------------------------------------------------------
# Stage 2: TIFF / BigTIFF pairs


def gen_tiff_pairs():
    W, H = 37, 29   # not a multiple of 16: tiles carry padding in both directions
    s16 = [(((x * 40 + c * 80 + y * 3) & 0xFF) << 8) | ((y * 61 + x * 17 + c * 5 + 1) & 0xFF)
           for y in range(H) for x in range(W) for c in range(3)]
    icc1 = icc_profile(0.9642)
    save("s2_icc1.icc", icc1)   # also used by the large-image generators
    ICC = (34675, 7, icc1)
    png_ref = save("s2_rgb16_icc1.png", png(W, H, 16, 2, rows_of(s16, W, 3), pre=[iccp(icc1)]))
    variants = [
        ("s2_tif_rgb16_le_strip_none.tif", dict(bo="<", comp="none", rows_per_strip=5)),
        ("s2_tif_rgb16_be_strip_lzw.tif", dict(bo=">", comp="lzw", rows_per_strip=4)),
        ("s2_tif_rgb16_le_tile_deflate.tif", dict(bo="<", comp="deflate", tile=(16, 16))),
        ("s2_tif_rgb16_be_tile_none.tif", dict(bo=">", comp="none", tile=(16, 32))),
        ("s2_btf_rgb16_le_strip_packbits.tif", dict(bo="<", comp="packbits", rows_per_strip=7, big=True)),
        ("s2_btf_rgb16_be_tile_deflate.tif", dict(bo=">", comp="deflate", tile=(32, 16), big=True)),
    ]
    for fname, kw in variants:
        save(fname, tiff(W, H, 3, 16, s16, tags=[ICC], **kw))
        pair("png_vs_" + fname[3:-4], png_ref, fname, 0, "match", note="cross-format 16-bit, same ICC bytes")
    pair("tiff_tile_vs_strip", "s2_tif_rgb16_le_tile_deflate.tif", "s2_tif_rgb16_be_strip_lzw.tif", 0, "match",
         note="different byte order, layout and compression")
    pair("adam7_png_vs_bigtiff_tile", save("s2_rgb16_icc1_adam7.png", png(W, H, 16, 2, rows_of(s16, W, 3), pre=[iccp(icc1)], adam7=True)),
         "s2_btf_rgb16_be_tile_deflate.tif", 0, "match", note="TIFF has no interlace field, so no interlace_differs")

    # Predictor 2 (horizontal differencing, computed here) and the old Adobe Deflate code 32946.
    pair("png_vs_tif_predictor2_le_deflate", png_ref,
         save("s2_tif_rgb16_pred2_le_deflate.tif", tiff(W, H, 3, 16, s16, comp="deflate", rows_per_strip=6, predictor=2, tags=[ICC])),
         0, "match", note="16-bit horizontal differencing undone by libtiff")
    pair("png_vs_tif_predictor2_be_lzw", png_ref,
         save("s2_tif_rgb16_pred2_be_lzw.tif", tiff(W, H, 3, 16, s16, bo=">", comp="lzw", rows_per_strip=29, predictor=2, tags=[ICC])),
         0, "match", note="big-endian 16-bit differencing")
    pair("png_vs_tif_deflate_32946", png_ref,
         save("s2_tif_rgb16_deflate32946.tif", tiff(W, H, 3, 16, s16, comp="deflate", comp_code=32946, rows_per_strip=5, tags=[ICC])),
         0, "match", note="Compression 32946 (old Adobe Deflate code)")

    # Differences next to tile padding and in the low byte only.
    d = list(s16); d[-1] ^= 1    # (36,28) channel B: last pixel, in the padded corner tile
    pair("tiff_tile_last_pixel_lsb", png_ref, save("s2_tif_rgb16_tile_lsb.tif", tiff(W, H, 3, 16, d, comp="deflate", tile=(16, 16), tags=[ICC])),
         1, "mismatch", ["samples_differ"], "padding must not hide or create differences",
         {"pixels_differing": "1", "first": [36, 28], "channel": "B", "max_abs": 1})
    low = [v ^ 0x005A if i % 11 == 0 else v for i, v in enumerate(s16)]
    pair("tiff_low_byte_only_be", png_ref, save("s2_tif_rgb16_be_lowbyte.tif", tiff(W, H, 3, 16, low, bo=">", rows_per_strip=3, tags=[ICC])),
         1, "mismatch", ["samples_differ"], "byte-swapped 16-bit: only low bytes differ", {"max_abs_below": 256})

    # Colour decisions (user-confirmed: no ICC in TIFF means unknown; sRGB chunk vs ICC stays unverified).
    pair("png_srgb_vs_tiff_icc_unverified", save("s2_rgb16_srgb.png", png(W, H, 16, 2, rows_of(s16, W, 3), pre=[SRGB])),
         "s2_tif_rgb16_le_strip_none.tif", 2, "indeterminate", ["color_unverified"],
         "sRGB chunk vs embedded ICC: equivalence cannot be proven from bytes")
    noicc = save("s2_tif_rgb16_noicc.tif", tiff(W, H, 3, 16, s16, rows_per_strip=8))
    pair("tiff_no_icc_both", noicc, save("s2_tif_rgb16_noicc_tile.tif", tiff(W, H, 3, 16, s16, tile=(16, 16))),
         2, "indeterminate", ["color_unknown"])
    pair("png_icc_vs_tiff_no_icc", png_ref, noicc, 2, "indeterminate", ["icc_missing_on_output"])

    # MinIsWhite gray8 vs PNG gray8 holding max - v (a GRAY profile: libpng rejects RGB ICC on gray).
    g = [((x * 7 + y * 11) * 37) & 0xFF for y in range(H) for x in range(W)]
    iccg = icc_profile(0.9642, b"GRAY")
    pair("tiff_miniswhite_vs_png_gray",
         save("s2_tif_gray8_miniswhite.tif", tiff(W, H, 1, 8, [255 - v for v in g], photometric=0, rows_per_strip=6, tags=[(34675, 7, iccg)])),
         save("s2_gray8_iccgray.png", png(W, H, 8, 0, rows_of(g, W, 1), pre=[iccp(iccg)])), 0, "match",
         note="MinIsWhite inverted to MinIsBlack at the same depth")

    # Alpha: straight with hidden RGB under A=0, then associated / unspecified.
    rgba = []
    for y in range(H):
        for x in range(W):
            rgba += [(x * 50 + 7) & 0xFF, (y * 90 + 11) & 0xFF, (x * y * 30 + 200) & 0xFF, 0 if (x + y) % 3 == 0 else 255 - x]
    p_rgba = save("s2_rgba8_icc1.png", png(W, H, 8, 6, rows_of(rgba, W, 4), pre=[iccp(icc1)]))
    pair("tiff_rgba8_straight_vs_png", p_rgba, save("s2_tif_rgba8_tile_lzw.tif", tiff(W, H, 4, 8, rgba, comp="lzw", tile=(16, 16), tags=[ICC])),
         0, "match", note="A=0 pixels carry distinct RGB in both files")
    hid = list(rgba); hid[0] ^= 0x01   # (0,0) has A=0
    pair("tiff_hidden_rgb_under_alpha0", p_rgba, save("s2_tif_rgba8_hidden.tif", tiff(W, H, 4, 8, hid, rows_per_strip=10, tags=[ICC])),
         1, "mismatch", ["samples_differ"], check={"first": [0, 0], "channel": "R"})
    assoc = save("s2_tif_rgba8_assoc.tif", tiff(W, H, 4, 8, rgba, extra_samples=1, rows_per_strip=10, tags=[ICC]))
    pair("tiff_associated_alpha", assoc, assoc, 2, "indeterminate", note="associated alpha never gives a strict match")
    pair("tiff_unspecified_alpha", p_rgba, save("s2_tif_rgba8_extra0.tif", tiff(W, H, 4, 8, rgba, extra_samples=0, rows_per_strip=10, tags=[ICC])),
         2, "indeterminate")

    # Orientation tag.
    pair("tiff_orientation_differs", png_ref, save("s2_tif_rgb16_orient6.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC, (274, 3, [6])])),
         1, "mismatch", ["orientation_differs"])

    # Outside the support matrix: reported, never decoded.
    pair("tiff_jpeg_compression_not_checked", png_ref,
         save("s2_tif_rgb8_jpeg_declared.tif", tiff(W, H, 3, 16, s16, comp="none", comp_code=7, rows_per_strip=5, tags=[ICC])),
         2, "not_evaluated", ["compression_not_supported"], "JPEG-in-TIFF (code 7) is outside the supported set")
    pair("tiff_lzma_compression_not_checked", png_ref,
         save("s2_tif_rgb16_lzma_declared.tif", tiff(W, H, 3, 16, s16, comp="none", comp_code=34925, rows_per_strip=5, tags=[ICC])),
         2, "not_evaluated", ["compression_not_supported"])
    pair("tiff_multipage", png_ref, save("s2_tif_rgb16_two_pages.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], next_ifd="second")),
         2, "not_evaluated", ["multi_image_not_supported"])
    pair("tiff_separate_planes", png_ref, save("s2_tif_rgb16_planar2.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], planar=2)),
         2, "not_evaluated", ["unsupported_sample_model"])
    pair("tiff_float_samples", png_ref, save("s2_tif_rgb16_float.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], sample_format=3)),
         2, "not_evaluated", ["unsupported_sample_model"])
    # Stage 4 gaps: remaining "unsupported" rows of the capability matrix (declared layouts, never decoded).
    pair("tiff_subifd", png_ref, save("s4_tif_rgb16_subifd.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC, (330, 4, [8])])),
         2, "not_evaluated", ["multi_image_not_supported"], "SubIFDs tag 330 present",
         {"message_contains": "SubIFDs"})
    pair("tiff_signed_samples", png_ref, save("s4_tif_rgb16_signed.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], sample_format=2)),
         2, "not_evaluated", ["unsupported_sample_model"], check={"message_contains": "SampleFormat 2"})
    s16x4 = [v for i in range(W * H) for v in s16[i * 3:i * 3 + 3] + [0x8000]]
    pair("tiff_cmyk", png_ref, save("s4_tif_cmyk16.tif", tiff(W, H, 4, 16, s16x4, rows_per_strip=5, photometric=5, extra_samples=None)),
         2, "not_evaluated", ["unsupported_sample_model"], "Photometric 5 (separated / CMYK)",
         {"message_contains": "Photometric 5"})
    pair("tiff_ycbcr", png_ref, save("s4_tif_ycbcr16.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, photometric=6)),
         2, "not_evaluated", ["unsupported_sample_model"], "Photometric 6 (YCbCr)",
         {"message_contains": "Photometric 6"})
    s16x5 = [v for i in range(W * H) for v in s16[i * 3:i * 3 + 3] + [0xFFFF, 0x1234]]
    pair("tiff_non_alpha_extra_sample", png_ref,
         save("s4_tif_rgba16_plus_extra.tif", tiff(W, H, 5, 16, s16x5, rows_per_strip=5, tags=[ICC, (338, 3, [2, 0])])),
         2, "not_evaluated", ["unsupported_sample_model"], "RGB + alpha + one unspecified extra sample (SamplesPerPixel 5)",
         {"message_contains": "SamplesPerPixel 5"})

    # Structure errors.
    full = tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], ifd_first=True)
    pair("tiff_truncated_strip", png_ref, save("s2_tif_trunc_strip.tif", full[:-300]), 3, "not_evaluated", ["truncated"])
    tt = tiff(W, H, 3, 16, s16, comp="deflate", tile=(16, 16), tags=[ICC], ifd_first=True)
    pair("tiff_truncated_tile", png_ref, save("s2_tif_trunc_tile.tif", tt[:-50]), 3, "not_evaluated", ["truncated"])
    pair("tiff_strip_offset_outside", png_ref, save("s2_tif_bad_offset.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], bad_offset=True)),
         3, "not_evaluated", ["structure_error"])
    ifd_last = tiff(W, H, 3, 16, s16, rows_per_strip=5)   # IFD written after the image data
    pair("tiff_ifd_missing", png_ref, save("s2_tif_no_ifd.tif", ifd_last[:len(ifd_last) // 2]),
         3, "not_evaluated", ["structure_error"], "header points to an IFD past the end of the file")
    pair("tiff_dangling_next_ifd", png_ref, save("s2_tif_dangling_next.tif", tiff(W, H, 3, 16, s16, rows_per_strip=5, tags=[ICC], next_ifd="dangling")),
         3, "not_evaluated", ["structure_error"])
    # Deflate stream corrupted inside one strip (container valid, codec must fail).
    good = bytearray(tiff(W, H, 3, 16, s16, comp="deflate", rows_per_strip=5, tags=[ICC], ifd_first=True))
    good[-200] ^= 0xFF
    pair("tiff_corrupt_deflate_data", png_ref, save("s2_tif_bad_deflate.tif", bytes(good)), 3, "not_evaluated", ["decode_failed"])

    # Oversized single strip (12 MB decoded) for budget tests; tiled image for the temp path.
    bw, bh = 2000, 1000
    raw = zlib.compress(b"\x00" * (bw * bh * 6), 9)
    save("s2_tif_rgb16_one_big_strip.tif", tiff_from_blocks(bw, bh, 3, 16, [raw], comp="deflate"))
    # Tile temp path: one row of 256x256 RGB16 tiles is 16 x 384 KiB = 6 MiB, more than half
    # of buffer_mib=8, so tiles go to temp. Height 300 pads the second tile row.
    tw_, th_ = 4096, 300
    big = tiff_samples(tw_, th_, 3, 16, 5)
    save("s2_tif_rgb16_4096x300_tile256.tif", tiff(tw_, th_, 3, 16, big, comp="deflate", tile=(256, 256), tags=[ICC]))
    save("s2_tif_rgb16_4096x300_strip16_be.tif", tiff(tw_, th_, 3, 16, big, bo=">", comp="none", rows_per_strip=16, tags=[ICC]))
    big[(290 * tw_ + 4000) * 3 + 1] ^= 0x0100   # (4000,290) channel G, second (padded) tile row
    save("s2_tif_rgb16_4096x300_tile256_diff.tif", tiff(tw_, th_, 3, 16, big, comp="deflate", tile=(256, 256), tags=[ICC]))
    # 147456 tiles sharing one block: ~1.2 MB file, libtiff offset arrays ~1.1 MiB each (uint64).
    save("s2_tif_gray8_147456_shared_tiles.tif", tiff_shared_tiles(4096, 9216, 16, zlib.compress(b"\x00" * 256, 9)))
    SINGLES.extend([
        {"name": "inspect_tiff_ok", "file": "s2_tif_rgb16_le_tile_deflate.tif", "exit": 0},
        {"name": "inspect_bigtiff_ok", "file": "s2_btf_rgb16_be_tile_deflate.tif", "exit": 0},
        {"name": "inspect_tiff_jpeg_not_checked", "file": "s2_tif_rgb8_jpeg_declared.tif", "exit": 2},
        {"name": "inspect_tiff_truncated", "file": "s2_tif_trunc_strip.tif", "exit": 3},
        {"name": "inspect_tiff_multipage", "file": "s2_tif_rgb16_two_pages.tif", "exit": 2},
    ])


def tiff_from_blocks(w, h, ch, bits, blocks, comp="deflate", bo="<"):
    """Single-strip TIFF from already-compressed block bytes (used for big strips)."""
    data_start = 8
    off = data_start
    data = b"".join(blocks)
    entries = [(256, 4, [w]), (257, 4, [h]), (258, 3, [bits] * ch), (259, 3, [COMP[comp]]), (262, 3, [2]),
               (273, 4, [off]), (277, 3, [ch]), (278, 4, [h]), (279, 4, [len(data)]), (284, 3, [1])]
    ifd_pos = data_start + len(data) + (len(data) % 2)
    extra = bytearray()
    extra_pos = ifd_pos + 2 + 12 * len(entries) + 4
    ifd = struct.pack(bo + "H", len(entries))
    for tag, typ, vals in entries:
        payload = struct.pack(bo + {3: "H", 4: "I"}[typ] * len(vals), *vals)
        if len(payload) <= 4:
            val = payload + b"\x00" * (4 - len(payload))
        else:
            val = struct.pack(bo + "I", extra_pos + len(extra))
            extra += payload
        ifd += struct.pack(bo + "HHI", tag, typ, len(vals)) + val
    ifd += struct.pack(bo + "I", 0)
    return b"II" + struct.pack(bo + "HI", 42, ifd_pos) + data + b"\x00" * (len(data) % 2) + ifd + bytes(extra)


def tiff_shared_tiles(w, h, tile, block):
    """Little-endian gray8 tiled TIFF whose tiles all point at one compressed block.
    Small on disk, but libtiff's in-memory offset/bytecount arrays grow with the tile count."""
    n = ((w + tile - 1) // tile) * ((h + tile - 1) // tile)
    data_pos = 8
    offs_pos = data_pos + len(block) + (len(block) % 2)
    cnts_pos = offs_pos + 4 * n
    ifd_pos = cnts_pos + 4 * n
    entries = [(256, 4, w), (257, 4, h), (258, 3, 8), (259, 3, COMP["deflate"]), (262, 3, 1), (277, 3, 1),
               (284, 3, 1), (322, 3, tile), (323, 3, tile), (324, 4, None), (325, 4, None)]
    ifd = struct.pack("<H", len(entries))
    for tag, typ, v in entries:
        if tag == 324:
            ifd += struct.pack("<HHII", tag, typ, n, offs_pos)
        elif tag == 325:
            ifd += struct.pack("<HHII", tag, typ, n, cnts_pos)
        elif typ == 3:
            ifd += struct.pack("<HHIHH", tag, typ, 1, v, 0)
        else:
            ifd += struct.pack("<HHII", tag, typ, 1, v)
    ifd += struct.pack("<I", 0)
    return (b"II" + struct.pack("<HI", 42, ifd_pos) + block + b"\x00" * (len(block) % 2)
            + struct.pack("<%dI" % n, *([data_pos] * n)) + struct.pack("<%dI" % n, *([len(block)] * n)) + ifd)


# --------------------------------------------------------------------------
# Stage 3: JPEG XL and WebP
#
# Samples, PNG references and truth come from here. gen_codec_fixture.exe only runs
# the libjxl / libwebp *encoders* on the raw samples; it never decodes. Containers
# (JXL boxes, WebP VP8X/ICCP/EXIF) and every corruption are assembled below from the
# specifications. Hand-written VP8L files (size limits, alpha hints) bypass libwebp.

CODEC_BIN = None
REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
IMBC = os.path.join(REPO, "tests", "fixtures", "imbc")


def raw_bytes(samples, bits):
    return bytes(samples) if bits <= 8 else struct.pack("<%dH" % len(samples), *samples)


def codec(fmt, name, samples, w, h, ch, bits, *opts):
    """Encode samples with gen_codec_fixture and return the encoded bytes (not saved)."""
    import subprocess
    tmp = os.path.join(OUT, "_s3_raw")
    os.makedirs(tmp, exist_ok=True)
    raw = os.path.join(tmp, name + ".raw")
    enc = os.path.join(tmp, name + ".enc")
    with open(raw, "wb") as f:   # --float-input: samples are floats in 0..1, written as float32
        f.write(struct.pack("<%df" % len(samples), *samples) if "--float-input" in opts else raw_bytes(samples, bits))
    args = [os.path.join(CODEC_BIN, "gen_codec_fixture.exe"), fmt, raw, enc, str(w), str(h), str(ch)]
    if fmt in ("jxl", "avif"):
        args.append(str(bits))
    extra = []
    for o in opts:   # (raw samples, bits) tuples become temporary raw files
        if isinstance(o, tuple):
            p = os.path.join(tmp, "%s.%d.raw" % (name, len(extra)))
            with open(p, "wb") as f:
                f.write(raw_bytes(*o))
            extra.append(p)
            args.append(p)
        else:
            args.append(str(o))
    r = subprocess.run(args, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("gen_codec_fixture %s failed (%d): %s" % (name, r.returncode, r.stderr.strip()))
    with open(enc, "rb") as f:
        data = f.read()
    for p in [raw, enc] + extra:
        os.remove(p)
    return data


def png_icc_of(path):
    """Decompressed iCCP profile of a PNG (stdlib only)."""
    with open(path, "rb") as f:
        d = f.read()
    pos = 8
    while pos < len(d):
        n = struct.unpack(">I", d[pos:pos + 4])[0]
        t = d[pos + 4:pos + 8]
        if t == b"iCCP":
            body = d[pos + 8:pos + 8 + n]
            nul = body.index(b"\0")
            return zlib.decompress(body[nul + 2:])
        pos += 12 + n
    raise ValueError("no iCCP in " + path)


# ---- JPEG XL container (ISO/IEC 18181-2)
JXL_SIG_BOX = b"\0\0\0\x0cJXL \r\n\x87\n"


def box(typ, payload):
    return struct.pack(">I", 8 + len(payload)) + typ + payload


JXL_FTYP = box(b"ftyp", b"jxl " + b"\0\0\0\0" + b"jxl ") + box(b"jxll", b"\x0a")   # level 10 (16-bit)


def jxlp(idx, part, last):
    return box(b"jxlp", struct.pack(">I", idx | (0x80000000 if last else 0)) + part)


def tiff_exif(orientation=1):
    ifd = struct.pack("<H", 1) + struct.pack("<HHIHH", 0x0112, 3, 1, orientation, 0) + struct.pack("<I", 0)
    return b"II*\x00" + struct.pack("<I", 8) + ifd


# ---- WebP container (RIFF)
def riff(chunks, pad=True):
    body = b"WEBP"
    for tag, payload in chunks:
        body += tag + struct.pack("<I", len(payload)) + payload + (b"\0" if pad and len(payload) % 2 else b"")
    return b"RIFF" + struct.pack("<I", len(body)) + body


def vp8x(w, h, flags):
    return b"VP8X", bytes([flags, 0, 0, 0]) + (w - 1).to_bytes(3, "little") + (h - 1).to_bytes(3, "little")


def webp_image_chunk(data):
    """(tag, payload) of the image chunk in a simple-format file written by libwebp."""
    assert data[:4] == b"RIFF" and data[8:12] == b"WEBP", "not a WebP file"
    tag = data[12:16]
    n = struct.unpack("<I", data[16:20])[0]
    assert tag in (b"VP8L", b"VP8 "), "encoder wrote %r first" % tag
    return tag, data[20:20 + n]


# ---- hand-written VP8L (lossless bitstream specification; every pixel the same ARGB)
class Bits:
    def __init__(self):
        self.out = bytearray()
        self.acc = 0
        self.n = 0

    def put(self, value, nbits):          # LSB first, as VP8L is read
        for i in range(nbits):
            self.acc |= ((value >> i) & 1) << self.n
            self.n += 1
            if self.n == 8:
                self.out.append(self.acc)
                self.acc = 0
                self.n = 0

    def done(self):
        if self.n:
            self.out.append(self.acc)
        return bytes(self.out)


def vp8l_simple_code(b, symbol):
    b.put(1, 1)                 # simple code
    b.put(0, 1)                 # num_symbols - 1 = 0
    if symbol < 2:
        b.put(0, 1)             # is_first_8bits = 0 -> 1-bit symbol
        b.put(symbol, 1)
    else:
        b.put(1, 1)
        b.put(symbol, 8)


def vp8l_flat(w, h, argb, alpha_is_used):
    """Single-colour VP8L: five one-symbol prefix codes, 0 bits per pixel."""
    a, r, g, bl = argb
    b = Bits()
    b.put(0x2F, 8)
    b.put(w - 1, 14)
    b.put(h - 1, 14)
    b.put(1 if alpha_is_used else 0, 1)
    b.put(0, 3)                 # version
    b.put(0, 1)                 # no transform
    b.put(0, 1)                 # no colour cache
    b.put(0, 1)                 # no meta prefix codes
    for sym in (g, r, bl, a):   # green, red, blue, alpha
        vp8l_simple_code(b, sym)
    vp8l_simple_code(b, 0)      # distance
    return b.done()


def hashed(w, h, ch, bits, seed, alpha_zero=None):
    """Deterministic pseudo-random samples; alpha_zero(x, y) -> True puts A=0 (hidden RGB kept)."""
    m = (1 << bits) - 1
    out = []
    for y in range(h):
        for x in range(w):
            for c in range(ch):
                v = ((x * 131 + y * 17 + c * 4099 + seed) * 2654435761 >> 9) & m
                if alpha_zero and c == ch - 1:
                    v = 0 if alpha_zero(x, y) else max(1, v)
                out.append(v)
    return out


def gen_stage3():
    icc_imbc = png_icc_of(os.path.join(IMBC, "v13-icc16.png"))
    assert hashlib_sha(icc_imbc).startswith("745265d1"), "unexpected IMB-C ICC"
    icc_file = save("s3_imbc_pq.icc", icc_imbc)   # real ICC: libjxl rejects the synthetic s2_icc1
    icc_path = os.path.join(OUT, icc_file)
    other_icc = icc_profile(0.9642)

    def jxl_bare(name, samples, w, h, ch, bits, *opts):
        """Bare codestream. libjxl wraps level-10 streams (e.g. > 12-bit) in a container with
        a jxll box; the jxlc payload is taken out here so every container below is built by hand."""
        data = codec("jxl", name, samples, w, h, ch, bits, *opts)
        if data[:2] == b"\xff\x0a":
            return data
        assert data[:12] == JXL_SIG_BOX, name + ": neither codestream nor container"
        pos, cs = 0, b""
        while pos < len(data):
            n = struct.unpack(">I", data[pos:pos + 4])[0]
            t = data[pos + 4:pos + 8]
            if t == b"jxlc":
                cs += data[pos + 8:pos + n]
            elif t == b"jxlp":            # parts in index order (libjxl writes them in order)
                cs += data[pos + 12:pos + n]
            pos += n
        assert cs[:2] == b"\xff\x0a", name + ": no jxlc/jxlp codestream"
        return cs

    # ---------------- JXL: equal
    W, H = 600, 40       # wider than 256: pieces arrive per group, out of raster order
    rgb16 = hashed(W, H, 3, 16, 1)
    rows16 = rows_of(rgb16, W, 3)
    p_srgb = save("s3_rgb16_600x40_srgb.png", png(W, H, 16, 2, rows16, pre=[SRGB]))
    cs_srgb = jxl_bare("jxl_rgb16_srgb", rgb16, W, H, 3, 16, "--color", "srgb")
    write_truth("s3_jxl_rgb16_600x40_srgb", W, H, 3, 16, rgb16)
    j_srgb = save("s3_jxl_rgb16_600x40_srgb.jxl", cs_srgb)
    pair("jxl_srgb_enum_vs_png_srgb", p_srgb, j_srgb, 0, "match", ["rendering_intent_differs"],
         "JXL sRGB enum (relative) vs PNG sRGB chunk (perceptual): intent recorded only")
    pair("jxl_effort3_vs_effort7", j_srgb,
         save("s3_jxl_rgb16_600x40_srgb_e7.jxl", jxl_bare("jxl_rgb16_srgb_e7", rgb16, W, H, 3, 16, "--effort", "7")),
         0, "match", note="two JXL enums: key equality")

    w, h = 37, 29
    pq16 = hashed(w, h, 3, 16, 7)
    rpq = rows_of(pq16, w, 3)
    j_pq = save("s3_jxl_rgb16_pq.jxl", jxl_bare("jxl_rgb16_pq", pq16, w, h, 3, 16, "--color", "pq"))
    pair("jxl_pq_enum_vs_png_cicp", j_pq, save("s3_rgb16_pq_cicp.png", png(w, h, 16, 2, rpq, pre=[chunk(b"cICP", bytes([9, 16, 0, 1]))])),
         0, "match", note="PQ enum equals cICP 9/16/0/1")
    pair("jxl_pq_enum_vs_png_same_icc", j_pq, save("s3_rgb16_pq_iccp_imbc.png", png(w, h, 16, 2, rpq, pre=[iccp(icc_imbc)])),
         0, "match", note="ICC synthesized by libjxl is byte-identical to the IMB-C iCCP")
    pair("jxl_pq_enum_vs_png_other_icc", j_pq, save("s3_rgb16_pq_iccp_other.png", png(w, h, 16, 2, rpq, pre=[iccp(other_icc)])),
         2, "indeterminate", ["color_unverified"])

    rgba16 = hashed(w, h, 4, 16, 11, lambda x, y: (x + 2 * y) % 5 == 0)
    pair("jxl_embedded_icc_rgba16_vs_png", save("s3_rgba16_iccp_imbc.png", png(w, h, 16, 6, rows_of(rgba16, w, 4), pre=[iccp(icc_imbc)])),
         save("s3_jxl_rgba16_icc.jxl", jxl_bare("jxl_rgba16_icc", rgba16, w, h, 4, 16, "--color", "icc:" + icc_path)),
         0, "match", note="embedded ICC kept byte for byte; A=0 pixels keep RGB")

    g8 = hashed(w, h, 1, 8, 3)
    pair("jxl_gray8_srgb_vs_png", save("s3_gray8_srgb.png", png(w, h, 8, 0, rows_of(g8, w, 1), pre=[SRGB])),
         save("s3_jxl_gray8_srgb.jxl", jxl_bare("jxl_gray8", g8, w, h, 1, 8)), 0, "match")

    rgba8 = hashed(w, h, 4, 8, 5, lambda x, y: (x + y) % 3 == 0)
    p_rgba8 = save("s3_rgba8_srgb.png", png(w, h, 8, 6, rows_of(rgba8, w, 4), pre=[SRGB]))
    j_rgba8 = save("s3_jxl_rgba8_srgb.jxl", jxl_bare("jxl_rgba8", rgba8, w, h, 4, 8))
    pair("jxl_rgba8_hidden_rgb_vs_png", p_rgba8, j_rgba8, 0, "match", note="RGB under A=0 is compared")

    LW, LH, LX, LY = 64, 48, 10, 8
    base = hashed(LW, LH, 3, 16, 21)
    layer = hashed(20, 16, 3, 16, 22)
    comp = list(base)
    for y in range(16):
        for x in range(20):
            i = ((LY + y) * LW + LX + x) * 3
            comp[i:i + 3] = layer[(y * 20 + x) * 3:(y * 20 + x) * 3 + 3]
    pair("jxl_two_layers_vs_composite_png", save("s3_rgb16_layers_composite.png", png(LW, LH, 16, 2, rows_of(comp, LW, 3), pre=[SRGB])),
         save("s3_jxl_rgb16_two_layers.jxl", jxl_bare("jxl_layers", base, LW, LH, 3, 16, "--layer", (layer, 16), LX, LY, 20, 16)),
         0, "match", ["layered_image", "rendering_intent_differs"], "REPLACE layer 20x16 at (10,8), composited here by hand")

    # ---------------- JXL: differences
    lsb = list(rgb16); lsb[-1] ^= 1
    pair("jxl_one_lsb_last_pixel", p_srgb, save("s3_jxl_rgb16_600x40_lsb.jxl", jxl_bare("jxl_lsb", lsb, W, H, 3, 16)),
         1, "mismatch", ["samples_differ"], "last pixel, channel B, 1 LSB",
         {"pixels_differing": "1", "first": [W - 1, H - 1], "channel": "B", "max_abs": 1})
    hid = list(rgba8); hid[0] ^= 1    # (0,0) has A=0
    pair("jxl_hidden_rgb_1lsb", p_rgba8, save("s3_jxl_rgba8_hidden_lsb.jxl", jxl_bare("jxl_hid", hid, w, h, 4, 8)),
         1, "mismatch", ["samples_differ"], check={"pixels_differing": "1", "first": [0, 0], "channel": "R", "max_abs": 1})
    pair("jxl_orientation_differs", p_srgb, save("s3_jxl_rgb16_600x40_orient6.jxl", jxl_bare("jxl_or6", rgb16, W, H, 3, 16, "--orient", "6")),
         1, "mismatch", ["orientation_differs"])
    v12 = [v >> 4 for v in rgb16]
    pair("jxl_12bit_vs_png_16bit", p_srgb,
         save("s3_jxl_rgb12_600x40.jxl", jxl_bare("jxl_12", [(v * 65535 + 2047) // 4095 for v in v12], W, H, 3, 12)),
         1, "mismatch", ["bit_depth_changed"], "input scaled to 16-bit full range; libjxl stores 12-bit codes")

    # ---------------- JXL: outside the strict scope
    pair("jxl_xyb_lossy", p_srgb, save("s3_jxl_rgb16_lossy_xyb.jxl", jxl_bare("jxl_xyb", rgb16, W, H, 3, 16, "--lossy")),
         2, "not_evaluated", ["xyb_not_native"])
    pair("jxl_animation", p_srgb, save("s3_jxl_rgb16_anim.jxl", jxl_bare("jxl_anim", rgb16, W, H, 3, 16, "--anim", (lsb, 16))),
         2, "not_evaluated", ["animation_not_supported"])
    # Stage 4 gap: float and > 16-bit integer samples are rejected from the header (capability matrix).
    fl = [((i * 37) % 1000) / 1000.0 for i in range(w * h * 3)]
    pair("jxl_float32_samples", p_rgba8,save("s4_jxl_rgb_float32.jxl", jxl_bare("jxl_f32", fl, w, h, 3, 32, "--float-input", "--exp", "8")),
         2, "not_evaluated", ["unsupported_sample_model"], "codestream declares 32-bit float (exponent 8)",
         {"message_contains": "floating-point samples"})
    pair("jxl_24bit_samples", p_rgba8,save("s4_jxl_rgb_int24.jxl", jxl_bare("jxl_i24", fl, w, h, 3, 24, "--float-input")),
         2, "not_evaluated", ["unsupported_sample_model"], "codestream declares 24-bit unsigned integer",
         {"message_contains": "24-bit samples"})
    pm = []
    for i in range(0, len(rgba16), 4):
        a_ = rgba16[i + 3]
        pm += [v * a_ // 65535 for v in rgba16[i:i + 3]] + [a_]
    j_pm = save("s3_jxl_rgba16_premul.jxl", jxl_bare("jxl_pm", pm, w, h, 4, 16, "--premul"))
    pair("jxl_premultiplied_alpha", j_pm, j_pm, 2, "indeterminate", note="associated alpha never gives a strict match")
    pair("jxl_depth_extra_channel", p_srgb, save("s3_jxl_rgb16_depth.jxl", jxl_bare("jxl_depth", rgb16, W, H, 3, 16, "--depth")),
         2, "indeterminate", ["extra_channel_not_supported"])

    # ---------------- JXL: container (boxes built here)
    cs = cs_srgb
    third = len(cs) // 3
    meta = box(b"Exif", b"\0\0\0\0" + tiff_exif(1)) + box(b"xml ", b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>")
    jxlc_full = JXL_SIG_BOX + JXL_FTYP + meta + box(b"jxlc", cs)
    pair("jxl_container_jxlc_exif_xml", p_srgb, save("s3_jxl_container_jxlc_meta.jxl", jxlc_full), 0, "match",
         ["rendering_intent_differs"])
    parts = [cs[:third], cs[third:2 * third], cs[2 * third:]]
    pair("jxl_container_jxlp_three_parts", p_srgb,
         save("s3_jxl_container_jxlp3.jxl", JXL_SIG_BOX + JXL_FTYP + b"".join(jxlp(i, p, i == 2) for i, p in enumerate(parts))),
         0, "match", ["rendering_intent_differs"])
    pair("jxl_container_jxlp_last_missing", p_srgb,
         save("s3_jxl_container_jxlp_nolast.jxl", JXL_SIG_BOX + JXL_FTYP + jxlp(0, parts[0], False) + jxlp(1, parts[1], False)),
         3, "not_evaluated", ["truncated"])
    pair("jxl_container_box_past_eof", p_srgb, save("s3_jxl_container_cut.jxl", jxlc_full[:-10]), 3, "not_evaluated", ["truncated"])
    pair("jxl_container_partial_box_header", p_srgb, save("s3_jxl_container_tail3.jxl", jxlc_full + b"\0\0\0"),
         3, "not_evaluated", ["truncated"])
    pair("jxl_codestream_truncated", p_srgb, save("s3_jxl_codestream_cut.jxl", cs[:len(cs) // 2]), 3, "not_evaluated", ["truncated"])
    pair("jxl_codestream_trailing", p_srgb, save("s3_jxl_codestream_trailing.jxl", cs + b"TRAILING"), 2, "indeterminate",
         ["trailing_data"])

    # ---------------- WebP
    wr8 = rgba8                                 # 37x29, A=0 where (x+y)%3==0
    p_w_icc = save("s3_rgba8_iccp_imbc.png", png(w, h, 8, 6, rows_of(wr8, w, 4), pre=[iccp(icc_imbc)]))
    _, vl = webp_image_chunk(codec("webp", "webp_rgba8", wr8, w, h, 4, 8))
    write_truth("s3_webp_rgba8_icc", w, h, 4, 8, wr8)
    w_icc = save("s3_webp_rgba8_icc.webp", riff([vp8x(w, h, 0x30), (b"ICCP", icc_imbc), (b"VP8L", vl)]))
    pair("webp_rgba8_hidden_rgb_vs_png_same_icc", p_w_icc, w_icc, 0, "match", note="lossless + exact keeps RGB under A=0")
    rgb8 = hashed(w, h, 3, 8, 9)
    _, vl3 = webp_image_chunk(codec("webp", "webp_rgb8", rgb8, w, h, 3, 8))
    pair("webp_opaque_rgb_vs_png_rgb", save("s3_rgb8_iccp_imbc.png", png(w, h, 8, 2, rows_of(rgb8, w, 3), pre=[iccp(icc_imbc)])),
         save("s3_webp_rgb8_icc.webp", riff([vp8x(w, h, 0x20), (b"ICCP", icc_imbc), (b"VP8L", vl3)])), 0, "match",
         note="alpha_is_used=0 and all alpha 255: compared as RGB")
    w_plain = save("s3_webp_rgba8_plain.webp", riff([(b"VP8L", vl)]))
    p_plain = save("s3_rgba8_plain.png", png(w, h, 8, 6, rows_of(wr8, w, 4)))
    pair("webp_default_srgb_vs_png_no_color", p_plain, w_plain, 2, "indeterminate", ["color_unverified"],
         "D12: WebP specification default is not a declaration")
    pair("webp_vs_webp_both_default", w_plain, save("s3_webp_rgba8_plain_b.webp", riff([(b"VP8L", vl)]) + b""), 0, "match",
         note="both WebP specification defaults")
    hw = list(wr8); hw[0] ^= 1
    _, vlh = webp_image_chunk(codec("webp", "webp_hid", hw, w, h, 4, 8))
    pair("webp_hidden_rgb_1lsb", p_w_icc, save("s3_webp_rgba8_hidden_lsb.webp", riff([vp8x(w, h, 0x30), (b"ICCP", icc_imbc), (b"VP8L", vlh)])),
         1, "mismatch", ["samples_differ"], check={"pixels_differing": "1", "first": [0, 0], "channel": "R", "max_abs": 1})
    pair("webp_lossy_vp8", p_plain, save("s3_webp_rgb8_lossy.webp", codec("webp", "webp_lossy", rgb8, w, h, 3, 8, "--lossy")),
         2, "not_evaluated", ["lossy_not_supported"])
    with open(os.path.join(IMBC, "animation.webp"), "rb") as f:
        anim = save("s3_imbc_animation.webp", f.read())
    pair("webp_animation_imbc", p_plain, anim, 2, "not_evaluated", ["animation_not_supported"])

    simple = riff([(b"VP8L", vl)])
    big = bytearray(simple); big[4:8] = struct.pack("<I", len(simple) - 8 + 100)
    pair("webp_riff_size_past_eof", p_plain, save("s3_webp_riff_too_long.webp", bytes(big)), 3, "not_evaluated", ["truncated"])
    odd = vl + (b"\0" if len(vl) % 2 == 0 else b"")   # trailing zero bits are ignored by VP8L
    pair("webp_missing_pad_byte", p_plain, save("s3_webp_no_pad.webp", riff([(b"VP8L", odd)], pad=False)), 3, "not_evaluated",
         ["truncated"])
    pair("webp_vp8l_truncated", p_plain, save("s3_webp_vp8l_cut.webp", riff([(b"VP8L", vl[:len(vl) // 2])])), 3, "not_evaluated")
    pair("webp_trailing_data", p_plain, save("s3_webp_trailing.webp", simple + b"TRAILING"), 2, "indeterminate", ["trailing_data"])
    pair("webp_vp8x_icc_flag_without_iccp", p_plain, save("s3_webp_iccflag_noiccp.webp", riff([vp8x(w, h, 0x30), (b"VP8L", vl)])),
         2, "indeterminate", ["webp_container_warning"])
    fa = [1, 2, 3, 0x40] * 35
    pair("webp_alpha_hint_off_data_has_alpha", save("s3_rgba8_7x5_a40.png", png(7, 5, 8, 6, rows_of(fa, 7, 4))),
         save("s3_webp_hint_off_alpha40.webp", riff([(b"VP8L", vp8l_flat(7, 5, (0x40, 1, 2, 3), False))])),
         2, "indeterminate", ["alpha_hint_mismatch"], "header hint says opaque, data has alpha 0x40; compared as RGBA")
    pair("webp_vp8x_alpha_flag_vs_hint", save("s3_rgb8_7x5.png", png(7, 5, 8, 2, rows_of([1, 2, 3] * 35, 7, 3))),
         save("s3_webp_vp8x_alpha_hint_off.webp", riff([vp8x(7, 5, 0x10), (b"VP8L", vp8l_flat(7, 5, (255, 1, 2, 3), False))])),
         2, "indeterminate", ["alpha_hint_mismatch"])

    # ---------------- mixed JXL <-> WebP
    pair("mixed_jxl_icc_vs_webp_icc", save("s3_jxl_rgba8_icc.jxl", jxl_bare("jxl_rgba8_icc", wr8, w, h, 4, 8, "--color", "icc:" + icc_path)),
         w_icc, 0, "match", note="same embedded ICC on both sides")
    pair("mixed_jxl_srgb_enum_vs_webp_default", j_rgba8, w_plain, 2, "indeterminate", ["color_unverified"],
         "D12: JXL enum vs WebP specification default stays unverified")

    # ---------------- inspect: size policy, IMB-C samples
    flat = (255, 10, 20, 30)
    for wd, ht in ((16383, 1), (1, 16383), (16384, 1), (1, 16384)):
        f = save("s3_webp_%dx%d.webp" % (wd, ht), riff([(b"VP8L", vp8l_flat(wd, ht, flat, False))]))
        ok = max(wd, ht) <= 16383
        SINGLES.append({"name": "inspect_webp_%dx%d" % (wd, ht), "file": f, "exit": 0 if ok else 2,
                        "codes": [] if ok else ["unsupported_dimension"],
                        "dimension_policy": {"spec_max": 16384, "policy_max": 16383, "within_spec": True, "within_policy": ok}})
    for n in ("pq-original.jxl", "hlg-original.jxl"):
        with open(os.path.join(IMBC, n), "rb") as f:
            SINGLES.append({"name": "inspect_imbc_" + n.split("-")[0], "file": save("s3_imbc_" + n, f.read()), "exit": 0})
    # ---------------- IMB-C v1.3.0 GUI outputs (tests/fixtures/imbc/PROVENANCE.md; numbers = conversion list)
    def imbc(n):
        with open(os.path.join(IMBC, n), "rb") as f:
            return save("imbc_" + n, f.read())
    acc = {"pq-original.jxl": "s3_imbc_pq-original.jxl", "hlg-original.jxl": "s3_imbc_hlg-original.jxl"}
    for num, src, out, exit_code, strict, codes in (
            (1, "v13-icc16.png", "v13-icc16__jxl.jxl", 0, "match", []),
            (2, "v13-Rec2100PQ.png", "v13-Rec2100PQ__jxl.jxl", 0, "match", []),
            (3, "v13-Rec2100HLG.png", "v13-Rec2100HLG__jxl.jxl", 0, "match", []),
            (4, "v13-cicp-only-pq.png", "v13-cicp-only-pq__jxl.jxl", 2, "indeterminate", ["color_unverified"]),   # cICP -> embedded ICC
            (5, "v13-hidden.png", "v13-hidden__webp.webp", 2, "indeterminate", ["color_unverified"]),
            (6, "rgba-v12.png", "rgba-v12__webp.webp", 2, "indeterminate", ["color_unverified"]),
            (7, "rgba-v12.png", "rgba-v12__jxl.jxl", 2, "indeterminate", ["color_unverified"]),
            (8, "gray-v12.png", "gray-v12__jxl.jxl", 2, "indeterminate", ["color_unverified"]),
            (9, "rgb-v12.png", "rgb-v12__webp-lossy.webp", 2, "not_evaluated", ["lossy_not_supported"]),
            (10, "rgb-v12.png", "rgb-v12__jxl.jxl", 2, "indeterminate", ["color_unverified"]),
            (12, "pq-original.jxl", "pq-original__png.png", 0, "match", []),
            (13, "hlg-original.jxl", "hlg-original__png.png", 0, "match", []),
            (14, "icc8.png", "icc8__webp.webp", 0, "match", [])):
        a = acc.get(src) or imbc(src)
        pair("imbc_%02d_%s" % (num, out.replace(".", "_")), a, imbc(out), exit_code, strict, codes,
             "IMB-C v1.3.0 conversion #%d; samples must be equal" % num, {"samples_equal": True} if strict != "not_evaluated" else None)

    SINGLES.append({"name": "inspect_jxl_container", "file": "s3_jxl_container_jxlp3.jxl", "exit": 0})
    SINGLES.append({"name": "inspect_jxl_truncated", "file": "s3_jxl_codestream_cut.jxl", "exit": 3})
    os.rmdir(os.path.join(OUT, "_s3_raw"))


def hashlib_sha(b):
    import hashlib
    return hashlib.sha256(b).hexdigest()


# --------------------------------------------------------------------------
# Stage 5 (0.5.0): PSD / PSB written byte by byte from the Adobe file format
# specification, and AVIF through gen_codec_fixture (libavif + aom, encode only).


def psd_resource(rid, data):
    return b"8BIM" + struct.pack(">H", rid) + b"\x00\x00" + struct.pack(">I", len(data)) + data + (b"\x00" if len(data) & 1 else b"")


def psd_version_info(has_real):
    return psd_resource(0x0421, struct.pack(">IB", 1, 1 if has_real else 0) + struct.pack(">II", 0, 0) + struct.pack(">I", 1))


def psd_plane_rows(values, w, h, depth):
    """One channel (h*w values) -> list of uncompressed row bytes."""
    rows = []
    for y in range(h):
        line = values[y * w:(y + 1) * w]
        if depth == 8:
            rows.append(bytes(line))
        elif depth == 16:
            rows.append(struct.pack(">%dH" % w, *line))
        elif depth == 32:
            rows.append(struct.pack(">%df" % w, *line))
        else:   # 1-bit, MSB first, rows padded to a byte
            b = bytearray((w + 7) // 8)
            for x, v in enumerate(line):
                if v:
                    b[x >> 3] |= 0x80 >> (x & 7)
            rows.append(bytes(b))
    return rows


def psd(w, h, mode, depth, planes, version=1, comp="rle", resources=b"", layer_field=0, cmdata=b"",
        trailing=b"", rle_hook=None, comp_code=None):
    """planes: list of channels, each a flat list of h*w native sample values (spec channel order)."""
    wide = version == 2
    hdr = b"8BPS" + struct.pack(">H", version) + b"\0" * 6 + struct.pack(">HIIHH", len(planes), h, w, depth, mode)
    out = bytearray(hdr)
    out += struct.pack(">I", len(cmdata)) + cmdata
    out += struct.pack(">I", len(resources)) + resources
    # Layer and mask information: only the signed layer count (no records), then an empty global mask.
    if layer_field:
        li = struct.pack(">h", layer_field)
        li_len = struct.pack(">Q" if wide else ">I", len(li))
        body = li_len + li + struct.pack(">I", 0)
        out += struct.pack(">Q" if wide else ">I", len(body)) + body
    else:
        out += struct.pack(">Q" if wide else ">I", 0)
    rows = [psd_plane_rows(p, w, h, depth) for p in planes]
    code = comp_code if comp_code is not None else {"raw": 0, "rle": 1}[comp]
    out += struct.pack(">H", code)
    if comp == "raw":
        for ch in rows:
            for r in ch:
                out += r
    else:
        packed = [[packbits_encode(r) for r in ch] for ch in rows]
        if rle_hook:
            rle_hook(packed)
        for ch in packed:
            for r in ch:
                out += struct.pack(">I" if wide else ">H", len(r))
        for ch in packed:
            for r in ch:
                out += r
    return bytes(out) + trailing


def planes_of(flat, w, h, ch):
    return [flat[c::ch] for c in range(ch)]


def gen_stage5_psd():
    prof = icc_profile(0.9642)
    icc_res = psd_resource(0x040F, prof)
    saved = psd_version_info(True)
    w, h = 23, 7

    def png_of(name, flat, ch, bits, ctype, pre=()):
        return save(name, png(w, h, bits, ctype, rows_of(flat, w, ch), pre=list(pre)))

    # RGB 8-bit, RLE and raw, same ICC on both sides -> match.
    rgb8 = hashed(w, h, 3, 8, 51)
    p_rgb8 = png_of("s5_rgb8.png", rgb8, 3, 8, 2, [iccp(prof)])
    for comp in ("rle", "raw"):
        f = save("s5_psd_rgb8_%s.psd" % comp, psd(w, h, 3, 8, planes_of(rgb8, w, h, 3), comp=comp, resources=saved + icc_res))
        write_truth("s5_psd_rgb8_%s" % comp, w, h, 3, 8, rgb8)
        pair("psd_rgb8_%s_vs_png" % comp, f, p_rgb8, 0, "match", note="merged image, Version Info hasRealMergedData=1")
    # 16-bit: low byte matters; PSB variant with 64-bit lengths and 32-bit RLE counts.
    rgb16 = hashed(w, h, 3, 16, 52)
    p_rgb16 = png_of("s5_rgb16.png", rgb16, 3, 16, 2, [iccp(prof)])
    f16 = save("s5_psd_rgb16_rle.psd", psd(w, h, 3, 16, planes_of(rgb16, w, h, 3), resources=saved + icc_res))
    pair("psd_rgb16_rle_vs_png", f16, p_rgb16, 0, "match")
    for comp in ("rle", "raw"):
        fb = save("s5_psb_rgb16_%s.psb" % comp, psd(w, h, 3, 16, planes_of(rgb16, w, h, 3), version=2, comp=comp,
                                                     resources=saved + icc_res, layer_field=2))
        pair("psb_rgb16_%s_vs_png" % comp, fb, p_rgb16, 0, "match", note="PSB: 64-bit section lengths" + (", u32 RLE counts" if comp == "rle" else ""))
    lsb = list(rgb16)
    lsb[(4 * w + 9) * 3 + 2] ^= 1
    pair("psd_rgb16_last_bit_differs", f16, png_of("s5_rgb16_lsb.png", lsb, 3, 16, 2, [iccp(prof)]), 1, "mismatch",
         ["samples_differ"], check={"pixels_differing": "1", "first": [9, 4], "max_abs": 1})
    # Gray 8, bitmap 1-bit (1 = black) vs PNG gray 1-bit (0 = black).
    g8 = hashed(w, h, 1, 8, 53)
    pair("psd_gray8_vs_png", save("s5_psd_gray8.psd", psd(w, h, 1, 8, [g8], resources=saved + psd_resource(0x040F, icc_profile(0.9642, b"GRAY")))),
         png_of("s5_gray8.png", g8, 1, 8, 0, [iccp(icc_profile(0.9642, b"GRAY"))]), 0, "match")
    bits = hashed(w, h, 1, 1, 54)
    gray_prof = icc_profile(0.9642, b"GRAY")
    pair("psd_bitmap_vs_png_gray1", save("s5_psd_bitmap.psd", psd(w, h, 0, 1, [[1 - v for v in bits]], resources=saved + psd_resource(0x040F, gray_prof))),
         png_of("s5_gray1.png", bits, 1, 1, 0, [iccp(gray_prof)]), 0, "match", note="bitmap 1 = black inverted to gray code 0")
    # Indexed + transparent index vs PNG palette + tRNS (both expand to RGBA8).
    pal = [((i * 37) & 0xFF, (i * 91 + 7) & 0xFF, (255 - i) & 0xFF) for i in range(256)]
    idx = [v % 16 for v in hashed(w, h, 1, 8, 55)]
    cm = bytes(p[0] for p in pal) + bytes(p[1] for p in pal) + bytes(p[2] for p in pal)
    res_idx = saved + icc_res + psd_resource(0x0416, struct.pack(">H", 16)) + psd_resource(0x0417, struct.pack(">H", 3))
    f_idx = save("s5_psd_indexed.psd", psd(w, h, 2, 8, [idx], resources=res_idx, cmdata=cm))
    plte = chunk(b"PLTE", b"".join(bytes(p) for p in pal[:16]))
    trns = chunk(b"tRNS", bytes(0 if i == 3 else 255 for i in range(16)))
    pair("psd_indexed_vs_png_palette", f_idx, save("s5_pal4.png", png(w, h, 8, 3, rows_of(idx, w, 1), pre=[iccp(prof), plte, trns])),
         0, "match", note="palette + transparent index 3 vs PLTE + tRNS")
    # Merged transparency (negative layer count): matted with white -> at most indeterminate.
    rgba8 = hashed(w, h, 4, 8, 56, alpha_zero=lambda x, y: x < 2)
    f_a = save("s5_psd_rgba8_merged_alpha.psd", psd(w, h, 3, 8, planes_of(rgba8, w, h, 4), resources=saved + icc_res, layer_field=-2))
    pair("psd_merged_alpha_vs_png_rgba", f_a, png_of("s5_rgba8.png", rgba8, 4, 8, 6, [iccp(prof)]), 2, "indeterminate",
         ["alpha_semantics_differ"], check={"samples_equal": True})
    # Composite validity.
    pair("psd_composite_not_saved", save("s5_psd_rgb8_no_composite.psd", psd(w, h, 3, 8, planes_of(rgb8, w, h, 3),
                                                                             resources=psd_version_info(False) + icc_res, layer_field=3)),
         p_rgb8, 2, "not_evaluated", ["psd_composite_not_saved"])
    pair("psd_composite_flag_absent", save("s5_psd_rgb8_flag_absent.psd", psd(w, h, 3, 8, planes_of(rgb8, w, h, 3), resources=icc_res, layer_field=3)),
         p_rgb8, 0, "match", ["psd_composite_flag_absent"], note="warning only, no downgrade (user decision Q4)")
    flat = [200] * (w * h * 3)
    pair("psd_composite_uniform", save("s5_psd_rgb8_uniform.psd", psd(w, h, 3, 8, planes_of(flat, w, h, 3), resources=saved + icc_res, layer_field=1)),
         png_of("s5_rgb8_uniform.png", flat, 3, 8, 2, [iccp(prof)]), 2, "indeterminate", ["psd_composite_uniform"], check={"samples_equal": True})
    # Rejected sample models: CMYK and 32-bit float (recognised, not decoded).
    cmyk = hashed(w, h, 4, 8, 57)
    SINGLES.append({"name": "inspect_psd_cmyk8", "file": save("s5_psd_cmyk8.psd", psd(w, h, 4, 8, planes_of(cmyk, w, h, 4), resources=saved)),
                    "exit": 2, "codes": ["unsupported_sample_model"]})
    SINGLES.append({"name": "inspect_psd_cmyk16", "file": save("s5_psd_cmyk16.psd", psd(w, h, 4, 16, planes_of(hashed(w, h, 4, 16, 58), w, h, 4), resources=saved)),
                    "exit": 2, "codes": ["unsupported_sample_model"]})
    f32 = [((x + y) % 10) / 10.0 for _ in range(3) for y in range(h) for x in range(w)]
    SINGLES.append({"name": "inspect_psd_rgb32", "file": save("s5_psd_rgb32.psd", psd(w, h, 3, 32, [f32[i * w * h:(i + 1) * w * h] for i in range(3)], comp="raw", resources=saved)),
                    "exit": 2, "codes": ["unsupported_sample_model"]})
    SINGLES.append({"name": "inspect_psd_zip", "file": save("s5_psd_rgb8_zip.psd", psd(w, h, 3, 8, planes_of(rgb8, w, h, 3), comp="raw", comp_code=2, resources=saved)),
                    "exit": 2, "codes": ["compression_not_supported"]})
    SINGLES.append({"name": "inspect_psd_rgb8", "file": "s5_psd_rgb8_rle.psd", "exit": 0})
    SINGLES.append({"name": "inspect_psb_rgb16", "file": "s5_psb_rgb16_rle.psb", "exit": 0})
    # Structure errors.
    good = psd(w, h, 3, 8, planes_of(rgb8, w, h, 3), resources=saved + icc_res)
    SINGLES.append({"name": "inspect_psd_truncated", "file": save("s5_psd_truncated.psd", good[:-5]), "exit": 3, "codes": ["truncated"]})
    SINGLES.append({"name": "inspect_psd_header_only", "file": save("s5_psd_header_only.psd", good[:40]), "exit": 3})
    SINGLES.append({"name": "inspect_psd_trailing", "file": save("s5_psd_trailing.psd", good + b"JUNK"), "exit": 2, "codes": ["trailing_data"]})

    def bad_row(packed):   # replace row 3 of channel 1 by a run that decodes one byte too long
        packed[1][3] = bytes([(257 - 128) & 0xFF, 7]) * (w // 128) + bytes([(257 - (w % 128 + 1)) & 0xFF, 7])
    SINGLES.append({"name": "inspect_psd_rle_row_overflow",
                    "file": save("s5_psd_rle_bad_row.psd", psd(w, h, 3, 8, planes_of(rgb8, w, h, 3), resources=saved + icc_res, rle_hook=bad_row)),
                    "exit": 3, "codes": ["structure_error"]})
    bad_res = bytearray(good)
    struct.pack_into(">I", bad_res, 26 + 4, len(good))   # resource section length past EOF
    SINGLES.append({"name": "inspect_psd_resources_past_eof", "file": save("s5_psd_bad_resources.psd", bytes(bad_res)), "exit": 3, "codes": ["truncated"]})
    pair("psd_trailing_vs_png", "s5_psd_trailing.psd", p_rgb8, 2, "indeterminate", ["trailing_data"])


def gen_stage5_avif():
    w, h = 29, 11
    cicp = chunk(b"cICP", bytes([1, 13, 0, 1]))

    def png_of(name, flat, ch, bits, ctype):
        return save(name, png(w, h, bits, ctype, rows_of(flat, w, ch), pre=[cicp]))

    def avif(name, samples, ch, bits, *opts):
        return save(name, codec("avif", name[:-5], samples, w, h, ch, bits, *opts))

    rgb8 = hashed(w, h, 3, 8, 61)
    p_rgb8 = png_of("s5_avif_ref_rgb8.png", rgb8, 3, 8, 2)
    a_rgb8 = avif("s5_avif_rgb8_lossless.avif", rgb8, 3, 8)
    pair("avif_rgb8_lossless_vs_png", a_rgb8, p_rgb8, 0, "match", ["avif_pixel_verified_lossless"])
    SINGLES.append({"name": "inspect_avif_rgb8_lossless", "file": a_rgb8, "exit": 0, "codes": ["avif_lossless_metadata_only"]})
    rgba8 = hashed(w, h, 4, 8, 62, alpha_zero=lambda x, y: x < 3)
    pair("avif_rgba8_hidden_rgb_vs_png", avif("s5_avif_rgba8_lossless.avif", rgba8, 4, 8), png_of("s5_avif_ref_rgba8.png", rgba8, 4, 8, 6),
         0, "match", ["avif_pixel_verified_lossless"], note="A=0 columns keep their RGB")
    g8 = hashed(w, h, 1, 8, 63)
    pair("avif_gray8_lossless_vs_png", avif("s5_avif_gray8_lossless.avif", g8, 1, 8), png_of("s5_avif_ref_gray8.png", g8, 1, 8, 0), 0, "match")
    # 10 / 12-bit: compared with a same-depth lossless JXL (sRGB enum == cICP 1/13/0/1).
    for bits in (10, 12):
        v = hashed(w, h, 3, bits, 64 + bits)
        va = hashed(w, h, 4, bits, 66 + bits, alpha_zero=lambda x, y: y == 0)
        j = save("s5_jxl_rgb%d.jxl" % bits, codec("jxl", "s5_jxl_rgb%d" % bits, v, w, h, 3, bits, "--color", "srgb"))
        ja = save("s5_jxl_rgba%d.jxl" % bits, codec("jxl", "s5_jxl_rgba%d" % bits, va, w, h, 4, bits, "--color", "srgb"))
        pair("avif_rgb%d_lossless_vs_jxl" % bits, avif("s5_avif_rgb%d_lossless.avif" % bits, v, 3, bits), j, 0, "match", ["avif_pixel_verified_lossless"])
        pair("avif_rgba%d_lossless_vs_jxl" % bits, avif("s5_avif_rgba%d_lossless.avif" % bits, va, 4, bits), ja, 0, "match")
    v16 = [x << 6 for x in hashed(w, h, 3, 10, 74)]
    pair("avif_rgb10_vs_png16", "s5_avif_rgb10_lossless.avif", png_of("s5_avif_ref_rgb16.png", v16, 3, 16, 2), 1, "mismatch", ["bit_depth_changed"])
    # Lossy but metadata-consistent: the two verdicts disagree.
    pair("avif_identity_lossy_vs_png", avif("s5_avif_rgb8_identity_q50.avif", rgb8, 3, 8, "--lossy", "50"), p_rgb8, 1, "mismatch",
         ["samples_differ"], note="metadata consistent with lossless, pixels prove otherwise")
    # Metadata not consistent with lossless: not decoded (exit 2).
    for name, opts, why in [("420_bt601_q60", ("--lossy", "60", "--yuv", "420", "--matrix", "6"), "chroma_subsampled"),
                            ("444_bt601_q100", ("--matrix", "6"), "matrix_not_identity"),
                            ("444_identity_limited", ("--range", "limited"), "limited_range"),
                            ("444_premultiplied", ("--premul",), "alpha_premultiplied")]:
        ch = 4 if "premul" in name else 3
        f = avif("s5_avif_%s.avif" % name, rgba8 if ch == 4 else rgb8, ch, 8, *opts)
        pair("avif_not_lossless_%s" % name, f, p_rgb8 if ch == 3 else "s5_avif_ref_rgba8.png", 2, "not_evaluated", ["avif_not_lossless_by_metadata"],
             check={"message_contains": why})
    # Limit too small: the report names the estimate and the --limits value that fits it
    # (estimate ~48 MiB + 64 MiB reserve -> job_commit_mib=256); rerunning with it succeeds.
    pair("avif_job_limit_suggests_value", p_rgb8, a_rgb8, 4, "not_evaluated", ["resource_limit"],
         check={"message_contains": "rerun with --limits job_commit_mib=256",
                "resource_need": {"side": "output", "limit": "job_commit_mib", "suggested_value": 256,
                                  "suggested_argument": "--limits job_commit_mib=256", "current_value": 64}},
         args=["--limits", "job_commit_mib=64"])
    pair("avif_job_limit_suggested_value_works", p_rgb8, a_rgb8, 0, "match", ["avif_pixel_verified_lossless"],
         args=["--limits", "job_commit_mib=256"])
    # irot: orientation differs from the PNG.
    pair("avif_irot_vs_png", avif("s5_avif_rgb8_irot1.avif", rgb8, 3, 8, "--irot", "1"), p_rgb8, 1, "mismatch", ["orientation_differs"])
    # Container: truncated, not an AVIF brand.
    with open(os.path.join(OUT, a_rgb8), "rb") as f:
        data = f.read()
    SINGLES.append({"name": "inspect_avif_truncated", "file": save("s5_avif_truncated.avif", data[:-40]), "exit": 3, "codes": ["truncated"]})
    SINGLES.append({"name": "inspect_avif_box_past_eof", "file": save("s5_avif_box_past_eof.avif", data[:len(data) // 2]), "exit": 3})


def gen_stage5():
    gen_stage5_psd()
    if CODEC_BIN:
        gen_stage5_avif()


def main():
    global OUT, CODEC_BIN
    OUT = sys.argv[1]
    if "--codec-bin" in sys.argv:
        CODEC_BIN = sys.argv[sys.argv.index("--codec-bin") + 1]
    os.makedirs(OUT, exist_ok=True)
    gen_png()
    gen_tiff()
    gen_pairs()
    gen_adam7_pairs()
    gen_tiff_pairs()
    if CODEC_BIN:
        gen_stage3()
    else:
        print("no --codec-bin: stage 3 (JPEG XL / WebP) and stage 5 AVIF fixtures not generated")
    gen_stage5()
    with open(os.path.join(OUT, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(MANIFEST, f, ensure_ascii=False, indent=1)
    with open(os.path.join(OUT, "pairs.json"), "w", encoding="utf-8") as f:
        json.dump({"pairs": PAIRS, "singles": SINGLES}, f, ensure_ascii=False, indent=1)
    print("wrote %d fixtures and %d pairs to %s" % (len(MANIFEST), len(PAIRS), OUT))


if __name__ == "__main__":
    main()
