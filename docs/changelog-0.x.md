# 變更紀錄（0.x 完整版）

本檔是 0.5.1 以前的 `CHANGELOG.md`，1.0.0 起移到這裡保存；新版紀錄見根目錄 `CHANGELOG.md`（D35）。

格式參考 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)；版本號依 0.x 慣例，次版號代表新增能力。決定編號（D、S）見 `docs/decisions.md`。

## [0.5.1] - 2026-11-17

決定見 `docs/decisions.md` D33。

### 修正
- PSD／PSB 的 RLE（PackBits）列長檢查過嚴：ImageMagick 寫出的 RLE（每列結尾有 0x80 空指令、literal 拆段）是合法的 PackBits，0.5.0 卻以 `structure_error`（`rle_row_too_long`）拒絕。現在 0x80 可出現在任何位置，也接受非最佳化的編碼；每列仍必須剛好用完 byte count 並解出剛好的列寬，run 超出該列或被 byte count 截斷仍為 `structure_error`（`rle_row_invalid`）。列解碼改為固定緩衝分段讀入，記憶體用量不變。
- 測試：PackBits 單元測試新增列尾／列中 0x80、非最佳化編碼、分段輸入、超出該列與 byte count 不符；以 IMB-C 的 ImageMagick 寫出 RLE 的 PSD 與 PSB 實測比對為 `match`。

## [0.5.0] - 2026-11-17

決定見 `docs/decisions.md` D24–D31。

### 新增
- **PSD／PSB**（自寫唯讀 parser，不依賴外部函式庫）：讀取合成影像（merged image data），Raw 與 RLE（PackBits）逐列解碼，不需要暫存檔；PSB 的 64-bit 區段長度與 32-bit RLE 列長。支援 Bitmap、Grayscale、Indexed（含透明索引）、RGB，8／16-bit；CMYK、Lab、Multichannel、Duotone 與 32-bit 浮點會辨識並回報 `unsupported_sample_model`（退出碼 2），ZIP 壓縮回報 `compression_not_supported`。
- PSD 合成影像有效性判定（`psd.composite`）：Version Info（0x0421）標示沒有真正合成影像時不比較（`psd_composite_not_saved`，退出碼 2）；有圖層但沒有 0x0421 時只警告（`psd_composite_flag_absent`），不降級；合成影像整張單一顏色時 strict 最多 indeterminate（`psd_composite_uniform`）。
- PSD 合成透明度（圖層數為負）記為 `matted_white`（Photoshop 與白色混合），strict 最多 indeterminate；另一側為直式 Alpha 時列 `alpha_semantics_differ`。
- **AVIF**（libavif 1.4.2 ＋ aom 3.15.2，與 IMB-C 同版）：8／10／12-bit、Alpha、4:4:4 identity 與 4:0:0。報告分開列出兩種無損判定：`avif.lossless_metadata`（metadata 是否符合無損的必要條件）與 `avif.pixel_verified`（compare 時逐樣本實證）。metadata 不符（4:2:0／4:2:2、matrix 不是 identity、limited range、premultiplied Alpha）時不解碼，`avif_not_lossless_by_metadata`，退出碼 2，與有損 WebP 一致。直接讀取解碼平面，不經 YUV→RGB 轉換。`irot`／`imir` 對應為有效方向。
- 自有 ISOBMFF box 掃描（box 邊界、截斷、ftyp brand、colr nclx）。
- JSON `schema_version` 1.1：`original`／`output` 新增 `psd`、`avif` 物件；新問題代碼見 `docs/cli-json-contract.md`。
- 預估超過記憶體上限而未檢查時，訊息寫出需要的位元組數與建議的 `--limits` 參數（例如 `--limits job_commit_mib=2048`），JSON 的 `original`／`output` 新增 `resource_need` 欄位（D32）。預設上限不變。
- 測試：PSD／PSB／AVIF 測試圖與配對（`gen_fixtures.py`、`gen_codec_fixture avif`），真實樣本檢查腳本 `tests/e2e/run_real_samples.py`。
- 新建本檔。

### 變更
- 工具集改鎖 **MSVC 14.52**（14.51 已從本機移除）。`build.ps1` 與 `vcpkg-env.cmd` 載入 vcvars 後驗證實際的 `VCToolsVersion` 與 `cl.exe`，不符即退出碼 1（D24）。
- 語言標準 C++20 → **C++23**，只升標準、不採用新特性；`CMAKE_CXX_SCAN_FOR_MODULES OFF`（D25）。
- 第三方清單產生器支援 `vcpkg_from_git` 的 port（以鎖定 commit 找封存檔），並以 SPDX 授權範本核對授權條款文字。

### 修正
- `tiff_scan.cpp` 標籤表在 MSVC 14.52 的 C4244：值都在 uint16 範圍內、不會截斷，改用聚合型別讓編譯器在編譯期檢查（D26）。
- 測試圖產生器：libjxl 編碼 8／16 以外的整數位深時，原本被預設的 `JXL_BIT_DEPTH_FROM_PIXEL_FORMAT` 重新縮放，改為 `FROM_CODESTREAM`，真值才會原樣寫入（D29）。只影響測試圖，不影響工具本身。
- `docs/dependencies.md` 的 libjxl port patch 清單補齊（先前漏記兩個）。

## [0.4.1] - 2026-11-08

0.4.0 交付後的審查修正（D20–D23）。

### 修正
- PNG 文字 chunk 關鍵字依規格以 Latin-1 轉 UTF-8 顯示（雜湊仍用原始位元組）；JSON 以 replace 輸出、Markdown 過濾無效 UTF-8，worker 不再因檔案中的字串序列化失敗（D20）。
- PNG eXIf／WebP EXIF 超過 `metadata_item_mib` 時，方向記為 unknown 並列 `orientation_unknown`，strict 不給 match（D21）。
- 殘留暫存檔只清理檔名中間為合法 UUID 的檔案（D22）。
- 輸入雜湊每 4 MiB 檢查取消旗標；雜湊中取消即停止，報告 `cancelled`（退出碼 4）。
- 報告寫入失敗時，程序退出碼 5 優先於報告內記錄的退出碼，原因見 stderr。

## [0.4.0] - 2026-11-08

第一個交付版（階段 4）：zip 免安裝、靜態 CRT、MIT 授權（D15–D19）。

### 新增
- 第三方清單與授權材料依 vcpkg copyright／SPDX 與 linker map 實際產生；封包腳本 `package.ps1`。
- 支援矩陣逐列對照測試；輸入唯讀與報告不覆寫的驗證。
- 使用說明 `docs/usage.md`（封包內為 `USAGE.md`）。

## 0.3.0-stage3 - 2026-11-07

- JPEG XL（libjxl 0.12.0）與 WebP 靜態無損（libwebp 1.6.0）；整圖型解碼器經依座標寫入的暫存；跨格式色彩等價規則（D12）；IMB-C v1.3.0 輸出配對。

## 0.2.0-stage2 - 2026-11-07

- TIFF／BigTIFF（libtiff 4.7.2）、Adam7 交錯 PNG、有額度的安全暫存。

## 階段 0–1 - 2026-11-07

- 建置骨架與依賴鎖定、可行性探針；PNG 嚴格比較 CLI、worker 與 Job Object 資源控制。（這兩個階段沒有獨立的版本號。）
