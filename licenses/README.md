# 第三方授權

本 repo 內含、或發布檔會帶到的第三方內容。crate 依賴的完整授權清單在階段 7 封包時另外產生；這裡先列出 repo 內直接收錄的第三方資料。

| 內容 | 位置 | 來源 | 授權 | 條文 |
|---|---|---|---|---|
| libjxl 0.12.0 從色彩列舉值產生的 ICC profile（289 筆） | `crates/iat-core/data/jxl_icc_table.bin`，編進 iat-core | 由 `tools/libjxl_gen/gen_jxl_icc_table.cpp` 連結 C++ 版 iat 1.0.0 建置裡的 libjxl 0.12.0（vcpkg，commit 7e3d314f5d）產生（D-R18） | BSD-3-Clause | `libjxl-BSD-3-Clause.txt` |
| 用 libjxl 0.12.0 編碼的 JPEG XL 測試圖 | `tests/fixtures/jxl_generated/` | 同上，`tools/libjxl_gen/gen_jxl_lossy.cpp`；只用於測試，不進發布檔 | BSD-3-Clause（編碼器） | `libjxl-BSD-3-Clause.txt` |
| jxl crate 0.8.0（jxl-rs）與其子 crate | Cargo 依賴，編進 iat-core | https://github.com/libjxl/jxl-rs | BSD-3-Clause | 同一份版權聲明（the JPEG XL Project Authors），見 `libjxl-BSD-3-Clause.txt` |

條文從 `D:\licenses\BSD-3-Clause.txt`（SPDX）複製，只把第一行的 `<year> <owner>` 換成 libjxl 與 jxl-rs 的 LICENSE 所寫的版權行。
