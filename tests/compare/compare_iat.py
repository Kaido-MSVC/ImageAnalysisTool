"""Runs the same cases with the C++ iat 1.0.0 baseline and the Rust iat, then compares them.

Based on the C++ repo's tools/regress/compare_runs.py, with the case list of
tests/e2e/run_e2e.py (pairs.json plus the argument / report-safety / I/O cases that do
not need a decoder). Standard library only.

usage:
  py -3.13 -I tests/compare/compare_iat.py --cpp <iat.exe> --rust <iat.exe> --out <dir>
         [--fixtures tests/fixtures/generated] [--allow docs/decisions.md] [--only <regex>]
         [--no-cancel]

Per case it compares: process exit code, stderr (masked), the JSON report (parsed, masked)
and its layout, and the Markdown report (masked). Masked / removed before comparing:
  JSON  : run_id, tool (name, version, compiler), codecs, metrics, settings.temp.free_bytes_at_start
  text  : run ids, the per-side case directory, the "工具" line and the 效能與資源 section of Markdown
Every other difference fails unless docs/decisions.md lists it in the allowed-differences
table (case, field, reason). Field is a dotted JSON path, "exit", "stderr" or "markdown";
"*" in case or field is a wildcard (fnmatch).

A case is "decode-free" when the baseline did not get to any decoder: every file has format
"", "unknown" or "jpeg", or the run stopped before analysis (exit 5 without a report).
Stage 0 acceptance: every decode-free case matches. Other cases are listed for information.
Exit code: 0 when every decode-free case matches; 1 otherwise.
"""
import argparse
import fnmatch
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

sys.stdout.reconfigure(encoding="utf-8")

