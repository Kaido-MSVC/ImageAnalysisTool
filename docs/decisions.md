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
- **未決**：需要硬性記憶體上限的案例（`job_commit_limit`、`webpbig_job_limit`）要不要恢復子程序加 Job Object，在階段 3（WebP）或階段 5（JXL）之前決定。

### D-R03 iat-core 的第二個 unsafe 例外：`win` 模組（2026-11-19）

規劃書與 CLAUDE.md 只允許 rav1d 包裝模組使用 unsafe。為了和 C++ 版行為一致，`crates/iat-core/src/win.rs` 也需要 Win32 FFI，std 沒有對應的安全介面：

- 英文的系統錯誤訊息（`FormatMessageW` 指定 en-US；std 的 `io::Error` 會用系統語言，`input_open_failed` 的訊息就會變成中文）
- 檔案識別碼（`GetFileInformationByHandleEx(FileIdInfo)`，判斷 `same_file`；std 的 `file_index` 仍是 unstable）
- 不取代既有檔的改名（`MoveFileExW` 不帶 `REPLACE_EXISTING`；`std::fs::rename` 會覆寫）
- 路徑比較（`CompareStringOrdinal` 不分大小寫）、`GetFileAttributesW` 的共用衝突判斷、`GetDiskFreeSpaceExW`
- 系統亂數（`BCryptGenRandom`，產生 run id）與主控台中斷處理（`SetConsoleCtrlHandler`）

做法：workspace 設 `unsafe_code = "deny"`，只有 `win.rs` 用 `#![allow(unsafe_code)]`。每個函式只包一次 FFI 呼叫，對外全是安全介面，並附 `SAFETY` 註解。依賴只多了 `windows-sys`（MIT OR Apache-2.0，純繫結，不編譯 C）。

### D-R04 階段 0 不建立暫存檔（2026-11-19）

C++ 版每次分析都建立兩個 delete-on-close 暫存檔，並清理舊的 `iat-tmp-<UUID>-a/b.bin`。階段 0 沒有需要暫存的解碼器，所以 Rust 版只檢查暫存目錄是否存在、記錄剩餘空間（`settings.temp`），不建檔也不清理。`temp_stale_removed`、`temp_cleanup_failed` 目前不會出現。階段 1（PNG Adam7）實作暫存時一起補上。

`settings.temp.location` 照 C++ 版的行為：開檔失敗或雜湊時被取消，即使指定 `--temp-dir` 也記為 `system_temp`。

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

## 允許的差異

比對腳本讀這張表；表格以外的差異一律算失敗。欄位為 JSON 的點路徑，或 `exit`、`stderr`、`markdown`；可用 `*` 萬用字元。

| 案例 | 欄位 | 原因 |
|---|---|---|
