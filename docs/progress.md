# iat Rust 版進度報告

Nov 22, 2026 · @YU SIANG CHEN

## 總覽

八個階段全部完成（階段 0 到 7），版本為 2.0.0，發布用的 zip 已能產生（不推送、不公開發布）。比對腳本的 339 個案例與 46 個私人樣本案例全部和 C++ 版 1.0.0 一致，只有 `docs/differences.md` 第 1 節列出的允許差異。剩下的是 IMB-C 實測（步驟在 `docs/imbc-trial.md`，由你操作）。

| 階段 | 內容 | 狀態 | 比對結果 |
| --- | --- | --- | --- |
| 0 | 骨架：workspace、報告型別、CLI、比對腳本 | 完成 | 不需解碼 32/32 |
| 1 | PNG | 完成 | 52/52 |
| 2 | GUI 第一版（Slint） | 完成 | GUI 匯出的 JSON 與 CLI 相同 |
| 3 | WebP、PSD/PSB | 完成 | WebP 24/24、PSD/PSB 25/25 |
| 4 | TIFF | 完成 | 134/134 |
| 5 | JXL | 完成 | 43/43 |
| 6 | AVIF（rav1d）與 worker | 完成 | AVIF 23/23，原有 310 個維持一致 |
| 7 | 收尾：封包、圖示、版本 2.0.0、GUI 收尾、IMB-C 實測準備 | 完成（IMB-C 實測待你操作） | 339/339、私人樣本 46/46 |

階段 1、3、4、5、6 的 301 個格式案例中，問題代碼、`decode_status`、`rows_decoded`、色彩與 metadata 判定都和 C++ 版相同。

## 各階段成果

**階段 0：骨架**

- 建立 workspace（iat-core、iat-cli、iat-gui）、`deny.toml`、靜態 CRT 設定；release 版 `iat.exe` 不匯入 VC++ 或 UCRT 的執行階段 DLL。
- 從 C++ repo 複製 345 個測試圖，SHA-256 逐一核對一致；C++ 基準 `iat.exe` 的雜湊已記進 `decisions.md`。
- 比對腳本 `tests/compare/compare_iat.py`：比對退出碼、stderr、JSON（含鍵順序與縮排）和 Markdown。
- 工具鏈：rustc 1.100.0、cargo-deny 0.21.0。

**錯誤處理審查的修正**（提交 c68da96、ba018ca、dca83a1）

- I/O 失敗不再被當成預設值：讀檔頭、雜湊、檔案大小、stdout 寫入失敗都會回報。
- 改寫時發現 Windows 的 `seek_read` 會移動檔案指標，雜湊改成依位移讀取。

**階段 1：PNG**（提交 09f3955、e760795、112a3b3、c2b3038、f2f432c）

- `png` 0.18.1 只負責解出影像列；tRNS、PLTE 和所有輔助 chunk 由移植的 `png_scan` 處理。
- 新增 `miniz_oxide` 0.8.9（Adler-32 驗證）和 `crc32fast` 1.5.5。
- 52 個案例中 14 個有允許的訊息差異；問題代碼全部一致。

**階段 2：GUI 第一版**

- Slint 1.19.0（FemtoVG WGPU 後端）、rfd 0.18.0、sys-locale 0.3.2；四個畫面、繁中英日三語、「關於」畫面的 AboutSlint。
- 顯示用縮圖金字塔：16K×8K 連續縮放從 4.5 FPS 提升到 37 FPS。
- 拖放已實測：中文與含空格路徑正常，比對結果正確。

**階段 3：WebP 與 PSD/PSB**

- WebP：`image-webp` 0.2.4，只取 VP8L 內容重新包裝後解碼；容器與旗標判定由移植的 `webp_scan` 負責。
- PSD/PSB：照 C++ 1.0.0 的範圍逐行移植；`apeireth-image-psd` 2.0.0-rc.5 只在 dev-dependencies，13 張合成影像的交叉比對逐值一致。

**開工前的小項目**（2026-11-21，提交 5fe1c57、6496ec4、4cac24b、a9365a4）

