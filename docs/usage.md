# Image Analysis Tool 使用說明（1.0.0）

`iat` 是 Windows x64 的命令列工具，用來嚴格確認兩個圖檔的像素樣本是否完全相同，例如轉檔前後的原檔與輸出檔。它也可以檢查單一檔案能否完整解碼。輸入檔一律唯讀開啟，報告只寫到新檔。

## 安裝

把 zip 解壓到任意資料夾即可，不需要安裝程式，也不需要另外安裝 Visual C++ 執行階段。`iat.exe` 與 `iat-worker.exe` 必須放在同一個資料夾；實際解碼由 `iat-worker.exe` 在受資源限制的子程序中進行。

需求：Windows 10／11 x64（開發與驗收在 Windows 11 上進行）。

## 基本用法

```powershell
# 比對兩個檔案，產生 Markdown 與 JSON 報告
iat compare 原檔.png 輸出檔.jxl --report-md 報告.md --report-json 報告.json

# 只檢查一個檔案，把 Markdown 報告印到畫面
iat inspect 檔案.tif --stdout md

# 顯示版本
iat --version
```

至少要指定一種報告輸出：`--report-md`、`--report-json` 或 `--stdout md|json`，否則會以參數錯誤結束。路徑可以有空白與中文，含空白時要加引號。

### 選項

| 選項 | 說明 |
|---|---|
| `--report-md <路徑>` | 寫出 Markdown 報告。檔案必須不存在。 |
| `--report-json <路徑>` | 寫出 JSON 報告（UTF-8）。檔案必須不存在。 |
| `--stdout md\|json` | 把報告印到標準輸出。診斷訊息一律在標準錯誤輸出。 |
| `--full-paths` | 報告記錄完整路徑。預設只記檔名。 |
| `--temp-dir <資料夾>` | 解碼後樣本需要暫存時使用的資料夾，必須已存在。預設為系統暫存資料夾。 |
| `--limits 名稱=值 ...` | 調整資源上限，見下表。 |

### 資源上限（`--limits`）

| 名稱 | 預設 | 意義 |
|---|---|---|
| `job_commit_mib` | 1024 | 解碼子程序的記憶體上限（MiB）。JPEG XL、WebP 與 AVIF 的解碼器另有硬上限＝此值 − 64 MiB。 |
| `buffer_mib` | 64 | 工具自己的列緩衝上限（MiB）。 |
| `metadata_item_mib` | 8 | 單一 metadata 項目的上限（MiB）。 |
| `metadata_file_mib` | 32 | 單一檔案 metadata 總量上限（MiB）。 |
| `temp_gib` | 8 | 暫存檔總量上限（GiB）。 |
| `deadline_s` | 600 | 整筆工作的時間上限（秒）。 |

沒有「無上限」的設定；0 或超出範圍會以參數錯誤結束。例如：

```powershell
iat compare 大圖.png 大圖.jxl --report-json r.json --limits job_commit_mib=4096 deadline_s=3600
```

## 退出碼

| 碼 | 意義 |
|---|---|
| 0 | compare：嚴格一致；inspect：支援範圍內完整檢查成功 |
| 1 | 確認有差異（樣本、位深、樣本模型、尺寸或方向不同） |
| 2 | 無法判定，或超出支援範圍（例如色彩無法確認、動畫、有損格式） |
| 3 | 解碼或結構失敗（截斷、CRC 錯誤、資料損壞） |
| 4 | 未檢查：超過資源上限、超過時間上限或被取消 |
| 5 | 參數、I/O、子程序或環境錯誤 |

同時有多種狀況時，依 5 > 4 > 3 > 1 > 2 > 0 取一個，報告會列出所有原因。工作已開始且報告路徑可用時，失敗也會產生報告。

## 怎麼讀報告

Markdown 報告第一段「結論」列出：

- **嚴格結論**：一致、有差異、無法判定或未評估，括號內是主要原因代碼。
- **原檔解碼／輸出檔解碼**：是否完整解碼、容器檢查是否通過。
- **樣本**：相同，或「不同：差異像素數 / 總像素數」。
- **色彩**、**ICC**：兩邊的有效色彩描述與 ICC 是否一致。
- **容量**：兩邊的檔案大小、輸出占比、節省率與壓縮倍數。

後面依序是檔案屬性、差異座標（前幾個差異的位置、通道與數值）、各通道統計、色彩與 metadata、效能與記憶體、問題清單。JSON 報告與 Markdown 使用同一份資料，欄位說明見 `docs/cli-json-contract.md`（原始碼庫內）。

「嚴格一致」的意思是：兩邊依原生位深逐樣本比較全部相等（包括 Alpha 為 0 的像素底下的 RGB），而且色彩描述可確認相同。只要色彩無法確認，即使樣本相同，結論也是「無法判定」（退出碼 2）。

## 支援範圍

