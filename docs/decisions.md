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

**補查 `decode_path`（2026-11-21，唯讀，IMB-C commit `b1fa739`）**：IMB-C 整個 repo（不含 `target`）沒有出現 `decode_path` 字樣，`src/iat.rs` 的 `parse_report`／`retry_limits` 讀的欄位也和上表相同。Rust 版改了 `decode_path` 的值（D-R11）不影響 IMB-C。

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

### D-R13 錯誤處理：不把 I/O 錯誤當成預設值（2026-11-20，錯誤處理審查）

錯誤處理審查列出幾處吞掉錯誤的地方。修正前逐一對照了 C++ 版 1.0.0 在同樣情況下的行為；下列各點 C++ 版同樣吞掉錯誤，所以 Rust 版的修正是**刻意的差異**。C++ 版不動。正常情況下輸出不變，比對腳本原本一致的案例仍然全部一致。

| 情況 | C++ 版（位置） | Rust 版 |
|---|---|---|
| 讀檔頭失敗 | `src/engine/analyze.cpp` `sniff_format`：`read_at(...).value_or(0)`，格式變成 unknown，退出碼 2 | `Input::new` 在開檔階段讀檔頭，失敗記為 `input_open_failed`（`cannot read input: …`），退出碼 5 |
| 取檔案大小失敗 | `src/core/run.cpp` `describe`：`h->size().value_or(0)`；`analyze.cpp` 第 190、195 行同樣 | `Input::new` 取大小，失敗同上 |
| 雜湊時讀取失敗 | `src/common/win_util.cpp` `sha256_file` 讀取失敗與取消都回傳 `nullopt`；`run.cpp` `value_or("")` 後照樣往下跑 | 分成完成、取消、讀取失敗三種；讀取失敗記為 `input_open_failed`（`cannot read input while hashing: …`），退出碼 5；取消照舊 |
| stdout 寫入失敗 | `src/iat_main.cpp` `write_stdout`：`WriteFile` 失敗就直接 return，退出碼不變 | stderr 印 `iat: stdout write failed: …`，退出碼 5（與報告寫入失敗相同）。讀取端提早關閉（`BrokenPipe`：ERROR_BROKEN_PIPE、ERROR_NO_DATA）照 C++ 版放過 |
| 系統亂數失敗 | `win_util.cpp` `new_run_id`：不檢查 `BCryptGenRandom` 的回傳值，失敗時 run_id 由未初始化的緩衝產生 | 檢查 NTSTATUS；失敗時 run_id 留空、記為 `environment_error`（退出碼 5），不建立暫存檔也不分析，絕不用固定值 |
| 報告暫存檔刪除失敗 | `src/core/report_io.cpp` `write_new_file`：`DeleteFileW` 不檢查 | 把「temporary file not removed: <路徑> (<錯誤>)」附加到原本的錯誤訊息 |

退出碼與問題代碼依 C++ 版的對應：讀不到輸入一律歸到開檔失敗（`input_open_failed`，退出碼 5），因為 C++ 版在這一層只有這個輸入錯誤代碼；`decode_status` 記為 `not_checked`／`input_open_failed`。

其他不影響行為的修正：`TempError.kind` 改成 `TempErrorKind` enum（`Display` 輸出原本的字串，報告的 `temp_<kind>` 原因不變）；引擎拿掉 `decodable` 旗標，改看解碼器的 `Option`，被預算或 `open()` 擋下的解碼器移到 `stopped`，只保留做記憶體統計；報告暫存檔名取 run_id 前 8 個字元時改用 `get(..8)`；workspace 加上 `clippy::let_underscore_must_use`。

### D-R14 GUI 繪圖後端改用 FemtoVG（WGPU），影像檢視用顯示縮圖金字塔（2026-11-20，階段 2）

**修訂規劃書「GUI（Slint）設計要點」的繪圖後端**：規劃書寫的是 `renderer-skia`，實際改用 `renderer-femtovg-wgpu`，整個 workspace 維持 `+crt-static`，不需要建置腳本或動態 CRT。

原因：
- Skia 是 C++ 預建檔，違反規劃書目標 4（純 Rust）。
- 而且 skia-bindings 0.157.0 沒有 `+crt-static` 的預建檔（`…-static` 下載 404），會退回從原始碼完整建置。
- FemtoVG（WGPU）實測 16K×8K 平移順暢，只有縮放會卡。

**縮放卡頓的處理**：
- iat-gui 為顯示用的 8-bit RGBA 影像建立縮圖金字塔：第 0 層原尺寸，之後每層長寬減半，直到長邊不超過 256。
- 縮放比例 z < 1 時取 k = floor(log2(1/z)) 那一層（2^-k ≥ z 的最小層）；100% 以上一律用原尺寸層，`pixelated` 不受影響。
- 影像縮小用 2×2 平均；差異熱圖用 2×2 取最亮，單一個不同的像素縮小後仍看得見。
- 像素值檢視與差異判定照舊讀原始樣本（`iat_core::DecodedImage`），金字塔只供顯示。

