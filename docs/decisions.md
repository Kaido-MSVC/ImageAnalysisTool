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

**第三次修訂（2026-11-21，階段 6，使用者決定）**：**所有格式**的分析都在 `iat-worker.exe` 裡執行（同 C++ 版），GUI 的分析也一樣。
- 父程序（`iat.exe`、`iat-gui.exe`）照 C++ 版 `run_worker`：唯讀開檔、雜湊、建立暫存檔，再以繼承的 handle 啟動 worker（`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 只列這些 handle）。worker 先以暫停狀態建立、放進 Job Object 後才開始執行。
- Job：整個 Job 與單一程序的 commit 上限都是 `job_commit_mib`，只允許一個程序，關閉 Job 時結束 worker，未處理的例外直接結束；記憶體上限通知經 completion port。取消與 `deadline_s` 由父程序結束 Job（結束代碼與 C++ 版相同）。
- 協定是 Rust 版自己的（`crates/iat-core/src/worker.rs`）：stdout 為報告 JSON 或 `{"worker_error": …}`；另一條管線傳進度，以及「rav1d 在哪一側 panic」。
- 結果的對應與 C++ 版相同：`environment_error`（找不到 worker、Job 設定或建立程序失敗）、`cancelled`、`deadline_exceeded`（訊息改為 C++ 版的 `worker exceeded deadline of …`）、`resource_limit`（worker 配置失敗）、`worker_crashed`、`worker_protocol_error`（stdout 超過 32 MiB 或報告格式不符）、`worker_stderr`（info）。
- **配置失敗**：worker 的全域配置器（`win::WorkerAllocator`）在系統配置器失敗時，把事先準備好的固定字串 `{"worker_error":"resource_limit",…}` 直接以 WriteFile 寫到 stdout 後 `ExitProcess(3)`，處理過程不再配置記憶體（使用者決定）。父程序回報 `worker allocation failed under Job commit limit N MiB`。
- **panic**：引擎用 `catch_unwind` 包住解碼器的 open／read_row／finish，panic 判為該側的 `decode_failed`（`decode_error`），和 C++ 版解碼器回報失敗相同；worker 的 panic hook 不印任何東西（不會多出 `worker_stderr`）。release profile 沒有設 `panic = "abort"`（只有 `lto`、`codegen-units`）。
  - rav1d 的 `dav1d_*` 是 `extern "C"`，panic 不能 unwind 出來，會直接中止 worker。為了仍判為解碼錯誤：AV1 解碼期間 panic hook 經通道告訴父程序是哪一側，父程序把那一側標記後重跑 worker（每側最多一次），重跑時該側不解碼、直接判為 `decode_error`。
- 報告經過一次 JSON 往返：serde_json 開 `float_roundtrip`，浮點數原值往返。`metrics` 現在有 `worker_wall`、`job_peak_commit`、`job_commit_limit`（仍不比對，D-R08）。
- **GUI 的檢視解碼**：AVIF 在 worker 裡解碼（套 GUI 的 `job_commit_mib`），樣本經 stdout 傳回；其他格式留在 GUI 程序裡。評估：其他格式的解碼器都是純 safe 的 Rust，沒有 rav1d 那樣的 unsafe 隔離需求；檢視不產生報告，沒有和 C++ 版一致的要求；改走 worker 會讓整張樣本多經過一次管線複製（16K×8K RGB16 約 800 MB）。
- **JXL 也走 worker 的評估**（D-R18）：已隨「所有格式」一起走 worker，Job 的 commit 上限因此對 JXL 生效，不會再無上限地配置。但和 C++ 版仍不完全相同：C++ 版的 libjxl 經自己的配置器，超過硬上限（`job_commit_mib` − 64 MiB）時那一側判為 `resource_limit`、報告其餘部分照常產生；Rust 版是整個 worker 在 Job 上限配置失敗，報告為執行層級的 `resource_limit`（退出碼 4 相同，`decode_status` 都是 `not_checked`／`resource_limit`，但沒有各側的其他欄位）。事前預估擋下的情況（大多數）和 C++ 版完全相同。
- ~~**已知差異**：C++ 版的 libpng 也有同樣的配置器上限。C++ `run_e2e.py` 的 `job_commit_limit`（`s1_wide_rgb16_srgb.png` 自己比自己、`buffer_mib=512 job_commit_mib=24`）：C++ 版是兩側各一個 `libpng: Out of memory`，報告完整；Rust 版是執行層級的 `worker allocation failed under Job commit limit 24 MiB (Job memory-limit notification received)`。退出碼 4、兩側 `not_checked`／`resource_limit` 相同。這個案例不放進比對腳本（差異是整份報告的內容，不是個別欄位）。~~ 已由第四次修訂處理。

**第四次修訂（2026-11-22，階段 7，使用者指示）**：Job 上限造成的配置失敗改成和 C++ 版相同的報告形式，沿用 rav1d panic 的機制。
- worker 在解碼器的 `open`／`read_row`／`finish` 期間（以及 `open` 之後配置該側的工具列緩衝時）把這一側登記到 `win::ALLOC_SIDE`。配置器失敗時，先把「哪一側、哪個階段」以 17 bytes 的通道紀錄（種類 3）直接用 WriteFile 寫出，再照舊寫固定字串並 `ExitProcess(3)`；整個過程不配置記憶體。
- 父程序收到後，把那一側標記成 `--alloc-failed <side>:<stage>` 重跑 worker（每側最多一次，和 rav1d 的 panic 共用重跑迴圈）。重跑時那一側不解碼，照 C++ 1.0.0 該格式的形式判為 `not_checked`／`resource_limit`，報告其餘部分照常產生。
- 各格式的形式（`engine::job_limit_failure`）：
  - PNG：`libpng: Out of memory`，`decoder_errors` 加 `Out of memory`。
  - WebP 工具緩衝階段（檔案與 RGBA 緩衝）：`buffers for the WebP file and its RGBA output could not be allocated`。
  - WebP 解碼階段：`libwebp ran out of memory (OUT_OF_MEMORY)`，`decoder_errors` 加 `libwebp: OUT_OF_MEMORY`。
  - JXL：`libjxl allocation refused at the hard cap of N bytes (…)`。
  - AVIF：`libavif: Out of memory`（`decoder_errors` 同）。
  - PSD／PSB：`PSD row buffers could not be allocated`。
  - TIFF：C++ 版沒有攔下 libtiff 以外的配置失敗（worker 的 `bad_alloc`），維持執行層級的 `resource_limit`，同 C++ 版。
  - 在解碼器呼叫以外（掃描、比較、報告）配置失敗時，同 C++ 版 worker 的 `bad_alloc`，維持執行層級。
- **驗證**：比對腳本新增 `job_commit_limit`（C++ `run_e2e.py` 的同名案例），和 C++ 版一致，只有允許的 `decode_path`。另外用 C++ 建置的 `gen_codec_fixture.exe` 產生 4096×4096 RGB16 JXL 與 8192×8192 RGBA8 WebP（`--big`，不進 git），`job_commit_mib=72`、預估縮成 1% 時 JXL、WebP 與 WebP 對 JXL 三個案例和 C++ 版一致（WebP 是工具緩衝階段）。
- **做不到完全一致的部分**：
  - **失敗與否取決於各自的記憶體用量**。Rust 版的解碼器用量和 C++ 版不同，同一個上限下可能一邊失敗、一邊完成。C++ `run_e2e.py` 的 `jxlbig_allocator`（`job_commit_mib=128`、預估 10%）與 `webpbig_job_limit`（`job_commit_mib=384`、預估 50%）：C++ 版判為 `resource_limit`（退出碼 4），Rust 版在上限內解碼完成（退出碼 0）。這兩個案例列為允許差異（`big_jxl_allocator_cap`、`big_webp_job_limit`）。
  - **失敗點不同**：C++ 版的 libpng、libjxl 是被自己的配置器以 `job_commit_mib` − 64 MiB 拒絕，Rust 版是整個 worker 碰到 `job_commit_mib`。PNG 在 `read_row` 中途失敗時，C++ 版可能已解出部分列，Rust 版重跑時那一側一律從 0 列判為 `resource_limit`（`rows_decoded` 可能不同，判定相同）。
  - **WebP 解碼階段的訊息實際上不會出現**：image-webp 直接解進 RGBA 緩衝，幾乎不另外配置，失敗都落在工具緩衝階段。C++ 版在 libwebp 內失敗的上限下，Rust 版通常會解碼完成（上一項）。
  - 重跑多花一次 worker 啟動與掃描的時間（只在配置失敗時）。

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

**階段 6 增補（2026-11-21）**：worker 需要的 Win32 呼叫也放在 `win.rs`（仍是唯一的模組）：Job Object 與 completion port、`CreateProcessW` 加繼承清單、匿名管線、handle 的繼承旗標、worker 端取得繼承的 handle（先以 `GetHandleInformation` 確認有效）、`SetErrorMode`，以及 worker 的全域配置器（`unsafe impl GlobalAlloc`，只轉給 `System`，失敗時寫固定字串後 `ExitProcess`）。`iat-worker` crate 本身沒有 unsafe（`#[global_allocator]` 只是指定 `win::WorkerAllocator`）。

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

