# 第三方授權

本 repo 內含、或發布檔會帶到的第三方內容。編進執行檔的 crate 與其授權條文由 cargo-about 產生在根目錄的 `THIRD_PARTY_NOTICES.txt`（`scripts/gen_third_party.ps1`）；這裡放該清單無法自動帶入的條文與 repo 內直接收錄的第三方資料。

| 內容 | 位置 | 來源 | 授權 | 條文 |
|---|---|---|---|---|
| libjxl 0.12.0 從色彩列舉值產生的 ICC profile（289 筆） | `crates/iat-core/data/jxl_icc_table.bin`，編進 iat-core | 由 `tools/libjxl_gen/gen_jxl_icc_table.cpp` 連結 C++ 版 iat 1.0.0 建置裡的 libjxl 0.12.0（vcpkg，commit 7e3d314f5d）產生（D-R18） | BSD-3-Clause | `libjxl-BSD-3-Clause.txt` |
| 用 libjxl 0.12.0 編碼的 JPEG XL 測試圖 | `tests/fixtures/jxl_generated/` | 同上，`tools/libjxl_gen/gen_jxl_lossy.cpp`；只用於測試，不進發布檔 | BSD-3-Clause（編碼器） | `libjxl-BSD-3-Clause.txt` |
| jxl crate 0.8.0（jxl-rs）與其子 crate | Cargo 依賴，編進 iat-core | https://github.com/libjxl/jxl-rs | BSD-3-Clause | 同一份版權聲明（the JPEG XL Project Authors），見 `libjxl-BSD-3-Clause.txt` |

| Slint 1.19.0（slint、slint-macros 與 7 個 i-slint-* crate） | Cargo 依賴，編進 iat-gui.exe | https://github.com/slint-ui/slint | LicenseRef-Slint-Royalty-free-2.0（免版稅授權，D-R14） | `LicenseRef-Slint-Royalty-free-2.0.md` |

條文來源：
- `libjxl-BSD-3-Clause.txt`：從 `D:\licenses\BSD-3-Clause.txt`（SPDX）複製，只把第一行的 `<year> <owner>` 換成 libjxl 與 jxl-rs 的 LICENSE 所寫的版權行。
- `LicenseRef-Slint-Royalty-free-2.0.md`：不是 SPDX 授權，原樣複製自 slint 1.19.0 crate 的 `LICENSES/LicenseRef-Slint-Royalty-free-2.0.md`（SHA-256 `5167f5056e850419106ab6265efbdca7cba4d99c849d1445ca0bbf6a1e2315fe`，9 個 crate 的檔案相同）。cargo-about 0.9.3 取不到 LicenseRef 的條文，所以放在這裡，並由 `about.hbs` 固定列出這些 crate（見 `about.toml` 的說明）。
