# 決策紀錄

規格以 `docs/plan.md` 為準，規劃書本身不改；修訂與實作時的決定都記在這裡。編號 `D-R` 開頭，避免和 C++ 版的 D 編號混淆。

## 驗證基準

| 項目 | 值 |
|---|---|
| 基準 | C++ 版 iat 1.0.0 發布檔 `D:\projects\ImageAnalysisTool\dist\ImageAnalysisTool-1.0.0-win-x64\iat.exe` |
| `iat.exe` SHA-256 | `0fbfffb382f6b795220d557b8e9d4463e1e35b3d2ec61ea0cc4edf071c9b8fa6`（643,584 bytes） |
| `iat-worker.exe` SHA-256 | `ef9d11de862e9844eb1b90c76c165503585fdb681fa8ec6c8b5643f7284257b3`（基準執行時需要，同目錄） |
| 核對 | 兩者與發布目錄 `SHA256SUMS.txt` 一致（2026-11-19） |
| C++ repo | commit `2dbb4de`；`tool.compiler` 為 `MSVC 195237029` |

C++ 版之後改版時基準不跟著換；要換就另外記一筆決定。

## 決定

### D-R01 規劃書的位置（2026-11-19）

規劃書原本放在 repo 根目錄，檔名為 `iat Rust 版實驗規劃書.md`。依規劃書「交給 Claude Code 的起始指示」，原樣移到 `docs/plan.md`，內容未修改。

### D-R02 引擎在同一程序內執行，沒有 worker 與 Job Object（2026-11-19）

C++ 版把解碼放在 `iat-worker.exe`，用 Job Object 套 `job_commit_mib` 記憶體上限。規劃書的 core API 是同步函式、GUI 直接呼叫，沒有提到 worker，所以 Rust 版目前在同一程序內執行引擎。

- `--limits job_commit_mib` 照樣解析、驗證範圍並寫進 `settings.limits`，整圖型解碼器的硬上限（`job_commit_mib` − 64 MiB）也照樣計算；但沒有作業系統層級的 commit 上限。
- `worker_crashed`、`worker_protocol_error`、`worker_stderr` 這些問題代碼目前不會出現。
- `metrics` 的鍵和 C++ 版不同（沒有 `worker_wall`、`job_peak_commit`、`job_commit_limit`、`worker_peak_*`、`libpng_alloc_peak`；多了 `engine_wall`、`analyze_total`），比對時整個 `metrics` 不比。
- ~~**未決**：需要硬性記憶體上限的案例（`job_commit_limit`、`webpbig_job_limit`）要不要恢復子程序加 Job Object，在階段 3（WebP）或階段 5（JXL）之前決定。~~ 已由下方修訂決定。

**修訂（2026-11-20）**：階段 0～5 維持單一程序。階段 6 恢復 worker：

- ~~做成同一個 `iat.exe` 的隱藏子命令（不另外出 `iat-worker.exe`），不出現在 `--help`。~~ 已由第二次修訂取代。
- 至少 AVIF 解碼要走 worker；其他格式是否也走 worker，階段 6 實作時再定並另記。
- worker 放進 Job Object，負責 `job_commit_mib` 的作業系統層級記憶體上限（`job_commit_limit`、`webpbig_job_limit` 這類案例在那之前列為允許差異）。
- worker 同時隔離 rav1d 的 unsafe：AV1 解碼的記憶體錯誤只會讓 worker 結束，主程序照 C++ 版回報 `worker_crashed` 等代碼。
- 恢復後 `worker_*` 問題代碼會出現；`metrics` 的鍵仍不比（D-R08）。

**第二次修訂（2026-11-20，因 D-R10）**：worker 改做成獨立的 bin crate `crates/iat-worker`，產出 `iat-worker.exe`，發布結構和 C++ 版相同（`iat.exe` 與 `iat-worker.exe` 放在同一個資料夾），不做成 `iat.exe` 的隱藏子命令。IMB-C 會檢查 `iat-worker.exe` 是否存在，這樣不用修改 IMB-C。

### D-R03 iat-core 的第二個 unsafe 例外：`win` 模組（2026-11-19）