**真實樣本驗證（2026-11-21）**：用 C++ repo `tools/regress/cases.local.json` 的樣本資料夾與 `gen_pairs.py` 產生的對照組資料夾（`compare_iat.py --local … --pairs …`），C++ 版與 Rust 版的 PNG 案例全部一致，只有允許的 `decode_path` 差異，沒有缺少的 libpng 警告，所以不補做。樣本與結果都不進 git。
- 涵蓋的 PNG：9 張（1 張 12000×8637 RGB 8-bit 真實圖，8 張 ImageMagick 輸出的 RGB／RGBA 8-bit，最大 2976×4175），共 15 個 PNG 相關案例（inspect、IMB-C 呼叫方式的 compare、同檔比對）。
- 這些樣本含有的輔助 chunk：cHRM（值為 sRGB／D65 原色）、bKGD、pHYs、tIME、zTXt、tEXt、iTXt。
- **仍未經真實樣本驗證**：gAMA、sRGB、iCCP（含已知 sRGB profile）以及 sBIT、cICP、mDCV、cLLI、hIST、sPLT、oFFs、pCAL、sCAL 的內容檢查；樣本裡沒有這些 chunk。遇到含這些 chunk 且 C++ 版有警告的檔案時再補做。
- 同一輪的 PSD／PSB（9 張 inspect、2 組對 ImageMagick PNG 的 compare）與 WebP 案例也全部一致。

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

