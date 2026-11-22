# libjxl 輔助程式（階段 5，D-R18）

連結 C++ 版 iat 1.0.0 建置裡的 libjxl 0.12.0 靜態庫（唯讀，不修改 C++ repo），產生 Rust 版需要、但不能自己算到逐位元組相同的資料。

| 程式 | 輸出 |
|---|---|
| `gen_jxl_icc_table.cpp` | `crates/iat-core/data/jxl_icc_table.bin`：libjxl 從色彩列舉值產生的 ICC（289 筆），格式見 `crates/iat-core/src/jxl_icc.rs` |
| `gen_jxl_lossy.cpp` | `tests/fixtures/jxl_generated/v_jxl_rgb16_lossy_vardct_noxyb.jxl`：非 XYB 的有損測試圖 |

重新產生：`pwsh -File tools/libjxl_gen/build.ps1`（需要 VS 2026 的 MSVC 14.52 工具集與 C++ 建置目錄；可用 `-CppBuild` 指定位置）。執行檔與中間檔放在暫存目錄。重產後要更新 `jxl_icc.rs` 的 `TABLE_SHA256`、`docs/decisions.md` D-R18 與 `tests/fixtures/SHA256SUMS`。

libjxl 是 BSD-3-Clause，條文在 `licenses/libjxl-BSD-3-Clause.txt`。