規劃書與 CLAUDE.md 只允許 rav1d 包裝模組使用 unsafe。為了和 C++ 版行為一致，`crates/iat-core/src/win.rs` 也需要 Win32 FFI，std 沒有對應的安全介面：

- 英文的系統錯誤訊息（`FormatMessageW` 指定 en-US；std 的 `io::Error` 會用系統語言，`input_open_failed` 的訊息就會變成中文）
- 檔案識別碼（`GetFileInformationByHandleEx(FileIdInfo)`，判斷 `same_file`；std 的 `file_index` 仍是 unstable）
- 不取代既有檔的改名（`MoveFileExW` 不帶 `REPLACE_EXISTING`；`std::fs::rename` 會覆寫）
- 路徑比較（`CompareStringOrdinal` 不分大小寫）、`GetFileAttributesW` 的共用衝突判斷、`GetDiskFreeSpaceExW`
- 系統亂數（`BCryptGenRandom`，產生 run id）與主控台中斷處理（`SetConsoleCtrlHandler`）

做法：workspace 設 `unsafe_code = "deny"`，只有 `win.rs` 用 `#![allow(unsafe_code)]`。每個函式只包一次 FFI 呼叫，對外全是安全介面，並附 `SAFETY` 註解。依賴只多了 `windows-sys`（MIT OR Apache-2.0，純繫結，不編譯 C）。

**接受（2026-11-20）**，條件：

- unsafe 只能出現在 `iat-core/src/win.rs` 和階段 6 的 AV1 包裝（`crates/iat-av1`，見 D-R09）。
- `iat-core` 的 `lib.rs` 維持 crate 層級 `#![deny(unsafe_code)]`（workspace lint 之外再明寫一次），只在允許的模組加 `#[allow(unsafe_code)]`；不用 `forbid`，因為 `forbid` 不能在子模組放寬。
- 每個 unsafe 區塊都附 `// SAFETY:` 註解。
- 已同步修改 CLAUDE.md 的 unsafe 規則。順帶把 `FILE_ID_INFO` 的初始化改成不需要 unsafe 的寫法，`on_ctrl` 改成安全的 `extern "system" fn`。

### D-R04 階段 0 不建立暫存檔（2026-11-19）

C++ 版每次分析都建立兩個 delete-on-close 暫存檔，並清理舊的 `iat-tmp-<UUID>-a/b.bin`。階段 0 沒有需要暫存的解碼器，所以 Rust 版只檢查暫存目錄是否存在、記錄剩餘空間（`settings.temp`），不建檔也不清理。`temp_stale_removed`、`temp_cleanup_failed` 目前不會出現。階段 1（PNG Adam7）實作暫存時一起補上。

`settings.temp.location` 照 C++ 版的行為：開檔失敗或雜湊時被取消，即使指定 `--temp-dir` 也記為 `system_temp`。

**階段 1 補上（2026-11-20）**，行為與 C++ 版 `create_temp` 相同（`crates/iat-core/src/temp.rs`）：

- 每次執行在暫存目錄建立 `iat-tmp-<run id>-a.bin`／`-b.bin`：CREATE_NEW、不共用、`FILE_ATTRIBUTE_TEMPORARY | NOT_CONTENT_INDEXED`、`FILE_FLAG_DELETE_ON_CLOSE`。建立前先刪除符合 `iat-tmp-<UUID>-a/b.bin` 形狀的舊檔（`temp_stale_removed`）；引擎結束、關閉 handle 後仍存在的檔案記為 `temp_cleanup_failed`。
- 配額（`temp_gib`）與剩餘空間（保留 64 MiB）在解碼前一次預留；附加模式只能讀回已寫到磁碟的範圍。
- 與 C++ 版唯一的差別：預先配置用 `File::set_len`，C++ 版用 `SetFileInformationByHandle(FileAllocationInfo)`（不改檔案長度）。用 std 就不需要新的 unsafe；讀取範圍一律以已寫入的長度檢查，`set_len` 補的 0 不會被讀到。
- 目前只有 PNG Adam7 用到暫存（每側一個檔，整張正規影像以 pass 順序附加寫入，再逐列重建）。

