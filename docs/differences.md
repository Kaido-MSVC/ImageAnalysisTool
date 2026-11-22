# 和 C++ 版 iat 1.0.0 的差異總表

Rust 版 iat 2.0.0 和 C++ 版 iat 1.0.0 的所有差異都列在這裡，作為 2.0.0 發布說明的依據，也作為日後決定 C++ 版是否封存的參考。每一項的完整說明在 `docs/decisions.md` 的 D 編號裡。

- **基準**：C++ 版 1.0.0 發布檔（`iat.exe` SHA-256 `0fbfffb3…`，見 decisions.md「驗證基準」）。
- **比對方式**：`tests/compare/compare_iat.py` 用同一組案例分別執行兩個版本，比對退出碼、stderr、JSON（含鍵順序與縮排）與 Markdown。比對前去掉每次都會變的欄位（D-R08）。表格以外的差異一律算失敗。
- **「影響判定」**：是否可能改變 `strict.result`、`strict.reasons`、`decode_status`、問題代碼或退出碼。「否」表示只有文字或數值資訊不同；「可能」表示特定輸入下會不同，但目前的測試圖與私人樣本都沒有觸發。

## 1. 比對腳本裡允許的差異

比對腳本實際遇到、經過確認後允許的差異（decisions.md「允許的差異」表）。

| 項目 | D 編號 | 影響範圍 | 影響判定 |
|---|---|---|---|
| 版本字串：`iat --version` 為 `iat 2.0.0`，報告 `tool.version` 為 `2.0.0` | D-R10 | `--version` 的輸出、`tool` | 否。IMB-C 只要求 ≥ 0.5.1 |
| `tool.compiler` 記 rustc 版本；`codecs` 列 Rust crate 名稱與版本 | D-R06 | `tool`、`codecs` | 否 |
| `metrics` 的鍵與數值（例如沒有 `libpng_alloc_peak`、`worker_peak_*`） | D-R02、D-R08 | `metrics`、Markdown「效能與資源」 | 否（`metrics` 本來就只供參考） |
| `decode_path` 寫 Rust 解碼器的名稱（例如 `png-rows-no-transform`、`jxl-rs+temp`、`rav1d-planes-identity+temp`） | D-R11、D-R17、D-R18、D-R19 | `original/output.decode_path` | 否。IMB-C 不讀這個欄位（D-R10 補查） |
| 解碼失敗訊息的字首：`png decoder`、`tiff decoder`、`jxl-rs`、`image-webp` 對 `libpng`、`libtiff`、`libjxl`、`libwebp`，其餘文字相同（AVIF 的 `rav1d: …` 對 `libavif: …` 同理，目前沒有案例觸發） | D-R11、D-R15、D-R17、D-R18、D-R19 | `issues[].message`、`decoder_errors` | 否 |
| PNG 濾波位元組錯誤的訊息：`Unknown filter method 40.` 對 `bad adaptive filter value` | D-R11 | `issues[].message`、`decoder_errors` | 否（失敗位置相同） |
| libpng 警告改由相容檢查產生：字首 `libpng-compatible check:`，而且不寫進 `decoder_warnings` | D-R12 | `issues[].message`、`decoder_warnings` | 否（問題代碼相同） |
| zlib 的資料錯誤文字：miniz_oxide 不區分種類（`invalid distance too far back` 回報為 `incorrect data check`） | D-R17 | TIFF 的 `issues[].message`、`decoder_errors` | 否 |
| 多圖層 JPEG XL 靜止影像不產生 `layered_image`（info）：jxl-rs 0.8.0 沒有實作 `coalescing = false` | D-R18 | `issues[]` 少一筆 info | 否（info 不影響判定）；上游支援後補做 |
| 非 XYB 的有損 JPEG XL：jxl-rs 與 libjxl 的樣本有少量最低位元差異，`mae`、`rmse` 與列出的差異樣本值不同 | D-R18 | `samples.per_channel[].mae/rmse`、`samples.first_differences[].output` | 否（不同的像素數、最大誤差、判定相同）。IMB-C 的 JXL 一律無損 |
| 記憶體用量不同：同一個 `job_commit_mib` 下可能一邊完成、一邊 `resource_limit`。C++ `run_e2e.py` 的 `jxlbig_allocator`（128 MiB、預估 10%）與 `webpbig_job_limit`（384 MiB、預估 50%）在 Rust 版解碼完成 | D-R02 第四次修訂 | 接近上限的大圖：退出碼 4 對 0 | **是**，但只在上限設得接近實際用量時；事前預估擋下的情況（大多數，含 `resource_need` 與建議值）完全相同 |

