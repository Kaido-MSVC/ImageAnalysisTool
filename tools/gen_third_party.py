"""Generate the third-party inventory and notices from what was actually built.

Sources (nothing is downloaded):
  * <installed>/vcpkg/info/*.list          which port installed which .lib
  * <installed>/<triplet>/share/<port>/     copyright and vcpkg.spdx.json per port
  * <bin>/iat.map, <bin>/iat-worker.map     linker maps: which libraries contributed
                                            objects to the shipped executables
  * <downloads>/libjxl-*.tar.gz             libjxl PATENTS and AUTHORS (the archive's
                                            SHA-512 must equal the port's SPDX checksum)

A port counts as distributed only when one of its libraries contributed objects to a
shipped executable (or, for header-only ports, its symbols appear in a map). Libraries
on the link line that contributed nothing are listed as "linked, not used".

Outputs (deterministic, no timestamps):
  THIRD_PARTY_NOTICES.txt, licenses/<port>/..., docs/third-party.md

Usage:
  py -3.13 -I tools/gen_third_party.py <repo-root> --installed <vcpkg-installed-dir>
      --triplet x64-windows-static-iat --bin build/release/bin
      --downloads <vcpkg-downloads-dir> [--spdx-templates <dir>] [--check]
--spdx-templates defaults to the IAT_SPDX_TEMPLATES environment variable; one of the two
is required.
With --check nothing is written; exit code 1 if any output would change.
"""

import glob
import hashlib
import json
import os
import re
import sys
import tarfile

SHIPPED = ("iat", "iat-worker")
# Libraries the linker map names that are not vcpkg ports.
OWN_LIBS = {"iat-core", "iat_engine"}
CRT_LIBS = {"libucrt", "libvcruntime", "libcpmt", "libcmt"}
SYSTEM_LIBS = {"kernel32", "bcrypt", "psapi", "advapi32", "user32", "shell32", "ole32", "oleaut32",
               "uuid", "gdi32", "winspool", "comdlg32", "shlwapi", "windowscodecs"}
# Header-only ports: detected by a symbol substring in the maps.
HEADER_ONLY = {"nlohmann-json": "nlohmann", "doctest": "doctest"}
TEST_ONLY = {"doctest"}
# Ports that only run during the vcpkg build.
BUILD_HELPERS = {"vcpkg-cmake", "vcpkg-cmake-config", "vcpkg-cmake-get-vars", "vcpkg-tool-meson"}
LICENSE_NOTE = {
    "highway": "Apache-2.0 OR BSD-3-Clause（雙授權；hwy/contrib/random 為 CC0-1.0，未連結）。本工具依 BSD-3-Clause 散布",
    "libavif": "BSD-2-Clause（port 宣告 BSD-2-Clause AND Apache-2.0；Apache-2.0 只涵蓋未連結的 android_jni/gradlew*）",
    "libyuv": "BSD-3-Clause（port 未宣告 license，依上游 LICENSE）；附 PATENTS",
    "aom": "BSD-2-Clause；AOM Patent License 1.0（已含在 LICENSE.txt）",
}
# SPDX template (<--spdx-templates>/<id>.txt, default $IAT_SPDX_TEMPLATES) whose terms each
# distributed port's license text must contain word for word.
SPDX_TEMPLATE = {"brotli": "MIT", "highway": "BSD-3-Clause", "libjxl": "BSD-3-Clause", "libwebp": "BSD-3-Clause",
                 "nlohmann-json": "MIT", "zlib": "Zlib", "libavif": "BSD-2-Clause", "aom": "BSD-2-Clause",
                 "libyuv": "BSD-3-Clause"}
# Extra upstream files to ship next to the port's copyright file.
EXTRA_FROM_ARCHIVE = {"libjxl": ["PATENTS", "AUTHORS"], "libyuv": ["PATENTS"]}


def arg(name, default=None):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def read_map(path):
    """Return {lib_name_lower: {object names}} and the raw text of a linker map."""
    text = open(path, encoding="latin-1").read()
    libs = {}
    started = False
    for line in text.splitlines():
        if "Publics by Value" in line:
            started = True
            continue
        if not started:
            continue
        parts = line.split()
        if len(parts) >= 4 and ":" in parts[-1] and re.match(r"^[0-9a-f]{4}:", parts[0]):
            lib, obj = parts[-1].split(":", 1)
            libs.setdefault(lib.lower(), set()).add(obj)
    return libs, text