### D-R05 不支援格式的訊息沿用 C++ 版文字（2026-11-19）

`unknown_format` 的訊息是 `format 'unknown' is not supported by this build (PNG, TIFF/BigTIFF, WebP, JPEG XL, PSD/PSB, AVIF)`。括號內是 C++ 版 1.0.0 的支援範圍，也是 Rust 版的最終目標，所以照抄，讓 `inspect_unknown_format` 一致。階段 0 實際上還不能解碼任何格式：辨識得出的格式一律為 `decode_status=unsupported`、`decode_status_reason=format_not_supported_in_this_build`，問題代碼 `unsupported_sample_model`（與 C++ 版遇到 JPEG 時相同）。

### D-R06 `tool` 與 `codecs` 的內容（2026-11-19）

- `tool.compiler` 記建置用的 rustc 版本（例如 `rustc 1.100.0 (0ef1bc386 2026-11-09)`），由 `build.rs` 取得。
- `codecs` 列出實際用到的 Rust crate 與版本；階段 0 沒有解碼器，所以是空物件。
- 兩者和 `tool.version` 一樣，比對時整個去掉。

### D-R07 表示狀態的欄位維持字串（2026-11-19）

報告模型中表示狀態的欄位（`decode_status`、`strict.result` 等）在 Rust 端仍是 `String`，不改成 enum：JSON 契約本身就是字串列舉，問題代碼也是開放集合。穩定代碼集中在 `report::code`。等各格式移植完、值域確定後再評估是否改成 enum。

### D-R08 比對腳本的遮罩範圍（2026-11-19）

`tests/compare/compare_iat.py` 比對前去掉 `run_id`、`tool`、`codecs`、`metrics`、`settings.temp.free_bytes_at_start`，文字中遮罩 run id 與各側的案例目錄，Markdown 去掉「工具」那一行與「效能與資源」整節。C++ 版 `compare_runs.py` 只比 `metrics` 的鍵；因為 D-R02，Rust 版的鍵必然不同，所以改成整個不比。

**階段 1 增補（2026-11-20）**：

- `--accept <格式,...>`：本階段的格式。所有檔案都是這些格式（或不需解碼）的案例也列入驗收，退出碼 0 表示「不需解碼」與「本階段」兩類案例全部一致。階段 1 用 `--accept png`。
- 允許差異表的欄位樣式中，`[` 一律當成字面字元（JSON 路徑本身就有 `issues[3]`），只有 `*`、`?` 是萬用字元。
- Markdown 比對前，把 Rust 版 Markdown 中「已允許的 JSON 字串差異」換回 C++ 版的值。否則像 `decode_path` 這種每個 PNG 案例都有的允許差異，會讓 Markdown 只能整份允許，後面的行就都沒比到。只換長度至少 4 的字串值。

### 階段 0 決定的審核（2026-11-20）

D-R04、D-R05、D-R07，以及階段 0 的其他增補（D-R01、D-R06、D-R08）使用者都同意。D-R02 修訂、D-R03 附條件接受，見各條。

### D-R09 AV1 解碼的做法（2026-11-20，階段 6 適用）

- 使用 rav1d 1.1.0，`default-features = false`，只開 `bitdepth_8`、`bitdepth_16`；不編入組合語言（不開 `asm`）。
- 包裝層獨立成 `crates/iat-av1`，unsafe 只在這個 crate；`iat-core` 只依賴它的安全介面。
- 本機有 NASM 3.02，但除非實測速度不可接受，否則不開 asm；要開時另記一筆決定。
- 階段 6 的 AVIF 解碼在 worker 子程序執行（D-R02 修訂）。

**風險**：rav1d 是 dav1d 的機械式移植，內部有大量 unsafe（指標運算、FFI 形式的 `dav1d_*` API）。`iat-core` 的 deny 規則管不到依賴內部；對策是只走 worker、用截斷與損壞的 AVIF 案例測試、包裝層的每個 unsafe 都附 SAFETY 說明。