### D-R17 TIFF：自己讀 strip／tile，照 libtiff 4.7.2 的行為（2026-11-21，階段 4）

**第 2 步驗證的結論**（使用者選擇做法 B）：tiff crate 0.11.3 對正常的測試圖能交出和 libtiff 相同的樣本，但下列情況對不上：
- ExtraSamples=0 時丟掉第 4 個聲道（驗收案例 `tiff_unspecified_alpha` 會不一致）。
- 不支援 MinIsWhite 加 alpha。
- 忽略 FillOrder。
- Deflate 會讀超過 byte count。
- 不會產生 libtiff 的警告。
- 沒有 strip chopping，也沒有 libtiff 的配置上限。

所以不用 tiff crate。

**做法**（`crates/iat-core/src/tiff_scan.rs`、`tiff_side.rs`、`tiff_codec.rs`）：
- `tiff_scan` 逐行移植 C++ 版，IFD 解析也給解碼器使用。支援範圍與 C++ 1.0.0 相同：多頁、SubIFD、分平面、非無號整數、8／16 以外的位元深度、Photometric 大於 2、不符的 ExtraSamples、Predictor 3 都在解碼前回報不支援；壓縮只解 none、LZW、Deflate（8 與 32946）、PackBits，其餘為 `not_checked`／`compression_not_supported`。
- 目錄層級照 libtiff 4.7.2 的 `TIFFReadDirectory`：
  - 未壓縮單一 strip 切成約 8192 bytes 的多條（`STRIP_SIZE_DEFAULT`），預算與失敗時的 `rows_decoded` 都以切分後的 strip 為準。
  - StripByteCounts 的「Bogus」「Wrong」修正與警告。
  - tag 未依序排列的警告；已知 ASCII tag 結尾缺 NUL 的警告。
- 區塊讀取照 `TIFFReadEncodedStrip`／`TIFFReadEncodedTile`：
  - 未壓縮時從 offset 直接讀整塊大小，不看 byte count。
  - 壓縮時讀 byte count，過大時照 libtiff 限制並發警告。
  - FillOrder 2 先反轉位元；16 位元換成主機順序。
  - Predictor 只在 LZW／Deflate 時套用（libtiff 的 none 與 PackBits 不設定預測器）。
  - MinIsWhite 只反轉灰階，alpha 不動。
- 解壓：
  - PackBits 逐行移植 `PackBitsDecode`，連警告一起。
  - Deflate 用 miniz_oxide，照 `ZIPDecode`：輸出滿了就停，但和 zlib 一樣在同一份輸入裡處理到結尾時檢查 Adler-32；串流比需要的短、輸入用完、zlib 檔頭錯誤的處理和訊息都照 libtiff 與 zlib。
  - LZW 用 weezl 0.1.12（TIFF 早換碼寬、輸出滿了就停），照 `LZWDecode`：不要求 EOI，輸入用完是「not terminated with EOI code」，EOI 太早是「Not enough data」；舊式位元反轉碼改用 LSB 順序解，並發 libtiff 的警告。
