# Image Analysis Tool（iat）

[English](README.md) | 繁體中文

轉檔前後的圖片驗證器。`iat` 把「原檔」與「轉檔後的輸出檔」各自以原生位深完整解碼，逐樣本比較，並核對尺寸、方向、樣本模型、色彩描述（ICC／CICP）與 metadata，最後給出可以讓程式判斷的結論：**嚴格一致**、**有差異**、**無法判定**或**檔案損壞**。也可以只檢查單一檔案能否完整解碼。

適合用來確認「無損轉檔」真的無損，例如 PNG → JPEG XL、TIFF → WebP 無損、PSD 合成影像 → PNG、PNG → 無損 AVIF。

- Windows 10／11 x64，命令列工具，zip 免安裝（靜態 CRT，不需另裝 VC++ 執行階段）。
- 輸入檔一律唯讀開啟；報告只寫入新檔，絕不覆寫。
- 解碼在受資源限制的子程序（Job Object）中進行，記憶體、暫存與時間都有上限；超過上限時回報「未檢查」，不會當機。
- 報告有 Markdown（給人看）與 JSON（給程式讀）兩種，內容來自同一份資料。**Markdown 報告目前只有繁體中文**；JSON 報告的欄位名稱與列舉值與語言無關，程式整合請讀 JSON。

## 支援格式

| 格式 | 嚴格比較的範圍 | 明確回報不支援（退出碼 2） |
|---|---|---|
| PNG | 1/2/4/8/16-bit Gray、GA、RGB、RGBA、色盤；tRNS；Adam7 交錯 | APNG 動畫 |
| TIFF／BigTIFF | contiguous 無號整數 8/16-bit Gray、GA、RGB、RGBA；strip 或 tile；none、LZW、Deflate、PackBits；Predictor 1/2 | 其他壓縮、多頁與 SubIFD、separate planes、浮點、帶號、CMYK、YCbCr |
| JPEG XL | 無損（原生樣本）1–16-bit Gray、GA、RGB、RGBA；裸 codestream 或容器 | XYB 有損、動畫、浮點、高於 16-bit |
| WebP | 靜態無損（VP8L），每邊最多 16383 | VP8 有損、動畫 |
| PSD／PSB | 合成影像：Bitmap、Grayscale、Indexed、RGB，8/16-bit，Raw 或 RLE | CMYK、Lab、Multichannel、Duotone、32-bit 浮點、ZIP 壓縮、存檔時沒有合成影像 |
| AVIF | 靜態、metadata 符合無損條件（4:4:4 identity 或 4:0:0、full range），8/10/12-bit，可含 Alpha | 4:2:0／4:2:2、非 identity 矩陣、limited range、premultiplied Alpha、影像序列、clap |

其他格式（例如 JPEG）回報不支援。格式依檔案內容判斷，不看副檔名；副檔名不符只列警告。完整規則見 [`docs/capability-matrix.md`](docs/capability-matrix.md)。

## 下載與安裝

