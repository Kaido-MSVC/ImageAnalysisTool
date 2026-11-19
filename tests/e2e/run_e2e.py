"""End-to-end tests for iat.exe (standard library only).

Usage:
  python -I tests/e2e/run_e2e.py <bin-dir> <fixture-dir> <out-dir>
         [--big <work-dir>] [--private <path-to-big-png-261mp.png>]
         [--only <regex> [--groups safety,io,limits,temp,big,private,tiff,tiffbig,jxlbig,webpbig]]

Stage 3 pairs and singles are named jxl_*, webp_*, mixed_*, imbc_*, inspect_webp_*, inspect_jxl_*,
inspect_imbc_* (fixtures need gen_fixtures.py --codec-bin). jxlbig / webpbig need --big;
their inputs are generated once into that directory.

Without --only every case runs (full regression). With --only, only the
pairs/singles whose name matches run, plus the listed groups.

Every run checks: exit code, strict result, expected issue codes, JSON
parseability, the same run_id in both reports, and that every input file has
the same SHA-256 before and after (read-only guarantee).

Private material (--private) is referred to only by its code name in output.
"""

import hashlib
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid

BIN, FIX, OUT = sys.argv[1:4]
BIG = sys.argv[sys.argv.index("--big") + 1] if "--big" in sys.argv else None
PRIVATE = sys.argv[sys.argv.index("--private") + 1] if "--private" in sys.argv else None
ONLY = re.compile(sys.argv[sys.argv.index("--only") + 1]) if "--only" in sys.argv else None
GROUPS = set(sys.argv[sys.argv.index("--groups") + 1].split(",")) if "--groups" in sys.argv else set()
IAT = os.path.join(BIN, "iat.exe")
REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
results = []

shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)


def want(group):
    return ONLY is None or group in GROUPS


def want_case(name):
    return ONLY is None or ONLY.search(name)


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 22), b""):
            h.update(b)
    return h.hexdigest()


def record(group, name, ok, detail, extra=None):
    results.append({"group": group, "name": name, "pass": bool(ok), "detail": detail, **(extra or {})})
    print("%-5s %-8s %-34s %s" % ("PASS" if ok else "FAIL", group, name, detail), flush=True)


def fingerprint(path):
    """Size, modification time (ns) and SHA-256: an input counts as unchanged only if all three are."""
    st = os.stat(path)
    return st.st_size, st.st_mtime_ns, sha(path)


def iat(args, inputs, timeout=1800):
    before = {p: fingerprint(p) for p in inputs if os.path.isfile(p)}
    t0 = time.perf_counter()
    p = subprocess.run([IAT, *args], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout)
    wall = time.perf_counter() - t0
    after = {p_: fingerprint(p_) for p_ in before}
    return p, before == after, wall


def report_temps(d):
    """Leftover report temporaries (<target>.iat-tmp-<run id prefix>) in a directory."""
    return sorted(n for n in os.listdir(d) if ".iat-tmp-" in n)