- 本報告移到 `docs/progress.md`；`out/` 是比對腳本的輸出，原本就在 `.gitignore`。
- 比對腳本加上 `--local`／`--pairs`，可跑 C++ repo `cases.local.json` 的私人樣本。真實 PNG 樣本全部一致，D-R12 不補做；樣本裡沒有 gAMA、sRGB、iCCP，這幾項仍未經真實樣本驗證。
- IMB-C 不讀 `decode_path`（D-R10）。
- `miniz_oxide` 維持 0.8.9：png 0.18.1 本身就用 0.8，改成 0.9 也消除不了重複。

**階段 4：TIFF**（提交 17676ce 起到 f061816）

- 驗證後不採用 `tiff` crate 0.11.3：未指定 alpha 會丟掉第 4 個聲道（驗收案例會不一致）、不支援 MinIsWhite 加 alpha、忽略 FillOrder、Deflate 會讀超過 byte count、不會產生 libtiff 的警告。
- 改成自己讀 strip／tile，照 libtiff 4.7.2 的行為（D-R17）：
  - `tiff_scan` 逐行移植；strip chopping、StripByteCounts 修正、未壓縮時不看 byte count、FillOrder 2、只在 LZW／Deflate 套用 Predictor、libtiff 的配置上限都照做。
  - 解壓：PackBits 逐行移植 libtiff（連警告），Deflate 用現有的 `miniz_oxide`，LZW 新增 `weezl` 0.1.12。
- 比對腳本新增 91 個 TIFF 案例（每張測試圖的 inspect、C++ `run_e2e.py` 的預算／tile 暫存／配置上限案例、20 張變形檔與 6 組配對），連同 `pairs.json` 原有的 43 個共 134 個，全部一致；13 個案例有允許的訊息差異（字首 `tiff decoder` 對 `libtiff`，以及一則 zlib 錯誤文字）。
- 變形檔與產生腳本在 `tests/fixtures/tiff_variants/`，改法與 C++ 1.0.0 的結果寫在該資料夾的 README，SHA-256 記進清單。
- 私人樣本裡 ImageMagick 輸出的 TIFF（Deflate＋Predictor 2）兩個案例一致；樣本資料夾本身沒有 TIFF。
- GUI 能開 TIFF：比對、結果與影像檢視都實測過；開檔對話方塊加上 TIF／TIFF。
- 順帶修正：GUI 檢視在配置解碼緩衝之前就檢查檢視上限（原本先配置再檢查，超大影像會白白配置）。
- 單元測試涵蓋畸形大小與位移（溢位、超出檔案、2^31 的 tile、2^32−1 的寬度），都回報錯誤而不會 panic。

**階段 5：JPEG XL**（提交 d0fe99f 起）

- 解碼用 `jxl` crate 0.8.0（jxl-rs）：不轉向、不渲染 spot color、不反乘，10／12-bit 交出原生數值；無損樣本在所有測試圖上都和 libjxl 逐值相同。`jxl_scan` 逐行移植（D-R18）。
- libjxl 從色彩列舉值產生的 ICC 不移植，改用查表：小 C++ 程式連結 C++ 建置裡的 libjxl 0.12.0 產生 289 筆（269,112 bytes），sRGB、灰階、PQ、HLG 的雜湊和 C++ 版相同；資料、產生程式與 BSD-3 授權（`licenses/`）一起提交。自訂白點／原色／gamma 查不到，記為已知差異。
- jxl-rs 沒有公開的 extra channel 位元深度，自己解析 ImageMetadata（讀到色彩空間為止），對所有 JXL 測試圖和 jxl-rs 交叉核對。
- jxl-rs 0.8.0 沒有實作 `coalescing = false`，無法得知圖層數：`jxl_two_layers_vs_composite_png` 的 `layered_image`（info）暫列允許差異，給上游的修改提案草稿在 `docs/jxl-rs-upstream-proposal.md`，等你決定是否提交。
- 非 XYB 的有損測試圖用 libjxl 0.12.0 產生：C++ 版會解碼；jxl-rs 的樣本有少量最低位元差異，判定、不同的像素數與最大誤差相同，只有 mae／rmse 與一個差異樣本值不同（只對這個案例允許）。
- 配置上限：jxl-rs 沒有分配器掛鉤，只做事前預估（C++ 公式）；階段 6 起 JXL 也在 worker 裡，Job 的 commit 上限對它生效，超過時的報告形式見「已知風險」（D-R02）。
- 比對腳本新增 `issues[code=X]` 的允許方式（只略過 C++ 版的那一筆問題），允許的數值差異也換回 Markdown。
- 私人樣本的 4 個 JXL 案例（IMB-C 的無損與有損）一致；GUI 能開 JXL（比對、結果與影像檢視都實測過），開檔對話方塊加上 JXL。