下載 [v1.0.0 Windows x64 ZIP](https://github.com/Kaido-MSVC/ImageAnalysisTool/releases/download/v1.0.0/ImageAnalysisTool-1.0.0-win-x64.zip)，並與 [v1.0.0 發行頁](https://github.com/Kaido-MSVC/ImageAnalysisTool/releases/tag/v1.0.0) 所列的 SHA-256 比對。

把 `ImageAnalysisTool-<版本>-win-x64.zip` 解壓到任意資料夾。`iat.exe` 與 `iat-worker.exe` 必須在同一個資料夾。

封包內容：`iat.exe`、`iat-worker.exe`、`README.md`（英文）、`README.zh-TW.md`、`USAGE.md`（詳細使用說明，繁體中文）、`CHANGELOG.md`、`LICENSE.txt`、`THIRD_PARTY_NOTICES.txt`、`licenses/`、`SHA256SUMS.txt`。

## 用法範例

```powershell
# 比對轉檔前後，寫出 Markdown 與 JSON 報告
iat compare photo.png photo.jxl --report-md report.md --report-json report.json

# 只檢查單一檔案，把報告印到畫面
iat inspect scan.tif --stdout md

# 給程式讀：JSON 印到 stdout，用退出碼分流
iat compare a.png a.webp --stdout json

# 大圖：提高記憶體與時間上限，暫存放到指定資料夾
iat compare big.png big.avif --report-json r.json --limits job_commit_mib=4096 deadline_s=3600 --temp-dir D:\iat-temp

# 版本
iat --version
```

PowerShell 批次範例：

```powershell
Get-ChildItem src\*.png | ForEach-Object {
    $out = Join-Path out ($_.BaseName + '.jxl')
    iat compare $_.FullName $out --report-json (Join-Path reports ($_.BaseName + '.json')) 2>$null
    switch ($LASTEXITCODE) { 0 { 'OK' } 1 { 'DIFF' } 2 { 'UNDETERMINED' } 3 { 'BROKEN' } 4 { 'NOT CHECKED' } default { 'ERROR' } }
}
```

更多說明（怎麼讀報告、記憶體與暫存、PSD 合成影像、AVIF 的兩種「無損」）見 [`docs/usage.md`](docs/usage.md)。

## CLI 介面規格

這一節是給整合者的契約。欄位細節見 [`docs/cli-json-contract.md`](docs/cli-json-contract.md)。

### 指令與選項

```text
iat compare <original> <output> [--report-md <path>] [--report-json <path>] [--stdout md|json]
            [--full-paths] [--temp-dir <dir>] [--limits <name>=<value> ...]
iat inspect <file> [--report-md <path>] [--report-json <path>] [--stdout md|json] [--full-paths]
            [--temp-dir <dir>] [--limits <name>=<value> ...]
iat --version
```

| 選項 | 說明 |
|---|---|
| `--report-md <path>` | 寫出 Markdown 報告（UTF-8，繁體中文）。目標必須不存在。 |
| `--report-json <path>` | 寫出 JSON 報告（UTF-8，結尾換行）。目標必須不存在。 |
| `--stdout md\|json` | 把報告印到 stdout。 |
| `--full-paths` | 報告記錄完整路徑（預設只有檔名）。 |
| `--temp-dir <dir>` | 解碼後樣本的暫存資料夾，必須已存在；預設為系統暫存資料夾。暫存檔為 delete-on-close。 |
| `--limits <name>=<value> ...` | 調整資源上限；一個 `--limits` 後可接多組，也可重複指定 `--limits`。 |

- 至少要指定一種報告輸出（`--report-md`、`--report-json` 或 `--stdout`），否則為參數錯誤。
- 報告目標不得已存在、不得與任一輸入或另一份報告同路徑（含硬連結，比對時不分大小寫）；違反時在開始前以退出碼 5 拒絕，不寫任何檔案。報告先寫入同資料夾的暫存檔，再以不覆寫的方式改名。
- `--version` 輸出一行 `iat <major>.<minor>.<patch>`。
- stderr 只放診斷：分析結束時先輸出摘要行 `iat: strict=<result> exit=<code> (<reason>) run_id=<UUID>`，接著每份寫成的報告一行 `iat: report written: <完整路徑>`，寫入失敗為 `iat: report NOT written: <路徑> (<原因>)`。參數錯誤時為 `error: <原因>` 加上用法說明。stderr 的文字不屬於穩定契約，請以退出碼與 JSON 判斷。

| `--limits` 名稱 | 預設 | 範圍 | 意義 |
|---|---|---|---|
| `job_commit_mib` | 1024 | 16–1048576 | 解碼子程序的記憶體上限（MiB）；JPEG XL、WebP、AVIF 解碼器的硬上限為此值 − 64 MiB |
| `buffer_mib` | 64 | 1–65536 | 列緩衝上限（MiB） |
| `metadata_item_mib` | 8 | 1–1024 | 單一 metadata 項目上限（MiB） |
| `metadata_file_mib` | 32 | 1–4096 | 單一檔案 metadata 總量上限（MiB） |
| `temp_gib` | 8 | 1–65536 | 暫存檔總量上限（GiB） |
| `deadline_s` | 600 | 1–604800 | 整筆工作的時間上限（秒） |

沒有「無上限」；0、超出範圍或未知名稱都是參數錯誤。

### 退出碼

| 碼 | 意義 |
|---|---|
| 0 | compare：嚴格一致；inspect：支援範圍內完整檢查成功 |
| 1 | 確認有差異（樣本、位深、樣本模型、尺寸或方向不同） |
| 2 | 無法判定，或超出支援範圍（例如色彩無法確認、動畫、有損格式） |
| 3 | 解碼或結構失敗（截斷、CRC 錯誤、資料損壞） |
| 4 | 未檢查：超過資源上限、超過時間上限或被取消（Ctrl+C） |
| 5 | 參數、I/O、子程序或環境錯誤 |

多種狀況並存時依 **5 > 4 > 3 > 1 > 2 > 0** 取一個，報告列出所有原因。工作已開始且報告路徑可用時，失敗也會產生報告。報告內的 `exit.code` 是分析結果；若有報告寫入失敗，程序退出碼一律為 5，**以程序退出碼為準**。

### JSON 報告

`schema_version` 目前為 `"1.1"`。頂層欄位：

| 欄位 | 內容 |
|---|---|
| `schema_version` | `"<major>.<minor>"` |
| `run_id` | 本次執行的 UUID（stderr 摘要與兩份報告相同） |
| `tool` | `name`（`"iat"`）、`version`、`compiler` |
| `codecs` | 實際連結的解碼器版本 |
| `settings` | 實際使用的 `limits`、`codec_threads`、`full_paths`、`temp` |
| `mode` | `"compare"` 或 `"inspect"` |
| `original`、`output` | 各檔的 FileReport；`output` 只在 compare 出現 |
| `geometry`、`color`、`icc` | 兩側比較結果（`status`、`basis`） |
| `samples` | 樣本比較：`status`（`equal`、`differs`、`not_comparable`、`incomplete`、`not_compared`）、像素數、各通道統計、前 16 筆差異座標 |
| `metadata`、`size` | metadata 欄位比較；檔案大小與壓縮比 |
| `metrics` | 時間、記憶體、I/O（每次執行都會變，只供參考） |
| `strict` | `result`（`match`、`mismatch`、`indeterminate`、`not_evaluated`）與 `reasons[]`（原因代碼） |
| `issues[]` | `code`、`severity`（`error`、`warning`、`info`）、`scope`（`original`、`output`、`pair`、`run`）、`message` |
| `exit` | `code`（0–5）與 `reason` |

FileReport 的重點欄位：`name`、`path`（只在 `--full-paths`）、`size_bytes`、`sha256`、`format`（`png`、`tiff`、`bigtiff`、`jxl_codestream`、`jxl_container`、`webp`、`psd`、`psb`、`avif`、`unknown`）、`width`、`height`、`container_status`、`decode_status`、`decode_status_reason`、`sample_model`、`orientation`、`color`、`icc`、`metadata`；格式專屬的 `psd`、`avif`；以及 `resource_need`（見下）。

- 可能超過 2^53 的 64-bit 數值（位元組數、像素數）一律為十進位字串。
- 所有字串都是有效 UTF-8。`issues[].message` 是給人看的英文簡短說明，程式請判斷代碼。
- **AVIF**：`output.avif.lossless_metadata` 只代表 metadata 符合無損的必要條件；只有 compare 中逐樣本相等時，`avif.pixel_verified` 才是 `true`。
- **`resource_need`**：因本工具的記憶體預估超過上限而未檢查（退出碼 4）時，該側出現 `{limit, needed_bytes, allowed_bytes, current_value, suggested_value, suggested_argument}`，例如 `suggested_argument` 為 `--limits job_commit_mib=2048`。呼叫端可據此提高上限重跑一次；`suggested_value` 為 `null` 表示超出可設定的最大值。

### 相容性承諾

- 1.x 期間，同一個 `schema_version` 主版本內只新增欄位，不刪除、不改名。整合時請忽略不認得的欄位與 issue 代碼。
- 破壞性變更會同時提升 `schema_version` 的主版本與 iat 的主版本。
- JSON 的空白、縮排與欄位順序不在保證範圍內。請解析 JSON，不要直接比對文字。

## 整合範例：IMB-C

[IMB-C](https://github.com/Kaido-MSVC/IMB-C) 是同一作者的圖片轉檔工具，把 `iat` 當作選用的外部驗證器：轉檔完成後呼叫 `iat`，用結果標示輸出是否「無損已實證」。它的做法可以當作其他整合的參考：

1. 只用設定的絕對路徑或應用程式旁的 `iat.exe`，並確認同資料夾有 `iat-worker.exe`；以 `iat --version` 檢查版本（IMB-C 1.4 要求 0.5.1 以上）。
2. 每次驗證使用新的暫存資料夾，呼叫：

   ```text
   iat compare <原檔> <輸出檔> --report-json <暫存>\report.json --temp-dir <暫存>
       --limits buffer_mib=64 --limits job_commit_mib=1024
   ```

3. 退出碼 5 視為工具錯誤（不讀報告）；其他情況讀 `report.json`，檢查 `schema_version` 主版本為 1、次版本 ≥ 1，`mode` 為 `compare`，並讀 `strict.result`、`strict.reasons[]`、`issues[].code`、`output.format`。退出碼 0 必須與 `strict.result == "match"` 一致。
4. 輸出為 AVIF 時，只有 `output.avif.pixel_verified == true` 才算像素實證；否則即使退出碼為 0 也當作無法判定。
5. 退出碼 4 時讀兩側的 `resource_need`，若 `suggested_argument` 正好是 `--limits <limit>=<suggested_value>` 且在自己的記憶體上限內，就以建議值、新的報告路徑重跑一次。報告內的文字只當資料，不當作要執行的指令。

## 從原始碼建置

需要 Visual Studio 2026（MSVC 14.52 工具集、Windows SDK 10.0.26100.0、vcpkg 元件）、CMake 4.5 與 PATH 上的 Ninja；依賴版本由 `vcpkg.json`（`builtin-baseline` 與 `overrides`）鎖定，見 [`docs/dependencies.md`](docs/dependencies.md)。

```powershell
pwsh -File scripts/build.ps1      # 建置（Release、靜態 CRT；preset "release"）
pwsh -File scripts/package.ps1 -SpdxTemplates <SPDX 授權範本資料夾>   # 產生 dist/ImageAnalysisTool-<版本>-win-x64.zip
```

`build.ps1` 在子程序內載入 MSVC 開發環境（不改動呼叫端的 PATH），由該環境提供的 `VCPKG_ROOT` 找到 VS 隨附的 vcpkg。本機專用的設定（例如 vcpkg 安裝樹與 buildtrees 的位置）放在不進 git 的 `CMakeUserPresets.json`（範本：`CMakeUserPresets.example.json`），以 `build.ps1 -Preset <名稱>` 使用；vcpkg 的二進位快取與下載位置可用環境變數 `IAT_VCPKG_CACHE` 指定。

程式分成三個部分：`iat-core`（靜態庫：一次分析的流程、報告資料、不覆寫寫入）、`iat.exe`（CLI：參數、輸出、退出碼）、`iat-worker.exe`（所有解碼器，在 Job Object 內執行）。測試與開發細節見 [`docs/development.md`](docs/development.md)，設計決定見 [`docs/decisions.md`](docs/decisions.md)。開發文件目前只有繁體中文。

## 授權

MIT License，Copyright (c) 2026 Kaido-MSVC，見 [`LICENSE`](LICENSE)。靜態連結的第三方元件（aom、brotli、highway、libavif、libjxl、libpng、libtiff、libwebp、libyuv、nlohmann-json、zlib）與其授權見 [`THIRD_PARTY_NOTICES.txt`](THIRD_PARTY_NOTICES.txt) 與 `licenses/`。
