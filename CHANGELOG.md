# 變更紀錄

格式參考 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)，版本號依 [Semantic Versioning](https://semver.org/lang/zh-TW/)：1.x 期間 CLI 參數、退出碼與 JSON `schema_version` 1 的既有欄位維持相容（只新增欄位，不刪除也不改名；破壞性變更同時提升 `schema_version` 與 iat 的主版本）。決定編號（D、S）見 `docs/decisions.md`。

## [1.0.0] - 2026-11-19

第一個公開版本。分析行為、CLI 參數、輸出（含 JSON 欄位名稱與結構，`schema_version` 仍為 1.1）與退出碼都與 0.5.1 相同，只有版本字串改變。

### 變更
- 程式結構分成 `iat-core`（靜態庫：一次分析的完整流程、報告資料、不覆寫寫入）與 `iat`（CLI：參數、輸出、退出碼）；解碼器仍只在 `iat-worker.exe` 的 Job Object 內執行（D34）。版本號只來自 CMake `project(VERSION)`，由 `iat-core` 提供給 CLI 的 `--version` 與報告。
- README 改為通用的「轉檔前後圖片驗證器」說明，內含 CLI 介面規格與整合範例；原 README 移到 `docs/development.md`（D35）。
- zip 封包加入 `README.md` 與 `CHANGELOG.md`，封包前檢查執行檔沒有匯入 VC++／UCRT 執行階段 DLL（靜態 CRT）。
- `README.md` 改為英文主版，繁體中文版為 `README.zh-TW.md`（zip 一併收錄）；註明 Markdown 報告目前只有繁體中文，JSON 報告與語言無關。
- 相容性承諾明確化：JSON 的空白、縮排與欄位順序不在保證範圍內，請解析 JSON 而不是比對文字（D36）。
- 建置設定不再含本機路徑：Ninja 依 PATH、vcpkg 依 MSVC 開發環境的 `VCPKG_ROOT`；本機設定改放 `CMakeUserPresets.json`。CLI 回歸比對腳本納入 `tools/regress/`（D36）。

### 驗證
- 以 0.5.1 為基準，62 個 CLI 案例（IMB-C 的呼叫方式、各格式無損與有差異的配對、常見參數錯誤、資源上限與 `resource_need`、真實 PSD／PSB／PNG 樣本）的 stdout、JSON、Markdown 與退出碼逐一比對一致（執行 ID、時間與記憶體量測值除外）。單元測試 42／42、e2e 240／240。

## 0.x 時期摘要（2026-11-07 至 2026-11-17）

完整紀錄見 `docs/changelog-0.x.md`。

- **0.5.1**：PSD／PSB 的 RLE（PackBits）改為接受 ImageMagick 寫出的合法非最佳化編碼（D33）。
- **0.5.0**：新增 PSD／PSB 合成影像（自寫唯讀 parser）與無損 AVIF（libavif＋aom；metadata 判定與像素實證分開）；記憶體預估超過上限時給出建議的 `--limits` 值與 `resource_need` 欄位；JSON schema 1.1；工具集 MSVC 14.52、C++23。
- **0.4.x**：第一個交付版（zip、靜態 CRT、MIT）；第三方清單依 linker map 產生；來自檔案的字串一律轉為有效 UTF-8；EXIF 超過預算時方向記為 unknown；輸入雜湊可取消；報告寫入失敗時程序退出碼 5 優先。
- **0.3**：JPEG XL 無損與 WebP 靜態無損；跨格式色彩等價規則。
- **0.2**：TIFF／BigTIFF、PNG Adam7、有額度的 delete-on-close 暫存。
- **0.1 以前（階段 0–1）**：依賴鎖定與可行性探針；PNG 嚴格比較、worker 子程序與 Job Object 資源控制。