**階段 6：AVIF 與 worker**（提交 7d31490 起）

- **驗證**（第 2 步）：rav1d 1.1.0（不開 asm）解出的 Y／U／V／A 平面，和 C++ 建置裡的 libavif 1.4.2 + aom 逐值相同：測試圖 13 張、私人樣本 2 張、12000×8637 大圖無損與有損各 1 張。C++ 版從不做 YUV→RGB（只讀 identity 4:4:4 與 4:0:0 的平面），所以不寫矩陣轉換（規劃書第 45 行的修訂，D-R19）。
- **速度**（單執行緒、不開 asm）：12000×8637 無損 13.0 s（C++ 的 aom 15.4–15.8 s）；有損 4:2:0 5.9 s（aom 2.2–2.6 s，但 C++ 版不解有損）。維持不開 asm。
- **`crates/iat-av1`**：rav1d 的 `dav1d_*` 安全包裝，unsafe 只在 `src/dav1d.rs`，每段附 `// SAFETY:`；關掉 rav1d 預設寫到 stderr 的日誌。
- **worker**（D-R02 第三次修訂）：獨立的 `iat-worker` bin crate；**所有格式**的分析（CLI 與 GUI）都在 worker 裡，放進套了 `job_commit_mib` 的 Job Object。Win32 部分在 `iat-core/src/win.rs`。
  - 配置失敗：worker 的全域配置器把固定字串 `worker_error: resource_limit` 直接寫到 stdout 後結束，不再配置記憶體。
  - 解碼器 panic：`catch_unwind` 判為 `decode_error`，panic 訊息不輸出。rav1d 的 C 介面不能 unwind，改由通道告訴父程序是哪一側，父程序重跑一次、把那一側判為解碼錯誤。
  - GUI 的影像檢視：AVIF 在 worker 裡解碼，其他格式留在 GUI 程序（評估見 D-R02）。
- **AVIF 移植**：`avif_scan` 逐行移植；item／property 解析照 libavif 1.4.2 重寫（`avif_parse`，含 strict 的 pixi、clap、alpha ispe 檢查與序列標頭的 CICP）；`avif_side` 照 C++ 版判定。grid（含 alpha grid 與每個 tile 各一個 alpha）、limited range 的 alpha（libavif 的整數公式）都已實作；`ispe` 與畫格尺寸不同時判為 `decoder_disagrees_with_parse`（使用者決定）。
- **測試圖**：`tests/fixtures/avif_generated/` 的 grid（RGB、RGBA），由 `tools/libavif_gen/gen_avif_grid.cpp` 連結 C++ 建置的 libavif 產生，重產逐位元組相同，C++ 1.0.0 的結果記在該資料夾的 README。
- **比對**：`pairs.json` 的 AVIF 19 個（含 `avif_job_limit_*` 與 AVIF 對 JXL）、grid 4 個全部一致，只有允許的 `decode_path`；私人樣本的 AVIF 4 個也一致。原本的 310 個案例改走 worker 後仍全部一致；連同 `--local`／`--pairs` 的私人樣本共 377 個案例全部一致（2026-11-21）。
- **GUI**：比對、結果與影像檢視（grid RGBA、1024×768 私人樣本）都實測過；開檔對話方塊加上 AVIF。
- **發布結構**：`cargo build --release` 在同一個資料夾產出 `iat.exe`、`iat-worker.exe`（與 `iat-gui.exe`），都只匯入系統 DLL。
- **依賴**：rav1d 1.1.0（BSD-2-Clause）；`paste`（rav1d 的 proc-macro）的「不再維護」通知加進 deny.toml 的忽略清單；windows-sys 多開 5 個 feature、serde_json 開 `float_roundtrip`。`cargo deny check` 通過。