| 格式 | 嚴格比較的範圍 | 明確拒絕（退出碼 2） |
|---|---|---|
| PNG | 1/2/4/8/16-bit 的 Gray、GA、RGB、RGBA、色盤；tRNS；Adam7 交錯 | APNG 動畫 |
| TIFF／BigTIFF | contiguous、無號整數 8/16-bit 的 Gray、GA、RGB、RGBA；strip 或 tile；壓縮 none、LZW、Deflate、PackBits；Predictor 1/2 | 其他壓縮、多頁與 SubIFD、separate planes、浮點、帶號、CMYK、YCbCr |
| JPEG XL | 無損（原生樣本）1–16-bit 的 Gray、GA、RGB、RGBA；裸 codestream 或容器；非動畫的多層圖 | XYB 有損、動畫、浮點、高於 16-bit |
| WebP | 靜態無損（VP8L），每邊最多 16383 | VP8 有損、動畫 |
| PSD／PSB | 合成影像（merged image）：Bitmap、Grayscale、Indexed、RGB，8／16-bit，Raw 或 RLE 壓縮；PSB 大檔 | CMYK、Lab、Multichannel、Duotone、32-bit 浮點、ZIP 壓縮；存檔時沒有合成影像（未開「最大相容性」） |
| AVIF | 靜態、metadata 符合無損條件的影像：4:4:4 identity 矩陣或 4:0:0 灰階、full range，8／10／12-bit，可含 Alpha | metadata 不符無損（4:2:0／4:2:2、BT.601 等矩陣、limited range、premultiplied Alpha，不解碼）、影像序列、clap 裁切 |

### PSD／PSB 的合成影像

本工具比較的是檔案裡存的合成影像，不會重新合成圖層。報告的「PSD 合成影像」欄位：

- **有**：檔案的 Version Info 標示這是真正的合成影像，或檔案沒有圖層。
- **沒有**：Photoshop 存檔時沒開「最大相容性」，合成影像不是真正的合成結果。這種檔案不比較（退出碼 2）。
- **無法確認**：有圖層，但檔案沒有 Version Info（例如部分非 Adobe 軟體匯出的 PSD）。照常比較，只列警告。
- 合成影像整張只有一種顏色時另列警告，嚴格結論最多為「無法判定」。

合成影像帶透明度時，Photoshop 會把半透明像素的顏色與白色混合；這種樣本照原值比較，但嚴格結論最多為「無法判定」。

### AVIF 的兩種「無損」

報告分開列出，彼此不互相推論：

- **metadata 顯示無損**：色度 4:4:4（或灰階 4:0:0）、矩陣為 identity、full range、Alpha 不是 premultiplied。這只是**必要條件**：同樣的 metadata 也可能是有損編碼。不符合時不解碼（退出碼 2），因為解碼一定要經過色彩轉換。
- **像素驗證無損**：只有 `compare` 才有，代表 AVIF 的每個樣本都與另一個檔案的原生樣本相同。`inspect` 只能看 metadata，這一欄為「未驗證」。

其他格式（例如 JPEG）回報不支援。完整的支援矩陣與色彩判定規則見原始碼庫的 `docs/capability-matrix.md` 與 `docs/decisions.md`（D4、D12）。

## 記憶體與暫存

- PNG 與 TIFF 依列解碼，記憶體用量與圖片高度無關。
- PNG Adam7、過寬的 TIFF tile 排，以及 JPEG XL、WebP 的整圖解碼，會把解碼後樣本寫入暫存檔。暫存檔在程式結束時自動刪除（包括被強制結束），總量受 `temp_gib` 限制，開始前會先預留。
- JPEG XL 與 WebP 的解碼器需要整張圖的記憶體。預設上限下大約可處理 69 Mpx 的 RGB 或 52 Mpx 的 RGBA JPEG XL，以及約 100 Mpx 的 WebP。更大的圖請調高 `job_commit_mib`；超過時結果為「未檢查」（退出碼 4），不會當機。
- PSD／PSB 依列解碼，不需要暫存，記憶體用量只與寬度有關。
- AVIF 也需要整張圖的記憶體（約每樣本 6 bytes 加上檔案大小）：預設上限下大約可處理 50 Mpx 的 8-bit RGB。例如 12000×8637（1 億像素）的無損 AVIF 需要 `--limits job_commit_mib=2048` 以上（預估 1.9 GiB、實測峰值約 1.7 GiB）。
- 因為預估超過上限而「未檢查」時，報告會寫出需要的記憶體與建議參數，例如「請加 `--limits job_commit_mib=2048` 重跑」；JSON 的 `resource_need` 欄位有同樣的內容，可供程式判斷。

## 安全保證

- 輸入檔只以唯讀方式開啟，不需要寫入權限；分析前後檔案內容、大小與修改時間都不變。
- 報告只寫入新檔：目標已存在、與任一輸入檔相同（包括硬連結），或兩份報告指向同一個檔案時，在開始前就以退出碼 5 拒絕，不會寫入任何檔案。
- 報告先寫到同資料夾的暫存檔，完成後才以不覆寫的方式改名。
- 要求兩份報告而只寫成其中一份時（例如磁碟已滿），程式以退出碼 5 結束，stderr 會列出沒寫成的報告與原因。已寫成那份報告裡的 `exit.code` 是分析結果的退出碼，不含這次寫入失敗；**以程式的退出碼為準**。已寫成的報告不會被刪除或改寫。

## 授權

本工具以 MIT 授權發佈（`LICENSE.txt`）。內含的第三方元件與其授權見 `THIRD_PARTY_NOTICES.txt` 與 `licenses/` 資料夾。
