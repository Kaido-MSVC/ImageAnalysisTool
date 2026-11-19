"""Builds the derived test pairs for run_cases.py from a local sample PNG with IMB-C's
magick.exe (read-only call; IMB-C's private environment: cleared env, temp and profile
inside <out>). The samples folder and the magick runtime are fingerprinted before/after.

usage: py -3.13 -I tools/regress/gen_pairs.py --magick <magick.exe> --out <pairs-dir>
           [--config <cases.local.json>] [--samples <dir>]

  --samples   folder holding the source images (default: the config's samples_dir;
              env IAT_REGRESS_SAMPLES)
  --config    env IAT_REGRESS_CONFIG; default tools/regress/cases.local.json if present.
              "pairs.crop_source": PNG to crop (default: the first PNG in the folder);
              "pairs.psd_references": PSD/PSB files whose composite is written as
              <name>.imbc-magick.png
"""
import hashlib, json, os, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))


def opt(name, env=None, default=None):
    if name in sys.argv:
        return sys.argv[sys.argv.index(name) + 1]
    return os.environ.get(env) if env and os.environ.get(env) else default


default_cfg = os.path.join(HERE, "cases.local.json")
CONFIG = opt("--config", "IAT_REGRESS_CONFIG", default_cfg if os.path.exists(default_cfg) else None)
cfg = {}
if CONFIG:
    with open(CONFIG, encoding="utf-8") as f:
        cfg = json.load(f)
SAMPLES = opt("--samples", "IAT_REGRESS_SAMPLES", cfg.get("samples_dir"))
MAGICK, OUT = opt("--magick"), opt("--out")
if not (SAMPLES and MAGICK and OUT):
    sys.exit(__doc__)
SAMPLES, MAGICK, OUT = (os.path.abspath(a) for a in (SAMPLES, MAGICK, OUT))
pcfg = cfg.get("pairs", {})
os.makedirs(OUT, exist_ok=True)
TMP = os.path.join(OUT, "magick-tmp"); os.makedirs(TMP, exist_ok=True)


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 22), b""):
            h.update(b)
    return h.hexdigest()


def fp(root):
    r = {}
    for d, _, fs in os.walk(root):
        for n in fs:
            p = os.path.join(d, n); st = os.stat(p)
            r[os.path.relpath(p, root)] = (st.st_size, st.st_mtime_ns, sha(p))
    return r


def magick(*args):
    home = os.path.dirname(MAGICK)
    win = os.environ.get("SystemRoot", r"C:\Windows")
    prof = os.path.join(OUT, "magick-profile"); os.makedirs(prof, exist_ok=True)
    env = {"SystemRoot": win, "WINDIR": win, "COMSPEC": os.path.join(win, "System32", "cmd.exe"),
           "PATHEXT": ".COM;.EXE;.BAT;.CMD", "PATH": home + ";" + os.path.join(win, "System32"),
           "MAGICK_TEMPORARY_PATH": TMP, "TEMP": TMP, "TMP": TMP, "TMPDIR": TMP,
           "HOME": prof, "USERPROFILE": prof, "APPDATA": prof, "LOCALAPPDATA": prof, "XDG_CONFIG_HOME": prof,
           "MAGICK_THREAD_LIMIT": "1", "OMP_NUM_THREADS": "1", "MAGICK_HOME": home, "MAGICK_CONFIGURE_PATH": home,
           "MAGICK_CODER_MODULE_PATH": os.path.join(home, "modules", "coders"),
           "MAGICK_FILTER_MODULE_PATH": os.path.join(home, "modules", "filters")}
    t0 = time.perf_counter()
    p = subprocess.run([MAGICK, *args], capture_output=True, text=True, errors="replace", cwd=OUT, env=env, timeout=3600)
    print("%s rc=%d %.1fs %s" % (" ".join(args[-1:]), p.returncode, time.perf_counter() - t0, p.stderr.strip()[:200]), flush=True)
    return p


s_before = fp(SAMPLES)
m_before = fp(os.path.dirname(MAGICK))
big_png = pcfg.get("crop_source") or [n for n in os.listdir(SAMPLES) if n.lower().endswith(".png")][0]
o = lambda n: os.path.join(OUT, n)

magick("png:" + os.path.join(SAMPLES, big_png), "-crop", "1024x768+512+512", "+repage", "png:" + o("crop.png"))
magick(o("crop.png"), "-depth", "8", "png:" + o("crop8.png"))
magick(o("crop8.png"), "-define", "webp:lossless=true", "webp:" + o("crop8__webp.webp"))
magick(o("crop8.png"), "-quality", "75", "webp:" + o("crop8__webp-lossy.webp"))
magick(o("crop8.png"), "-quality", "100", "jxl:" + o("crop8__jxl.jxl"))
magick(o("crop8.png"), "-quality", "90", "jxl:" + o("crop8__jxl-lossy.jxl"))
magick(o("crop8.png"), "-compress", "zip", "tiff:" + o("crop8__tiff.tif"))
magick(o("crop8.png"), "-quality", "100", "-define", "heic:speed=6", "-define", "heic:chroma=444",
       "-define", "heic:cicp=1/13/0/1", "avif:" + o("crop8__avif.avif"))
magick(o("crop8.png"), "-quality", "60", "avif:" + o("crop8__avif-lossy.avif"))
magick(o("crop8.png"), "psd:" + o("crop8__psd.psd"))
magick(o("crop8.png"), "-fill", "#FF0000", "-draw", "point 10,10", "png:" + o("crop8__modified.png"))
magick(o("crop8.png"), "-depth", "16", "png:" + o("crop8__png16.png"))
magick(o("crop8.png"), "-crop", "1000x768+0+0", "+repage", "png:" + o("crop8__smaller.png"))
for n in pcfg.get("psd_references", []):
    magick("psd:" + os.path.join(SAMPLES, n) + "[0]", "-depth", "8", "-define", "png:preserve-iCCP=true",
           "png:" + o(n + ".imbc-magick.png"))

ok = fp(SAMPLES) == s_before and fp(os.path.dirname(MAGICK)) == m_before
files = {n: sha(o(n)) for n in sorted(os.listdir(OUT)) if os.path.isfile(o(n))}
with open(o("pairs-manifest.json"), "w", encoding="utf-8") as f:
    json.dump({"samples_unchanged_and_magick_unchanged": ok, "files": files}, f, indent=1)
print("read-only check:", "OK" if ok else "CHANGED!")
sys.exit(0 if ok else 1)