**階段 7：收尾**（提交 8b51cb4 起）

- **Job 上限的報告形式**（D-R02 第四次修訂）：worker 在解碼器呼叫中配置失敗時，經通道告訴父程序是哪一側、哪個階段，父程序重跑一次把那一側照 C++ 版判為 `resource_limit`，報告其餘部分照常。C++ `run_e2e.py` 的 `job_commit_limit` 已加進比對腳本並一致；另用 C++ 建置產生的大圖（`--big`）驗證 JXL、WebP 的形式。C++ 版 `jxlbig_allocator`、`webpbig_job_limit` 的設定下 Rust 版用的記憶體較少、解碼完成，列為允許差異。
- **版本 2.0.0**：`iat --version` 輸出 `iat 2.0.0`。
- **程式圖示**：`assets/icons/`；winresource 0.1.34 把圖示與版本資訊嵌入三個執行檔，GUI 視窗也有圖示（D-R20）。
- **封包**：`scripts/package.ps1` 產生 `dist\ImageAnalysisTool-2.0.0-win-x64.zip`。D-R22 修正與 README 補上 `cpp-1.x` 說明後，於 commit `4419d2d` 重新產生（SHA-256 `447832afa82a4b4b890f7649d3bb4d56528d7273079cc8cddb9966516898c31a`；先前的 `829a9436…d32` 與 `46fd260b…550d` 作廢），THIRD_PARTY_NOTICES 重新產生後內容不變，dumpbin 確認三個執行檔的匯入表沒有 VC++／UCRT 的 DLL。三個執行檔都不匯入 VC++／UCRT 的 DLL；解壓到新資料夾後 CLI、worker（AVIF）與 GUI 都能直接執行，SHA256SUMS 全部相符。THIRD_PARTY_NOTICES 用 cargo-about 0.9.3 產生（2026-11-22 經同意安裝），含 libjxl ICC 資料表與 Slint 兩節。README（英文、繁中）與 CHANGELOG 新增。
- **差異總表**：`docs/differences.md`，分成允許的差異、已知差異（未觸發）、刻意修正的錯誤處理、保留的 C++ 版問題、Rust 版新增功能，附封存 C++ 版前的參考。
- **GUI**：記住介面語言；「以建議值（N MiB）重新分析」（12000×8637 PNG 對 JXL：建議 1536，重新分析後完成比對）；「像素一致」標題（未標記 PNG→JXL、WebP→PNG 兩組，匯出的 JSON 和 CLI 相同）；色彩欄顯示 `jxl_enum (sRGB)`。
- **安全通報**：RUSTSEC-2026-0592（url 2.5.8，經 Slint）依新規則只升到 2.5.9（form_urlencoded 隨之升到 1.2.3）。
- **IMB-C 實測準備**：`docs/imbc-trial.md`（放置方式、樣本與轉換、確認 IMB-C 接受 2.0.0 與 iat-worker.exe、和 C++ 版的對照方式）。
- 驗證：`cargo deny check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 都通過。

## 重要決策

完整內容在 repo 的 `docs/decisions.md`，這裡只列重點。規劃書 `docs/plan.md` 本身沒有改動，所有修訂都記在這些編號裡。

| 編號 | 決策 |
| --- | --- |
| D-R02 | 所有格式的分析都在獨立的 `iat-worker.exe` 裡執行（Job Object 套 `job_commit_mib`）；解碼器呼叫中配置失敗時照 C++ 版把那一側判為 `resource_limit`（第四次修訂）、解碼器 panic 判為解碼錯誤；GUI 的 AVIF 檢視也走 worker |
| D-R03 | iat-core 的 unsafe 只允許在 `win.rs` 和之後的 AV1 包裝，每段附 `// SAFETY:` 註解 |
| D-R10 | IMB-C 會檢查版本號（≥ 0.5.1）和 `iat-worker.exe` 是否存在；階段 7 改為 2.0.0，IMB-C 維持不改 |
| D-R11 | PNG：`png` crate 只解影像列，Adler-32 由 `png_scan` 另外完整驗證 |
| D-R12 | PNG：libpng 的 cHRM/gAMA/sRGB 內容檢查與已知 sRGB profile 辨識沒有重做 |
| D-R13 | 錯誤處理修正；C++ 版同樣有問題，Rust 版記為刻意的差異 |
| D-R14 | GUI 改用 FemtoVG（WGPU），移除 Skia，維持靜態 CRT；影像檢視加上顯示用縮圖金字塔 |
| D-R15 | WebP：只取 VP8L 內容重新包裝，把 `alpha_is_used` 設為 1 再解碼 |
| D-R16 | PSD/PSB：照 C++ 1.0.0 範圍；ZIP、CMYK、32-bit 延到 2.0.0 當成新功能 |
| D-R17 | TIFF：不用 tiff crate，自己讀 strip／tile 並照 libtiff 4.7.2 的行為；libtiff 讀目錄時的部分警告沒有重做，列為已知風險 |
| D-R18 | JXL：jxl-rs 解碼、libjxl 的 ICC 查表、自己解析 ImageMetadata；圖層數、配置上限與自訂色彩列舉值的 ICC 是已知差異 |
| D-R19 | AVIF：rav1d 解碼、平面直接讀取、不做 YUV→RGB；libavif 行為的 item 解析；grid 與 limited range alpha 照 libavif；`ispe` 與畫格不符判為 `decoder_disagrees_with_parse` |
| D-R20 | 階段 7：版本 2.0.0、winresource 嵌入圖示、封包與 THIRD_PARTY_NOTICES（cargo-about）、GUI 的語言設定／以建議值重新分析／「像素一致」 |