COMMENT = re.compile(r"/\*.*?\*/|(?:^[ \t]*//[^\n]*\n)+", re.S | re.M)
# "Copyright (c) ...", "Copyright © ..." or "Copyright 2013 ..."; not "copyright string/notice/holders".
HOLDER = re.compile(r"copyright\s*(?:(?:\(c\)|©)\s*[0-9 ,\-–]*|([0-9]{4}[0-9 ,\-–]*))\s*(?:by\s+)?(.+)", re.I)


def norm(s):
    return re.sub(r"[^a-z0-9]+", " ", s.lower()).strip()


def read_archive(archive, wanted):
    """{path relative to the source root: bytes} for regular files for which wanted(rel_path) is
    true, read in archive order (a gzip stream cannot seek backwards cheaply). GitHub tarballs have
    one top-level directory; archives that vcpkg_from_git exports (aom, libyuv) have none."""
    with tarfile.open(archive) as t:
        members = [m for m in t.getmembers() if m.isfile()]
        strip = len({m.name.split("/", 1)[0] for m in members}) == 1 and all("/" in m.name for m in members)
        out = {}
        for m in members:
            rel = m.name.split("/", 1)[1] if strip else m.name
            if wanted(rel):
                out[rel] = t.extractfile(m).read()
    return out


def template_terms_present(template_text, license_text):
    """True when the SPDX template's terms (from 'Redistribution' / 'Permission' / 'This software'
    on, placeholder lines dropped) appear word for word in the port's license text. As in the SPDX
    matching guidelines, punctuation, line breaks and list markers ("1.", "*") are ignored, and the
    name in BSD-3's "Neither the name of <the copyright holder> nor" clause is replaceable text."""
    def toks(s):
        return [t for t in re.findall(r"[a-z0-9]+", s.lower()) if not t.isdigit()]
    body = " ".join(l for l in template_text.splitlines() if "<" not in l)
    starts = [i for i in (body.find("Redistribution"), body.find("Permission"), body.find("This software")) if i >= 0]
    want = " ".join(toks(body[min(starts) if starts else 0:]))
    pattern = re.escape(want).replace(re.escape("neither the name of the copyright holder nor"),
                                      r"neither the name of [a-z0-9 ]{1,200}? nor")
    return re.search(pattern, " ".join(toks(license_text))) is not None


def extra_notices(archive, objects, main_text):
    """Comment blocks with a copyright holder that the port's main license does not name,
    taken from the archive's source files whose basename matches a linked object."""
    want = {o[:-4] for o in objects if o.endswith(".obj")}
    main = norm(main_text)
    found = []
    sources = read_archive(archive, lambda rel: os.path.basename(rel) in want)
    for name in sorted(sources):
        src = sources[name].decode("utf-8", "replace")
        for block in COMMENT.findall(src):
            holders = []
            for line in block.splitlines():
                h = HOLDER.search(line.lstrip(" /*"))
                if h and line.lstrip(" /*").lower().startswith("copyright"):
                    holder = norm(h.group(2))[:40]
                    if holder and holder not in main:
                        holders.append(h.group(2).strip())
            if holders:
                found.append((name, block.replace("\r\n", "\n")))
    return found


def port_libs(installed, triplet):
    """{port: (version_dir, [lib basenames lower])} from vcpkg/info/*.list."""
    out = {}
    for lst in glob.glob(os.path.join(installed, "vcpkg", "info", "*_%s.list" % triplet)):
        port = os.path.basename(lst)[: -len("_%s.list" % triplet)].rsplit("_", 1)[0]
        libs = []
        for line in open(lst, encoding="utf-8"):
            line = line.strip()
            if line.startswith(triplet + "/lib/") and line.endswith(".lib"):
                libs.append(os.path.basename(line)[:-4].lower())
        out[port] = libs
    return out


def spdx(share):
    d = json.load(open(os.path.join(share, "vcpkg.spdx.json"), encoding="utf-8"))
    pk = d["packages"]
    port = pk[0]
    upstream = next((p for p in pk[1:] if p.get("downloadLocation", "NONE") not in ("NONE", "NOASSERTION")
                     and "microsoft/vcpkg" not in p["downloadLocation"]), None)
    sha = ""
    if upstream:
        for c in upstream.get("checksums", []):
            if c.get("algorithm") == "SHA512":
                sha = c["checksumValue"]
    return {"version": port.get("versionInfo", ""), "license": port.get("licenseConcluded", "NOASSERTION"),
            "port_tree": port.get("downloadLocation", ""),
            "source": upstream["downloadLocation"] if upstream else "", "sha512": sha}