**實測**（release，16384×8192 RGB 8-bit，視窗約 1100×800）：
- 每一畫格改一次縮放比例（5% 到 100% 來回，6 秒）：沒有金字塔 4.5 FPS；有金字塔 37 FPS。另有一次約 0.5 秒的停頓，推測是第一次切到原尺寸層時上傳 512 MB 貼圖。
- 縮放比例固定、全速重繪時，有沒有金字塔都在 230 FPS 左右。所以卡頓出在縮放比例改變的時候，不是靜態繪圖。沒有金字塔時縮小畫面還會有明顯的疊紋。
- 400%（原尺寸層、pixelated）約 180 FPS。
- 只檢查一個檔案並開啟檢視時，程序約使用 1.6 GB 記憶體。

**階段 2 的其他決定**：
- 檢視 API：`iat_core::decode_for_view(path, temp_dir, limits, max_pixels, cancel)` 回傳原始正規樣本（16-bit 完整保留），交錯 PNG 用 delete-on-close 暫存檔。
  - 寬×高超過 `max_pixels` 時，在配置樣本緩衝前就停止。GUI 設為 2^28（16K×16K）。
- 進度：引擎回報 `Stage::Analyze`，done／total 是兩側已解出的列數與總列數。GUI 最多每 50 ms 更新一次畫面；CLI 不使用進度，輸出不變。
- 錯誤型別（審查第 9 點）：`LimitError`、`ReportTargetError`、`WriteReportError`（含 `CleanupFailure`）、`CreateTempError`，`Display` 文字與 C++ 版相同。GUI 依種類提示，例如匯出目標已存在時請使用者另選檔名。
- GUI 匯出用的是和 CLI 相同的 `report_json_text`／`render_markdown`、`validate_report_targets`、`write_new_file`，請求設定也和 CLI 預設相同（`full_paths = false`、預設 limits）。實測同一組檔案的 JSON，除了 run_id、metrics 與暫存空間的剩餘量之外完全相同。
- 拖放：照 slint-probe v4，接 winit 的 `HoveredFile`／`DroppedFile`，加 50 ms 批次計時器。
  - 一次兩個以上：依拖入順序填入原檔與輸出（多的忽略）。
  - 只有一個：先填原檔，原檔已有就填輸出。
  - 執行中不換檔。命令列參數 `iat-gui <原檔> [<輸出檔>]` 也用同一個規則。
