# IMB-C 整合規格（iat 0.5.0）

給 IMB-C（v1.4 起）呼叫 Image Analysis Tool 驗證轉檔結果用。對象版本：iat 0.5.0、JSON `schema_version` 1.1（0.5.1 介面相同，只修正 PSD／PSB 的 RLE 列長誤判；IMB-C 要求的最低版本是 0.5.1）。完整欄位定義見 `cli-json-contract.md`，判定規則見 `decisions.md`。IMB-C 端的實作見 <https://github.com/Kaido-MSVC/IMB-C>。本文提到的 `imbc-v1.4-codex-guide.md` 是 IMB-C v1.4 開發時的內部整合說明，未收錄於公開 repo。

## 1. 呼叫方式

```text
<iat 資料夾>\iat.exe compare <原檔> <IMB-C 輸出> --report-json <新檔路徑> [--report-md <新檔路徑>] [--limits 名稱=值 ...]
```

- `iat.exe` 與 `iat-worker.exe` 放在同一個資料夾；靜態 CRT，不需安裝、不需 VC++ 執行階段。
- 兩個輸入都以唯讀開啟（`GENERIC_READ`＋`FILE_SHARE_READ`），分析期間其他程式不能寫入或刪除它們。
- 報告路徑必須是**不存在的新檔**，而且不能與任一輸入相同（包括硬連結）；否則開始前以退出碼 5 拒絕，不寫任何檔案。建議每次用 IMB-C 自己的暫存資料夾加唯一檔名。
- 標準輸出只在 `--stdout json|md` 時使用；診斷訊息在 stderr。IMB-C 以子程序呼叫即可，與目前呼叫 ImageMagick 的方式相同。
- 解碼在 Job Object 限制的子程序中進行，預設上限：`job_commit_mib=1024`、`buffer_mib=64`、`temp_gib=8`、`deadline_s=600`。

## 2. 退出碼

| 碼 | 意義 | IMB-C 建議處理 |
|---|---|---|
| 0 | 嚴格一致：兩檔完整解碼，樣本逐值相等（含 A=0 底下的 RGB），色彩描述確認相同 | 驗證通過 |
| 1 | 可確認的差異：樣本不同、位深或樣本模型改變、尺寸或方向不同 | 驗證失敗；只標示與回報，不刪除任何檔案（`imbc-v1.4-codex-guide.md` §3.3） |
| 2 | 不可判定或超出支援範圍：樣本相等但色彩未確認、Alpha 語意不同、AVIF metadata 不符無損、PSD 沒有合成影像、不支援的格式或樣本模型 | 依 JSON 原因決定（見 §3） |
| 3 | 解碼或結構失敗（截斷、損壞） | 驗證失敗 |
| 4 | 資源上限、期限或取消（**未檢查**，不是失敗） | 有 `resource_need` 時以建議值重試一次（§4） |
| 5 | 參數、I/O、worker 或環境錯誤 | 視為工具錯誤；看 stderr |

多種狀態並存時優先序為 5 > 4 > 3 > 1 > 2 > 0。報告寫入失敗時，以**程式的退出碼**為準（報告內的 `exit.code` 不含寫入失敗）。

## 3. 要讀的 JSON 欄位

| 欄位 | 用途 |
|---|---|
| `exit.code`、`exit.reason` | 與退出碼相同 |
| `strict.result`、`strict.reasons[]` | `match`／`mismatch`／`indeterminate`／`not_evaluated` 與原因代碼（`original:`／`output:` 字首表示哪一側） |
| `samples.status`、`samples.pixels_differing` | `equal`／`differs`／`not_comparable`／`incomplete`／`not_compared`；十進位字串 |
| `color.status`、`icc.status` | 色彩與 ICC 的比較結果（`equal`、`differs`、`unverified`、`unknown`；ICC 另有 `missing_on_output` 等） |
| `original.decode_status`、`output.decode_status`、`*.decode_status_reason` | 各側是否完整解碼；`not_checked`＋`resource_limit` 代表被上限擋下 |
| `original.resource_need`、`output.resource_need` | 被預估擋下時的需要量與建議參數（§4） |
| `output.avif.lossless_metadata`、`output.avif.lossless_metadata_reasons[]` | AVIF 的 metadata 是否符合無損的必要條件（`consistent`／`inconsistent`） |
| `output.avif.pixel_verified` | `true`＝逐樣本實證無損；`false`＝樣本不同；`null`＝沒有比較 |
| `original.psd.composite`、`original.psd.composite_uniform` | PSD 合成影像狀態（`saved`／`not_saved`／`flattened`／`flag_absent`） |
| `issues[].code` | 穩定代碼，見下 |

