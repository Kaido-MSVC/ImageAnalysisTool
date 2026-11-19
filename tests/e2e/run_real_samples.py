"""Real-sample checks for 0.5.0 (PSD / PSB / AVIF), standard library only.

Usage:
  python -I tests/e2e/run_real_samples.py <bin-dir> <samples-dir> <imbc-magick.exe> <out-dir> [--full-avif]

<samples-dir> (the user's test images) and the IMB-C runtime are READ ONLY: every file in the
samples directory and in the magick.exe directory tree is fingerprinted (size, mtime, SHA-256)
before and after the run, and any change fails the run. All outputs (reports, the cropped
reference PNG, AVIF files written by gen_codec_fixture and by IMB-C's ImageMagick, ImageMagick's
pixel-cache temporaries) go to <out-dir>.

What runs:
  1. iat inspect on every .psd / .psb.
  2. PSD/PSB merged image vs a PNG decoded by IMB-C's ImageMagick ("psd:<file>[0]"): an
     independent decoder of the same composite.
  3. AVIF from the largest .png: a top-left crop decoded here (zlib + PNG filters, no codec) is
     the truth; gen_codec_fixture writes lossless 8-bit / 10-bit / 8-bit+alpha and lossy AVIF;
     IMB-C's magick writes AVIF with IMB-C's current settings and with an identity matrix.
  4. --full-avif: the whole PNG as lossless AVIF via IMB-C's magick, compared with the PNG.
"""

import hashlib
import json
import os
import struct
import subprocess
import sys
import time
import zlib

BIN, SAMPLES, MAGICK, OUT = (os.path.abspath(a) for a in sys.argv[1:5])   # magick runs with cwd=OUT
FULL = "--full-avif" in sys.argv
IAT = os.path.join(BIN, "iat.exe")
GEN = os.path.join(BIN, "gen_codec_fixture.exe")
os.makedirs(OUT, exist_ok=True)
TMP = os.path.join(OUT, "magick-tmp")
os.makedirs(TMP, exist_ok=True)
results = []


def record(name, ok, detail, **extra):
    results.append({"name": name, "pass": bool(ok), "detail": detail, **extra})
    print("%-5s %-44s %s" % ("PASS" if ok else "FAIL", name, detail), flush=True)


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 22), b""):
            h.update(b)
    return h.hexdigest()


def tree_fingerprint(root):
    fp = {}
    for d, _, files in os.walk(root):
        for n in files:
            p = os.path.join(d, n)
            st = os.stat(p)
            fp[os.path.relpath(p, root)] = (st.st_size, st.st_mtime_ns, sha(p))
    return fp


def iat(args, timeout=3600):
    t0 = time.perf_counter()
    p = subprocess.run([IAT, *args], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout)
    return p, time.perf_counter() - t0


def report(name, *args, limits=()):
    js, md = os.path.join(OUT, name + ".json"), os.path.join(OUT, name + ".md")
    for p in (js, md):
        if os.path.exists(p):
            os.remove(p)
    p, wall = iat([*args, "--report-json", js, "--report-md", md, *(["--limits", *limits] if limits else [])])
    with open(js, encoding="utf-8") as f:
        return p.returncode, json.load(f), wall


def codes(j):
    return {i["code"] for i in j["issues"]}


def magick(*args):
    """Runs IMB-C's ImageMagick with the private environment IMB-C itself uses (engine.rs, run()):
    a cleared environment, PATH = runtime dir + System32, module/config paths inside the runtime,
    and temp / profile directories inside <out-dir>. Only this child process sees it."""
    home = os.path.dirname(MAGICK)
    windows = os.environ.get("SystemRoot", r"C:\Windows")
    profile = os.path.join(OUT, "magick-profile")
    os.makedirs(profile, exist_ok=True)
    env = {"SystemRoot": windows, "WINDIR": windows, "COMSPEC": os.path.join(windows, "System32", "cmd.exe"),
           "PATHEXT": ".COM;.EXE;.BAT;.CMD", "PATH": home + ";" + os.path.join(windows, "System32"),
           "MAGICK_TEMPORARY_PATH": TMP, "TEMP": TMP, "TMP": TMP, "TMPDIR": TMP,
           "HOME": profile, "USERPROFILE": profile, "APPDATA": profile, "LOCALAPPDATA": profile, "XDG_CONFIG_HOME": profile,
           "MAGICK_THREAD_LIMIT": "1", "OMP_NUM_THREADS": "1", "MAGICK_HOME": home, "MAGICK_CONFIGURE_PATH": home,
           "MAGICK_CODER_MODULE_PATH": os.path.join(home, "modules", "coders"),
           "MAGICK_FILTER_MODULE_PATH": os.path.join(home, "modules", "filters")}
    t0 = time.perf_counter()
    p = subprocess.run([MAGICK, *args], capture_output=True, text=True, errors="replace", cwd=OUT, env=env, timeout=3600)
    return p, time.perf_counter() - t0


