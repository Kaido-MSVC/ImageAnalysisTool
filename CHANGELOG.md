# 變更紀錄

格式參考 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)，版本號依 [Semantic Versioning](https://semver.org/lang/zh-TW/)。CLI 參數、退出碼與 JSON `schema_version` 1 的既有欄位維持和 iat 1.x 相容。決定編號（D-R）見原始碼 repo 的 `docs/decisions.md`，和 1.0.0 的差異總表見 `docs/differences.md`。

## [2.0.0] - 2026-11-22

以 Rust 重寫 iat 1.0.0（C++）。命令列參數、退出碼與 JSON 報告（`schema_version` 仍為 1.1）都和 1.0.0 相同，呼叫 1.0.0 的程式（例如 IMB-C）不用修改就能換用。

### 新增
- 桌面 GUI `iat-gui.exe`（Slint）：選檔或拖放後比對或單檔檢查；結果、問題清單、影像檢視（原檔、輸出、差異熱圖、像素值）；匯出和 CLI 相同的 Markdown 與 JSON 報告。
  - 介面有繁體中文、英文與日文，會記住上次選擇的語言（`%APPDATA%\iat\gui-settings.json`）。
  - 某一側因記憶體預估超過 `job_commit_mib` 而未檢查時，可「以建議值重新分析」（只提高這項上限）；建議值超過目前可用的實體記憶體時會提醒。
  - 樣本完全相同、只因色彩描述無法證明等價而判為無法判定時，標題顯示「像素一致」並說明雙方的色彩來源；報告、JSON 與退出碼不變。
  - JPEG XL 色彩列舉值顯示實際的色彩空間名稱，例如 `jxl_enum (sRGB)`。
- 三個執行檔都有程式圖示與版本資訊。

### 變更
- 解碼器全部改用 Rust crate：png、image-webp、jxl（jxl-rs）、rav1d（AV1），TIFF 與 PSD／PSB 照 C++ 版與 libtiff 4.7.2 的行為自行實作。不連結任何 C 或 C++ 函式庫。
- 報告的 `tool.version` 為 `2.0.0`、`tool.compiler` 記 rustc 版本，`codecs` 列出 Rust crate；`decode_path` 與部分訊息文字寫的是 Rust 解碼器的名稱。
- 讀檔頭、取檔案大小、雜湊時的 I/O 錯誤，stdout 寫入失敗與系統亂數失敗，改為回報正確的錯誤（退出碼 5），不再當成預設值繼續（D-R13）。
- 分析仍在 `iat-worker.exe` 的 Job Object 內執行。超過 `job_commit_mib` 而配置失敗時，和 1.0.0 一樣只把那一側判為 `resource_limit`，報告其餘部分照常產生（D-R02）。

### 已知差異
- 記憶體用量和 1.0.0 不同：接近 `job_commit_mib` 上限時，同一組檔案可能一個版本完成、另一個版本未檢查。事前預估擋下的情況與建議值相同。
- 多圖層的 JPEG XL 靜止影像不會出現 `layered_image`（info）；非 XYB 有損 JPEG XL 的 `mae`、`rmse` 可能略有不同。
- 部分 libpng、libtiff 的警告與 AVIF 影像序列的處理不同（目前的測試圖都沒有觸發）。完整清單見 `docs/differences.md`。

### 驗證
- 比對腳本 `tests/compare/compare_iat.py` 以 C++ 版 1.0.0 為基準，所有案例的退出碼、JSON（含鍵順序與縮排）與 Markdown 一致，只有 `docs/differences.md` 第 1 節列出的允許差異。
- 封包前檢查三個執行檔都不匯入 VC++ 或 UCRT 的執行階段 DLL（靜態 CRT）。
