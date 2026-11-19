"""Runs the iat CLI regression cases (IMB-C call shapes + common CLI errors) and stores
stdout / stderr / exit code / report files per case, for compare_runs.py.

usage: py -3.13 -I tools/regress/run_cases.py --bin <iat-bin-dir> --out <new-dir> --pairs <pairs-dir>
           [--config <cases.local.json>] [--repo <repo-root>] [--skip-big]

  --bin     folder with iat.exe and iat-worker.exe
  --out     output folder; must not exist
  --pairs   derived test pairs written by gen_pairs.py      (env IAT_REGRESS_PAIRS)
  --config  local case list with the private sample cases   (env IAT_REGRESS_CONFIG;
            default tools/regress/cases.local.json when it exists). Without a config
            only the built-in cases run. Format: cases.example.json.
  --repo    repository root (default: two levels above this script); the IMB-C
            fixtures are read from <repo>/tests/fixtures/imbc
  --skip-big  skip the config's "inspect_all" cases

All inputs are only read. The samples folder named in the config is fingerprinted
(size, mtime, SHA-256) before and after the run.
"""
import hashlib, json, os, subprocess, sys
sys.stdout.reconfigure(encoding="utf-8")

HERE = os.path.dirname(os.path.abspath(__file__))


def opt(name, env=None, default=None):
    if name in sys.argv:
        return sys.argv[sys.argv.index(name) + 1]
    return os.environ.get(env) if env and os.environ.get(env) else default


BIN, OUT, PAIRS = opt("--bin"), opt("--out"), opt("--pairs", "IAT_REGRESS_PAIRS")
if not (BIN and OUT and PAIRS):
    sys.exit(__doc__)
BIN, OUT, PAIRS = (os.path.abspath(p) for p in (BIN, OUT, PAIRS))
REPO = os.path.abspath(opt("--repo", default=os.path.join(HERE, "..", "..")))
default_cfg = os.path.join(HERE, "cases.local.json")
CONFIG = opt("--config", "IAT_REGRESS_CONFIG", default_cfg if os.path.exists(default_cfg) else None)
SKIP_BIG = "--skip-big" in sys.argv
IAT = os.path.join(BIN, "iat.exe")
IMBC = os.path.join(REPO, "tests", "fixtures", "imbc")
if not os.path.isfile(IAT):
    sys.exit("iat.exe not found in " + BIN)
if not os.path.isdir(PAIRS):
    sys.exit("pairs folder not found: " + PAIRS + " (run gen_pairs.py first)")

cfg = {}
if CONFIG:
    with open(CONFIG, encoding="utf-8") as f:
        cfg = json.load(f)
SAMPLES = os.path.abspath(cfg["samples_dir"]) if cfg.get("samples_dir") else None
os.makedirs(OUT)


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 22), b""):
            h.update(b)
    return h.hexdigest()


def fp(root):
    r = {}
    for n in sorted(os.listdir(root)):
        p = os.path.join(root, n); st = os.stat(p)
        if os.path.isfile(p):
            r[n] = (st.st_size, st.st_mtime_ns, sha(p))
    return r


cases = []   # (name, args) ; "{R}" -> case dir


def imbc_compare(name, a, b, extra=()):
    # Exact IMB-C 1.4 call shape (src/iat.rs compare_using)
    cases.append((name, ["compare", a, b, "--report-json", "{R}\\report.json", "--temp-dir", "{R}",
                         "--limits", "buffer_mib=64", "--limits", "job_commit_mib=1024", *extra]))


def full(name, *args):
    cases.append((name, [*args, "--report-json", "{R}\\report.json", "--report-md", "{R}\\report.md", "--stdout", "json"]))


P = lambda n: os.path.join(PAIRS, n)
I = lambda n: os.path.join(IMBC, n)

# --- version / argument errors (exit 5)
cases += [("version", ["--version"]), ("noargs", []), ("unknown_cmd", ["frobnicate", "a"]),
          ("unknown_opt", ["inspect", P("crop8.png"), "--stdout", "json", "--bogus"]),
          ("compare_one_path", ["compare", P("crop8.png"), "--stdout", "json"]),
          ("no_report_output", ["inspect", P("crop8.png")]),
          ("bad_stdout_fmt", ["inspect", P("crop8.png"), "--stdout", "xml"]),
          ("bad_limit", ["inspect", P("crop8.png"), "--stdout", "json", "--limits", "nosuch=1"]),
          ("limits_no_value", ["inspect", P("crop8.png"), "--stdout", "json", "--limits"]),
          ("temp_dir_missing", ["inspect", P("crop8.png"), "--stdout", "json", "--temp-dir", P("no-such-dir")]),
          ("report_exists", ["inspect", P("crop8.png"), "--report-json", P("pairs-manifest.json")]),
          ("report_is_input", ["inspect", P("crop8.png"), "--report-json", P("crop8.png")]),
          ("report_dir_missing", ["inspect", P("crop8.png"), "--report-json", P("nodir\\r.json")]),
          ("missing_input", ["compare", P("crop8.png"), P("does-not-exist.png"), "--stdout", "json"]),
          ("stdout_md", ["compare", P("crop8.png"), P("crop8__webp.webp"), "--stdout", "md"]),
          ("full_paths", ["inspect", P("crop8.png"), "--stdout", "json", "--full-paths"]),
          ]