# ------------------------------------------------------------ PNG crop (own decoder)
def png_crop(path, cw, chh):
    """Top-left cw x chh of a non-interlaced 8-bit RGB/RGBA PNG; only the needed rows are inflated."""
    with open(path, "rb") as f:
        assert f.read(8) == b"\x89PNG\r\n\x1a\n"
        dec = zlib.decompressobj()
        w = h = None
        rows, prev, buf = [], None, b""
        while len(rows) < chh:
            n, t = struct.unpack(">I4s", f.read(8))
            data = f.read(n)
            f.read(4)
            if t == b"IHDR":
                w, h, depth, ctype, _, _, il = struct.unpack(">IIBBBBB", data)
                assert depth == 8 and ctype in (2, 6) and il == 0, "crop supports 8-bit RGB/RGBA, non-interlaced"
                bpp = 3 if ctype == 2 else 4
                stride = w * bpp
                prev = bytearray(stride)
            elif t == b"IDAT":
                buf += dec.decompress(data)
                while len(buf) >= stride + 1 and len(rows) < chh:
                    ft, line = buf[0], bytearray(buf[1:stride + 1])
                    buf = buf[stride + 1:]
                    for i in range(stride):
                        a = line[i - bpp] if i >= bpp else 0
                        b = prev[i]
                        c = prev[i - bpp] if i >= bpp else 0
                        if ft == 1: line[i] = (line[i] + a) & 255
                        elif ft == 2: line[i] = (line[i] + b) & 255
                        elif ft == 3: line[i] = (line[i] + ((a + b) >> 1)) & 255
                        elif ft == 4:
                            p = a + b - c
                            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                            line[i] = (line[i] + (a if pa <= pb and pa <= pc else (b if pb <= pc else c))) & 255
                    prev = line
                    rows.append(bytes(line[:cw * bpp]))
            elif t == b"IEND":
                break
    out = []
    for r in rows:
        for x in range(cw):
            out += r[x * bpp:x * bpp + 3]
    return w, h, out


def write_png(path, w, h, ch, bits, samples, cicp=True):
    def chunk(t, p):
        return struct.pack(">I", len(p)) + t + p + struct.pack(">I", zlib.crc32(t + p) & 0xFFFFFFFF)
    ctype = {1: 0, 3: 2, 4: 6}[ch]
    raw = bytearray()
    bpr = w * ch
    for y in range(h):
        raw.append(0)
        line = samples[y * bpr:(y + 1) * bpr]
        raw += bytes(line) if bits == 8 else struct.pack(">%dH" % len(line), *line)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, bits, ctype, 0, 0, 0))
    if cicp:
        png += chunk(b"cICP", bytes([1, 13, 0, 1]))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 6)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def gen(fmt, name, samples, w, h, ch, bits, *opts):
    raw = os.path.join(OUT, name + ".raw")
    with open(raw, "wb") as f:
        f.write(bytes(samples) if bits <= 8 else struct.pack("<%dH" % len(samples), *samples))
    out = os.path.join(OUT, name + "." + fmt)
    args = [GEN, fmt, raw, out, str(w), str(h), str(ch), str(bits), *opts]
    p = subprocess.run(args, capture_output=True, text=True)
    os.remove(raw)
    if p.returncode:
        raise RuntimeError("%s: %s" % (" ".join(args), p.stderr))
    return out, " ".join(["gen_codec_fixture"] + args[1:2] + ["<truth>", os.path.basename(out)] + args[4:])


# ------------------------------------------------------------ run
samples_before = tree_fingerprint(SAMPLES)
magick_root = os.path.dirname(MAGICK)
magick_before = tree_fingerprint(magick_root)
print("fingerprinted %d sample files and %d IMB-C runtime files" % (len(samples_before), len(magick_before)), flush=True)
commands = []