- 配置上限：照 C++ 版給 libtiff 的設定，單筆 max(`buffer_mib`, `metadata_item_mib`)、累計 `buffer_mib + metadata_file_mib`，記帳模擬 libtiff 的主要配置：目錄項目、tag 值、strip／tile 表（轉成 64 位元陣列）、strip chopping 的新表、壓縮資料讀取緩衝（1 MiB 起、每次乘 10）、LZW 解碼表。
  - strip／tile 表配置失敗時開檔失敗，判為 `not_checked`／`resource_limit`（`tiff_libtiff_alloc_limit` 與 C++ 版的 `decoder_errors` 逐字相同）。
  - 其他 tag 配置失敗時照 libtiff 略過，只在之後解碼失敗時改判為 `resource_limit`。
- C++ 版的「libtiff 目錄與掃描結果不符」（`decoder_disagrees_with_scan`）檢查：Rust 版只有一份 IFD 解析，這些比較必然一致，只保留區塊個數的檢查。
- 問題訊息的字首是 `tiff decoder` 對 C++ 版的 `libtiff`，括號內 libtiff 的錯誤文字盡量照原樣產生；`decoder_errors`／`decoder_warnings` 只有 zlib 的資料錯誤文字不同（miniz_oxide 不區分錯誤種類，例如 `invalid distance too far back` 回報為 `incorrect data check`）。這些都列在允許差異。
- 輸入不可信：offset、byte count、尺寸的算術一律做溢位檢查（`checked_*`），來自檔案的索引用 `get()`；`decode_path` 改名為 `tiff-strips-encoded`、`tiff-tiles-encoded`（允許差異）。

**已知風險（libtiff 讀目錄時的警告沒有全部重做）**：libtiff 4.7.2 的 `TIFFReadDirectory` 還有其他警告，Rust 版遇到時不會產生對應的 `decoder_warning`，問題代碼就會和 C++ 版不同。目前的測試圖、20 張變形檔與真實樣本都沒有觸發。

沒有重做的警告：
- tag 的 count、型別或值不正確而略過（`Incorrect count/value for "…"; tag ignored`、`Wrong data type … for "…"`、`incorrect count for field …`）。
- 超過檔案大小的配置請求（`Requested memory size … is greater than filesize`），只重做了 strip chopping 的那一則。
- OJPEG／YCbCr 專用的修正。這些在支援範圍外，會先回報不支援。

另外：
- 未知 tag 的警告在 libtiff 4.7 預設關閉，C++ 版沒有打開，所以不需要。
- 大於 2 GB 的未壓縮 strip 的虛擬切分（`TryChopUpUncompressedBigTiff`）也沒有做，只影響失敗時的 `rows_decoded` 與預算。

**驗證**：
- 比對腳本的 TIFF 案例（測試圖全部 inspect、`pairs.json` 的 TIFF 配對、C++ `run_e2e.py` 的預算／tile 暫存／libtiff 配置上限案例、20 張變形檔與 6 組配對）全部和 C++ 版一致，允許的只有訊息文字。
- 變形檔放在 `tests/fixtures/tiff_variants/`，連同產生腳本，改法與 C++ 1.0.0 的結果見該資料夾的 README，SHA-256 記在 `tests/fixtures/SHA256SUMS`。

新增的直接依賴：weezl 0.1.12（MIT OR Apache-2.0，原本已在 Cargo.lock 中，經 GUI 依賴帶入）。

### D-R18 JPEG XL：jxl-rs 解碼，libjxl 的 ICC 改用查表（2026-11-21，階段 5）

**做法**（`crates/iat-core/src/jxl_scan.rs`、`jxl_header.rs`、`jxl_icc.rs`、`jxl_side.rs`）：
- `jxl_scan` 逐行移植 C++ 版的 box 掃描（另外記下 codestream 各段的位置）。
- 解碼用 jxl crate 0.8.0（jxl-rs）：
  - 不轉向（`adjust_orientation = false`）、不渲染 spot color、不反乘 alpha。
  - 輸出 codestream 原本位元深度的整數樣本，10／12-bit 交出原生數值，同 C++ 版的 `JXL_BIT_DEPTH_FROM_CODESTREAM`。
  - 整張影像解出後依列寫進暫存檔，同 C++ 版的 WholeImageDecoder。
