# CLI 回歸比對（tools/regress）

用來確認重構或重建後，`iat` 的 CLI 行為（退出碼、stdout、stderr、JSON 與 Markdown 報告）和先前保存的基準一致。只用 Python 標準函式庫（本機以 `py -3.13` 執行）。

| 檔案 | 用途 |
|---|---|
| `gen_pairs.py` | 以 IMB-C 的 `magick.exe`（唯讀呼叫）從一張本機 PNG 裁切並轉出各格式的對照組 |
| `run_cases.py` | 執行全部案例，每個案例存一個資料夾（`stdout`、`stderr`、`exit`、`report.json`、`report.md`、`args.json`） |
| `compare_runs.py` | 比對兩次執行結果；會遮罩 `run_id`、時間／記憶體／I/O 數值、暫存可用空間與案例資料夾路徑 |
| `cases.example.json` | 本機設定檔範例（不含真實路徑） |

案例分兩類：

- **內建案例**：參數錯誤（退出碼 5）、`gen_pairs.py` 產生的對照組（IMB-C 的呼叫方式）、資源上限，以及 repo 內 `tests/fixtures/imbc/` 的 IMB-C 輸出。
- **本機樣本案例**：寫在本機設定檔 `tools/regress/cases.local.json`（已列入 `.gitignore`，不進 repo）。私人圖片的檔名與路徑只放在這裡。格式見 `cases.example.json`：
  - `samples_dir`：樣本資料夾（只讀；執行前後核對大小、修改時間與 SHA-256）。
  - `cases[]`：`name`、`shape`（`full`＝加上 JSON／Markdown 報告與 `--stdout json`；`imbc`＝IMB-C 的 compare 呼叫方式，`args` 為兩個輸入；`raw`＝原樣傳入）、`args`。`{S}/`、`{P}/`、`{I}/` 開頭分別代表樣本資料夾、對照組資料夾、`tests/fixtures/imbc`。
  - `inspect_all`：對樣本資料夾內符合副檔名的每個檔案執行 inspect（`--skip-big` 時略過）。
  - `pairs`：給 `gen_pairs.py` 用；`crop_source` 是要裁切的 PNG（預設為資料夾內第一個 PNG），`psd_references` 是要以 ImageMagick 轉出合成影像 PNG 的 PSD／PSB。

沒有設定檔時只跑內建案例。

## 用法

```powershell
# 1. 產生對照組（只需一次；輸出資料夾放在 repo 外）
py -3.13 -I tools/regress/gen_pairs.py --magick <IMB-C>\target\release\ImageMagick\magick.exe --out <work>\pairs

# 2. 以目前的建置執行（--out 必須是新資料夾）
py -3.13 -I tools/regress/run_cases.py --bin build/release/bin --out <work>\run-new --pairs <work>\pairs

# 3. 與基準比對（版本不同時加 --ignore-version <基準版本> <新版本>）
py -3.13 -I tools/regress/compare_runs.py <work>\baseline <work>\run-new
```

`--pairs`、`--config` 也可以用環境變數 `IAT_REGRESS_PAIRS`、`IAT_REGRESS_CONFIG` 指定；`gen_pairs.py` 的樣本資料夾可用 `--samples` 或 `IAT_REGRESS_SAMPLES` 覆寫。

基準資料（先前保存的 `run_cases.py` 輸出）與對照組都不放進 repo。`full_paths` 與幾個參數錯誤案例的輸出會帶有對照組或案例資料夾的完整路徑；基準若是在別的位置產生後才搬移，用 `--path-map <基準當時的路徑> <這次的路徑>` 把兩邊的同一資料夾換成同一個代號（可重複指定，例如對照組資料夾與執行結果資料夾各一組）。`compare_runs.py` 全部一致時退出碼為 0，否則為 1 並列出差異的案例與欄位。