## 2. 已知差異（目前的測試圖與私人樣本都沒有觸發）

| 項目 | D 編號 | 影響範圍 | 影響判定 |
|---|---|---|---|
| libpng 的 cHRM、gAMA、sRGB、sBIT、cICP、mDCV、cLLI、bKGD、hIST、pHYs、tIME、sPLT、oFFs、pCAL、sCAL 內容檢查沒有重做（長度與位置規則有做）；iCCP 是否為已知 sRGB profile 的警告沒有重做 | D-R12 | 遇到這些內容錯誤時少一則 `decoder_warning` | 可能（問題代碼不同） |
| libtiff 讀目錄時的部分警告沒有重做（tag 的 count／型別錯誤而略過、超過檔案大小的配置請求） | D-R17 | 少一則 `decoder_warning` | 可能 |
| 大於 2 GB 的未壓縮 BigTIFF strip 沒有虛擬切分 | D-R17 | 失敗時的 `rows_decoded`、列緩衝預算 | 可能（只在失敗時） |
| JPEG XL 自訂白點、原色或 gamma 的色彩列舉值查不到 libjxl 產生的 ICC：記為 `unreadable`（C++ 版為 `synthesized`） | D-R18 | `icc`、`icc.status` | 可能（ICC 比較結果不同；色彩判定依 key 仍相同） |
| JPEG XL 轉換函數為 unknown 的色彩列舉值：jxl-rs 在標頭就回報錯誤，libjxl 照樣解碼 | D-R18 | `decode_status` | 可能 |
| JPEG XL 自訂色度值寫進 key 時 jxl-rs 給 f32、C++ 版為 double，極少數值的第 6 位小數不同 | D-R18 | `color.key` | 可能（兩側 key 不同時色彩判定不同） |
| AVIF 主要 brand 為 `avis` 的影像序列不解析，直接回報 `animation_not_supported`；moov 損壞時 C++ 版為 `structure_error` | D-R19 | `avif.*`、寬高、問題代碼 | 可能 |
| AVIF gain map（`tmap`）、sample transform（`sato`）的 libavif 解析檢查沒有重做；`a1op`／`lsel` 只做解析檢查，解碼一律用運算點 0、所有圖層 | D-R19 | 問題代碼、樣本 | 可能 |
| AVIF 解碼後的位元深度或 layout 和 av1C 不同：判為 `decoder_disagrees_with_parse`（C++ 版用解碼後的值） | D-R19 | `decode_status` | 可能 |
| AVIF `ispe` 與 AV1 畫格尺寸不同：判為 `decoder_disagrees_with_parse`（libavif 會用 libyuv 縮放後比較） | D-R19（使用者決定） | `decode_status` | 可能 |
| AVIF grid 的 tile 之間 CICP 不同：rav1d 沒有交出每個 tile 的 CICP，這項檢查沒做 | D-R19 | 問題代碼 | 可能 |
| rav1d 的記憶體錯誤（不是 panic）讓 worker 中止：判為 `worker_crashed`（C++ 版是 `decode_error`）；rav1d 的 panic 則和 C++ 版一樣判為解碼錯誤 | D-R02、D-R19 | `decode_status`、退出碼 3 對 5 | 可能 |
| Job 上限的配置失敗：C++ 版的 libpng、libjxl 被自己的配置器以 `job_commit_mib` − 64 MiB 拒絕，Rust 版是 worker 碰到 `job_commit_mib` 後告訴父程序是哪一側、重跑一次。報告形式相同，但 PNG 在解碼中途失敗時 `rows_decoded` 一律為 0；WebP 的「libwebp ran out of memory」形式實際上不會出現（image-webp 不另外配置大緩衝） | D-R02 第四次修訂 | `rows_decoded`、`issues[].message` | 否（`decode_status`、問題代碼、退出碼相同） |
| 解碼器呼叫以外（掃描、比較）的配置失敗：兩版都是執行層級的 `resource_limit`，但失敗點依各自的記憶體用量而定 | D-R02 | 哪一個輸入會碰到上限 | 可能（同第 1 節最後一項） |