- **標頭輪**：jxl-rs 讀基本資訊、色彩與各 frame 標頭（`scan_frames_only`）；讀到最後一個 frame 的標頭就算成功，同 libjxl 的標頭輪。
- **ImageMetadata 自己解析**（`jxl_header`，讀到色彩空間為止）：extra channel 的位元深度與色彩聲道數 jxl-rs 沒有公開，C++ 版用來判斷「alpha 深度與色彩深度不同」。單元測試對所有 JXL 測試圖和 jxl-rs 的基本資訊交叉核對。
- 支援範圍與 C++ 1.0.0 相同：XYB、動畫、浮點、超過 16-bit、alpha 深度不同都在解碼前回報不支援；extra channel 照樣警告。
- 截斷：整個檔案都給了解碼器還要輸入，判為截斷。裸 codestream 結尾之後的位元組用解碼器的 `file_length` 算出。
- `jxl_equivalence`（D12 的色彩等價規則）移植到 engine。

**ICC 查表（使用者決定，不移植 libjxl 的 ICC 產生程式）**：浮點的 pow／log 在 MSVC 與 Rust 不保證逐位元一致，所以改用查表。
- 產生方式：`tools/libjxl_gen/gen_jxl_icc_table.cpp` 連結 C++ 版建置裡的 libjxl 0.12.0 靜態庫（唯讀，MSVC 14.52、靜態 CRT）。對每個非自訂的列舉值組合，用 libjxl 編碼 1×1 的無損影像，再用 libjxl 解碼器以 `JxlDecoderGetColorAsICCProfile(TARGET_ORIGINAL)` 讀回，和 C++ iat 用的是同一個呼叫；`build.ps1` 一次重產。
- 涵蓋範圍：RGB 216 組（白點 D65／E／DCI × 原色 sRGB／BT.2100／P3 × 6 種轉換函數 × 4 種演算意圖）、灰階 72 組、XYB 1 組，共 289 筆。
  - libjxl 自己也產生不了的組合不收：轉換函數或色彩空間為 unknown；XYB 只有 perceptual。
- 資料檔 `crates/iat-core/data/jxl_icc_table.bin`：269,112 bytes，原始格式，SHA-256 `569fabcd015146eefdb783021ed0e4b13c8d4e7e930f54ce91218e53653e0d98`。單元測試確認嵌入的資料與記錄的雜湊相符，並確認 sRGB、灰階、PQ、HLG 這 4 筆和 C++ 版報告裡的雜湊相同。重產兩次逐位元組相同。
- 來源：libjxl 0.12.0（BSD-3-Clause），條文與說明在 `licenses/`。報告裡的 `profile_name` 照 C++ 版寫「synthesized by libjxl 0.12.0 from the colour enum」，資料確實出自這一版。
- **已知差異**：自訂白點、原色或 gamma 查不到。C++ 版會有 libjxl 產生的 ICC（`synthesized`），Rust 版記為 `unreadable`，所以 ICC 的比較結果不同；色彩判定依 key 比較，兩側都是同一組自訂值時仍判為相同，和 ICC 比對時判為未驗證。目前的測試圖沒有用到。

**已知差異（jxl-rs 0.8.0 的限制）**：
- **圖層數**：jxl-rs 0.8.0 有 `coalescing` 選項但沒有實作，`scan_frames_only` 也只記錄可見的 frame，無法得知圖層數，所以不產生 `layered_image`（info，不影響判定）。
  - 樣本照樣是合成後的結果，和 libjxl 一致。
  - `jxl_two_layers_vs_composite_png` 暫時列為允許差異：允許差異表的欄位 `issues[code=layered_image]` 表示比對時只略過 C++ 版的這一筆問題（JSON 與 Markdown），其餘問題照常逐項比對。
  - 給上游的修改提案見 `docs/jxl-rs-upstream-proposal.md`。上游公開 frame header 後再補做並移除這筆允許差異。
- **配置上限**：jxl-rs 沒有記憶體分配器的掛鉤。C++ 版的 libjxl 配置超過硬上限（`job_commit_mib` − 64 MiB）時判為 `resource_limit`，Rust 版只做事前預估（照 C++ 公式：(4 × 所有聲道 + 1) B/px + 8 MiB），所以「預估通過但實際超過」的情況不會判為 `resource_limit`。**階段 6 恢復 worker 時，評估讓 JXL 也走 worker**，用 Job Object 補上硬上限。
- 轉換函數為 unknown 的色彩列舉值：libjxl 照樣解碼（ICC 為 absent），jxl-rs 會在標頭就回報錯誤。
- 自訂色度值寫進 key 時，jxl-rs 給的是 f32，C++ 版是 double，極少數值的第 6 位小數可能不同。
- 這兩項目前的測試圖都沒有用到。

