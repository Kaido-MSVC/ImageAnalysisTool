"""Compares two run_cases.py outputs. Volatile values are masked:
run_id (UUID), timing/memory/io metrics, temp free bytes, the case output directory,
and the tool version (only when --ignore-version).

JSON is compared parsed (masked values), plus a layout check of the JSON text with values
masked; Markdown, stdout and stderr are compared as masked text. Exit code 0 only when
every case matches and both sides have the same cases.

usage: py -3.13 -I tools/regress/compare_runs.py <dirA> <dirB> [--ignore-version VER_A VER_B]
           [--path-map PREFIX_A PREFIX_B ...]

--path-map (repeatable): the same input folder lived at PREFIX_A when run A was made and
at PREFIX_B for run B (for example the pairs folder was moved). Both prefixes are replaced
by the same token before comparing; nothing else about the paths is ignored.
"""
import json, os, re, sys
sys.stdout.reconfigure(encoding="utf-8")

A, B = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
VA = VB = None
if "--ignore-version" in sys.argv:
    i = sys.argv.index("--ignore-version"); VA, VB = sys.argv[i + 1], sys.argv[i + 2]
MAP_A, MAP_B = [], []
for i, a in enumerate(sys.argv):
    if a == "--path-map":
        MAP_A.append(os.path.abspath(sys.argv[i + 1])); MAP_B.append(os.path.abspath(sys.argv[i + 2]))


def map_paths(b, prefixes):
    # JSON-escaped form first (backslashes doubled), then the plain form.
    for k, p in enumerate(prefixes):
        tok = b"<PATHMAP%d>" % k
        b = b.replace(p.replace("\\", "\\\\").encode("utf-8"), tok).replace(p.encode("utf-8"), tok)
    return b
UUID = re.compile(rb"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def mask_json(j, ver):
    if isinstance(j, dict):
        j = dict(j)
        if "run_id" in j: j["run_id"] = "<RUN>"
        if "metrics" in j: j["metrics"] = {k: sorted(v.keys()) if isinstance(v, dict) else type(v).__name__
                                           for k, v in j["metrics"].items()}
        if "tool" in j and isinstance(j["tool"], dict) and ver:
            t = dict(j["tool"]); t["version"] = "<VER>" if t.get("version") == ver else t.get("version"); j["tool"] = t
        if "settings" in j and isinstance(j["settings"], dict) and "temp" in j["settings"]:
            s = json.loads(json.dumps(j["settings"])); s["temp"].pop("free_bytes_at_start", None); j["settings"] = s
        return {k: mask_json(v, ver) for k, v in j.items()}
    if isinstance(j, list): return [mask_json(v, ver) for v in j]
    return j


def mask_text(b, case_dir, ver):
    b = b.replace(case_dir.encode("utf-8"), b"<CASE>").replace(case_dir.replace("\\", "\\\\").encode("utf-8"), b"<CASE>")
    b = UUID.sub(b"<RUN>", b)
    if ver:
        b = b.replace(("iat " + ver).encode(), b"iat <VER>").replace(('"version": "%s"' % ver).encode(), b'"version": "<VER>"')
        b = b.replace(("| " + ver + " ").encode(), b"| <VER> ")
    # metric numbers in Markdown (lines that carry timings / memory / io / free space)
    out = []
    for line in b.split(b"\n"):
        if re.search(rb"(ms|time|wall|peak|commit|io_read|I/O|free|MiB|bytes read|\xe6\x99\x82\xe9\x96\x93)", line, re.I) and re.search(rb"\d", line):
            line = re.sub(rb"\d[\d.,]*", b"#", line)
        out.append(line)
    return b"\n".join(out)


def load(d, ver, prefixes):
    r = {}
    for n in ("exit", "stdout", "stderr", "report.json", "report.md"):
        p = os.path.join(d, n)
        if not os.path.exists(p): r[n] = None; continue
        b = map_paths(open(p, "rb").read(), prefixes)
        if n == "report.json" or (n == "stdout" and b.startswith(b"{")):
            try:
                r[n + ":json"] = mask_json(json.loads(b), ver)
                r[n + ":raw"] = mask_text(b, d, ver) if n == "stdout" else None
                # keep the textual layout check (indent, key order) with masked values
                r[n + ":layout"] = re.sub(rb'": ("[^"]*"|-?[\d.e+-]+)', b'": V', mask_text(b, d, ver))
                continue
            except Exception:
                pass
        r[n] = mask_text(b, d, ver)
    return r


cases = sorted(x for x in os.listdir(A) if os.path.isdir(os.path.join(A, x)))
missing = sorted(set(cases) ^ {x for x in os.listdir(B) if os.path.isdir(os.path.join(B, x))})
bad = 0
for c in cases:
    if c in missing: continue
    ra, rb = load(os.path.join(A, c), VA, MAP_A), load(os.path.join(B, c), VB, MAP_B)
    diffs = [k for k in sorted(set(ra) | set(rb)) if not k.endswith(":raw") and ra.get(k) != rb.get(k)]
    if diffs:
        bad += 1
        print("DIFF %-46s %s" % (c, ", ".join(diffs)))
        for k in diffs:
            va, vb = ra.get(k), rb.get(k)
            if isinstance(va, bytes) and isinstance(vb, bytes):
                la, lb = va.split(b"\n"), vb.split(b"\n")
                for i, (x, y) in enumerate(zip(la, lb)):
                    if x != y: print("   line %d\n     A: %r\n     B: %r" % (i, x[:200], y[:200])); break
                else: print("   length %d vs %d lines" % (len(la), len(lb)))
            elif isinstance(va, dict) and isinstance(vb, dict):
                for kk in sorted(set(va) | set(vb)):
                    if va.get(kk) != vb.get(kk): print("   key %s\n     A: %.300s\n     B: %.300s" % (kk, va.get(kk), vb.get(kk)))
            else:
                print("   A: %.200r\n   B: %.200r" % (va, vb))
print("cases: %d, differing: %d, only in one side: %s" % (len(cases), bad, missing or "none"))
sys.exit(1 if bad or missing else 0)
