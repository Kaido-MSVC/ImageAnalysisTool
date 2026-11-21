# iat Rust 版進度報告

Nov 20, 2026 · @YU SIANG CHEN

## 總覽

八個階段完成了前四個（階段 0 到 3）。比對腳本中已涵蓋的 133 個案例全部和 C++ 版 1.0.0 一致，允許的差異只有訊息文字。下一步是階段 4（TIFF）。

| 階段 | 內容 | 狀態 | 比對結果 |
| --- | --- | --- | --- |
| 0 | 骨架：workspace、報告型別、CLI、比對腳本 | 完成 | 不需解碼 32/32 |
| 1 | PNG | 完成 | 52/52 |
| 2 | GUI 第一版（Slint） | 完成 | GUI 匯出的 JSON 與 CLI 相同 |
| 3 | WebP、PSD/PSB | 完成 | WebP 24/24、PSD/PSB 25/25 |
| 4 | TIFF | 尚未開始 | — |
| 5 | JXL | 尚未開始 | — |
| 6 | AVIF（rav1d）與 worker | 尚未開始 | — |
| 7 | 收尾：封包、圖示、版本 2.0.0、IMB-C 實測 | 尚未開始 | — |

階段 1 與階段 3 的 101 個格式案例中，問題代碼、`decode_status`、`rows_decoded`、色彩與 metadata 判定都和 C++ 版相同。

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

下一步是階段 4（TIFF），提示詞已備妥：範圍以 C++ 1.0.0 的實際行為為準，第一步先驗證 `tiff` crate 能否交出和 libtiff 相同的原始樣本。

**還沒做的小項目**（階段 2 之前交代過，之後的提示詞沒有再列入）

- [ ] 用 `tools/regress/cases.local.json` 的真實樣本跑 PNG 比對，確認 D-R12 沒重做的 libpng 檢查是否造成差異
- [ ] 唯讀確認 IMB-C 是否讀取 JSON 的 `decode_path` 欄位（Rust 版改了這個值）
- [ ] 若 `miniz_oxide` 0.9 的 API 可用，直接依賴改用和 `png` 相同的版本，消除重複

**留到後面階段**

- [ ] 階段 6：恢復 `iat-worker.exe`，AVIF 走 worker
- [ ] 階段 7：版本改為 2.0.0、設計程式圖示（C++ 版沒有圖示，要重新設計）、IMB-C 不改程式實測換用
- [ ] GUI：記住介面語言的設定
- [ ] 在 C++ repo 開 issue，記下上一節的問題

**2.0.0 之後的新功能**

- PSD/PSB 的 ZIP、CMYK、32-bit；若 IMB-C 1.4 輸出的 PSB 用到這些，列為第一優先
- 修正刻意保留的 C++ 訊息錯誤
- C++ 版封存：等 IMB-C 實際改用 Rust 版一段時間後再決定
