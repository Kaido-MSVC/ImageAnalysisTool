"""檢查翻譯檔是否涵蓋 ui/*.slint 裡所有的 @tr() 字串（只用標準函式庫）。

用法：py -3.13 -I crates/iat-gui/lang/check_po.py
缺少或多餘的條目會列出來，並以退出碼 1 結束。沒有 msgctxt（build.rs 關閉了預設的翻譯 context）。
"""

import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
UI = HERE.parent / "ui"
TR = re.compile(r'@tr\("((?:[^"\\]|\\.)*)"')
MSGID = re.compile(r'^msgid "((?:[^"\\]|\\.)*)"$')
MSGSTR = re.compile(r'^msgstr "((?:[^"\\]|\\.)*)"$')


def source_strings():
    found = []
    for f in sorted(UI.glob("*.slint")):
        for m in TR.finditer(f.read_text(encoding="utf-8")):
            if m.group(1) not in found:
                found.append(m.group(1))
    return found


def po_entries(path):
    entries = {}
    msgid = None
    for line in path.read_text(encoding="utf-8").splitlines():
        m = MSGID.match(line)
        if m:
            msgid = m.group(1)
            continue
        m = MSGSTR.match(line)
        if m and msgid is not None:
            if msgid:
                entries[msgid] = m.group(1)
            msgid = None
    return entries


def placeholders(s):
    return s.count("{}")


def main():
    wanted = source_strings()
    ok = True
    for po in sorted(HERE.glob("*/LC_MESSAGES/iat-gui.po")):
        lang = po.parent.parent.name
        have = po_entries(po)
        for s in wanted:
            if not have.get(s):
                print(f"{lang}: missing: {s}")
                ok = False
            elif placeholders(have[s]) != placeholders(s):
                print(f"{lang}: placeholder count differs: {s}")
                ok = False
        for s in have:
            if s not in wanted:
                print(f"{lang}: stale: {s}")
                ok = False
    print(f"{len(wanted)} strings checked" if ok else "translation check FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