def msvc_toolset(bindir):
    """MSVC toolset (major.minor, e.g. 14.52) of this build, from <bin>/../CMakeCache.txt."""
    cache = os.path.join(os.path.dirname(os.path.abspath(bindir)), "CMakeCache.txt")
    with open(cache, encoding="utf-8", errors="replace") as f:
        m = re.search(r"^CMAKE_CXX_COMPILER:\w+=.*/MSVC/(\d+\.\d+)\.\d+/", f.read(), re.M)
    if not m:
        sys.exit("cannot determine the MSVC toolset from " + cache)
    return m.group(1)


def main():
    root = sys.argv[1]
    installed, triplet = arg("--installed"), arg("--triplet", "x64-windows-static-iat")
    bindir, downloads, check = arg("--bin"), arg("--downloads"), "--check" in sys.argv
    if not (installed and bindir and downloads):
        sys.exit(__doc__)
    share = os.path.join(installed, triplet, "share")

    maps = {}
    alltext = ""
    for exe in SHIPPED:
        libs, text = read_map(os.path.join(bindir, exe + ".map"))
        maps[exe] = libs
        alltext += text

    archives = {}
    for a in glob.glob(os.path.join(downloads, "*.tar.gz")):
        archives[hashlib.sha512(open(a, "rb").read()).hexdigest()] = a
    templates = arg("--spdx-templates", os.environ.get("IAT_SPDX_TEMPLATES"))
    if not templates or not os.path.isdir(templates):
        sys.exit("SPDX template folder not found: pass --spdx-templates <dir> or set IAT_SPDX_TEMPLATES")

    p2l = port_libs(installed, triplet)
    lib2port = {lib: port for port, libs in p2l.items() for lib in libs}

    # Every library a shipped executable used must be classified.
    used_by = {}
    for exe, libs in maps.items():
        for lib in libs:
            if lib in OWN_LIBS or lib in CRT_LIBS or lib in SYSTEM_LIBS:
                continue
            port = lib2port.get(lib)
            if not port:
                sys.exit("unclassified library in %s.map: %s" % (exe, lib))
            used_by.setdefault(port, {}).setdefault(lib, set()).add(exe)
    objects_of = {}
    for libs in maps.values():
        for lib, objs in libs.items():
            if lib in lib2port:
                objects_of.setdefault(lib2port[lib], set()).update(objs)

    rows, files = [], {}
    for port in sorted(p2l):
        if port in BUILD_HELPERS:
            continue
        sp = spdx(os.path.join(share, port))
        libs = p2l[port]
        if port in HEADER_ONLY:
            exes = sorted(e for e in SHIPPED if HEADER_ONLY[port] in open(os.path.join(bindir, e + ".map"), encoding="latin-1").read())
            used = {"(header-only)": set(exes)} if exes else {}
        else:
            used = used_by.get(port, {})
        if port in TEST_ONLY and used:
            sys.exit("test-only port %s appears in a shipped executable" % port)
        unused = sorted(l for l in libs if l not in used)
        distributed = bool(used)
        if port in TEST_ONLY:
            use, how = "僅測試（iat_unit.exe）", "靜態，只連進測試執行檔"
        elif port in HEADER_ONLY and distributed:
            use, how = "執行檔", "header-only，編進 " + "、".join(e + ".exe" for e in sorted(used["(header-only)"]))
        elif distributed:
            parts = ["%s → %s" % (l, "、".join(sorted(e + ".exe" for e in es))) for l, es in sorted(used.items())]
            use, how = "執行檔", "靜態連結：" + "；".join(parts)
        else:
            use, how = "未使用", "連結命令列上有，但 map 中沒有任何物件"
        if unused and distributed and port not in HEADER_ONLY:
            how += "（未使用：%s）" % "、".join(unused)
        attach = []
        if distributed:
            attach.append("licenses/%s/LICENSE.txt" % port)
            main_text = open(os.path.join(share, port, "copyright"), "rb").read()
            files["licenses/%s/LICENSE.txt" % port] = main_text
            archive = archives.get(sp["sha512"]) if sp["sha512"] else None
            git = re.search(r"@([0-9a-f]{40})$", sp["source"])
            if not archive and not sp["sha512"] and git:
                # vcpkg_from_git ports (aom, libyuv): no upstream archive hash in the SPDX file. The
                # portfile pins an immutable commit and vcpkg keeps the exported tree as
                # <name>-<commit>.tar.gz; that archive's SHA-512 is recorded instead.
                hits = glob.glob(os.path.join(downloads, "*-%s.tar.gz" % git.group(1)))
                if len(hits) == 1:
                    archive = hits[0]
                    sp["sha512"] = hashlib.sha512(open(archive, "rb").read()).hexdigest()
                    sp["pin"] = "git commit " + git.group(1)
            if port in EXTRA_FROM_ARCHIVE or port in objects_of:
                if not archive:
                    sys.exit("%s: no source archive in %s matches the port's SPDX SHA-512" % (port, downloads))
            if port in EXTRA_FROM_ARCHIVE:
                top = read_archive(archive, lambda rel: rel in EXTRA_FROM_ARCHIVE[port])
                for n in EXTRA_FROM_ARCHIVE[port]:
                    if n not in top:
                        sys.exit("%s not found at the top of %s" % (n, archive))
                    files["licenses/%s/%s.txt" % (port, n)] = top[n]
                    attach.append("licenses/%s/%s.txt" % (port, n))
            if port in objects_of:
                notices = extra_notices(archive, objects_of[port], main_text.decode("utf-8", "replace"))
                if notices:
                    body = ["Per-file notices in %s %s source files that are linked into the executables" % (port, sp["version"]),
                            "and name copyright holders not listed in LICENSE.txt. Copied verbatim from the",
                            "source archive (SHA-512 %s)." % sp["sha512"], ""]
                    for name, block in notices:
                        body += ["--- %s ---" % name, block, ""]
                    files["licenses/%s/NOTICES-files.txt" % port] = ("\r\n".join("\n".join(body).split("\n")) + "\r\n").encode("utf-8")
                    attach.append("licenses/%s/NOTICES-files.txt" % port)
            if port in ("libwebp", "aom"):
                attach.append("（PATENTS 已含在 LICENSE.txt）")
            tid = SPDX_TEMPLATE.get(port)
            tpath = os.path.join(templates, tid + ".txt") if tid else ""
            if tid:
                if not os.path.isfile(tpath):
                    sys.exit("%s: SPDX template %s not found" % (port, tpath))
                if not template_terms_present(open(tpath, encoding="utf-8").read(), main_text.decode("utf-8", "replace")):
                    sys.exit("%s: license text does not contain the terms of %s" % (port, tpath))
                sp["template"] = tid
        rows.append({"port": port, "version": sp["version"], "source": sp["source"], "sha512": sp["sha512"],
                     "pin": sp.get("pin", ""), "template": sp.get("template", ""),
                     "port_tree": sp["port_tree"], "license": LICENSE_NOTE.get(port, sp["license"]),
                     "use": use, "how": how, "distributed": distributed, "attach": attach})

    # ---- THIRD_PARTY_NOTICES.txt
    nl = "\r\n"
    out = ["Image Analysis Tool - third-party notices", "",
           "The executables iat.exe and iat-worker.exe statically link the components below.",
           "Each component's full license text is reproduced here and in licenses/<component>/.",
           "The Microsoft C/C++ runtime is statically linked under the Visual Studio license terms.", ""]
    for r in rows:
        if not r["distributed"]:
            continue
        out += ["=" * 78, "%s %s" % (r["port"], r["version"]), "Source: %s" % r["source"],
                "License: %s" % r["license"].split("（")[0], "=" * 78, ""]
        for path in r["attach"]:
            if path.startswith("licenses/"):
                out.append("--- %s ---" % path)
                out.append(files[path].decode("utf-8").replace("\r\n", "\n").rstrip("\n").replace("\n", nl))
                out.append("")
    files["THIRD_PARTY_NOTICES.txt"] = (nl.join(out) + nl).encode("utf-8")

    # ---- docs/third-party.md
    md = ["# 第三方元件清單", "",
          "由 `tools/gen_third_party.py` 依 vcpkg 安裝目錄（`share/<port>/copyright`、`vcpkg.spdx.json`、`vcpkg/info/*.list`）"
          "與 Release 連結的 linker map（`build/release/bin/iat.map`、`iat-worker.map`）產生，請勿手改；重跑產生器更新。",
          "「散布」以 map 為準：該 port 的 lib 至少有一個物件連進 `iat.exe` 或 `iat-worker.exe`，header-only 則看符號。", "",
          "| 元件 | 版本 | 授權 | 用途 | 連結方式 | 散布 | 隨附材料 |", "|---|---|---|---|---|---|---|"]
    for r in rows:
        md.append("| %s | %s | %s | %s | %s | %s | %s |" % (
            r["port"], r["version"], r["license"], r["use"], r["how"], "是" if r["distributed"] else "否",
            "、".join("`%s`" % a if a.startswith("licenses/") else a for a in r["attach"]) or "—"))
    md += ["| MSVC 執行階段（UCRT、vcruntime、C++ 標準程式庫） | MSVC %s | Visual Studio 授權條款 | 執行檔 | 靜態 CRT（`/MT`；map：%s） | 是（編入執行檔） | — |"
           % (msvc_toolset(bindir), "、".join(sorted({l for libs in maps.values() for l in libs if l in CRT_LIBS}))),
           "| Windows 系統程式庫 | — | 作業系統元件 | 執行檔 | 匯入 DLL（%s） | 否 | — |"
           % "、".join(sorted({l for libs in maps.values() for l in libs if l in SYSTEM_LIBS})),
           "| 測試圖、IMB-C 樣本 | — | — | 僅測試 | — | 否 | — |", "",
           "## 來源與雜湊", "",
           "| 元件 | 上游來源 | 來源封存檔 SHA-512 | vcpkg port |", "|---|---|---|---|"]
    for r in rows:
        md.append("| %s | %s | `%s`%s | %s |" % (r["port"], r["source"], r["sha512"],
                                              "（vcpkg 依 %s 匯出的封存檔）" % r["pin"] if r["pin"] else "", r["port_tree"]))
    md += ["", "僅建置期使用、不散布的 vcpkg 輔助埠：%s。" % "、".join(sorted(p for p in p2l if p in BUILD_HELPERS)), "",
           "## 個別檔案聲明（`NOTICES-files.txt`）", "",
           "對每個散布的 port，以 SPDX 的 SHA-512 找到鎖版原始碼封存檔，取 map 中實際連進的物件所對應的原始檔"
           "（依檔名比對，同名檔一併納入），擷取含版權宣告（`Copyright (c)`／`©`／`Copyright <年份>`）"
           "且版權人未出現在該 port `LICENSE.txt` 的註解區塊，原文照錄。範圍是編譯單元；"
           "header 經由 inline／template 編入的部分不另掃描（libjxl、highway 的 header 與其編譯單元同屬一個專案與授權）。"
           "highway 的 `hwy/contrib/random`（CC0-1.0）未被 libjxl 0.12.0 或本工具引用。"
           "aom、libyuv 由 vcpkg_from_git 取得，SPDX 沒有上游封存檔的 SHA-512，改以 portfile 鎖定的 commit 找到 vcpkg 匯出的封存檔。", "",
           "## 通用條文核對（SPDX 範本）", "",
           "散布元件的授權條文一律取自 vcpkg 安裝的上游 `copyright`（含實際版權行）。產生器另以 SPDX 授權範本庫（`--spdx-templates`）核對："
           "範本中占位符以外的條款文字必須逐字出現在上游條文中，否則中止。比照 SPDX 比對準則，忽略標點、換行與條列記號，BSD-3 第三條「Neither the name of …」的權利人名稱視為可替換文字。libpng、libtiff 的授權不在 SPDX 範本庫中，只照錄上游原文。", "",
           "| 元件 | SPDX 範本 | 結果 |", "|---|---|---|"]
    for r in rows:
        if r["distributed"] and r["template"]:
            md.append("| %s | `%s.txt` | 條款一致 |" % (r["port"], r["template"]))
    md += [""]
    files["docs/third-party.md"] = ("\n".join(md)).encode("utf-8")

    changed = []
    for rel, data in sorted(files.items()):
        path = os.path.join(root, *rel.split("/"))
        old = open(path, "rb").read() if os.path.exists(path) else None
        # git core.autocrlf may rewrite line endings on checkout; compare content, not EOLs.
        if old is None or old.replace(b"\r\n", b"\n") != data.replace(b"\r\n", b"\n"):
            changed.append(rel)
            if not check:
                os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
                with open(path, "wb") as f:
                    f.write(data)
    # Files under licenses/ that this run did not produce are stale (e.g. a port no longer linked).
    lic_root = os.path.join(root, "licenses")
    for dirpath, _, names in os.walk(lic_root):
        for n in names:
            rel = os.path.relpath(os.path.join(dirpath, n), root).replace(os.sep, "/")
            if rel not in files:
                changed.append(rel + " (stale)")
                if not check:
                    os.remove(os.path.join(dirpath, n))
    if check:
        print("up to date" if not changed else "out of date: " + ", ".join(changed))
        sys.exit(1 if changed else 0)
    print("written: " + (", ".join(changed) if changed else "(no changes)"))


if __name__ == "__main__":
    main()