# --- derived pairs (IMB-C call shape)
for out in ["crop8__webp.webp", "crop8__webp-lossy.webp", "crop8__jxl.jxl", "crop8__jxl-lossy.jxl", "crop8__tiff.tif",
            "crop8__avif.avif", "crop8__avif-lossy.avif", "crop8__psd.psd", "crop8__modified.png", "crop8__png16.png",
            "crop8__smaller.png"]:
    imbc_compare("pair_" + out, P("crop8.png"), P(out))
imbc_compare("pair_crop16_vs_crop8", P("crop.png"), P("crop8.png"))
imbc_compare("same_file", P("crop8.png"), P("crop8.png"))
# resource limit -> exit 4 with resource_need (IMB-C retries from it)
cases.append(("limit_resource_need", ["compare", P("crop8.png"), P("crop8__webp.webp"), "--report-json", "{R}\\report.json",
                                      "--temp-dir", "{R}", "--limits", "buffer_mib=1", "--limits", "job_commit_mib=1024"]))
cases.append(("limit_job_small", ["compare", P("crop8.png"), P("crop8__avif.avif"), "--report-json", "{R}\\report.json",
                                  "--limits", "job_commit_mib=16", "buffer_mib=64"]))

# --- IMB-C real outputs copied into the repo (small; many formats, lossless and differing)
for orig, out in [("rgb-v12.png", "rgb-v12__jxl.jxl"), ("rgb-v12.png", "rgb-v12__webp-lossy.webp"),
                  ("rgba-v12.png", "rgba-v12__jxl.jxl"), ("rgba-v12.png", "rgba-v12__webp.webp"),
                  ("gray-v12.png", "gray-v12__jxl.jxl"), ("icc8.png", "icc8__webp.webp"),
                  ("v13-icc16.png", "v13-icc16__jxl.jxl"), ("v13-Rec2100PQ.png", "v13-Rec2100PQ__jxl.jxl"),
                  ("v13-Rec2100HLG.png", "v13-Rec2100HLG__jxl.jxl"), ("v13-cicp-only-pq.png", "v13-cicp-only-pq__jxl.jxl"),
                  ("v13-hidden.png", "v13-hidden__webp.webp"), ("pq-original.jxl", "pq-original__png.png"),
                  ("hlg-original.jxl", "hlg-original__png.png")]:
    imbc_compare("imbc_" + out, I(orig), I(out))
for n in ["animation.webp", "multipage-v12.tiff", "invalid-idat-v121.png"]:
    full("imbc_inspect_" + n, "inspect", I(n))


# --- local sample cases from the config (private files; never in the repository)
def expand(tok):
    roots = {"{S}": SAMPLES, "{P}": PAIRS, "{I}": IMBC}
    for key, root in roots.items():
        if tok.startswith(key + "/"):
            if root is None:
                sys.exit("config uses %s but samples_dir is not set" % key)
            return os.path.join(root, *tok[len(key) + 1:].split("/"))
    return tok


for c in cfg.get("cases", []):
    args = [expand(t) for t in c["args"]]
    shape = c.get("shape", "raw")
    if shape == "full":
        full(c["name"], *args)
    elif shape == "imbc":
        imbc_compare(c["name"], args[0], args[1], args[2:])
    elif shape == "raw":
        cases.append((c["name"], args))
    else:
        sys.exit("unknown case shape: " + shape)
ia = cfg.get("inspect_all")
if ia and SAMPLES and not SKIP_BIG:
    exts = tuple(e.lower() for e in ia.get("extensions", []))
    for n in sorted(os.listdir(SAMPLES)):
        if n.lower().endswith(exts):
            full(ia.get("name_prefix", "sample_inspect_") + n, "inspect", os.path.join(SAMPLES, n), *ia.get("extra_args", []))

names = [n for n, _ in cases]
if len(set(names)) != len(names):
    sys.exit("duplicate case names")

before = fp(SAMPLES) if SAMPLES else None
for name, args in cases:
    d = os.path.join(OUT, name.replace(" ", "_"))
    os.makedirs(d)
    a = [x.replace("{R}", d) for x in args]
    p = subprocess.run([IAT, *a], capture_output=True, timeout=7200)
    with open(os.path.join(d, "stdout"), "wb") as f: f.write(p.stdout)
    with open(os.path.join(d, "stderr"), "wb") as f: f.write(p.stderr)
    with open(os.path.join(d, "exit"), "w") as f: f.write(str(p.returncode))
    with open(os.path.join(d, "args.json"), "w", encoding="utf-8") as f: json.dump(a, f, ensure_ascii=False)
    print("%-48s exit=%d" % (name, p.returncode), flush=True)
print("cases:", len(cases))
if SAMPLES:
    after = fp(SAMPLES)
    print("samples folder unchanged:", before == after)
    with open(os.path.join(OUT, "samples-readonly.json"), "w") as f:
        json.dump({"unchanged": before == after, "files": {k: v[2] for k, v in after.items()}}, f, indent=1)
    if before != after:
        sys.exit(1)