names = sorted(os.listdir(SAMPLES))
for n in names:
    if not n.lower().endswith((".psd", ".psb")):
        continue
    src = os.path.join(SAMPLES, n)
    code, j, wall = report("inspect_" + n, "inspect", src)
    o = j["original"]
    record("inspect " + n, code == 0 and o["decode_status"] == "complete",
           "exit=%d decode=%s composite=%s %dx%d %s %.1fs" % (code, o["decode_status"], (o.get("psd") or {}).get("composite"),
                                                           o["width"], o["height"], o["format"], wall),
           exit=code, composite=(o.get("psd") or {}).get("composite"), wall_s=round(wall, 2),
           peak_ws=j["metrics"]["memory_bytes"].get("worker_peak_working_set"))
    ref = os.path.join(OUT, n + ".imbc-magick.png")
    if os.path.exists(ref):
        os.remove(ref)
    # As IMB-C writes PNG (engine.rs): keep the embedded ICC.
    p, mwall = magick("psd:" + src + "[0]", "-depth", "8", "-define", "png:preserve-iCCP=true", "png:" + ref)
    commands.append("magick \"psd:%s[0]\" -depth 8 -define png:preserve-iCCP=true \"png:%s\"" % (n, os.path.basename(ref)))
    if p.returncode:
        record("compare %s vs IMB-C magick PNG" % n, False, "magick failed: " + p.stderr.strip()[:200])
        continue
    code, j, wall = report("compare_" + n, "compare", src, ref)
    s = j["samples"]
    record("compare %s vs IMB-C magick PNG" % n, s["status"] == "equal",
           "exit=%d strict=%s samples=%s color=%s %.1fs" % (code, j["strict"]["result"], s["status"], j["color"]["status"], wall),
           exit=code, strict=j["strict"]["result"], samples=s["status"])