**非 XYB 的有損 JXL（使用者決定）**：
- 測試圖 `tests/fixtures/jxl_generated/v_jxl_rgb16_lossy_vardct_noxyb.jxl` 用 libjxl 0.12.0 產生（VarDCT、保留原始色彩），C++ 1.0.0 會解碼並和 PNG 比較。
- jxl-rs 解出的樣本和 libjxl 有少量最低位元差異。不同的像素數、各聲道最大誤差、strict 判定與問題代碼相同；各聲道的 `mae`、`rmse` 與一個列出的差異樣本值不同。
- 理由：有損解碼的結果依解碼器而異，libjxl 與 jxl-rs 都是合規的解碼器。IMB-C 的 JXL 輸出一律無損，不受影響。
- 只對這類案例、只對這些數值列為允許差異。比對腳本也把允許的數值差異換回 Markdown，格式為 6 位有效數字。

訊息字首是 `jxl-rs` 對 C++ 版的 `libjxl`，列在允許差異；`decode_path` 改名為 `jxl-rs+temp`，已由萬用規則涵蓋。

新增的直接依賴：jxl 0.8.0（BSD-3-Clause），帶入 jxl_macros、jxl_simd、jxl_transforms、array-init、num-derive、byteorder。`cargo deny check` 通過。

### D-R19 AVIF：rav1d 解碼，平面直接讀取（2026-11-21，階段 6）

**C++ 1.0.0 的實際行為**（`src/engine/avif_side.cpp`，libavif 1.4.2 + aom 3.15.2；CMakeCache：`AVIF_CODEC_AOM=SYSTEM`、`AVIF_CODEC_DAV1D=OFF`、`AVIF_LIBYUV=SYSTEM`）：
- 從不呼叫 `avifImageYUVToRGB`，所以 libavif 的 YUV→RGB 與 libyuv 的 YUV→RGB 都不會用到，沒有矩陣或捨入要重現。
- 只解碼「metadata 符合無損」的影像：4:4:4 且 matrix 0（identity），或 4:0:0；全範圍；alpha 不是預乘；單張靜止影像；沒有 clap。平面直接讀取（identity：R = V、G = Y、B = U；4:0:0：Y = 灰階），8-bit 為 uint8，10／12-bit 為原生數值。
- 其餘組合（4:2:0、4:2:2、BT.601／709／2020 等矩陣、YCgCo-R、limited range、預乘 alpha）一律 `avif_not_lossless_by_metadata`、不解碼。
- libavif 解碼時本身會做的處理：
  - alpha 的 AV1 序列標頭為 limited range 時，用 `avifLimitedToFullY` 轉成全範圍：`v = ((v − lo) × full + (hi − lo) / 2) / (hi − lo)`，再夾到 0..full（8-bit 16/235/255、10-bit 64/940/1023、12-bit 256/3760/4095）。
  - 色彩影像的 range 以 `colr` nclx 為準（覆蓋 AV1 序列標頭），沒有 nclx 才用序列標頭。`s5_avif_444_identity_limited.avif` 就是 nclx 寫 limited、序列標頭寫 full。
  - `ispe` 與 AV1 畫格尺寸不同時用 `avifImageScaleWithLimit` 縮放（libyuv）。
  - grid 影像把各 tile 原樣拼接。
- `avif_job_limit_*` 兩個案例不是 Job Object 擋下的，是解碼前的預估：(w × h × 樣本數 × 6 + 檔案大小 + 48 MiB) 超過 `job_commit_mib` − 64 MiB 時 `resource_limit`，並給建議值（C++ D32）。

**第 2 步驗證**：
- `crates/iat-av1`：rav1d 1.1.0（不開 `asm`）的安全包裝，unsafe 只在 `src/dav1d.rs`。設定同 libavif 的 dav1d 介面：1 執行緒、`max_frame_delay` 1、`frame_size_limit` 16384²、關閉日誌（預設會寫到 stderr）。
- 對照：`tools/libavif_gen/dump_avif_planes.cpp` 連結 C++ 建置的靜態 libavif + aom（唯讀，設定同 C++ iat），傾印解出的平面。`crates/iat-av1/examples/verify_planes.rs` 用 rav1d 解同一個 item 後逐值比較。
- 結果：測試圖中 libavif 能解的 13 張（4:2:0 BT.601 有損、4:4:4 BT.601、identity limited、預乘、灰階、8／10／12-bit、含 alpha 的 4 張）與私人樣本 2 張（`crop8__avif.avif` 無損 4:4:4、`crop8__avif-lossy.avif` 4:2:0），以及從私人樣本 PNG 產生的 12000×8637 無損與有損各 1 張，Y／U／V／A 全部逐值相同。截斷與 box 超出檔尾的 2 張 libavif 在解析階段就拒絕，不比平面。
- 解碼時間（release，單執行緒）：

