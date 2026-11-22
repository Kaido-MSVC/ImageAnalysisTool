# 和 C++ 版 iat 比對

`compare_iat.py`（只用標準函式庫）用同一組案例分別執行 C++ 版基準 `iat.exe` 與 Rust 版，比對退出碼、stderr、JSON 報告（含版面）與 Markdown 報告。

```powershell
cargo build --release
py -3.13 -I tests\compare\compare_iat.py `
  --cpp D:\projects\ImageAnalysisTool\dist\ImageAnalysisTool-1.0.0-win-x64\iat.exe `
  --rust target\release\iat.exe --out out\compare --accept png
```

## 發布前的完整比對（385 個案例）

比對腳本的 339 個案例加上 46 個私人樣本案例；不加 `--no-cancel`（含取消案例），加上大圖與私人樣本：

```powershell
cargo build --release
py -3.13 -I tests\compare\compare_iat.py `
  --cpp D:\projects\ImageAnalysisTool\dist\ImageAnalysisTool-1.0.0-win-x64\iat.exe `
  --rust target\release\iat.exe --out out\compare_full `
  --accept png,webp,psd,psb,tiff,bigtiff,jxl_codestream,jxl_container,avif `
  --big D:\projects\iat-archive\big `
  --local D:\projects\ImageAnalysisTool\tools\regress\cases.local.json `
  --pairs D:\projects\iat-archive\regress-baseline\pairs
```

- 預期結果：不需解碼 34 個、本階段 351 個，全部一致（只有允許差異）。少了 `cancel_during_hash` 表示加了
  `--no-cancel`；少了 5 個 `big_*` 表示沒加 `--big`；少了 46 個 `local_*` 表示沒加 `--local`／`--pairs`。
- 大圖（不進 git，2026-11-22 從暫存資料夾移來）：
  `D:\projects\iat-archive\big\big.jxl` SHA-256 `17a4a86e2985ecb7d2921c159fb03ca98daa3b6adab1247505f50872e2a068d1`；
  `big.webp` SHA-256 `31b8108cf84287ea517cfa524fccb544de7f147fe60909cfd51dc0c3870a525f`。
- 私人樣本的對照組：`D:\projects\iat-archive\regress-baseline\pairs`（`gen_pairs.py` 的輸出）。
- 取消案例會在 `--out` 建立 16 GiB 的稀疏檔，`--out` 要放在 NTFS 磁碟上。

- 案例：`tests/fixtures/generated/pairs.json` 的 pairs／singles，加上 C++ 版 `run_e2e.py` 裡不需要解碼器的參數、報告目標安全性、I/O 與取消案例。
- 比對前去掉：`run_id`、`tool`（名稱、版本、編譯器）、`codecs`、`metrics`、`settings.temp.free_bytes_at_start`；文字裡的 run id 與各側的案例目錄也遮罩。
- 允許的差異只看 `docs/decisions.md`「允許的差異」表格；表格以外的差異一律失敗。
- 結果表寫在 `<out>/diff-table.md`。退出碼 0 表示所有「不需解碼」案例，以及 `--accept` 指定格式的「本階段」案例都一致（允許差異算一致）。階段 1 用 `--accept png`；其他格式的案例只列出供參考。
- Markdown 比對前換回 C++ 版的值時，也會替換經過 Markdown 跳脫的形式（同 core 的 `md_escape`），例如 `BITSTREAM\_ERROR`。
- 允許差異表的欄位樣式中 `[` 是字面字元，只有 `*`、`?` 是萬用字元；已允許的 JSON 字串差異在 Markdown 比對前會換回 C++ 版的值（D-R08）。
- TIFF（階段 4，D-R17）：`generated/` 的每張 TIFF 各一個 inspect 案例（`tiffinspect_*`）、C++ 版 `run_e2e.py` 的預算／tile 暫存／libtiff 配置上限案例（`tiff_big_strip_*`、`tiff_tile_*`、`tiff_libtiff_alloc_limit`、`tiff_shared_tiles_default`），以及 `tests/fixtures/tiff_variants/` 的變形檔（`tiffvar_*`，含 6 組配對）。階段 4 用 `--accept png,tiff,bigtiff`。
- JPEG XL（階段 5，D-R18）：`pairs.json` 的 JXL、`imbc_*`、`mixed_*` 案例，加上 `tests/fixtures/jxl_generated/` 的非 XYB 有損圖（`jxlgen_*`）。階段 5 用 `--accept png,webp,jxl_codestream,jxl_container`。
- AVIF（階段 6，D-R19）：`pairs.json` 的 AVIF 案例（含 `avif_job_limit_*` 與 AVIF 對 JXL），加上 `tests/fixtures/avif_generated/` 的 grid 測試圖（`avifgen_*`）。階段 6 用 `--accept png,webp,psd,psb,tiff,bigtiff,jxl_codestream,jxl_container,avif`。分析在 `iat-worker.exe` 裡執行，Rust 版的 `iat.exe` 旁邊要有它（`cargo build --release` 會一起產生）。
- 允許差異表的欄位若寫成 `issues[code=X]`，比對前只略過 C++ 版中代碼為 X 的問題（JSON 與 Markdown 的那一列），其他問題照常逐項比對；用於 Rust 版暫時無法產生的問題（D-R18 的 `layered_image`）。
- 已允許的數值差異（例如有損解碼的 mae／rmse）也會換回 Markdown，浮點數用 6 位有效數字（同 C++ 版的輸出）。
- 本機私人樣本：`--local <cases.local.json>`（C++ repo `tools/regress` 的格式，讀 `cases[]` 與 `inspect_all`）、`--pairs <資料夾>`（`gen_pairs.py` 的輸出，加入 `run_cases.py` 的對照組案例）。案例名稱前加 `local_`；樣本唯讀，結果只寫到 `--out`，都不進 git。例如 `--local D:\projects\ImageAnalysisTool\tools\regress\cases.local.json --pairs <pairs> --only ^local_`。
- Job 上限（階段 7，D-R02 第四次修訂）：`job_commit_limit` 是 C++ 版 `run_e2e.py` 的同名案例。`--big <資料夾>` 另外加入 `big_*` 五個案例，資料夾裡要有 `big.jxl`（4096×4096 RGB16）與 `big.webp`（8192×8192 RGBA8），用 C++ 建置的 `build\release\bin\gen_codec_fixture.exe jxl|webp <raw> <輸出> <寬> <高> <聲道> [位元]` 產生（raw 為交錯的小端序樣本，內容不影響結果）；檔案不進 git。
- `--only <regex>` 只跑名稱符合的案例；`--no-cancel` 跳過需要 16 GiB 稀疏檔的取消案例。
- 用到硬連結，`--out` 建議放在 NTFS 磁碟上。
