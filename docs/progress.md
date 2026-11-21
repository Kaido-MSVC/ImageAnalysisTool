# iat Rust 版進度報告

Nov 21, 2026 · @YU SIANG CHEN

## 總覽

八個階段完成了前五個（階段 0 到 4）。比對腳本中已涵蓋的 267 個案例全部和 C++ 版 1.0.0 一致，允許的差異只有訊息文字。下一步是階段 5（JXL）。

| 階段 | 內容 | 狀態 | 比對結果 |
| --- | --- | --- | --- |
| 0 | 骨架：workspace、報告型別、CLI、比對腳本 | 完成 | 不需解碼 32/32 |
| 1 | PNG | 完成 | 52/52 |
| 2 | GUI 第一版（Slint） | 完成 | GUI 匯出的 JSON 與 CLI 相同 |
| 3 | WebP、PSD/PSB | 完成 | WebP 24/24、PSD/PSB 25/25 |
| 4 | TIFF | 完成 | 134/134 |
| 5 | JXL | 尚未開始 | — |
| 6 | AVIF（rav1d）與 worker | 尚未開始 | — |
| 7 | 收尾：封包、圖示、版本 2.0.0、IMB-C 實測 | 尚未開始 | — |

階段 1、3、4 的 235 個格式案例中，問題代碼、`decode_status`、`rows_decoded`、色彩與 metadata 判定都和 C++ 版相同。

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

## 重要決策

完整內容在 repo 的 `docs/decisions.md`，這裡只列重點。規劃書 `docs/plan.md` 本身沒有改動，所有修訂都記在這些編號裡。

| 編號 | 決策 |
| --- | --- |
| D-R02 | 目前單一程序；階段 6 恢復 worker，做成獨立的 `iat-worker.exe`（不是隱藏子命令），至少 AVIF 走 worker |
| D-R03 | iat-core 的 unsafe 只允許在 `win.rs` 和之後的 AV1 包裝，每段附 `// SAFETY:` 註解 |
| D-R10 | IMB-C 會檢查版本號（≥ 0.5.1）和 `iat-worker.exe` 是否存在；階段 7 改為 2.0.0，IMB-C 維持不改 |
| D-R11 | PNG：`png` crate 只解影像列，Adler-32 由 `png_scan` 另外完整驗證 |
| D-R12 | PNG：libpng 的 cHRM/gAMA/sRGB 內容檢查與已知 sRGB profile 辨識沒有重做 |
| D-R13 | 錯誤處理修正；C++ 版同樣有問題，Rust 版記為刻意的差異 |
| D-R14 | GUI 改用 FemtoVG（WGPU），移除 Skia，維持靜態 CRT；影像檢視加上顯示用縮圖金字塔 |
| D-R15 | WebP：只取 VP8L 內容重新包裝，把 `alpha_is_used` 設為 1 再解碼 |
| D-R16 | PSD/PSB：照 C++ 1.0.0 範圍；ZIP、CMYK、32-bit 延到 2.0.0 當成新功能 |
| D-R17 | TIFF：不用 tiff crate，自己讀 strip／tile 並照 libtiff 4.7.2 的行為；libtiff 讀目錄時的部分警告沒有重做，列為已知風險 |

**AV1 的方向（階段 6 適用）**：rav1d 1.1.0 關掉 `asm`，只開 `bitdepth_8`、`bitdepth_16`；包裝層獨立成 `crates/iat-av1`。rav1d 內部有大量 unsafe，所以放進 worker 隔離。本機有 NASM 3.02，只在實測太慢時才開 asm。

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

下一步是階段 5（JXL）。

**已完成的小項目**（2026-11-21）

- [x] 用 `cases.local.json` 的真實樣本跑 PNG 比對：沒有差異，D-R12 註記已驗證的範圍
- [x] IMB-C 不讀 `decode_path`（D-R10）
- [x] `miniz_oxide` 維持 0.8.9（理由見 dependencies.md）

**已知風險**

- TIFF：libtiff 讀目錄時的部分警告（tag 的 count／型別不正確而略過等）沒有重做，遇到時問題代碼會和 C++ 版不同（D-R17）。
- PNG：gAMA、sRGB、iCCP 的 libpng 內容檢查仍未經真實樣本驗證（D-R12）。

**留到後面階段**

- [ ] 階段 6：恢復 `iat-worker.exe`，AVIF 走 worker
- [ ] 階段 7：版本改為 2.0.0、設計程式圖示（C++ 版沒有圖示，要重新設計）、IMB-C 不改程式實測換用
- [ ] GUI：記住介面語言的設定
- [ ] 在 C++ repo 開 issue，記下上一節的問題

**2.0.0 之後的新功能**

- PSD/PSB 的 ZIP、CMYK、32-bit；若 IMB-C 1.4 輸出的 PSB 用到這些，列為第一優先
- 修正刻意保留的 C++ 訊息錯誤
- C++ 版封存：等 IMB-C 實際改用 Rust 版一段時間後再決定