| 檔案 | libavif + aom（C++ 建置，有 SIMD） | rav1d 不開 asm（含複製平面） |
|---|---|---|
| 12000×8637 無損 4:4:4（110 MB） | 15.4–15.8 s | 13.0 s |
| 12000×8637 有損 4:2:0（5.9 MB） | 2.2–2.6 s | 5.9 s |
| crop8__avif.avif 1024×768 無損 | 121–131 ms | 102 ms |
| crop8__avif-lossy.avif 1024×768 | 17.5 ms | 35 ms |
| 測試圖（29×11） | — | 0.3–0.8 ms |

**使用者的決定（2026-11-21）與實作**：
1. **不寫 YUV→RGB 矩陣轉換**。規劃書第 45 行「AVIF 色彩轉換：有損：各標準矩陣」是誤寫，以 C++ 1.0.0 為準（這是對規劃書的修訂）。有損與 limited range 照 C++ 版回報 `avif_not_lossless_by_metadata`，不解碼。
2. **`ispe` 與 AV1 畫格尺寸不同**（libavif 會用 libyuv 縮放）：判為 `decoder_disagrees_with_parse`（`failed`），記為已知差異。alpha 的畫格與它的 `ispe` 不同時也一樣。
3. **grid**：實作。tile 依 dimg 順序解碼後原樣拼接；libavif 在 NextImage 的檢查（tile 蓋滿畫布、最右／最下一排和畫布重疊、tile 至少 64、色度取樣時寬高為偶數、所有 tile 尺寸／位元深度／layout／range 相同）照做，失敗是 `decode_error`。alpha 可以是自己的 grid，或是每個色彩 tile 各一個 `auxl` alpha（libavif 的假 grid item）。測試圖見 `tests/fixtures/avif_generated/README.md`。
4. **limited range 的 alpha**：照 libavif `avifLimitedToFullY` 的整數公式轉成全範圍（`avif_side::limited_to_full`，單元測試涵蓋端點、夾值、捨入與單調性）。libavif 的編碼器做不出這種檔案，沒有測試圖。
5. **容器解析**：`avif_scan` 逐行移植；item／property 的解析（`avif_parse`）照 libavif 1.4.2 `avifParse`／`avifDecoderParse`／`avifDecoderReset` 的 item 路徑重寫：頂層 box（ftyp 先出現、相容 brand、size 0、uuid、重複的 ftyp／meta／moov、找到需要的 box 就停）、meta 的子 box（hdlr、iloc、pitm、idat、iprp／ipco／ipma、iinf／infe、iref、grpl 與唯一性）、各 property 的解析與 essential 規則、ispe 與尺寸上限、primary item、alpha（auxC）、Exif（TIFF 標頭位移檢查）與 XMP、av1C、pixi（strict）、clap（strict 的有效性）、alpha 與色彩的轉換 property 是否一致、nclx 或序列標頭的 CICP（序列標頭解析移植 `obu.c`）。錯誤只分 C++ 版會區分的 `truncated`／`resource_limit`／其他（`structure_error`）。

**rav1d 的錯誤處理**：rav1d 回傳錯誤時是 `decode_failed`／`decode_error`，問題訊息 `rav1d: Decoding of color planes failed`（C++ 版是 `libavif: …`）；`decoder_errors` 另附 rav1d 的錯誤碼。

**已知差異與風險**（同 D-R17，遇到時問題代碼可能和 C++ 版不同；目前的測試圖、私人樣本都沒有觸發）：
- 主要 brand 為 `avis` 的影像序列：libavif 走 tracks（解析 moov），這裡不解析，直接回報 `animation_not_supported`，`avif.*`、寬高等欄位是空的；moov 本身損壞時 C++ 版會是 `structure_error`。
- gain map（`tmap` brand）與 sample transform（`sato`）：libavif 解析時的檢查沒有重做。
- `a1op`（運算點）、`a1lx`／`lsel`（分層影像）：只做 libavif 的解析檢查；解碼一律用運算點 0、所有圖層（libavif 依 `a1op`／`lsel` 選）。
- 解碼後的位元深度或 layout 和 av1C 不同：判為 `decoder_disagrees_with_parse`（C++ 版會用解碼後的值讀平面）。
- grid tile 之間 CICP 不同：rav1d 沒有交出每個 tile 的 CICP，這項檢查沒做。
- rav1d 的 panic：見 D-R02 第三次修訂。若是 rav1d 的記憶體錯誤（不是 panic）讓 worker 中止，判為 `worker_crashed`。