## 3. 刻意的差異（Rust 版修正了 C++ 版的錯誤處理）

C++ 版在這些情況下吞掉錯誤；Rust 版回報正確的原因。正常情況下輸出相同，只在環境出錯時不同。

| 情況 | C++ 1.0.0 | Rust 2.0.0 | D 編號 | 影響判定 |
|---|---|---|---|---|
| 讀檔頭失敗 | 格式變成 unknown，退出碼 2 | `input_open_failed`，退出碼 5 | D-R13 | 是（只在 I/O 錯誤時） |
| 取檔案大小失敗 | 大小當成 0 繼續 | `input_open_failed`，退出碼 5 | D-R13 | 是（只在 I/O 錯誤時） |
| 雜湊時讀取失敗 | 雜湊留空照樣往下跑 | `input_open_failed`，退出碼 5 | D-R13 | 是（只在 I/O 錯誤時） |
| stdout 寫入失敗（例如磁碟滿） | 不理會，退出碼不變 | stderr 說明，退出碼 5；讀取端提早關閉照 C++ 版放過 | D-R13 | 是（只在寫入錯誤時） |
| 系統亂數失敗 | 用未初始化的緩衝產生 run_id | `environment_error`，退出碼 5，不分析 | D-R13 | 是（只在系統錯誤時） |
| 報告暫存檔刪除失敗 | 不檢查 | 錯誤訊息附上未刪除的路徑 | D-R13 | 否 |

另外，暫存檔的預先配置改用 `File::set_len`（C++ 版是 `SetFileInformationByHandle(FileAllocationInfo)`），不影響報告（D-R04）。

## 4. 為了一致而保留的 C++ 版問題

| 項目 | D 編號 | 說明 |
|---|---|---|
| PSD 訊息 `image resource 0x<id> name runs past the resource section` 在 `0x` 後面印十進位的 id | D-R16 | 和 C++ 1.0.0 的輸出相同；排在 2.0.0 之後修正（修正後會成為新的差異） |

## 5. 只有 Rust 版有的功能（不影響 CLI 的契約）

| 項目 | D 編號 |
|---|---|
| 桌面 GUI `iat-gui.exe`（Slint）：比對、結果、影像檢視、匯出和 CLI 相同的報告；繁中、英、日三語並記住選擇；「以建議值重新分析」；樣本相同但色彩只差在無法證明等價時，標題顯示「像素一致」（報告、JSON 與退出碼不變） | D-R14、階段 7 |
| 三個執行檔的程式圖示與版本資訊（Windows 資源） | 階段 7 |
| 不連結任何 C 或 C++ 函式庫；rav1d 的 unsafe 隔離在 worker 裡 | D-R09、D-R03 |

## 6. 封存 C++ 版前的參考

- 第 1 節的差異都不影響 IMB-C：IMB-C 只讀 `schema_version`、`mode`、`strict`、`output.format`、`output.avif.pixel_verified`、`issues[].code/scope` 與 `resource_need`（D-R10），這些欄位在比對腳本的所有案例中都相同。
- 第 2 節標「可能」的項目都是罕見輸入；遇到時用 `tests/compare` 加一個案例重現，再決定補做或列為允許差異。其中 PNG 的 gAMA／sRGB／iCCP 內容檢查與 AVIF 影像序列，是真實檔案最可能碰到的。
- 建議等 IMB-C 實際改用 Rust 版一段時間、沒有發現第 2 節的情況後，再封存 C++ 版。封存前保留 C++ 1.0.0 的發布檔，比對腳本仍需要它當基準。