### D-R10 IMB-C 呼叫 iat 的相容性檢查（2026-11-20，唯讀）

檢查對象：C++ repo `docs/imbc-integration.md`，與 IMB-C `src/iat.rs`（IMB-C commit `b1fa739`，v1.4.0-rc.2）。

| 項目 | 結果 | 依據 | 對 Rust 版的影響 |
|---|---|---|---|
| 1. 檢查 iat 版本號 | **會檢查** | `Client::probe`：執行 `iat --version`（Job 上限 128 MiB、10 秒），stdout 必須恰為 `iat X.Y.Z`（可帶 `\n` 或 `\r\n`），退出碼 0，且版本 ≥ 0.5.1 | **會被拒用**：Rust 版目前輸出 `iat 0.1.0`，IMB-C 回報「需要 iat 0.5.1 以上」 |
| 2. 要求 `iat-worker.exe` 存在 | **會要求** | `Client::probe`：`executable.with_file_name("iat-worker.exe").is_file()`，不存在就回報「缺少 iat-worker.exe」；UI 說明文字（`i18n.rs`）也寫明需要 | **會被拒用**：Rust 版沒有 `iat-worker.exe`，D-R02 修訂後也只做隱藏子命令 |
| 3. 讀取 JSON 的 `metrics` | **不讀** | `parse_report` 只讀 `schema_version`（主版本 1、次版本 ≥ 1）、`mode`、`strict.result`、`strict.reasons[]`、`output.format`、`output.avif.pixel_verified`、`issues[].code`／`scope`；`retry_limits` 讀 `*.resource_need` 的 `limit`、`suggested_value`、`current_value`、`suggested_argument` | 無影響 |

其他觀察（不會造成拒用，但階段 7 要注意）：

- IMB-C 把 iat 放進自己的 Job Object，記憶體預算為 `job_commit_mib + buffer_mib + 128` MiB，並以低完整性層級啟動；Rust 版單一程序時，整個 `iat.exe` 的用量都要落在這個預算內。
- 退出碼 5 時 IMB-C 讀 stderr；其他退出碼一律讀 `--report-json` 指定的報告，報告缺少上述欄位就視為工具錯誤。
- 退出碼 4 時只依 `resource_need` 自動重試一次，`suggested_argument` 必須與 `--limits <name>=<value>` 完全相同。

~~**未決**：項目 1、2 會讓 IMB-C 拒用 Rust 版，與規劃書階段 7「IMB-C 不改程式直接換用」衝突。~~

**決定（2026-11-20）**，IMB-C 維持唯讀不改：

- 版本號：實驗期間維持 `0.1.0`。階段 7 驗收時改為 `2.0.0`。不可用預發布版號，因為 IMB-C 要求輸出恰為 `iat X.Y.Z`。
- worker：見 D-R02 第二次修訂，階段 6 起產出獨立的 `iat-worker.exe`，和 `iat.exe` 放在同一個資料夾。
- 在那之前（階段 1～5），IMB-C 不能直接使用 Rust 版；比對只靠 `tests/compare`。

### D-R11 PNG 解碼：png crate 只解 IDAT，其餘由 png_scan 決定（2026-11-20，階段 1）

第 2 步驗證（2026-11-20）：png 0.18.1 以 `Transformations::IDENTITY` 交出未經轉換的列。有 `.truth` 的 11 張 PNG 逐值相同；7 組 Adam7 與對應的非交錯圖逐值相同；16-bit 為大端序原值，1/2/4-bit 與調色盤索引不展開，tRNS 不套用。展開規則照 C++ 版 `expand_row` 自己做。

實作方式（`crates/iat-core/src/png_side.rs`、`png_scan.rs`）：