**AV1**：rav1d 1.1.0 關掉 `asm`，只開 `bitdepth_8`、`bitdepth_16`；包裝層獨立成 `crates/iat-av1`。rav1d 內部有大量 unsafe，放進 worker 隔離。實測速度可以接受，不開 asm。

## 順帶發現的 C++ 版問題

移植時對照 C++ 版，發現下列問題。都只在罕見的環境錯誤或訊息格式上，不影響主流程的判定。C++ 版沒有修改；Rust 版前四項已修正，最後一項為了一致刻意保留。

| 問題 | 影響 | Rust 版 |
| --- | --- | --- |
| 讀檔頭、雜湊、檔案大小的 I/O 錯誤被當成預設值 | 報告的原因與退出碼錯誤，例如網路磁碟斷線時報成「不支援的格式」 | 已修正（D-R13） |
| stdout 寫入失敗完全忽略 | `--stdout json > out.json` 遇到磁碟滿時，檔案不完整，退出碼卻是成功 | 已修正（D-R13） |
| 沒檢查 `BCryptGenRandom` 的回傳值 | 失敗時用未初始化的緩衝產生 `run_id`，暫存檔名可能衝突 | 已修正（D-R13） |
| 其他被吞掉的錯誤 | 詳見 D-R13 附的 C++ 位置 | 已修正（D-R13） |
| 某則 PSD 訊息在 `0x` 後面印十進位的資源 id | 看的人會誤以為是十六進位 | 刻意保留，2.0.0 時修正 |

建議在 C++ repo 開 issue 記下來，等 C++ 版因為其他原因要發 1.0.1 時再順便修。

## 待辦與下一步

下一步是 IMB-C 實測（`docs/imbc-trial.md`，由你操作），結果記回本報告。

**已完成的小項目**（2026-11-21）