常用問題代碼：`samples_differ`、`bit_depth_changed`、`sample_model_changed`、`orientation_differs`、`icc_missing_on_output`、`icc_differs`、`color_unverified`、`alpha_channel_removed`、`alpha_semantics_differ`、`avif_not_lossless_by_metadata`、`avif_pixel_verified_lossless`、`psd_composite_not_saved`、`psd_composite_flag_absent`、`psd_composite_uniform`、`resource_limit`、`truncated`、`structure_error`。

建議判斷（「無損轉檔已實證」）：`exit.code == 0`；或 `samples.status == "equal"` 且 `strict.reasons` 只有色彩相關原因（IMB-C 依自己的色彩政策決定）。AVIF 另要求 `output.avif.pixel_verified == true`。**不要**只看 `lossless_metadata == "consistent"`：同樣的 metadata 也可能是有損編碼（實測 identity／4:4:4 品質 50 的檔案 metadata 相同、像素不同）。

## 4. 大圖與 `--limits`

預估超過上限時結果為「未檢查」（退出碼 4），該側的 `resource_need` 寫出需要量與建議值，例如：

```json
"resource_need": {"limit": "job_commit_mib", "needed_bytes": "2027747242", "allowed_bytes": "1006632960",
                  "current_value": 1024, "suggested_value": 2048, "suggested_argument": "--limits job_commit_mib=2048"}
```

IMB-C 可把 `suggested_argument` 原樣附加後重跑一次；`suggested_value` 為 null 表示超過可設定的最大值。沒有 `resource_need` 的退出碼 4（Job 上限、期限、取消）不要自動放大重試。

實測參考（i5-8600K、單執行緒）：

| 輸入 | 需要的參數 | 時間／峰值 |
|---|---|---|
| PSD／PSB 2976×4175 8-bit RGB（93–423 MB） | 預設即可（逐列解碼，不需暫存） | 約 1 s，worker 峰值 working set 約 6 MB |
| AVIF 12000×8637 8-bit RGB 無損（110 MB）對 PNG | `--limits job_commit_mib=2048`（預估 1934 MiB；實測峰值 1707 MiB） | 比對 21 s，暫存 297 MiB |
| 預設上限下的 AVIF 大致上限 | 8-bit RGB 約 50 Mpx | — |
| JXL／WebP | 預設約 69 Mpx RGB JXL、約 100 Mpx WebP（`usage.md`） | — |

暫存放在系統暫存資料夾，可用 `--temp-dir` 指定；額度 `temp_gib`。

## 5. 產生無損 AVIF 的 ImageMagick 參數

以 IMB-C 1.3.3 的 ImageMagick 7.1.2-35（libheif 1.23.7＋libaom 3.15.2）實測：

| 參數 | iat 判定 |
|---|---|
| **`-quality 100 -define heic:speed=6 -define heic:cicp=1/13/0/1`** | metadata 符合、**像素驗證無損**（1024×512 裁切與 12000×8637 全尺寸皆是） |
| IMB-C 現行：`-quality 90 -define heic:speed=6 -define heic:chroma=444 -define heic:cicp=1/13/6/1` | `matrix_not_identity`，不解碼，退出碼 2 |
| `-quality 100` 加 `heic:cicp=1/13/6/1` | 同上（quality ≥ 100 雖然開啟 AV1 無損，但 BT.601 的 RGB→YCbCr 不可逆） |

