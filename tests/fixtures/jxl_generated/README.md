# JPEG XL 測試圖（用 C++ 建置裡的 libjxl 0.12.0 產生）

階段 5 加入（docs/decisions.md D-R18）。不是從 C++ repo 複製的：由 `tools/libjxl_gen/gen_jxl_lossy.cpp` 連結 C++ 版 iat 1.0.0 建置裡的 libjxl 0.12.0 編碼；`pwsh -File tools/libjxl_gen/build.ps1` 會重新產生（同時重產 ICC 資料表）。SHA-256 記在 `../SHA256SUMS`。授權見 `licenses/README.md`。

| 檔案 | 內容 | C++ 1.0.0 的實際結果（2026-11-21） |
|---|---|---|
| `v_jxl_rgb16_lossy_vardct_noxyb.jxl` | 從 `../generated/s3_jxl_rgb16_600x40_srgb.truth`（600×40 RGB 16-bit）編碼：VarDCT、distance 1.0、effort 7、`uses_original_profile = true`（不是 XYB）、sRGB 列舉值；47,165 bytes | 會解碼（不是 `xyb_not_native`）。和 `s3_rgb16_600x40_srgb.png` 比較：strict `mismatch`（`samples_differ`），退出碼 1；24000 個像素全部不同，各聲道不同的像素數 R 24000、G 23995、B 23998，最大誤差 17009、3856、16398 |

jxl-rs 解出的樣本和 libjxl 有少量最低位元差異：不同的像素數、最大誤差、判定與問題代碼相同，各聲道的 mae／rmse 與一個列出的差異樣本值不同，已列為允許差異（只限這個案例）。