- **只餵關鍵 chunk**：png crate 只收到簽章、IHDR、PLTE（僅調色盤影像）、全部 IDAT、IEND，以及掃描停下處之後的原始位元組（例如截斷或 CRC 錯誤的 IDAT）。輔助 chunk 的語意、PLTE 與 tRNS 的值都來自 png_scan。原因：png crate 的 `Info.trns` 在位深低於 16 時只留低位元組（libpng 保留完整 16 bit）；它對重複或位置錯誤的輔助 chunk 也比 libpng 嚴格（直接錯誤，libpng 只警告）。
- **Adler-32 由 png_scan 檢查**：使用者指示 `set_ignore_adler32(false)`，實作改為 `true`，IDAT 的 Adler-32 由 png_scan 另外完整解壓一次來驗證（不保留資料）。原因：png crate 自己檢查時，小圖會在交出第一列之前就失敗（`adler32_error_output` 會變成 `rows_decoded` 0、`failed`）；libpng 則是解完前面的列，在最後一列失敗（`rows_decoded` 2、`partial`）。Adler-32 錯誤仍然一律偵測到，判定與 C++ 版相同；代價是 IDAT 多解壓一次。`set_skip_ancillary_crc_failures(false)` 照指示設定（只餵關鍵 chunk，所以實際上不起作用）。
- **libpng 的失敗位置**：依 libpng 1.6.59 的讀取方式，由掃描結果決定。
  - 最後一個 IDAT 的 CRC 錯誤與 Adler-32 不符，是在最後一列的 `png_read_row`（`png_read_finish_IDAT`）中發現，所以算成最後一列失敗。
  - libpng 以 8192 bytes 為單位讀 IDAT，截斷的 IDAT 只交出完整的 8192 區塊。
  - IDAT 之後的錯誤（缺 IEND、後面 chunk 的 CRC 或截斷）算成 `after_image_data`。
  - 第一個 IDAT 之前的錯誤算成 `png_read_info` 失敗（`failed`／`header`）。
- **`decode_path`** 改名為 `png-rows-no-transform`、`png-adam7-passes-no-transform+temp`（允許差異）。`codecs` 為 `{"png": "0.18.1"}`（不比對）。
- 解碼失敗的問題訊息字首是 `png decoder at …`／`png decoder after image data: …`（C++ 版是 `libpng …`）；`decoder_errors` 是 png crate 或相容檢查的訊息。兩者都列在允許差異。

新增的直接依賴：png 0.18.1、miniz_oxide 0.8.9（png 已經間接使用；iCCP／zTXt／iTXt 與 IDAT 串流檢查要區分 Adler-32 不符、資料損壞與截斷）、crc32fast 1.5.5（chunk CRC）。

### D-R12 libpng 相容檢查（2026-11-20，階段 1）

C++ 版有些問題代碼來自 libpng 的警告：`metadata_parse_failed`（警告含 zTXt／iTXt／iCCP／tEXt）、`decoder_warning`（其他警告）、`icc_rejected_by_decoder`（libpng 沒有採用 iCCP）。依使用者指示，png_scan 自己解壓 zTXt／iCCP，做 libpng 1.6.59 同樣的檢查來產生這些代碼。依據是 C++ 版建置用的原始碼（`build/release/vcpkg_installed/.../libpng/src/v1.6.59-*/pngrutil.c`、`png.c`）與其 `pnglibconf.h`。

**有重做的**：
- `png_handle_chunk` 的位置規則（`out of place`）、`duplicate`、`too short`、`too long`，以及 `length exceeds libpng limit`（`png_chunk_max` = C++ 版設定的 `metadata_item_bytes`）。
- 未知的關鍵 chunk 一律是錯誤。
- 文字 chunk 快取上限 1000（`no space in chunk cache`）。
- tEXt／zTXt／iTXt 的關鍵字、截斷、壓縮方法、解壓失敗與 `extra compressed data`；`png_read_buffer` 超過上限時的 `out of memory`。
- iCCP：
  - 長度與關鍵字、壓縮方法；
  - `png_icc_check_length`／`check_header`／`check_tag_table`，包括只發警告的項目；
  - libpng 只解壓到 profile 長度，之後的 Adler-32 不檢查；
  - 剩下未讀的壓縮資料只發警告。
- eXIf 的開頭檢查（`invalid`）。
- PLTE：
  - 重複、位置、灰階影像的 PLTE、長度；
  - 調色盤影像上的錯誤是 libpng 的致命錯誤；
  - 調色盤截到 2^bit_depth 筆。