說明：

- `heic:cicp` 是「primaries/transfer/matrix/full_range」。無損的關鍵是 **matrix = 0（identity）**；`1/13` 是 BT.709 primaries 與 sRGB transfer，來源色彩不同時換成對應的 P/T，matrix 維持 0、range 維持 1。
- matrix 為 0 時 ImageMagick 會自動用 4:4:4（`coders/heic.c` 的 `HasHEICIdentityCicp`），也可以明確加 `-define heic:chroma=444`；不可用 4:2:0／4:2:2。
- quality 必須 ≥ 100（ImageMagick 在此時呼叫 `heif_encoder_set_lossless`）。`heic:speed=6` 只影響速度，實測全尺寸編碼 167 s。
- 有嵌入 ICC 的來源，IMB-C 目前不加 `heic:cicp`，實際矩陣由 ImageMagick／libheif 預設決定；建議一律明確指定 identity。
- 實測時 magick 以 IMB-C 自己的私有環境啟動（`engine.rs` 的 `run()`：清空環境、PATH 只含 runtime 與 System32、`MAGICK_HOME`／`MAGICK_CONFIGURE_PATH`／`MAGICK_CODER_MODULE_PATH` 指向 runtime、暫存與 profile 指到工作資料夾）；直接從 runtime 資料夾以一般環境執行時，coder 模組載入失敗。

## 6. PSD／PSB 注意事項

- iat 比較的是檔案中存的合成影像，與 IMB-C 讀 `psd:-[0]` 相同。7 個 PSD、2 個 PSB 的真實樣本，iat 解出的合成影像與 IMB-C ImageMagick 解出的結果逐值相同。
- 0x0421 標示沒有真正合成影像：iat 不比較（`psd_composite_not_saved`，退出碼 2），與 IMB-C 的拒絕一致。沒有 0x0421（CLIP Studio 等匯出的 PSD）：只警告，照常比較。
- 合成透明度：Photoshop 的合成色彩已與白色混合（`alpha=matted_white`），而 IMB-C 讀取時會反混合並升到 16-bit，所以 PSD 對 IMB-C 帶 Alpha 的輸出不會樣本相等。
- IMB-C 1.3.3 拒絕 PSB；iat 支援 PSB，IMB-C 支援之前只能用於 inspect。

## 7. 待確認：PSD → PNG 的 ICC

直接執行 `magick "psd:<檔>[0]" -depth 8 -define png:preserve-iCCP=true <out>.png` 時，PSD 的嵌入 ICC（resource 0x040F，測試樣本為 3144 bytes）**沒有**寫進 PNG，只有 cHRM；樣本本身相同。iat 因此回報 `color=unverified`（原檔 ICC 對輸出只有 cHRM）。這只是直接呼叫 magick 的結果，**尚未驗證** IMB-C 完整流程（經 MIFF 中介、另有 ICC 處理）是否也遺失 ICC。

2026-11-17 補充：原因已查明，CSP 檔案在 ICC 之前有長度 0 的 1006／1045 資源，ImageMagick 7.1.2-35 讀到時會停止解析其後的資源；套用 IMB-C 的正規化（`psd_compatible_source`）後，模擬流程中 ICC 有保留。詳見 `imbc-v1.4-codex-guide.md` §4.2、§4.3。

確認方式：用 IMB-C 把帶 ICC 的 PSD 轉成 PNG，再執行 `iat compare <psd> <png> --report-json r.json`：`icc.status` 為 `missing_on_output` 或 `color.status` 不是 `equal`，就表示 ICC 沒有保留。

## 8. 重現測試

```powershell
# iat 專案內；樣本資料夾與 IMB-C runtime 只讀，前後各算一次 SHA-256，輸出只寫到最後一個參數
py -3.13 -I tests/e2e/run_real_samples.py build/release/bin <樣本資料夾> <IMB-C>\target\release\ImageMagick\magick.exe build/real --full-avif
```