- 多語系：
  - `.slint` 原文為英文，用 `@tr()` 包住；`build.rs` 關閉預設的翻譯 context（`DefaultTranslationContext::None`），所以 `.po` 沒有 msgctxt。
  - 翻譯檔在 `crates/iat-gui/lang/{zh_TW,ja}/LC_MESSAGES/iat-gui.po`，`lang/check_po.py` 檢查有沒有缺漏或多餘的條目。
  - 啟動時依 Windows 顯示語言選擇（sys-locale；中文一律用繁中），`IAT_GUI_LANG`（`en`、`zh_TW`、`ja`）優先。介面上可以切換，但這次沒有記住選擇。
  - 字型依語言指定：英文 Segoe UI、繁中 Microsoft JhengHei UI、日文 Yu Gothic UI，避免漢字用到另一種語言的字形。日文介面下路徑的 `\` 顯示成「¥」，這是 Yu Gothic 的既有行為。
- 報告內容（問題訊息等）維持英文，和 CLI 相同，不跟介面語言變；Markdown 匯出也和 CLI 一樣。
  - core 的取消訊息是 `cancelled by user (Ctrl+C / Ctrl+Break)`，GUI 按「取消」時也是這句，因為報告文字要和 CLI 及 C++ 版相同。
- Slint 免版稅授權：關於畫面放 `AboutSlint`；deny.toml 的授權例外與 advisories 忽略見 docs/dependencies.md。

### D-R15 WebP：image-webp 只解 VP8L chunk，解碼輸入的 alpha 提示位元設為 1（2026-11-20，階段 3）

WebP 前端照 C++ 版：
- 自己的 RIFF 掃描（移植 `webp_scan`）負責容器完整性、VP8X 旗標一致性與 `alpha_hint_mismatch`。
- 只解 still VP8L；lossy（VP8）與動畫回報不支援。
- 整張影像解出後依列寫進這一側的 delete-on-close 暫存檔，再逐列交給引擎（對應 C++ 版 WholeImageDecoder）。

**解碼器的輸入**：從檔案取出第一個 VP8L chunk 的內容，在記憶體裡包成只含這個 chunk 的簡單格式 RIFF（`RIFF` + 大小 + `WEBPVP8L` + chunk 大小 + 內容 + 奇數長度時的補齊位元組），交給 image-webp 0.2.4。
- **會修改的只有一個位元**：VP8L 標頭的 alpha_is_used 設為 1。
- **內容照檔案實際的位元組原樣傳入**，不補資料、不改長度。chunk 大小是實際長度，奇數時只在 RIFF 層補一個 0，和 RIFF 規則相同。所以截斷的位元串流仍由解碼器回報錯誤，判定和 C++ 版一致。
- **只影響解碼輸入，不影響容器判定**：
  - 容器狀態、旗標與提示的核對都在掃描階段，依檔案原本的位元組完成。
  - 報告裡的 `chunks`、`coverage`、`alpha_hint_mismatch` 都來自掃描，不是來自包裝後的資料。

理由（2026-11-20 用 C++ 版的測試圖驗證）：
- image-webp 解碼 VP8L 位元串流時讀過 alpha_is_used 但不使用，只拿它（簡單格式）或 VP8X 的 alpha 旗標（擴充格式）決定要不要把 alpha 丟掉、只輸出 RGB。C++ 版用 libwebp 的 MODE_RGBA，一律拿到完整的 RGBA，再依實際的 alpha 決定以 RGB 或 RGBA 比較。
  - 直接把整個檔案交給 image-webp 時，`s3_webp_hint_off_alpha40.webp`（提示關閉但 alpha=64）的 alpha 會被丟掉。包裝後 35 個像素的 alpha=64 全部保留。
- image-webp 遇到 VP8X 的 EXIF／XMP／動畫旗標和內容不符時會整個拒絕（`ChunkMissing`），C++ 版只發 `webp_container_warning` 照樣解碼。只交 VP8L chunk 就不經過它的 VP8X 檢查。
- alpha 為 0 底下的 RGB 會保留：`s3_webp_rgba8_icc.webp` 的 358 個 alpha=0 像素和原始 PNG 逐值相同，`s3_webp_rgba8_hidden_lsb.webp` 的 1 LSB 差異也找得到。
- VP8L 截斷（`s3_webp_vp8l_cut.webp`）：image-webp 回傳 `BitStreamError`，C++ 版是 libwebp `BITSTREAM_ERROR`，兩者都判為 `decode_failed`／`decode_error`。
  - 對應規則：`IoError(UnexpectedEof)` 判為 `truncated`，`MemoryLimitExceeded` 判為 `resource_limit`，其餘判為 `decode_error`。
  - 錯誤文字是 `image-webp: <錯誤>`，C++ 版是 `libwebp: <狀態>`，列在允許差異。

**記憶體預估**：整圖型解碼器的預估值沿用 C++ 版第 3 階段量測 libwebp 得到的公式：(8 B/px + px/8 + 檔案大小 + 4 MiB) × 1.15。這樣 `job_commit_mib` 的上限判定和建議值和 C++ 版一致。
- image-webp 的實際用量不同：記憶體裡有 VP8L 內容的一份複本、整張 RGBA 輸出（4 B/px）和解碼器內部的 ARGB 緩衝。
- Rust 版沒有 Job Object（D-R02），這個預估只用來判定，不是硬上限。

### D-R16 PSD／PSB：支援範圍同 C++ 版 1.0.0，擴充延到 2.0.0（2026-11-20，階段 3）

`psd_scan` 與 `psd_side` 逐行移植 C++ 版，支援範圍也一樣：
- 合併影像（composite）的 Raw 與 RLE（PackBits）。
- Bitmap、灰階、Indexed、RGB 的 8／16 位元（Bitmap 為 1 位元）。
- PSB 的大尺寸：每邊最多 300000，64 位元區段長度，32 位元 RLE 列位元組數。
- ZIP 與帶預測的 ZIP 回報 `compression_not_supported`；CMYK、32 位元與其他色彩模式回報 `unsupported_sample_model`，和 C++ 版一樣。
- 比對腳本的 25 個 PSD／PSB 案例全部和 C++ 版一致。

階段 3 的提示詞原本列了 ZIP、帶預測的 ZIP、CMYK、32 位元，經使用者確認是誤寫，以 C++ 1.0.0 的行為為準。
- **這些擴充延到 Rust 版以 2.0.0 接手之後，當成新功能處理**，屆時用 apeireth-image-psd 交叉驗證。
- 這次刻意不做，以免和 C++ 版的問題代碼不一致。

**交叉比對**：apeireth-image-psd 2.0.0-rc.5 只放在 iat-core 的 dev-dependencies（`default-features = false`，沒有其他依賴）。`crates/iat-core/tests/psd_cross.rs` 依這次的支援範圍產生 13 張合成影像，用 apeireth-image-psd 的 `encode_planes` 編碼合併影像資料，再分別用 iat-core（`decode_for_view`，和分析用的是同一個前端）與 apeireth-image-psd 的 `ImageData::decode` 解出，逐個樣本比較。全部一致。
- 涵蓋 RGB 8／16 位元 Raw 與 RLE、灰階、Bitmap、Indexed。
- PSB 比 PSD 上限 30000 更寬或更高的影像。

C++ 版的訊息 `image resource 0x<id> name runs past the resource section` 在 `0x` 後面印的是十進位的 id，照抄以保持一致。這是 **C++ 版的錯誤**：Rust 版為了和 C++ 1.0.0 的輸出一致而刻意保留，2.0.0 時改成正確的十六進位。

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
| webp_vp8l_truncated | issues[*].message | 解碼錯誤文字 `image-webp: BitStreamError` 對 `libwebp: BITSTREAM_ERROR`；代碼與判定相同（D-R15） |
| webp_vp8l_truncated | *.decoder_errors[*] | 同上 |