- tRNS：
  - 長度、位置、帶 alpha 的影像；
  - 被拒收的 tRNS 不套用，像素一律不透明。
- IDAT 收尾的 `Extra compressed data`、`Too much image data`、非連續 IDAT 的 `Too many IDATs found`。
- 最多記錄 32 個警告，每個截到 159 字元，同 C++ 版 png_shim。

警告的時機同 libpng：IDAT 之前的 chunk 一定記錄；IDAT 收尾的警告在讀到最後一列時記錄；IDAT 之後的在 `png_read_end` 有執行時才記錄。

**沒有重做的**（目前的測試圖都沒有用到，遇到時 Rust 版不會發出 libpng 的那個警告）：
- cHRM、gAMA、sRGB、sBIT、cICP、mDCV、cLLI、bKGD、hIST、pHYs、tIME、sPLT、oFFs、pCAL、sCAL 的內容檢查（長度與位置規則有做）。
- iCCP 是否為已知的 sRGB profile（libpng 的 `png_icc_set_sRGB` 警告）。
- 超過 64 MiB、無法讀進記憶體檢查的 iCCP，視為 libpng 會採用。
- zlib 自己的錯誤文字（例如 `invalid distance too far back`）：Rust 版依錯誤種類回報 `damaged LZ stream`、`truncated`、`incorrect data check`。

問題訊息的字首是 `libpng-compatible check: `（C++ 版是 `libpng: `），後面的文字相同。這些警告不寫進 `decoder_warnings`，因為它們不是解碼器產生的；兩者都列在允許差異。

## 允許的差異

比對腳本讀這張表；表格以外的差異一律算失敗。欄位為 JSON 的點路徑，或 `exit`、`stderr`、`markdown`；可用 `*` 萬用字元。

| 案例 | 欄位 | 原因 |
|---|---|---|
| * | *.decode_path | 解碼器名稱：Rust 版是 png crate（`png-rows-no-transform`、`png-adam7-passes-no-transform+temp`），C++ 版是 libpng（D-R11） |
| truncated_output | issues[*].message | 解碼失敗訊息字首 `png decoder` 對 `libpng`，其餘文字相同（D-R11） |
| crc_error_output | issues[*].message | 同上 |
| missing_iend_output | issues[*].message | 同上 |
| adler32_error_output | issues[*].message | 同上 |
| ancillary_crc_after_idat_output | issues[*].message | 同上 |
| adam7_truncated_output | issues[*].message | 同上 |
| inspect_truncated | issues[*].message | 同上 |
| inspect_adam7_truncated | issues[*].message | 同上 |
| interlace_flag_without_adam7_data | issues[*].message | 濾波位元組錯誤的訊息是 png crate 的 `Unknown filter method 40.`，libpng 是 `bad adaptive filter value`；判定、失敗位置（Adam7 pass 2 第 0 列）相同（D-R11） |
| interlace_flag_without_adam7_data | *.decoder_errors[*] | 同上 |
| inspect_interlace_flag_only | issues[*].message | 同上 |
| inspect_interlace_flag_only | *.decoder_errors[*] | 同上 |
| broken_ztxt_output | issues[*].message | libpng 警告由相容檢查產生，字首 `libpng-compatible check:` 對 `libpng:`，其餘文字相同（D-R12） |
| broken_ztxt_output | *.decoder_warnings* | 相容檢查的警告不寫進 `decoder_warnings`（D-R12） |
| inspect_broken_ztxt | issues[*].message | 同 broken_ztxt_output |
| inspect_broken_ztxt | *.decoder_warnings* | 同 broken_ztxt_output |
| png_ztxt_broken_keyword_latin1 | issues[*].message | 同 broken_ztxt_output |
| png_ztxt_broken_keyword_latin1 | *.decoder_warnings* | 同 broken_ztxt_output |
| png_exif_over_limit_orientation_differs | issues[*].message | 同 broken_ztxt_output（`eXIf: length exceeds libpng limit`） |
| png_exif_over_limit_orientation_differs | *.decoder_warnings* | 同 broken_ztxt_output |