# AVIF from the largest PNG.
pngs = sorted((n for n in names if n.lower().endswith(".png")), key=lambda n: -os.path.getsize(os.path.join(SAMPLES, n)))
if pngs:
    src_png = os.path.join(SAMPLES, pngs[0])
    CW, CH = 1024, 512
    t0 = time.perf_counter()
    W, H, crop = png_crop(src_png, CW, CH)
    print("cropped %dx%d of %dx%d in %.1fs" % (CW, CH, W, H, time.perf_counter() - t0), flush=True)
    ref8 = os.path.join(OUT, "crop_rgb8.png")
    write_png(ref8, CW, CH, 3, 8, crop)
    # 8-bit lossless, 8-bit + alpha (A=0 on the first 8 columns, RGB kept), 10-bit (exact v*1023/255 rounding)
    a_ll, c1 = gen("avif", "crop_rgb8_lossless", crop, CW, CH, 3, 8)
    rgba = []
    for i in range(CW * CH):
        x = i % CW
        rgba += crop[i * 3:i * 3 + 3] + [0 if x < 8 else (x * 7 + i // CW) % 255 + 1]
    ref_a = os.path.join(OUT, "crop_rgba8.png")
    write_png(ref_a, CW, CH, 4, 8, rgba)
    a_a, c2 = gen("avif", "crop_rgba8_lossless", rgba, CW, CH, 4, 8)
    v10 = [(v * 1023 + 127) // 255 for v in crop]
    a_10, c3 = gen("avif", "crop_rgb10_lossless", v10, CW, CH, 3, 10)
    j10, c4 = gen("jxl", "crop_rgb10_ref", v10, CW, CH, 3, 10, "--color", "srgb")
    a_q, c5 = gen("avif", "crop_rgb8_identity_q60", crop, CW, CH, 3, 8, "--lossy", "60")
    a_420, c6 = gen("avif", "crop_rgb8_420_q60", crop, CW, CH, 3, 8, "--lossy", "60", "--yuv", "420", "--matrix", "6")
    commands += [c1, c2, c3, c4, c5, c6]
    cases = [("avif crop 8-bit lossless vs PNG", a_ll, ref8, 0, "match", True),
             ("avif crop 8-bit+alpha lossless vs PNG", a_a, ref_a, 0, "match", True),
             ("avif crop 10-bit lossless vs JXL 10-bit", a_10, j10, 0, "match", True),
             ("avif crop 10-bit vs PNG 8-bit", a_10, ref8, 1, "mismatch", None),
             ("avif crop identity q60 vs PNG", a_q, ref8, 1, "mismatch", False),
             ("avif crop 4:2:0 BT.601 q60 vs PNG", a_420, ref8, 2, "not_evaluated", None)]
    # IMB-C's ImageMagick: current IMB-C settings, quality 100 with BT.601, and identity.
    for tag, opts in [("imbc_default_q90", ["-quality", "90", "-define", "heic:speed=6", "-define", "heic:chroma=444",
                                            "-define", "heic:cicp=1/13/6/1"]),
                      ("imbc_q100_bt601", ["-quality", "100", "-define", "heic:speed=6", "-define", "heic:chroma=444",
                                           "-define", "heic:cicp=1/13/6/1"]),
                      ("imbc_q100_identity", ["-quality", "100", "-define", "heic:speed=6", "-define", "heic:cicp=1/13/0/1"])]:
        out = os.path.join(OUT, "crop_" + tag + ".avif")
        if os.path.exists(out):
            os.remove(out)
        p, mw = magick(ref8, *opts, "avif:" + out)
        commands.append("magick crop_rgb8.png %s \"avif:%s\"" % (" ".join(opts), os.path.basename(out)))
        if p.returncode:
            record("avif " + tag, False, "magick failed: " + p.stderr.strip()[:300])
            continue
        want = (0, "match", True) if tag.endswith("identity") else (2, "not_evaluated", None)
        cases.append(("avif crop %s vs PNG" % tag, out, ref8) + want)
    for name, a, b, ex, st, pv in cases:
        code, j, wall = report(name.replace(" ", "_").replace(":", "").replace("/", ""), "compare", b, a)
        av = j["output"].get("avif") or {}
        ok = code == ex and j["strict"]["result"] == st and (pv is None or av.get("pixel_verified") == pv)
        record(name, ok, "exit=%d strict=%s metadata=%s pixel_verified=%s reasons=%s" % (
            code, j["strict"]["result"], av.get("lossless_metadata"), av.get("pixel_verified"), av.get("lossless_metadata_reasons")),
            exit=code, strict=j["strict"]["result"], avif=av)
    if FULL:
        out = os.path.join(OUT, "full_imbc_q100_identity.avif")
        if os.path.exists(out):
            os.remove(out)
        p, mw = magick(src_png, "-quality", "100", "-define", "heic:speed=6", "-define", "heic:cicp=1/13/0/1", "avif:" + out)
        commands.append("magick <largest png> -quality 100 -define heic:speed=6 -define heic:cicp=1/13/0/1 avif:full_imbc_q100_identity.avif  (%.0f s)" % mw)
        if p.returncode:
            record("avif full-size IMB-C identity vs original PNG", False, "magick failed after %.0fs: %s" % (mw, p.stderr.strip()[:300]))
        else:
            code, j, wall = report("full_avif_vs_png", "compare", src_png, out, limits=("job_commit_mib=4096",))
            av = j["output"].get("avif") or {}
            mem = j["metrics"]["memory_bytes"]
            record("avif full-size IMB-C identity vs original PNG", j["samples"]["status"] == "equal" and av.get("pixel_verified") is True,
                   "exit=%d strict=%s samples=%s %.1fs peak_commit=%s codec_estimate=%s avif_bytes=%d" % (
                       code, j["strict"]["result"], j["samples"]["status"], wall, mem.get("worker_peak_commit"), mem.get("codec_estimate"),
                       os.path.getsize(out)), wall_s=round(wall, 1), memory=mem, encode_s=round(mw, 1))

samples_after = tree_fingerprint(SAMPLES)
magick_after = tree_fingerprint(magick_root)
record("read-only: sample directory unchanged", samples_before == samples_after, "%d files (size, mtime, SHA-256)" % len(samples_after))
record("read-only: IMB-C runtime unchanged", magick_before == magick_after, "%d files (size, mtime, SHA-256)" % len(magick_after))
with open(os.path.join(OUT, "real_samples.json"), "w", encoding="utf-8") as f:
    json.dump({"results": results, "commands": commands,
               "samples_sha256": {k: v[2] for k, v in samples_before.items()}}, f, ensure_ascii=False, indent=1)
failed = [r for r in results if not r["pass"]]
print("%d checks, %d failed" % (len(results), len(failed)))
sys.exit(1 if failed else 0)