**驗證**：比對腳本的 AVIF 案例（`pairs.json` 的 16 組與 3 個 inspect，含 `avif_job_limit_*` 與 AVIF 對 JXL；grid 的 4 個；私人樣本 4 個）全部和 C++ 版一致，只有允許的 `decode_path`（`rav1d-planes-identity+temp` 對 `libavif-aom-planes-identity+temp`）。GUI：比對、結果與影像檢視（grid 的 RGBA 與 1024×768 私人樣本）都實測過，開檔對話方塊加上 AVIF。

新增的直接依賴：rav1d 1.1.0（BSD-2-Clause），見 dependencies.md。

## 允許的差異

比對腳本讀這張表；表格以外的差異一律算失敗。欄位為 JSON 的點路徑，或 `exit`、`stderr`、`markdown`；可用 `*` 萬用字元。

| 案例 | 欄位 | 原因 |
|---|---|---|
| * | *.decode_path | 解碼器名稱：Rust 版是 png crate（`png-rows-no-transform`、`png-adam7-passes-no-transform+temp`），C++ 版是 libpng（D-R11）；AVIF 是 `rav1d-planes-*` 對 `libavif-aom-planes-*`（D-R19）；其他格式同理 |
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
| tiff_corrupt_deflate_data | issues[*].message | 解碼失敗訊息字首 `tiff decoder` 對 `libtiff`，括號內 libtiff 的錯誤文字相同；代碼、狀態、`rows_decoded` 相同（D-R17） |
| tiffinspect_s2_tif_bad_deflate | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_deflate_last_short | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_deflate_short_mid | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_lzw_corrupt | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_lzw_no_eoi | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_lzw_short_mid | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_lzw_tile_short | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_packbits_short_last | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_packbits_short_mid | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiffvar_v_rgb8_fill2_deflate | issues[*].message | 同 tiff_corrupt_deflate_data |
| tiff_libtiff_alloc_limit | issues[*].message | 字首 `tiff decoder allocation limit` 對 `libtiff allocation limit`，其餘文字與 `decoder_errors` 相同（D-R17） |
| tiffvar_v_deflate_corrupt | issues[*].message | 同 tiff_corrupt_deflate_data；另外 zlib 的 `invalid distance too far back` 在 miniz_oxide 回報為 `incorrect data check`（D-R17） |
| tiffvar_v_deflate_corrupt | *.decoder_errors[*] | 同上 |
| jxl_two_layers_vs_composite_png | issues[code=layered_image] | **暫時**：jxl crate 0.8.0 沒有實作 `coalescing = false`，無法得知圖層數，所以不產生 `layered_image`（info，不影響判定）；比對時只略過 C++ 版的這一筆問題，其餘問題照常逐項比對。等上游公開 frame header 後移除（D-R18） |
| jxl_codestream_truncated | issues[*].message | 解碼失敗訊息字首 `jxl-rs` 對 `libjxl`，其餘文字相同；代碼與判定相同（D-R18） |
| inspect_jxl_truncated | issues[*].message | 同 jxl_codestream_truncated |
| jxlgen_lossy_noxyb_vs_png | samples.per_channel[*].mae | 非 XYB 的有損 JXL：有損解碼的結果依解碼器而異，libjxl 與 jxl-rs 都是合規的解碼器，樣本有少量最低位元差異；不同的像素數、最大誤差、strict 判定與問題代碼仍要求一致。IMB-C 的 JXL 輸出一律無損，不受影響（D-R18） |
| jxlgen_lossy_noxyb_vs_png | samples.per_channel[*].rmse | 同上 |
| jxlgen_lossy_noxyb_vs_png | samples.first_differences[*].output[*] | 同上（只限列出的差異樣本的輸出值） |
| big_jxl_allocator_cap | * | 記憶體用量不同：C++ 版的 libjxl 在 64 MiB 硬上限被拒（`resource_limit`，退出碼 4），jxl-rs 在 128 MiB 的 Job 內解碼完成（退出碼 0）；只在 `--big` 時執行（D-R02 第四次修訂） |
| big_jxl_allocator_cap | exit | 同上 |
| big_jxl_allocator_cap | stderr | 同上 |
| big_jxl_allocator_cap | markdown | 同上 |
| big_webp_job_limit | * | 記憶體用量不同：C++ 版的 libwebp 在 384 MiB 的 Job 內配置失敗（`resource_limit`），image-webp 不另外配置大緩衝、解碼完成；只在 `--big` 時執行（D-R02 第四次修訂） |
| big_webp_job_limit | exit | 同上 |
| big_webp_job_limit | stderr | 同上 |
| big_webp_job_limit | markdown | 同上 |