ap = argparse.ArgumentParser()
ap.add_argument("--cpp", required=True)
ap.add_argument("--rust", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--fixtures", default=os.path.join(os.path.dirname(__file__), "..", "fixtures", "generated"))
ap.add_argument("--allow", default=os.path.join(os.path.dirname(__file__), "..", "..", "docs", "decisions.md"))
ap.add_argument("--only")
ap.add_argument("--no-cancel", action="store_true", help="skip the Ctrl+Break during hashing case (needs a 16 GiB sparse file)")
ap.add_argument("--accept", default="",
                help="comma-separated formats of the current stage (e.g. png): cases whose files are all of these "
                     "formats (or need no decoder) must match too")
ap.add_argument("--local", help="local sample config in the format of the C++ repo's tools/regress/cases.local.json "
                                "(cases[] and inspect_all); private files, results stay under --out")
ap.add_argument("--pairs", help="pairs folder written by the C++ repo's tools/regress/gen_pairs.py ({P}); adds the "
                                "derived pair cases of run_cases.py")
args = ap.parse_args()

FIX = os.path.abspath(args.fixtures)
OUT = os.path.abspath(args.out)
ONLY = re.compile(args.only) if args.only else None
SIDES = {"cpp": os.path.abspath(args.cpp), "rust": os.path.abspath(args.rust)}
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

shutil.rmtree(OUT, ignore_errors=True)
for s in SIDES:
    os.makedirs(os.path.join(OUT, s))


# ------------------------------------------------------------------ allowed differences
def load_allowed(path):
    """Rows of the table under the heading '允許的差異' in decisions.md: | 案例 | 欄位 | 原因 |."""
    rows, inside = [], False
    if not os.path.exists(path):
        return rows
    for line in open(path, encoding="utf-8"):
        if line.startswith("#"):
            inside = "允許的差異" in line
            continue
        if inside and line.startswith("|"):
            cells = [c.strip().strip("`") for c in line.strip().strip("|").split("|")]
            if len(cells) >= 3 and cells[0] not in ("案例", "") and not set(cells[0]) <= set("-: "):
                rows.append((cells[0], cells[1], cells[2]))
    return rows


ALLOWED = load_allowed(os.path.abspath(args.allow))


def _pat(p):
    """'[' is literal in the table (JSON paths use issues[3]); only * and ? are wildcards."""
    return p.replace("[", "[[]")


def allowed(case, field):
    return next((r for r in ALLOWED
                 if fnmatch.fnmatchcase(case, _pat(r[0])) and fnmatch.fnmatchcase(field, _pat(r[1]))), None)


# ------------------------------------------------------------------ cases
class Case:
    def __init__(self, name, argv, inputs=(), setup=None, reports=True, cancel_after=None):
        self.name, self.argv, self.inputs, self.setup = name, list(argv), list(inputs), setup
        self.reports, self.cancel_after = reports, cancel_after


def fx(n):
    return os.path.join(FIX, n)


cases = []
spec = json.load(open(fx("pairs.json"), encoding="utf-8"))
for c in spec["pairs"]:
    cases.append(Case(c["name"], ["compare", fx(c["original"]), fx(c["output"]), *c.get("args", [])]))
for c in spec["singles"]:
    cases.append(Case(c["name"], ["inspect", fx(c["file"])]))

A, B = fx("s1_rgb16_srgb.png"), fx("s1_rgb16_srgb_lsb.png")
UNK = fx("s2_unknown_format.bin")


def write_file(name, text):
    def make(d):
        with open(os.path.join(d, name), "w", encoding="utf-8") as f:
            f.write(text)
    return make


def hardlink(name, copy_name, source):
    """Copies source into the case directory and hard-links name to the copy (same volume)."""
    def make(d):
        shutil.copyfile(source, os.path.join(d, copy_name))
        os.link(os.path.join(d, copy_name), os.path.join(d, name))
    return make


# "{D}" is replaced by the side's case directory.
cases += [
    Case("missing_input", ["compare", A, fx("does_not_exist.png")]),
    Case("missing_original_inspect", ["inspect", fx("does_not_exist.bin")]),
    Case("inspect_directory", ["inspect", FIX]),
    Case("unknown_vs_unknown", ["compare", UNK, UNK]),
    Case("unknown_vs_missing", ["compare", UNK, fx("nope.bin")]),
    Case("unknown_full_paths_stdout", ["inspect", UNK, "--stdout", "json", "--full-paths"], reports=False),
    Case("unknown_stdout_md", ["inspect", UNK, "--stdout", "md"], reports=False),
    Case("unknown_test_option", ["inspect", UNK, "--test-codec-estimate-pct", "10"]),
    Case("unknown_limits_echoed", ["inspect", UNK, "--limits", "buffer_mib=8", "job_commit_mib=24", "deadline_s=5"]),
    Case("unknown_temp_dir", ["inspect", UNK, "--temp-dir", "{D}"]),
    Case("unknown_temp_dir_full_paths", ["inspect", UNK, "--temp-dir", "{D}", "--full-paths", "--stdout", "json"],
         reports=False),
    # argument and report-target errors: exit 5 before any analysis
    Case("version", ["--version"], reports=False),
    Case("missing_command", [], reports=False),
    Case("unknown_command", ["frobnicate", A], reports=False),
    Case("unknown_option", ["inspect", A, "--bogus", "--stdout", "json"], reports=False),
    Case("compare_one_path", ["compare", A, "--stdout", "json"], reports=False),
    Case("stdout_bad_format", ["inspect", A, "--stdout", "xml"], reports=False),
    Case("report_md_needs_path", ["inspect", A, "--report-md"], reports=False),
    Case("no_report_requested", ["compare", A, B], reports=False),
    Case("limit_zero_rejected", ["compare", A, B, "--stdout", "json", "--limits", "deadline_s=0"], reports=False),
    Case("limits_needs_value", ["compare", A, B, "--stdout", "json", "--limits"], reports=False),
    Case("limit_unknown_name", ["compare", A, B, "--stdout", "json", "--limits", "foo=1"], reports=False),
    Case("test_pct_out_of_range", ["inspect", A, "--stdout", "json", "--test-codec-estimate-pct", "0"], reports=False),
    Case("temp_dir_missing_rejected", ["compare", A, B, "--stdout", "json", "--temp-dir", "{D}\\missing_dir"],
         reports=False),
    Case("existing_report_not_overwritten", ["compare", A, B, "--report-md", "{D}\\existing.md"], reports=False,
         setup=write_file("existing.md", "keep me\n")),
    Case("report_md_is_original_input", ["compare", A, B, "--report-md", A, "--report-json", "{D}\\o.json"],
         reports=False),
    Case("report_path_is_input", ["compare", A, B, "--report-md", B], reports=False),
    Case("report_path_is_hardlink_of_input", ["compare", A, "{D}\\input.png", "--report-json", "{D}\\link.png"],
         reports=False, setup=hardlink("link.png", "input.png", B)),
    Case("both_reports_same_path", ["compare", A, B, "--report-md", "{D}\\same.json", "--report-json", "{D}\\SAME.JSON"],
         reports=False),
    Case("report_dir_missing", ["compare", A, B, "--report-md", "{D}\\no_such_dir\\r.md"], reports=False),
]


# ---- TIFF (stage 4): every TIFF fixture inspected, the C++ run_e2e.py budget / tile temp / libtiff
# allocation cases, and the Rust-side variants in tests/fixtures/tiff_variants (D-R17).
VAR = os.path.join(FIX, "..", "tiff_variants")
TIFFS = sorted(n for n in os.listdir(FIX) if n.endswith(".tif"))
for n in TIFFS:
    cases.append(Case("tiffinspect_" + n[:-4], ["inspect", fx(n)]))
cases.append(Case("tiffinspect_imbc_multipage", ["inspect", os.path.join(FIX, "..", "imbc", "multipage-v12.tiff")]))
ONE, SHARED = fx("s2_tif_rgb16_one_big_strip.tif"), fx("s2_tif_gray8_147456_shared_tiles.tif")
TILE, TDIFF, STRIP = (fx("s2_tif_rgb16_4096x300_tile256.tif"), fx("s2_tif_rgb16_4096x300_tile256_diff.tif"),
                      fx("s2_tif_rgb16_4096x300_strip16_be.tif"))
cases += [
    Case("tiff_big_strip_budget", ["compare", ONE, ONE, "--limits", "buffer_mib=8"]),
    Case("tiff_big_strip_default", ["compare", ONE, ONE]),
    Case("tiff_tile_temp", ["compare", TILE, STRIP, "--limits", "buffer_mib=8"]),
    Case("tiff_tile_temp_diff", ["compare", TDIFF, STRIP, "--limits", "buffer_mib=8"]),
    Case("tiff_tile_band", ["compare", TILE, STRIP]),
    Case("tiff_libtiff_alloc_limit", ["inspect", SHARED, "--limits", "buffer_mib=1", "metadata_item_mib=1"]),
    Case("tiff_shared_tiles_default", ["compare", SHARED, SHARED]),
]
if os.path.isdir(VAR):
    v = lambda n: os.path.join(VAR, n)
    for n in sorted(os.listdir(VAR)):
        if n.endswith(".tif"):
            cases.append(Case("tiffvar_" + n[:-4], ["inspect", v(n)]))
    for a, b in [("v_rgb8_fill2.tif", v("v_rgb8_fill2_expected.tif")),
                 ("v_gray8a_miniswhite.tif", v("v_gray8a_miniswhite_expected.tif")),
                 ("v_rgb8_pred2_deflate_be.tif", v("v_rgb8_none_ref.tif")),
                 ("v_none_short_count.tif", fx("s2_tif_rgb16_le_strip_none.tif")),
                 ("v_none_short_count_last.tif", fx("s2_tif_rgb16_le_strip_none.tif")),
                 ("v_deflate_no_adler.tif", fx("s2_tif_rgb16_pred2_le_deflate.tif"))]:
        cases.append(Case("tiffvar_pair_" + a[:-4], ["compare", v(a), b]))


# ---- JPEG XL (stage 5): files written with the C++ build's libjxl (tests/fixtures/jxl_generated, D-R18)
JXLGEN = os.path.join(FIX, "..", "jxl_generated")
if os.path.isdir(JXLGEN):
    lossy = os.path.join(JXLGEN, "v_jxl_rgb16_lossy_vardct_noxyb.jxl")
    cases.append(Case("jxlgen_lossy_noxyb_vs_png", ["compare", fx("s3_rgb16_600x40_srgb.png"), lossy]))
    cases.append(Case("jxlgen_inspect_lossy_noxyb", ["inspect", lossy]))


# ---- AVIF (stage 6): grid images written with the C++ build's libavif (tests/fixtures/avif_generated, D-R19)
AVIFGEN = os.path.join(FIX, "..", "avif_generated")
if os.path.isdir(AVIFGEN):
    g = lambda n: os.path.join(AVIFGEN, n)
    for n in ("rgb8", "rgba8"):
        cases.append(Case("avifgen_inspect_grid_" + n, ["inspect", g("v_avif_grid_%s.avif" % n)]))
        cases.append(Case("avifgen_grid_%s_vs_png" % n, ["compare", g("v_avif_grid_%s.avif" % n),
                                                          g("v_avif_grid_ref_%s.png" % n)]))


def make_sparse(d):
    p = os.path.join(d, "sparse_16gib.bin")
    open(p, "wb").close()
    if subprocess.run(["fsutil", "sparse", "setflag", p], capture_output=True).returncode != 0:
        raise RuntimeError("fsutil sparse setflag failed")
    with open(p, "r+b") as f:
        f.truncate(16 << 30)


if not args.no_cancel:
    cases.append(Case("cancel_during_hash", ["compare", "{D}\\sparse_16gib.bin", A], setup=make_sparse, cancel_after=1.0))


# ------------------------------------------------------------------ local samples (never in git)
def imbc_case(name, a, b, extra=()):
    """IMB-C 1.4 call shape, as in the C++ repo's run_cases.py (reports=False: the JSON path is in argv)."""
    return Case(name, ["compare", a, b, "--report-json", "{D}\\report.json", "--temp-dir", "{D}",
                       "--limits", "buffer_mib=64", "--limits", "job_commit_mib=1024", *extra], reports=False)


PAIRS = os.path.abspath(args.pairs) if args.pairs else None
if PAIRS:
    P = lambda n: os.path.join(PAIRS, n)
    for o in ["crop8__webp.webp", "crop8__webp-lossy.webp", "crop8__jxl.jxl", "crop8__jxl-lossy.jxl", "crop8__tiff.tif",
              "crop8__avif.avif", "crop8__avif-lossy.avif", "crop8__psd.psd", "crop8__modified.png",
              "crop8__png16.png", "crop8__smaller.png"]:
        cases.append(imbc_case("local_pair_" + o, P("crop8.png"), P(o)))
    cases.append(imbc_case("local_pair_crop16_vs_crop8", P("crop.png"), P("crop8.png")))
    cases.append(imbc_case("local_same_file", P("crop8.png"), P("crop8.png")))
    for n in sorted(os.listdir(PAIRS)):
        if os.path.isfile(P(n)):
            cases.append(Case("local_pairs_inspect_" + n, ["inspect", P(n)]))
if args.local:
    cfg = json.load(open(args.local, encoding="utf-8"))
    SAMPLES = cfg.get("samples_dir")
    roots = {"{S}": SAMPLES, "{P}": PAIRS}

    def expand(tok):
        for key, root in roots.items():
            if tok.startswith(key + "/"):
                if root is None:
                    sys.exit("local config uses %s but its folder is not given" % key)
                return os.path.join(root, *tok[len(key) + 1:].split("/"))
        return tok

    for c in cfg.get("cases", []):
        a = [expand(t) for t in c["args"]]
        shape = c.get("shape", "raw")
        if shape == "full":
            cases.append(Case("local_" + c["name"], a))
        elif shape == "imbc":
            cases.append(imbc_case("local_" + c["name"], a[0], a[1], a[2:]))
        else:
            cases.append(Case("local_" + c["name"], a, reports=False))
    ia = cfg.get("inspect_all")
    if ia and SAMPLES:
        exts = tuple(e.lower() for e in ia.get("extensions", []))
        for n in sorted(os.listdir(SAMPLES)):
            if n.lower().endswith(exts):
                cases.append(Case("local_" + ia.get("name_prefix", "sample_inspect_") + n,
                                  ["inspect", os.path.join(SAMPLES, n), *ia.get("extra_args", [])]))
if len({c.name for c in cases}) != len(cases):
    sys.exit("duplicate case names")

if ONLY:
    cases = [c for c in cases if ONLY.search(c.name)]


# ------------------------------------------------------------------ running
def run_side(side, c):
    d = os.path.join(OUT, side, c.name)
    os.makedirs(d)
    if c.setup:
        c.setup(d)
    argv = [a.replace("{D}", d) for a in c.argv]
    if c.reports:
        argv += ["--report-json", os.path.join(d, "report.json"), "--report-md", os.path.join(d, "report.md")]
    cmd = [SIDES[side], *argv]
    if c.cancel_after:
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        time.sleep(c.cancel_after)
        p.send_signal(signal.CTRL_BREAK_EVENT)
        out, err = p.communicate(timeout=300)
        rc = p.returncode
    else:
        p = subprocess.run(cmd, capture_output=True, timeout=600)
        out, err, rc = p.stdout, p.stderr, p.returncode
    res = {"exit": rc, "stdout": out, "stderr": err, "dir": d}
    for n in ("report.json", "report.md"):
        pth = os.path.join(d, n)
        res[n] = open(pth, "rb").read() if os.path.exists(pth) else None
    if os.path.exists(os.path.join(d, "sparse_16gib.bin")):
        os.remove(os.path.join(d, "sparse_16gib.bin"))
    return res


def mask_text(b, d):
    t = b.decode("utf-8", errors="replace").replace("\r\n", "\n")
    t = t.replace(d.replace("\\", "\\\\"), "<CASE>").replace(d, "<CASE>")
    return UUID.sub("<RUN>", t)


def mask_json(j):
    j = json.loads(json.dumps(j))
    for k in ("run_id", "tool", "codecs", "metrics"):
        j.pop(k, None)
    temp = j.get("settings", {}).get("temp")
    if isinstance(temp, dict):
        temp.pop("free_bytes_at_start", None)
    return j


def layout(text):
    """JSON text with every scalar value replaced: checks key order and indentation."""
    t = re.sub(r'"(?:[^"\\]|\\.)*"', '"S"', text)
    t = re.sub(r"(?<![\w\"])-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?", "N", t)
    return t


def canonical_layout_ok(raw):
    try:
        j = json.loads(raw)
    except Exception:
        return False
    return layout(raw.rstrip("\n")) == layout(json.dumps(j, indent=1, ensure_ascii=False, sort_keys=True))


def mask_md(t):
    out, skip = [], False
    for line in t.split("\n"):
        if line.startswith("## "):
            skip = line.startswith("## 效能與資源")
        if skip or line.startswith("- 工具："):
            continue
        out.append(line)
    return "\n".join(out)


def flatten(j, prefix=""):
    if isinstance(j, dict):
        r = {}
        for k, v in j.items():
            r.update(flatten(v, prefix + "." + k if prefix else k))
        return r or {prefix: {}}
    if isinstance(j, list):
        r = {}
        for i, v in enumerate(j):
            r.update(flatten(v, "%s[%d]" % (prefix, i)))
        return r or {prefix: []}
    return {prefix: j}


def json_of(res):
    raw = res["report.json"]
    if raw is None and res["stdout"].lstrip().startswith(b"{"):
        raw = res["stdout"]
    return raw.decode("utf-8") if raw is not None else None


def md_escape(t):
    """與 iat-core markdown::md_escape 的固定跳脫字元相同（行首字元的規則在表格內用不到）。"""
    return "".join("\\" + ch if ch in "\\`*_[]<>|~" else ch for ch in t)


def tolerate_md(lines, diffs, c):
    """Rust Markdown with allowed JSON string differences put back to the C++ value, so a difference
    already allowed in the JSON (e.g. decode_path) does not hide every later Markdown line."""
    pairs = {rv: cv for f, cv, rv in diffs
             if allowed(c.name, f) and isinstance(cv, str) and isinstance(rv, str) and len(rv) >= 4 and rv != cv}
    # Markdown 表格裡的字串經過跳脫（core 的 md_escape）：跳脫後的形式也要換回 C++ 版跳脫後的值。
    for rv, cv in list(pairs.items()):
        if md_escape(rv) != rv or md_escape(cv) != cv:
            pairs[md_escape(rv)] = md_escape(cv)
    # Allowed numeric differences (the mae / rmse of a lossy decode, D-R18): Markdown prints floats with
    # 6 significant digits (C++ ostream default) and integers as they are.
    for f, cv, rv in diffs:
        nums = all(isinstance(v, (int, float)) and not isinstance(v, bool) for v in (cv, rv))
        if nums and cv != rv and allowed(c.name, f):
            fmt = (lambda v: "%g" % v) if isinstance(cv, float) or isinstance(rv, float) else str
            if fmt(rv) != fmt(cv):
                pairs[fmt(rv)] = fmt(cv)
    subs = sorted(pairs.items(), key=lambda s: -len(s[0]))
    out = []
    for line in lines:
        for rv, cv in subs:
            line = line.replace(rv, cv)
        out.append(line)
    return out


def compare(c, a, b):
    """List of (field, cpp value, rust value)."""
    diffs = []
    if a["exit"] != b["exit"]:
        diffs.append(("exit", a["exit"], b["exit"]))
    sa, sb = mask_text(a["stderr"], a["dir"]), mask_text(b["stderr"], b["dir"])
    if c.name == "version":   # the version string is a known, expected difference
        sa, sb = re.sub(r"iat \S+", "iat <VER>", mask_text(a["stdout"], a["dir"])), re.sub(r"iat \S+", "iat <VER>", mask_text(b["stdout"], b["dir"]))
    if sa != sb:
        diffs.append(("stderr", sa, sb))
    ja, jb = json_of(a), json_of(b)
    dropped = [r[1][len("issues[code="):-1] for r in ALLOWED
               if r[1].startswith("issues[code=") and r[1].endswith("]") and fnmatch.fnmatchcase(c.name, _pat(r[0]))]
    if (ja is None) != (jb is None):
        diffs.append(("report.json", "present" if ja else "absent", "present" if jb else "absent"))
    elif ja is not None:
        # The case directory differs per side: mask it in the raw text (JSON-escaped form) before parsing.
        ja_obj = json.loads(mask_text(ja.encode("utf-8"), a["dir"]))
        # An allowed row whose field is "issues[code=X]" drops the C++ issues with code X before comparing
        # (an issue the Rust decoder cannot produce yet, e.g. layered_image in D-R18); every other issue
        # is still compared position by position.
        if dropped and isinstance(ja_obj.get("issues"), list):
            kept = [i for i in ja_obj["issues"] if i.get("code") not in dropped]
            if len(kept) != len(ja_obj["issues"]):
                diffs.extend(("issues[code=%s]" % d, "present", None) for d in dropped)
            ja_obj["issues"] = kept
        fa = flatten(mask_json(ja_obj))
        fb = flatten(mask_json(json.loads(mask_text(jb.encode("utf-8"), b["dir"]))))
        for k in sorted(set(fa) | set(fb)):
            if fa.get(k, "<absent>") != fb.get(k, "<absent>"):
                diffs.append((k, fa.get(k, "<absent>"), fb.get(k, "<absent>")))
        if not canonical_layout_ok(jb):
            diffs.append(("json_layout", "sorted, indent 1", "not canonical"))
    ma = a["report.md"] or (a["stdout"] if c.argv[-1:] == ["md"] else None)
    mb = b["report.md"] or (b["stdout"] if c.argv[-1:] == ["md"] else None)
    if (ma is None) != (mb is None):
        diffs.append(("markdown", "present" if ma else "absent", "present" if mb else "absent"))
    elif ma is not None:
        la = [line for line in mask_md(mask_text(ma, a["dir"])).split("\n")
              if not any(line.startswith("| `%s` |" % d) for d in dropped)]
        lb = tolerate_md(mask_md(mask_text(mb, b["dir"])).split("\n"), diffs, c)
        if la != lb:
            i = next((k for k, (x, y) in enumerate(zip(la, lb)) if x != y), min(len(la), len(lb)))
            diffs.append(("markdown", "line %d: %s" % (i, la[i] if i < len(la) else "<end>"),
                          "line %d: %s" % (i, lb[i] if i < len(lb) else "<end>")))
    return diffs


def decode_free(a):
    j = json_of(a)
    if j is None:
        md = a["stdout"].decode("utf-8", errors="replace")
        m = re.search(r"^\| 格式 \|(.*)\|\s*$", md, re.M)
        if m:   # Markdown on stdout: the 格式 row lists one cell per file
            return all(c.strip() in ("", "unknown", "jpeg") for c in m.group(1).split("|"))
        return a["exit"] == 5 or a["stdout"].startswith(b"iat ")
    j = json.loads(j)
    files = [j["original"]] + ([j["output"]] if j.get("output") else [])
    return all(f.get("format", "") in ("", "unknown", "jpeg") for f in files)


ACCEPT = {f.strip() for f in args.accept.split(",") if f.strip()}


def stage_case(a):
    """Every file is of a format the current stage supports (or needs no decoder)."""
    j = json_of(a)
    if j is None or not ACCEPT:
        return False
    j = json.loads(j)
    files = [j["original"]] + ([j["output"]] if j.get("output") else [])
    return all(f.get("format", "") in ACCEPT | {"", "unknown", "jpeg"} for f in files)


# ------------------------------------------------------------------ main
rows = []
for c in cases:
    a, b = run_side("cpp", c), run_side("rust", c)
    diffs = compare(c, a, b)
    unallowed = [d for d in diffs if not allowed(c.name, d[0])]
    status = "SAME" if not diffs else ("ALLOWED" if not unallowed else "DIFF")
    free = decode_free(a)
    kind = "free" if free else ("stage" if stage_case(a) else "other")
    rows.append((c.name, kind, status, diffs, unallowed))

KIND = {"free": "不需解碼", "stage": "本階段", "other": "其他格式"}
gated = [r for r in rows if r[1] in ("free", "stage")]
other = [r for r in rows if r[1] == "other"]
lines = ["# C++ iat 對 Rust iat 比對結果", "",
         "- C++：`%s`" % SIDES["cpp"], "- Rust：`%s`" % SIDES["rust"],
         "- 本階段格式：%s" % (", ".join(sorted(ACCEPT)) or "（無）"), "",
         "| 案例 | 類別 | 結果 | 差異欄位 |", "|---|---|---|---|"]
for name, kind, status, diffs, unallowed in gated + other:
    fields = ", ".join(("%s%s" % (d[0], "" if d in unallowed else "（允許）")) for d in diffs[:8])
    if len(diffs) > 8:
        fields += " …（共 %d 項）" % len(diffs)
    lines.append("| %s | %s | %s | %s |" % (name, KIND[kind], status, fields))
lines += ["", "## 驗收案例（不需解碼與本階段）的差異明細", ""]
for name, kind, status, diffs, unallowed in gated:
    for d in diffs:
        lines.append("- `%s` `%s`%s：C++ `%.200r` / Rust `%.200r`" % (
            name, d[0], "（允許：%s）" % allowed(name, d[0])[2] if d not in unallowed else "", d[1], d[2]))
with open(os.path.join(OUT, "diff-table.md"), "w", encoding="utf-8") as f:
    f.write("\n".join(lines) + "\n")

for name, kind, status, diffs, unallowed in gated:
    print("%-8s %-6s %-44s %s" % (status, kind, name, ", ".join(d[0] for d in diffs)))
    for d in unallowed:
        print("    %s\n      C++ : %.300r\n      Rust: %.300r" % d)
same = lambda rs: sum(1 for r in rs if r[2] != "DIFF")
for k in ("free", "stage"):
    rs = [r for r in gated if r[1] == k]
    print("\n%s cases: %d, matching: %d (allowed differences included)" % (k, len(rs), same(rs)), end="")
print("\nother cases (informational): %d, matching: %d" % (len(other), same(other)))
print("diff table: %s" % os.path.join(OUT, "diff-table.md"))
sys.exit(0 if same(gated) == len(gated) else 1)