def load(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return None


def check_reports(name, rc, md, js):
    """Both reports exist, JSON parses, same run_id, exit code recorded."""
    j = load(js)
    if j is None:
        return None, "json missing or unparseable"
    if not os.path.isfile(md):
        return j, "markdown missing"
    with open(md, encoding="utf-8") as f:
        mtext = f.read()
    if j["run_id"] not in mtext:
        return j, "run_id differs between reports"
    if j["exit"]["code"] != rc:
        return j, "exit code in JSON %s != process %s" % (j["exit"]["code"], rc)
    return j, ""


def codes_of(j):
    return {i["code"] for i in j.get("issues", [])}


def run_pair(name, a, b, extra_args=(), expect_exit=None):
    md, js = os.path.join(OUT, name + ".md"), os.path.join(OUT, name + ".json")
    p, unchanged, wall = iat(["compare", a, b, "--report-md", md, "--report-json", js, *extra_args], [a, b])
    j, problem = check_reports(name, p.returncode, md, js)
    return p, j, problem, unchanged, wall


def mem_of(j, key):
    return int(((j or {}).get("metrics", {}).get("memory_bytes", {}).get(key)) or 0)


def own_temp_files(d):
    return sorted(n for n in os.listdir(d) if re.fullmatch(r"iat-tmp-[0-9a-f-]{36}-[ab]\.bin", n))


def system_temp_dir():
    # GetTempPath2W for a normal user resolves TMP, then TEMP, then USERPROFILE.
    for k in ("TMP", "TEMP", "USERPROFILE"):
        if os.environ.get(k):
            return os.path.normcase(os.path.abspath(os.environ[k]))
    return ""


# ------------------------------------------------------------------ pairs
spec = load(os.path.join(FIX, "pairs.json"))
for c in spec["pairs"]:
    if not want_case(c["name"]):
        continue
    a, b = os.path.join(FIX, c["original"]), os.path.join(FIX, c["output"])
    p, j, problem, unchanged, _ = run_pair(c["name"], a, b, c.get("args", []))
    ok = not problem and unchanged and p.returncode == c["exit"] and j["strict"]["result"] == c["strict"]
    missing = [x for x in c["codes"] if j and x not in codes_of(j)]
    ok = ok and not missing
    chk = c.get("check", {})
    if ok and chk:
        s = j["samples"]
        if "pixels_differing" in chk:
            ok = ok and s["pixels_differing"] == chk["pixels_differing"]
        if "first" in chk:
            f0 = s["first_differences"][0] if s["first_differences"] else {}
            ok = ok and [f0.get("x"), f0.get("y")] == chk["first"]
        if "coords" in chk:
            ok = ok and [[f["x"], f["y"]] for f in s["first_differences"]] == chk["coords"]
        if "channel" in chk:
            ok = ok and any(ch["name"] == chk["channel"] and ch["differing"] != "0" for ch in s["per_channel"])
        if "max_abs" in chk:
            ok = ok and max(ch["max_abs_error"] for ch in s["per_channel"]) == chk["max_abs"]
        if "max_abs_below" in chk:
            ok = ok and max(ch["max_abs_error"] for ch in s["per_channel"]) < chk["max_abs_below"]
        if "samples_equal" in chk:
            ok = ok and (s["status"] == "equal") == chk["samples_equal"]
        if "message_contains" in chk:
            ok = ok and any(chk["message_contains"] in i.get("message", "") for i in j.get("issues", []))
        if "orientation_source" in chk:
            ok = ok and all(j[k]["orientation"]["source"] == chk["orientation_source"] for k in ("original", "output"))
        if "temp_used" in chk:
            ok = ok and (mem_of(j, "temp_reserved_peak") > 0) == chk["temp_used"]
        if "resource_need" in chk:   # 0.5.0: machine-readable limit suggestion on one side
            want_rn = dict(chk["resource_need"])
            got_rn = j[want_rn.pop("side")].get("resource_need") or {}
            ok = ok and all(got_rn.get(k) == v for k, v in want_rn.items()) and int(got_rn.get("needed_bytes", "0")) > 0
    record("pair", c["name"], ok, "exit=%s strict=%s samples=%s color=%s %s%s%s" % (
        p.returncode, j and j["strict"]["result"], j and j["samples"]["status"], j and j["color"]["status"],
        ("missing codes %s " % missing) if missing else "", problem, "" if unchanged else " INPUT CHANGED"))

for c in spec["singles"]:
    if not want_case(c["name"]):
        continue
    f = os.path.join(FIX, c["file"])
    md, js = os.path.join(OUT, c["name"] + ".md"), os.path.join(OUT, c["name"] + ".json")
    p, unchanged, _ = iat(["inspect", f, "--report-md", md, "--report-json", js], [f])
    j, problem = check_reports(c["name"], p.returncode, md, js)
    missing = [x for x in c.get("codes", []) if j and x not in codes_of(j)]
    dp_ok = "dimension_policy" not in c or (j and j["original"].get("dimension_policy") == c["dimension_policy"])
    # Expected metadata summaries (exact) and issue message text, e.g. file strings shown as UTF-8.
    meta = (j or {}).get("original", {}).get("metadata", {})
    bad_meta = [k for k, v in c.get("meta", {}).items() if meta.get(k, {}).get("summary") != v]
    if "message_contains" in c and not (j and any(c["message_contains"] in i.get("message", "") for i in j.get("issues", []))):
        bad_meta.append("message")
    dp_ok = dp_ok and not bad_meta
    record("inspect", c["name"], not problem and unchanged and p.returncode == c["exit"] and not missing and dp_ok,
           "exit=%s decode=%s/%s %s%s%s%s" % (p.returncode, j and j["original"]["decode_status"],
                                          j and j["original"]["decode_status_reason"],
                                          ("missing codes %s " % missing) if missing else "",
                                          "" if dp_ok or bad_meta else "dimension_policy=%s " % (j and j["original"].get("dimension_policy")),
                                          ("unexpected %s " % bad_meta) if bad_meta else "", problem))

a = os.path.join(FIX, "s1_rgb16_srgb.png")
b = os.path.join(FIX, "s1_rgb16_srgb_lsb.png")

# ------------------------------------------------------------------ report safety and I/O errors
if want("safety"):
    existing = os.path.join(OUT, "existing.md")
    with open(existing, "w", encoding="utf-8") as f:
        f.write("keep me\n")
    ex_fp = fingerprint(existing)
    p, unchanged, _ = iat(["compare", a, b, "--report-md", existing], [a, b])
    record("safety", "existing_report_not_overwritten", p.returncode == 5 and fingerprint(existing) == ex_fp and unchanged,
           "exit=%s stderr=%s" % (p.returncode, p.stderr.strip()[:80]))

    # Output already exists: JSON exists and Markdown is new -> neither is written.
    ex_js, new_md = os.path.join(OUT, "existing_only.json"), os.path.join(OUT, "existing_only_new.md")
    with open(ex_js, "w", encoding="utf-8") as f:
        f.write('{"keep": true}\n')
    ex_fp = fingerprint(ex_js)
    p, unchanged, _ = iat(["compare", a, b, "--report-md", new_md, "--report-json", ex_js], [a, b])
    record("safety", "existing_json_not_overwritten_md_not_written",
           p.returncode == 5 and unchanged and fingerprint(ex_js) == ex_fp and not os.path.exists(new_md) and not report_temps(OUT),
           "exit=%s md_written=%s temps=%s" % (p.returncode, os.path.exists(new_md), report_temps(OUT)))

    # Output already exists: both reports exist -> both keep size, mtime and content.
    both = [os.path.join(OUT, "both_existing.md"), os.path.join(OUT, "both_existing.json")]
    for path in both:
        with open(path, "w", encoding="utf-8") as f:
            f.write("keep me\n")
    fps = [fingerprint(x) for x in both]
    p, unchanged, _ = iat(["compare", a, b, "--report-md", both[0], "--report-json", both[1]], [a, b])
    record("safety", "both_existing_reports_not_overwritten",
           p.returncode == 5 and unchanged and [fingerprint(x) for x in both] == fps and not report_temps(OUT),
           "exit=%s" % p.returncode)

    # Output path equals an input file: the original (first argument), for each report format.
    for fmt, other in (("md", "json"), ("json", "md")):
        other_path = os.path.join(OUT, "report_is_original_%s.%s" % (fmt, other))
        p, unchanged, _ = iat(["compare", a, b, "--report-" + fmt, a, "--report-" + other, other_path], [a, b])
        record("safety", "report_%s_is_original_input" % fmt,
               p.returncode == 5 and unchanged and not os.path.exists(other_path) and not report_temps(OUT)
               and "input file" in p.stderr,
               "exit=%s other_written=%s stderr=%s" % (p.returncode, os.path.exists(other_path), p.stderr.strip()[:60]))

    # Opening an input never asks for write access: deny this user every write right on
    # copies of both inputs (ACL), then compare. The ACE is removed in finally so a failed
    # run never leaves an undeletable file behind.
    acl_in = [os.path.join(OUT, "acl_original.png"), os.path.join(OUT, "acl_output.png")]
    shutil.copyfile(a, acl_in[0])
    shutil.copyfile(b, acl_in[1])
    user = subprocess.run(["whoami"], capture_output=True, text=True).stdout.strip()
    denied, detail = [], ""
    try:
        for path in acl_in:
            # Specific rights only: "(W)" would also deny SYNCHRONIZE/READ_CONTROL and block reading.
            r = subprocess.run(["icacls", path, "/deny", user + ":(WD,AD,WEA,WA)"], capture_output=True, text=True)
            if r.returncode != 0:
                detail = "icacls deny failed: " + (r.stdout + r.stderr).strip()[:80]
                break
            try:
                with open(path, "r+b"):
                    pass
                denied.append(False)
            except PermissionError:
                denied.append(True)
        if not detail:
            p, j, problem, unchanged, _ = run_pair("input_write_denied_acl", acl_in[0], acl_in[1])
            ok = (denied == [True, True] and p.returncode == 1 and not problem and unchanged
                  and "input_open_failed" not in codes_of(j))
            detail = "write_denied=%s exit=%s %s" % (denied, p.returncode, problem)
        else:
            ok = False
    finally:
        for path in acl_in:
            subprocess.run(["icacls", path, "/remove:d", user], capture_output=True)
    record("safety", "input_write_denied_acl", ok, detail)

    p, unchanged, _ = iat(["compare", a, b, "--report-md", b], [a, b])
    record("safety", "report_path_is_input", p.returncode == 5 and unchanged, "exit=%s" % p.returncode)

    link = os.path.join(OUT, "hardlink_to_input.png")
    os.link(b, link)
    p, unchanged, _ = iat(["compare", a, b, "--report-json", link], [a, b])
    record("safety", "report_path_is_hardlink_of_input", p.returncode == 5 and unchanged and sha(link) == sha(b),
           "exit=%s" % p.returncode)

    same = os.path.join(OUT, "same.json")
    p, _, _ = iat(["compare", a, b, "--report-md", same, "--report-json", same.upper()], [a, b])
    record("safety", "both_reports_same_path", p.returncode == 5 and not os.path.exists(same), "exit=%s" % p.returncode)

    p, _, _ = iat(["compare", a, b, "--report-md", os.path.join(OUT, "no_such_dir", "r.md")], [a, b])
    record("safety", "report_dir_missing", p.returncode == 5, "exit=%s" % p.returncode)

    p, _, _ = iat(["compare", a, b], [a, b])
    record("safety", "no_report_requested", p.returncode == 5, "exit=%s" % p.returncode)

    p, _, _ = iat(["compare", a, b, "--stdout", "json", "--limits", "deadline_s=0"], [a, b])
    record("safety", "limit_zero_rejected", p.returncode == 5, "exit=%s %s" % (p.returncode, p.stderr.strip()[:60]))

if want("io"):
    missing_in = os.path.join(FIX, "does_not_exist.png")
    md, js = os.path.join(OUT, "missing_input.md"), os.path.join(OUT, "missing_input.json")
    p, _, _ = iat(["compare", a, missing_in, "--report-md", md, "--report-json", js], [a])
    j, problem = check_reports("missing_input", p.returncode, md, js)
    record("io", "missing_input_reports_written", p.returncode == 5 and not problem and "input_open_failed" in codes_of(j),
           "exit=%s %s" % (p.returncode, problem))

    # Unicode file names with spaces, read-only attribute, stdout JSON
    uni_dir = os.path.join(OUT, "中文 目錄")
    os.makedirs(uni_dir)
    ua, ub = os.path.join(uni_dir, "原檔 測試.png"), os.path.join(uni_dir, "輸出 檔案（1）.png")
    shutil.copyfile(a, ua)
    shutil.copyfile(b, ub)
    os.chmod(ua, stat.S_IREAD)
    os.chmod(ub, stat.S_IREAD)
    md, js = os.path.join(uni_dir, "報告 一.md"), os.path.join(uni_dir, "報告 一.json")
    p, unchanged, _ = iat(["compare", ua, ub, "--report-md", md, "--report-json", js], [ua, ub])
    j, problem = check_reports("unicode", p.returncode, md, js)
    record("io", "unicode_names_readonly_inputs", p.returncode == 1 and not problem and unchanged and
           j["original"]["name"] == "原檔 測試.png" and not (os.stat(ua).st_mode & stat.S_IWRITE),
           "exit=%s %s" % (p.returncode, problem))
    p = subprocess.run([IAT, "compare", ua, ub, "--stdout", "json"], capture_output=True)
    try:
        sj = json.loads(p.stdout.decode("utf-8"))
        ok = p.returncode == 1 and sj["exit"]["code"] == 1
    except Exception:
        ok = False
    record("io", "stdout_json", ok, "exit=%s" % p.returncode)
    p = subprocess.run([IAT, "compare", ua, ub, "--stdout", "json", "--full-paths"], capture_output=True)
    record("io", "full_paths_opt_in", p.returncode == 1 and "中文 目錄" in json.loads(p.stdout.decode("utf-8"))["original"]["path"],
           "exit=%s" % p.returncode)
    for f in (ua, ub):
        os.chmod(f, stat.S_IREAD | stat.S_IWRITE)

    # Ctrl+Break while the inputs are being hashed (before any worker exists): a 16 GiB
    # sparse file takes about 40 s to hash (~400 MB/s measured), so the run must stop within seconds.
    sparse = os.path.join(OUT, "sparse_16gib.bin")
    with open(sparse, "wb"):
        pass
    r = subprocess.run(["fsutil", "sparse", "setflag", sparse], capture_output=True)
    if r.returncode == 0:
        with open(sparse, "r+b") as f:
            f.truncate(16 << 30)
        md, js = os.path.join(OUT, "cancel_during_hash.md"), os.path.join(OUT, "cancel_during_hash.json")
        proc = subprocess.Popen([IAT, "compare", sparse, a, "--report-md", md, "--report-json", js],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        time.sleep(1.0)
        t0 = time.perf_counter()
        proc.send_signal(signal.CTRL_BREAK_EVENT)
        proc.communicate(timeout=300)
        after = time.perf_counter() - t0
        j, problem = check_reports("cancel_during_hash", proc.returncode, md, js)
        ok = (proc.returncode == 4 and not problem and after < 5 and j["original"]["decode_status_reason"] == "cancelled"
              and "cancelled" in codes_of(j))
        record("io", "cancel_during_input_hash", ok, "exit=%s after=%.2fs %s" % (proc.returncode, after, problem))
    else:
        record("io", "cancel_during_input_hash", False, "fsutil sparse setflag failed: %s" % r.returncode)
    os.remove(sparse)

# ------------------------------------------------------------------ resource limits
if want("limits"):
    wide = os.path.join(FIX, "s1_wide_rgb16_srgb.png")
    p, j, problem, unchanged, _ = run_pair("budget_rows", wide, wide, ["--limits", "buffer_mib=8"])
    ok = (p.returncode == 4 and not problem and unchanged and j["original"]["decode_status"] == "not_checked"
          and j["original"]["decode_status_reason"] == "resource_limit" and j["strict"]["result"] == "not_evaluated")
    record("limits", "row_budget_not_checked", ok, "exit=%s decode=%s/%s %s" % (
        p.returncode, j and j["original"]["decode_status"], j and j["original"]["decode_status_reason"], problem))
    with open(os.path.join(OUT, "budget_rows.md"), encoding="utf-8") as f:
        record("limits", "markdown_says_not_checked", "未檢查" in f.read(), "")
    p, j, problem, unchanged, _ = run_pair("budget_ok", wide, wide, ["--limits", "buffer_mib=128"])
    record("limits", "row_budget_raised_ok", p.returncode == 0 and not problem, "exit=%s strict=%s" % (p.returncode, j and j["strict"]["result"]))
    p, j, problem, unchanged, _ = run_pair("job_commit", wide, wide, ["--limits", "buffer_mib=512", "job_commit_mib=24"])
    ok = (p.returncode == 4 and not problem and unchanged and j["strict"]["result"] == "not_evaluated"
          and "resource_limit" in codes_of(j) and j["original"]["decode_status"] == "not_checked")
    record("limits", "job_commit_limit", ok, "exit=%s decode=%s/%s %s" % (
        p.returncode, j and j["original"]["decode_status"], j and j["original"]["decode_status_reason"], problem))

# ------------------------------------------------------------------ temporary storage
if want("temp"):
    a7 = os.path.join(FIX, "s2_rgba16_37x29_adam7.png")
    a7p = os.path.join(FIX, "s2_rgba16_37x29_plain.png")
    # Default location: the system temp directory, never inside the repository.
    p = subprocess.run([IAT, "compare", a7p, a7, "--stdout", "json", "--full-paths"], capture_output=True)
    j = json.loads(p.stdout.decode("utf-8"))
    tdir = os.path.normcase(os.path.abspath(j["settings"]["temp"].get("dir", "")))
    inside_repo = tdir == os.path.normcase(REPO) or tdir.startswith(os.path.normcase(REPO) + os.sep)
    record("temp", "default_dir_is_system_temp", p.returncode == 0 and j["settings"]["temp"]["location"] == "system_temp"
           and not inside_repo and tdir == system_temp_dir() and mem_of(j, "temp_reserved_peak") > 0,
           "exit=%s temp_dir=%s inside_repo=%s reserved=%s" % (p.returncode, tdir, inside_repo, mem_of(j, "temp_reserved_peak")))
    # Without --full-paths the directory is not disclosed.
    p = subprocess.run([IAT, "compare", a7p, a7, "--stdout", "json"], capture_output=True)
    j = json.loads(p.stdout.decode("utf-8"))
    record("temp", "temp_dir_hidden_without_full_paths", "dir" not in j["settings"]["temp"], "")

    # --temp-dir: used, then left empty (delete-on-close).
    td = os.path.join(OUT, "temp dir 暫存")
    os.makedirs(td)
    p, j, problem, unchanged, _ = run_pair("temp_dir_option", a7p, a7, ["--temp-dir", td])
    record("temp", "temp_dir_option_used_and_empty_after", p.returncode == 0 and not problem and
           j["settings"]["temp"]["location"] == "user_specified" and not own_temp_files(td) and
           "temp_cleanup_failed" not in codes_of(j), "exit=%s left=%s" % (p.returncode, own_temp_files(td)))
    p, _, _ = iat(["compare", a7p, a7, "--stdout", "json", "--temp-dir", os.path.join(OUT, "missing_dir")], [a7p, a7])
    record("temp", "temp_dir_missing_rejected", p.returncode == 5, "exit=%s" % p.returncode)

    # Stale leftovers: only names this tool creates are removed.
    stale = "iat-tmp-%s-a.bin" % uuid.uuid4()
    foreign = "iat-tmp-not-ours.bin"
    # Same prefix, suffix and length as ours, but the middle 36 characters are not a UUID.
    lookalikes = ["iat-tmp-%s-a.bin" % ("x" * 36), "iat-tmp-%s-b.bin" % (uuid.uuid4().hex + "0000"),
                  "iat-tmp-%s-a.bin" % str(uuid.uuid4()).replace("-", "_")]
    for n in (stale, foreign, *lookalikes):
        with open(os.path.join(td, n), "wb") as f:
            f.write(b"x" * 16)
    p, j, problem, _, _ = run_pair("temp_stale", a7p, a7, ["--temp-dir", td])
    record("temp", "stale_own_file_removed_foreign_kept", p.returncode == 0 and not os.path.exists(os.path.join(td, stale))
           and os.path.exists(os.path.join(td, foreign)) and "temp_stale_removed" in codes_of(j),
           "exit=%s stale_gone=%s foreign_kept=%s" % (p.returncode, not os.path.exists(os.path.join(td, stale)),
                                                      os.path.exists(os.path.join(td, foreign))))
    os.remove(os.path.join(td, foreign))
    kept = [os.path.exists(os.path.join(td, n)) for n in lookalikes]
    record("temp", "stale_lookalike_non_uuid_kept", all(kept), "kept=%s" % kept)
    for n in lookalikes:
        if os.path.exists(os.path.join(td, n)):
            os.remove(os.path.join(td, n))

    # Non-interlaced pairs never touch temp.
    p, j, problem, _, _ = run_pair("temp_not_used_plain", a, os.path.join(FIX, "s1_rgb16_srgb_split.png"))
    record("temp", "plain_png_uses_no_temp", p.returncode == 0 and mem_of(j, "temp_reserved_peak") == 0 and
           mem_of(j, "temp_written") == 0, "exit=%s reserved=%s" % (p.returncode, mem_of(j, "temp_reserved_peak")))


# ------------------------------------------------------------------ TIFF budget and tile temp path
# Sub-blocks are also filtered by --only: "tiff_budget_temp" and "tiff_libtiff_alloc".
if want("tiff") and want_case("tiff_budget_temp"):
    one =os.path.join(FIX, "s2_tif_rgb16_one_big_strip.tif")   # one Deflate strip, 12 MB decoded
    p, j, problem, unchanged, _ = run_pair("tiff_big_strip_budget", one, one, ["--limits", "buffer_mib=8"])
    ok = (p.returncode == 4 and not problem and unchanged and j["original"]["decode_status"] == "not_checked"
          and j["original"]["decode_status_reason"] == "resource_limit" and j["strict"]["result"] == "not_evaluated")
    record("tiff", "big_strip_over_budget_not_checked", ok, "exit=%s decode=%s/%s %s" % (
        p.returncode, j and j["original"]["decode_status"], j and j["original"]["decode_status_reason"], problem))
    p, j, problem, unchanged, _ = run_pair("tiff_big_strip_default", one, one)
    # No ICC in this file, so colour is unknown and strict stays indeterminate; samples must be equal.
    ok = (p.returncode == 2 and not problem and unchanged and j["samples"]["status"] == "equal"
          and j["samples"]["pixels_compared"] == str(2000 * 1000) and j["strict"]["reasons"] == ["color_unknown"])
    record("tiff", "big_strip_default_budget_ok", ok, "exit=%s samples=%s reasons=%s %s" % (
        p.returncode, j and j["samples"]["status"], j and j["strict"]["reasons"], problem))

    tile = os.path.join(FIX, "s2_tif_rgb16_4096x300_tile256.tif")
    tdiff = os.path.join(FIX, "s2_tif_rgb16_4096x300_tile256_diff.tif")
    strip = os.path.join(FIX, "s2_tif_rgb16_4096x300_strip16_be.tif")
    native = 4096 * 300 * 6
    p, j, problem, unchanged, _ = run_pair("tiff_tile_temp", tile, strip, ["--limits", "buffer_mib=8"])
    ok = (p.returncode == 0 and not problem and unchanged and j["strict"]["result"] == "match"
          and j["original"]["decode_path"].endswith("+temp") and not j["output"]["decode_path"].endswith("+temp")
          and mem_of(j, "temp_reserved_peak") == native and mem_of(j, "temp_written") == native
          and j["samples"]["pixels_compared"] == str(4096 * 300) and "temp_cleanup_failed" not in codes_of(j))
    record("tiff", "tile_temp_path_matches_strip", ok, "exit=%s path=%s reserved=%s written=%s %s" % (
        p.returncode, j and j["original"]["decode_path"], mem_of(j, "temp_reserved_peak"), mem_of(j, "temp_written"), problem))
    p, j, problem, unchanged, _ = run_pair("tiff_tile_temp_diff", tdiff, strip, ["--limits", "buffer_mib=8"])
    s = (j or {}).get("samples", {})
    f0 = (s.get("first_differences") or [{}])[0]
    ok = (p.returncode == 1 and not problem and unchanged and j["original"]["decode_path"].endswith("+temp")
          and s["pixels_differing"] == "1" and [f0.get("x"), f0.get("y")] == [4000, 290]
          and any(ch["name"] == "G" and ch["differing"] == "1" and ch["max_abs_error"] == 256 for ch in s["per_channel"]))
    record("tiff", "tile_temp_finds_one_diff_in_padded_row", ok, "exit=%s differing=%s first=%s,%s %s" % (
        p.returncode, s.get("pixels_differing"), f0.get("x"), f0.get("y"), problem))
    p, j, problem, unchanged, _ = run_pair("tiff_tile_band", tile, strip)
    ok = (p.returncode == 0 and not problem and unchanged and not j["original"]["decode_path"].endswith("+temp")
          and mem_of(j, "temp_reserved_peak") == 0)
    record("tiff", "tile_band_in_memory_by_default", ok, "exit=%s path=%s reserved=%s" % (
        p.returncode, j and j["original"]["decode_path"], mem_of(j, "temp_reserved_peak")))

if want("tiff") and want_case("tiff_libtiff_alloc"):
    # libtiff's own allocation cap (TIFFOpenOptionsSetMaxSingleMemAlloc) -> resource_limit, not decode_failed.
    shared =os.path.join(FIX, "s2_tif_gray8_147456_shared_tiles.tif")
    md, js = os.path.join(OUT, "tiff_libtiff_alloc_limit.md"), os.path.join(OUT, "tiff_libtiff_alloc_limit.json")
    p, unchanged, _ = iat(["inspect", shared, "--report-md", md, "--report-json", js,
                           "--limits", "buffer_mib=1", "metadata_item_mib=1"], [shared])
    j, problem = check_reports("tiff_libtiff_alloc_limit", p.returncode, md, js)
    msgs = [i.get("message", "") for i in (j or {}).get("issues", []) if i["code"] == "resource_limit"]
    ok = (p.returncode == 4 and not problem and unchanged and j["original"]["decode_status"] == "not_checked"
          and j["original"]["decode_status_reason"] == "resource_limit" and any("libtiff allocation limit" in m for m in msgs))
    record("tiff", "libtiff_alloc_cap_is_resource_limit", ok, "exit=%s decode=%s/%s msg=%s %s" % (
        p.returncode, j and j["original"]["decode_status"], j and j["original"]["decode_status_reason"],
        (msgs[0][:90] if msgs else None), problem))
    p, j, problem, unchanged, wall = run_pair("tiff_shared_tiles_default", shared, shared)
    record("tiff", "shared_tiles_default_limits_ok", p.returncode == 2 and not problem and unchanged and
           j["samples"]["status"] == "equal" and j["strict"]["reasons"] == ["color_unknown"],
           "exit=%s samples=%s wall=%.1fs %s" % (p.returncode, j and j["samples"]["status"], wall, problem))


def kill_during_run(name, args, td, how):
    """Start a long run with --temp-dir td, wait until its temp file exists,
    kill the parent (how='parent') or only the worker (how='worker'), then
    check that the temp files disappear."""
    md, js = os.path.join(OUT, name + ".md"), os.path.join(OUT, name + ".json")
    proc = subprocess.Popen([IAT, *args, "--temp-dir", td, "--report-md", md, "--report-json", js],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
    seen, size_seen, t0 = [], 0, time.perf_counter()
    while time.perf_counter() - t0 < 120:
        seen = own_temp_files(td)
        if seen:
            size_seen = max(os.path.getsize(os.path.join(td, n)) for n in seen)
            if size_seen > (64 << 20):
                break
        if proc.poll() is not None:
            break
        time.sleep(0.05)
    if how == "parent":
        proc.kill()          # TerminateProcess on iat.exe; its Job (KILL_ON_JOB_CLOSE) takes the worker down
        proc.communicate(timeout=60)
    else:
        subprocess.run(["taskkill", "/F", "/IM", "iat-worker.exe"], capture_output=True)
        proc.communicate(timeout=120)
    left = own_temp_files(td)
    for _ in range(100):
        if not left:
            break
        time.sleep(0.1)
        left = own_temp_files(td)
    return proc.returncode, seen, size_seen, left, md, js


# ------------------------------------------------------------------ large images
if BIG and want("big"):
    os.makedirs(BIG, exist_ok=True)
    gen = os.path.join(BIN, "gen_large_png.exe")
    s1, s2 = os.path.join(BIG, "synthetic_12949x20167_rgb8_l1.png"), os.path.join(BIG, "synthetic_12949x20167_rgb8_l2.png")
    s3 = os.path.join(BIG, "synthetic_12949x20167_rgb8_adam7.png")
    s4 = os.path.join(BIG, "synthetic_12949x20167_rgb8_adam7_l2.png")
    for f, extra in ((s1, ["--level", "1"]), (s2, ["--level", "2"]), (s3, ["--level", "1", "--adam7"]),
                     (s4, ["--level", "2", "--adam7"])):
        if not os.path.exists(f):
            t0 = time.perf_counter()
            g = subprocess.run([gen, f, "12949", "20167", "--rgb8", "--srgb", *extra], capture_output=True, text=True)
            print("generated", os.path.basename(f), g.stdout.strip(), "%.1fs" % (time.perf_counter() - t0))
    samples_bytes = 12949 * 20167 * 3
    if want_case("big_synthetic"):
        p, j, problem, unchanged, wall = run_pair("big_synthetic", s1, s2)
        peak = mem_of(j, "job_peak_commit")
        ok = p.returncode == 0 and not problem and unchanged and j["samples"]["pixels_compared"] == "261142483" and peak < (64 << 20)
        record("big", "synthetic_compare", ok, "exit=%s wall=%.1fs job_peak_commit=%.1fMiB ws=%.1fMiB" % (
            p.returncode, wall, peak / 2**20, mem_of(j, "worker_peak_working_set") / 2**20), {"metrics": j and j["metrics"]})

    # Adam7 of the same size as big-png-261mp: the default temp_gib must be enough.
    p, j, problem, unchanged, wall = run_pair("big_adam7_vs_plain", s1, s3)
    peak = mem_of(j, "job_peak_commit")
    ok = (p.returncode == 0 and not problem and unchanged and j["samples"]["pixels_compared"] == "261142483"
          and mem_of(j, "temp_reserved_peak") == samples_bytes and mem_of(j, "temp_written") == samples_bytes
          and peak < (64 << 20) and "temp_cleanup_failed" not in codes_of(j))
    record("big", "adam7_vs_plain_default_temp", ok, "exit=%s wall=%.1fs temp=%.1fMiB job_peak_commit=%.1fMiB temp_io=%.0fms" % (
        p.returncode, wall, mem_of(j, "temp_reserved_peak") / 2**20, peak / 2**20,
        (j or {}).get("metrics", {}).get("time_ms", {}).get("temp_io", 0)), {"metrics": j and j["metrics"]})

    # Both sides interlaced: 2 x 747 MiB in temp; temp_gib=1 must stop the second side up front.
    p, j, problem, unchanged, wall = run_pair("big_adam7_quota", s3, s4, ["--limits", "temp_gib=1"])
    ok = (p.returncode == 4 and not problem and unchanged and j["output"]["decode_status"] == "not_checked"
          and j["output"]["decode_status_reason"] == "resource_limit" and j["strict"]["result"] == "not_evaluated")
    record("big", "adam7_temp_quota_not_checked", ok, "exit=%s wall=%.1fs out=%s/%s orig=%s" % (
        p.returncode, wall, j and j["output"]["decode_status"], j and j["output"]["decode_status_reason"],
        j and j["original"]["decode_status"]))
    p, j, problem, unchanged, wall = run_pair("big_adam7_both", s3, s4)
    record("big", "adam7_both_sides_temp", p.returncode == 0 and not problem and unchanged and
           mem_of(j, "temp_reserved_peak") == 2 * samples_bytes,
           "exit=%s wall=%.1fs temp=%.1fMiB job_peak_commit=%.1fMiB" % (
               p.returncode, wall, mem_of(j, "temp_reserved_peak") / 2**20, mem_of(j, "job_peak_commit") / 2**20),
           {"metrics": j and j["metrics"]})

    # Crash / termination: delete-on-close must remove the temp file.
    td = os.path.join(OUT, "kill-temp")
    os.makedirs(td, exist_ok=True)
    rc, seen, size_seen, left, md, js = kill_during_run("kill_parent", ["compare", s1, s3], td, "parent")
    record("big", "temp_gone_after_parent_killed", bool(seen) and size_seen > 0 and not left,
           "rc=%s seen=%d max_size=%.1fMiB left=%s" % (rc, len(seen), size_seen / 2**20, left))
    rc, seen, size_seen, left, md, js = kill_during_run("kill_worker", ["compare", s1, s3], td, "worker")
    j, problem = check_reports("kill_worker", rc, md, js)
    record("big", "temp_gone_after_worker_killed", bool(seen) and not left and rc == 5 and j and
           "worker_crashed" in codes_of(j) and "temp_cleanup_failed" not in codes_of(j),
           "rc=%s seen=%d max_size=%.1fMiB left=%s %s" % (rc, len(seen), size_seen / 2**20, left, problem))

    if want_case("big_deadline"):
        p, j, problem, unchanged, _ = run_pair("big_deadline", s1, s2, ["--limits", "deadline_s=1"])
        ok = p.returncode == 4 and not problem and j["original"]["decode_status"] == "not_checked" and \
            j["original"]["decode_status_reason"] == "deadline" and j["strict"]["result"] == "not_evaluated"
        record("big", "deadline_not_checked", ok, "exit=%s decode=%s/%s" % (
            p.returncode, j and j["original"]["decode_status"], j and j["original"]["decode_status_reason"]))
    if want_case("big_cancel"):
        md, js = os.path.join(OUT, "big_cancel.md"), os.path.join(OUT, "big_cancel.json")
        before = sha(s1)
        proc = subprocess.Popen([IAT, "compare", s1, s2, "--report-md", md, "--report-json", js],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        time.sleep(2.0)
        t0 = time.perf_counter()
        proc.send_signal(signal.CTRL_BREAK_EVENT)
        proc.communicate(timeout=60)
        j, problem = check_reports("big_cancel", proc.returncode, md, js)
        ok = proc.returncode == 4 and not problem and j["original"]["decode_status_reason"] == "cancelled" and sha(s1) == before
        record("big", "ctrl_break_cancel", ok, "exit=%s after=%.2fs %s" % (proc.returncode, time.perf_counter() - t0, problem))

def allocated_bytes(path):
    """Bytes the file really occupies on disk (sparse ranges excluded)."""
    import ctypes
    from ctypes import wintypes
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    f = k32.GetCompressedFileSizeW
    f.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
    f.restype = wintypes.DWORD
    hi = wintypes.DWORD(0)
    ctypes.set_last_error(0)
    lo = f(path, ctypes.byref(hi))
    # INVALID_FILE_SIZE with a high-part pointer is ambiguous: only GetLastError() != 0 means failure.
    if lo == 0xFFFFFFFF and ctypes.get_last_error() != 0:
        raise OSError(ctypes.get_last_error(), "GetCompressedFileSizeW failed", path)
    return (hi.value << 32) | lo


# ------------------------------------------------------------------ large TIFF / BigTIFF
if BIG and want("tiffbig"):
    os.makedirs(BIG, exist_ok=True)
    gpng, gtif = os.path.join(BIN, "gen_large_png.exe"), os.path.join(BIN, "gen_large_tiff.exe")
    icc = os.path.join(FIX, "s2_icc1.icc")
    W, H = 16384, 8192
    base = os.path.join(BIG, "syn_%dx%d_rgb16" % (W, H))
    files = {
        "png": (base + "_icc.png", [gpng, None, str(W), str(H), "--rgb16", "--icc", icc, "--level", "1"]),
        "tif_strip": (base + "_strip16_le_none.tif", [gtif, None, str(W), str(H), "--icc", icc, "--rps", "16"]),
        "btf_tile256": (base + "_btf_tile256_be_deflate.tif",
                        [gtif, None, str(W), str(H), "--icc", icc, "--big", "--be", "--tile", "256", "--deflate"]),
        "btf_tile512": (base + "_btf_tile512_be_deflate.tif",
                        [gtif, None, str(W), str(H), "--icc", icc, "--big", "--be", "--tile", "512", "--deflate"]),
        "sparse_png": (os.path.join(BIG, "syn_2048x1024_rgb16_icc.png"),
                       [gpng, None, "2048", "1024", "--rgb16", "--icc", icc]),
        "sparse_btf": (os.path.join(BIG, "syn_2048x1024_rgb16_btf_gap5gib.tif"),
                       [gtif, None, "2048", "1024", "--icc", icc, "--big", "--rps", "64", "--deflate", "--gap-gib", "5"]),
    }
    gen_info = {}
    for key, (path, cmd) in files.items():
        if not os.path.exists(path):
            cmd = list(cmd)
            cmd[1] = path
            t0 = time.perf_counter()
            g = subprocess.run(cmd, capture_output=True, text=True)
            print("generated", os.path.basename(path), g.stdout.strip(), g.stderr.strip(), "%.1fs" % (time.perf_counter() - t0), flush=True)
            if g.returncode == 0:
                gen_info[key] = json.loads(g.stdout)
    px = W * H
    native = px * 6

    def big_tiff_pair(name, key, expect_temp):
        p, j, problem, unchanged, wall = run_pair(name, files["png"][0], files[key][0])
        o = (j or {}).get("output", {})
        reserved = mem_of(j, "temp_reserved_peak")
        ok = (p.returncode == 0 and not problem and unchanged and j["strict"]["result"] == "match"
              and j["samples"]["pixels_compared"] == str(px)
              and o.get("decode_path", "").endswith("+temp") == expect_temp
              and reserved == (native if expect_temp else 0)
              and mem_of(j, "job_peak_commit") < (128 << 20) and "temp_cleanup_failed" not in codes_of(j))
        record("tiffbig", name, ok, "exit=%s wall=%.1fs path=%s temp=%.1fMiB job_peak_commit=%.1fMiB ws=%.1fMiB %s" % (
            p.returncode, wall, o.get("decode_path"), reserved / 2**20, mem_of(j, "job_peak_commit") / 2**20,
            mem_of(j, "worker_peak_working_set") / 2**20, problem),
            {"metrics": j and j["metrics"], "wall_s": round(wall, 1), "output_size": os.path.getsize(files[key][0])})

    if want_case("tiffbig_strip"):
        big_tiff_pair("tiffbig_png_vs_tiff_strip", "tif_strip", False)
    if want_case("tiffbig_tile256"):
        big_tiff_pair("tiffbig_png_vs_bigtiff_tile256_band", "btf_tile256", False)
    if want_case("tiffbig_tile512"):
        big_tiff_pair("tiffbig_png_vs_bigtiff_tile512_temp", "btf_tile512", True)

    if want_case("tiffbig_height"):
        # Same width, a quarter of the height: row buffers and the budget estimate must not change
        # (values from the 16384x8192 strip run), so the row path never holds the whole image.
        hp, ht = os.path.join(BIG, "syn_16384x2048_rgb16_icc.png"), os.path.join(BIG, "syn_16384x2048_rgb16_strip16_le_none.tif")
        for path, cmd in ((hp, [gpng, hp, str(W), "2048", "--rgb16", "--icc", icc, "--level", "1"]),
                          (ht, [gtif, ht, str(W), "2048", "--icc", icc, "--rps", "16"])):
            if not os.path.exists(path):
                subprocess.run(cmd, capture_output=True, text=True)
        p, j, problem, unchanged, wall = run_pair("tiffbig_height_2048_strip", hp, ht)
        ok = (p.returncode == 0 and not problem and unchanged and j["samples"]["pixels_compared"] == str(W * 2048)
              and mem_of(j, "tool_row_buffers") == 1867776 and mem_of(j, "row_budget_estimate") == 2588864
              and mem_of(j, "job_peak_commit") < (8 << 20))
        record("tiffbig", "row_path_independent_of_height", ok, "exit=%s wall=%.1fs row_buffers=%s budget=%s job_peak_commit=%.1fMiB ws=%.1fMiB %s" % (
            p.returncode, wall, mem_of(j, "tool_row_buffers"), mem_of(j, "row_budget_estimate"),
            mem_of(j, "job_peak_commit") / 2**20, mem_of(j, "worker_peak_working_set") / 2**20, problem),
            {"metrics": j and j["metrics"], "wall_s": round(wall, 1)})

    if want_case("tiffbig_sparse"):
        sp = files["sparse_btf"][0]
        with open(sp, "rb") as f:   # BigTIFF header: II, 43, 8, 0, IFD offset (LE)
            hdr = f.read(16)
        ifd_off = int.from_bytes(hdr[8:16], "little")
        size, alloc = os.path.getsize(sp), allocated_bytes(sp)
        p, j, problem, unchanged, wall = run_pair("tiffbig_sparse_offsets_over_4gib", files["sparse_png"][0], sp)
        ok = (p.returncode == 0 and not problem and unchanged and j["strict"]["result"] == "match"
              and j["samples"]["pixels_compared"] == str(2048 * 1024)
              and ifd_off > (5 << 30) and size > (5 << 30) and alloc < (256 << 20))
        record("tiffbig", "sparse_offsets_over_4gib", ok, "exit=%s wall=%.1fs ifd_offset=%d size=%.2fGiB on_disk=%.1fMiB %s" % (
            p.returncode, wall, ifd_off, size / 2**30, alloc / 2**20, problem),
            {"metrics": j and j["metrics"], "ifd_offset": ifd_off, "size": size, "allocated": alloc})

    with open(os.path.join(OUT, "tiffbig-generated.json"), "w", encoding="utf-8") as f:
        json.dump(gen_info, f, indent=1)

# ------------------------------------------------------------------ large JPEG XL / WebP (stage 3)
def big_pattern(path_raw, path_png, w, h, ch, bits, png_pre=b""):
    """Writes the encoder input (raw, little endian) and the PNG truth from one pattern:
    row y is a pseudo-random base row rotated by 7*y pixels. Independent of every decoder."""
    import struct
    import zlib
    bps = 2 if bits > 8 else 1
    m = (1 << bits) - 1
    vals = [((i * 131 + (i // ch) * 17 + 5) * 2654435761 >> 9) & m for i in range(w * ch)]
    if bits > 8:
        le, be = struct.pack("<%dH" % len(vals), *vals), struct.pack(">%dH" % len(vals), *vals)
    else:
        le = be = bytes(vals)
    step = 7 * ch * bps
    ctype = {1: 0, 2: 4, 3: 2, 4: 6}[ch]

    def chunk(t, p):
        return struct.pack(">I", len(p)) + t + p + struct.pack(">I", zlib.crc32(t + p) & 0xFFFFFFFF)

    comp = zlib.compressobj(1)
    with open(path_raw, "wb") as fr, open(path_png, "wb") as fp:
        fp.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, bits, ctype, 0, 0, 0)) + png_pre)
        for y in range(h):
            k = (y * step) % len(le)
            fr.write(le[k:] + le[:k])
            z = comp.compress(b"\x00" + be[k:] + be[:k])
            if len(z) > (1 << 20) or y == h - 1:
                z += comp.flush() if y == h - 1 else b""
                if z:
                    fp.write(chunk(b"IDAT", z))
            elif z:
                fp.write(chunk(b"IDAT", z))
        fp.write(chunk(b"IEND", b""))


def big_codec(name, fmt, w, h, ch, bits, png_pre=b"", wrap=None):
    """(png, encoded) in BIG; generated once and reused."""
    png_p, enc_p = os.path.join(BIG, name + ".png"), os.path.join(BIG, name + "." + fmt)
    if not (os.path.exists(png_p) and os.path.exists(enc_p)):
        raw = os.path.join(BIG, name + ".raw")
        t0 = time.perf_counter()
        big_pattern(raw, png_p, w, h, ch, bits, png_pre)
        args = [os.path.join(BIN, "gen_codec_fixture.exe"), fmt, raw, enc_p, str(w), str(h), str(ch)] + ([str(bits)] if fmt == "jxl" else [])
        g = subprocess.run(args, capture_output=True, text=True)
        os.remove(raw)
        if g.returncode != 0:
            raise RuntimeError("gen_codec_fixture failed: " + g.stderr)
        if wrap:
            with open(enc_p, "rb") as f:
                data = f.read()
            with open(enc_p, "wb") as f:
                f.write(wrap(data))
        print("generated", name, g.stdout.strip(), "%.1fs" % (time.perf_counter() - t0), flush=True)
    return png_p, enc_p


def big_metrics(j):
    return "job_peak_commit=%.1fMiB libjxl_alloc_peak=%.1fMiB codec_estimate=%.1fMiB need_peak=%.1fMiB temp=%.1fMiB" % (
        mem_of(j, "job_peak_commit") / 2**20, mem_of(j, "libjxl_alloc_peak") / 2**20, mem_of(j, "codec_estimate") / 2**20,
        mem_of(j, "codec_need_estimate_peak") / 2**20, mem_of(j, "temp_reserved_peak") / 2**20)


def limit_stopped(j, side="output"):
    return (j and j["strict"]["result"] == "not_evaluated" and j[side]["decode_status"] == "not_checked"
            and j[side]["decode_status_reason"] == "resource_limit" and "resource_limit" in codes_of(j))


if BIG and want("jxlbig"):
    os.makedirs(BIG, exist_ok=True)
    srgb = b"\x00\x00\x00\x01sRGB\x00\xae\xce\x1c\xe9"   # sRGB chunk, intent 0
    pj, jj = big_codec("s3_jxl_4096x4096_rgb16", "jxl", 4096, 4096, 3, 16, srgb)
    px = 4096 * 4096
    if want_case("jxlbig_ok"):
        p, j, problem, unchanged, wall = run_pair("jxlbig_4096_rgb16_default", pj, jj)
        ok = (p.returncode == 0 and not problem and unchanged and j["strict"]["result"] == "match"
              and j["samples"]["pixels_compared"] == str(px) and mem_of(j, "libjxl_alloc_peak") > 0
              and mem_of(j, "job_peak_commit") < (960 << 20))
        record("jxlbig", "rgb16_4096_within_default_cap", ok, "exit=%s wall=%.1fs %s %s" % (p.returncode, wall, big_metrics(j), problem),
               {"metrics": j and j["metrics"], "wall_s": round(wall, 1), "output_size": os.path.getsize(jj)})
    if want_case("jxlbig_estimate"):
        # Layer a (D10): the pre-decode estimate (~216 MiB) exceeds the hard cap 128 - 64 = 64 MiB.
        p, j, problem, unchanged, wall = run_pair("jxlbig_estimate_stops", pj, jj, ["--limits", "job_commit_mib=128"])
        ok = p.returncode == 4 and not problem and unchanged and limit_stopped(j) and mem_of(j, "libjxl_alloc_peak") < (1 << 20)
        record("jxlbig", "estimate_stops_before_decode", ok, "exit=%s wall=%.1fs %s %s" % (p.returncode, wall, big_metrics(j), problem),
               {"metrics": j and j["metrics"]})
    if want_case("jxlbig_allocator"):
        # Layer b (D10, D13): the estimate is scaled to 10 % so it passes; libjxl's allocator must refuse at 64 MiB.
        p, j, problem, unchanged, wall = run_pair("jxlbig_allocator_cap", pj, jj,
                                                  ["--limits", "job_commit_mib=128", "--test-codec-estimate-pct", "10"])
        msgs = [i.get("message", "") for i in (j or {}).get("issues", []) if i["code"] == "resource_limit"]
        ok = (p.returncode == 4 and not problem and unchanged and limit_stopped(j)
              and any("libjxl allocation refused" in m for m in msgs) and mem_of(j, "libjxl_alloc_refusals") > 0
              and mem_of(j, "libjxl_alloc_peak") <= (64 << 20) and "test_option_used" in codes_of(j)
              and j["settings"].get("test", {}).get("codec_estimate_pct") == 10)
        record("jxlbig", "allocator_refuses_at_hard_cap", ok, "exit=%s wall=%.1fs refusals=%s %s %s" % (
            p.returncode, wall, mem_of(j, "libjxl_alloc_refusals"), big_metrics(j), problem), {"metrics": j and j["metrics"]})

if BIG and want("webpbig"):
    os.makedirs(BIG, exist_ok=True)
    import struct as _st
    import zlib as _z
    icc = open(os.path.join(FIX, "s3_imbc_pq.icc"), "rb").read()
    zicc = b"imbc\x00\x00" + _z.compress(icc, 0)
    iccp = _st.pack(">I", len(zicc)) + b"iCCP" + zicc + _st.pack(">I", _z.crc32(b"iCCP" + zicc) & 0xFFFFFFFF)

    def add_iccp(data, w=8192, h=8192):
        n = _st.unpack("<I", data[16:20])[0]
        assert data[12:16] == b"VP8L"
        vl = data[20:20 + n]
        body = b"WEBP"
        for tag, pl in ((b"VP8X", bytes([0x30, 0, 0, 0]) + (w - 1).to_bytes(3, "little") + (h - 1).to_bytes(3, "little")),
                        (b"ICCP", icc), (b"VP8L", vl)):
            body += tag + _st.pack("<I", len(pl)) + pl + (b"\0" if len(pl) % 2 else b"")
        return b"RIFF" + _st.pack("<I", len(body)) + body

    pw, ww = big_codec("s3_webp_8192x8192_rgba8", "webp", 8192, 8192, 4, 8, iccp, add_iccp)
    if want_case("webpbig_ok"):
        p, j, problem, unchanged, wall = run_pair("webpbig_8192_rgba8_default", pw, ww)
        ok = (p.returncode == 0 and not problem and unchanged and j["strict"]["result"] == "match"
              and j["samples"]["pixels_compared"] == str(8192 * 8192) and mem_of(j, "job_peak_commit") < (960 << 20))
        record("webpbig", "rgba8_8192_within_default_cap", ok, "exit=%s wall=%.1fs %s %s" % (p.returncode, wall, big_metrics(j), problem),
               {"metrics": j and j["metrics"], "wall_s": round(wall, 1), "output_size": os.path.getsize(ww)})
    if want_case("webpbig_estimate"):
        # libwebp has no allocator hook: the estimate (~8 B/px = 512 MiB) against 256 - 64 = 192 MiB is the guard.
        p, j, problem, unchanged, wall = run_pair("webpbig_estimate_stops", pw, ww, ["--limits", "job_commit_mib=256"])
        ok = p.returncode == 4 and not problem and unchanged and limit_stopped(j) and mem_of(j, "job_peak_commit") < (256 << 20)
        record("webpbig", "estimate_stops_before_decode", ok, "exit=%s wall=%.1fs %s %s" % (p.returncode, wall, big_metrics(j), problem),
               {"metrics": j and j["metrics"]})
    if want_case("webpbig_job_limit"):
        # No allocator hook in libwebp: if the estimate is too low, only the Job limit remains.
        # The estimate is scaled to 50 % (~262 MiB) so it passes the 384 - 64 = 320 MiB hard cap;
        # the real need (~517 MiB) must then fail cleanly inside the 384 MiB Job, without a crash.
        p, j, problem, unchanged, wall = run_pair("webpbig_job_limit", pw, ww,
                                                  ["--limits", "job_commit_mib=384", "--test-codec-estimate-pct", "50"])
        msgs = [i.get("message", "") for i in (j or {}).get("issues", []) if i["code"] == "resource_limit"]
        ok = (p.returncode == 4 and not problem and unchanged and limit_stopped(j)
              and "worker_crashed" not in codes_of(j) and "test_option_used" in codes_of(j)
              and j["settings"].get("test", {}).get("codec_estimate_pct") == 50
              and mem_of(j, "job_peak_commit") <= (384 << 20))
        record("webpbig", "job_limit_stops_low_estimate", ok, "exit=%s wall=%.1fs %s msgs=%s %s" % (
            p.returncode, wall, big_metrics(j), msgs, problem), {"metrics": j and j["metrics"]})

if PRIVATE and want("private"):
    code_name = "big-png-261mp"
    before = sha(PRIVATE)
    st_before = os.stat(PRIVATE)
    pdir = os.path.join(OUT, "private")
    os.makedirs(pdir)
    md, js = os.path.join(pdir, code_name + "-inspect.md"), os.path.join(pdir, code_name + "-inspect.json")
    p, unchanged, wall = iat(["inspect", PRIVATE, "--report-md", md, "--report-json", js], [PRIVATE])
    j, problem = check_reports(code_name, p.returncode, md, js)
    o = j["original"] if j else {}
    record("private", code_name + " inspect", not problem and unchanged and o.get("decode_status") == "complete" and
           o.get("interlace") == 0 and mem_of(j, "job_peak_commit") < (64 << 20) and mem_of(j, "temp_reserved_peak") == 0,
           "exit=%s decode=%s %sx%s interlace=%s wall=%.1fs job_peak_commit=%.1fMiB row_budget=%sB" % (
               p.returncode, o.get("decode_status"), o.get("width"), o.get("height"), o.get("interlace"), wall,
               mem_of(j, "job_peak_commit") / 2**20, mem_of(j, "row_budget_estimate")),
           {"metrics": j and j["metrics"], "exit": p.returncode})
    md, js = os.path.join(pdir, code_name + "-self.md"), os.path.join(pdir, code_name + "-self.json")
    p, unchanged, wall = iat(["compare", PRIVATE, PRIVATE, "--report-md", md, "--report-json", js], [PRIVATE])
    j, problem = check_reports(code_name, p.returncode, md, js)
    record("private", code_name + " compare_self", not problem and unchanged and j["samples"]["status"] == "equal" and
           j["samples"]["pixels_compared"] == "261142483" and mem_of(j, "job_peak_commit") < (64 << 20) and
           p.returncode == 2 and j["strict"]["reasons"] == ["color_unknown"],
           "exit=%s strict=%s (%s) wall=%.1fs job_peak_commit=%.1fMiB" % (
               p.returncode, j and j["strict"]["result"], j and ",".join(j["strict"]["reasons"]), wall,
               mem_of(j, "job_peak_commit") / 2**20),
           {"metrics": j and j["metrics"], "exit": p.returncode})
    after = sha(PRIVATE)
    st_after = os.stat(PRIVATE)
    record("private", code_name + " unchanged", before == after and st_before.st_mtime_ns == st_after.st_mtime_ns and
           st_before.st_size == st_after.st_size, "sha256 %s before == after: %s" % (before[:16], before == after))

with open(os.path.join(OUT, "e2e-results.json"), "w", encoding="utf-8") as f:
    json.dump(results, f, ensure_ascii=False, indent=1)
fails = [r for r in results if not r["pass"]]
print("\n%d checks, %d failed" % (len(results), len(fails)))
sys.exit(1 if fails else 0)