- [x] 用 `cases.local.json` 的真實樣本跑 PNG 比對：沒有差異，D-R12 註記已驗證的範圍
- [x] IMB-C 不讀 `decode_path`（D-R10）
- [x] `miniz_oxide` 維持 0.8.9（理由見 dependencies.md）

**已知風險**

- TIFF：libtiff 讀目錄時的部分警告（tag 的 count／型別不正確而略過等）沒有重做，遇到時問題代碼會和 C++ 版不同（D-R17）。
- PNG：gAMA、sRGB、iCCP 的 libpng 內容檢查仍未經真實樣本驗證（D-R12）。
- JXL：多圖層的靜止影像不產生 `layered_image`；自訂色彩列舉值沒有 ICC（D-R18）。
- 配置上限：報告形式已和 C++ 版一致（D-R02 第四次修訂）。剩下的差異是記憶體用量：接近上限時兩版可能一個完成、一個 `resource_limit`；PNG 中途失敗時 `rows_decoded` 為 0。事前預估擋下的情況和 C++ 版完全相同。
- AVIF：主要 brand 為 `avis` 的影像序列不解析（直接回報不支援）；gain map、sample transform、`a1op`／`lsel` 的 libavif 檢查沒有全部重做；`ispe` 與畫格尺寸不同時判為 `decoder_disagrees_with_parse`（D-R19）。
- rav1d 的記憶體錯誤（不是 panic）讓 worker 中止時，判為 `worker_crashed`（C++ 版是 `decode_error`）。

**留到後面階段**

- [x] 階段 7：版本 2.0.0、程式圖示、封包、差異總表、GUI 收尾（2026-11-22）
- [x] 發布前的安全與穩定審查，11 項修正（D-R22，2026-11-22）；完整比對 385 個案例全部一致，指令見 `tests/compare/README.md`「發布前的完整比對」
- [x] GUI：記住介面語言的設定
- [ ] IMB-C 不改程式實測換用（步驟見 `docs/imbc-trial.md`，由你操作）
- [ ] 介面語言的下拉選單「選了就存檔」只有單元測試與讀取端的實測：這個環境無法用無障礙操作選取 Slint 下拉清單的項目，請手動切一次語言、重開確認
- [ ] 建議值超過可用記憶體的警告文字沒有在畫面上實測（需要建議值大於目前可用記憶體的檔案）
- [ ] 在 C++ repo 開 issue，記下上一節的問題
- [ ] 決定是否把 `docs/jxl-rs-upstream-proposal.md` 提交給 jxl-rs 上游；上游支援後補做 `layered_image`

**2.0.x／2.1.0：剩下的 clippy pedantic 警告**（D-R21；cast_* 已在 2026-11-22 清完）

- [ ] `doc_markdown`（約 120 筆）：文件中的識別字補反引號，可批次處理
- [ ] `unreachable_pub`（約 31 筆）：iat-gui 模組內的 `pub` 改為 `pub(crate)`，可用 `--fix`
- [ ] `format_push_string`（約 30 筆）：`push_str(&format!(…))` 改為 `write!`
- [ ] `missing_debug_implementations`（約 16 筆）：補 `#[derive(Debug)]` 或手寫 Debug
- [ ] `borrow_as_ptr`（約 15 筆，集中在 `win.rs`）：改用 `&raw const`／`&raw mut`
- [ ] 其餘約 170 筆（`many_single_char_names`、`map_unwrap_or`、`trivially_copy_pass_by_ref`、`assigning_clones` 等）：逐類評估，雜訊大的在 `[workspace.lints]` 設為 allow 並記理由
- 每類修正後照例用 `compare_iat.py` 對照 C++ 1.0.0，分開提交

**2.0.0 之後的新功能**

- PSD/PSB 的 ZIP、CMYK、32-bit；若 IMB-C 1.4 輸出的 PSB 用到這些，列為第一優先
- 修正刻意保留的 C++ 訊息錯誤
- C++ 版封存：等 IMB-C 實際改用 Rust 版一段時間後再決定
