# AVIF 測試圖（用 C++ 建置裡的 libavif 1.4.2 + aom 3.15.2 產生）

階段 6 加入（docs/decisions.md D-R19）。不是從 C++ repo 複製的：由 `tools/libavif_gen/gen_avif_grid.cpp` 連結 C++ 版 iat 1.0.0 建置裡的 libavif 與 aom 編碼；`pwsh -File tools/libavif_gen/build.ps1 -Grid` 會重新產生（重產兩次逐位元組相同）。SHA-256 記在 `../SHA256SUMS`。

樣本是固定的算式（不平滑，偏移或有損都會被發現），PNG 存的是同一份樣本。grid 用 `avifEncoderAddImageGrid`：2×2 個 64×64 的 tile，輸出 128×128；4:4:4、identity、全範圍、無損（`AVIF_QUALITY_LOSSLESS`），nclx 1/13/0/1，速度 6、單執行緒。

| 檔案 | 內容 | C++ 1.0.0 的實際結果（2026-11-21） |
|---|---|---|
| `v_avif_grid_rgb8.avif` | 色彩 grid（41,936 bytes） | inspect：`complete`，退出碼 0，問題只有 `avif_lossless_metadata_only` |
| `v_avif_grid_rgba8.avif` | 色彩 grid 加 alpha grid；左邊 8 欄 alpha 為 0，RGB 照樣保留（44,127 bytes） | 同上 |
| `v_avif_grid_ref_rgb8.png` | 和上面相同的 RGB 樣本 | 和 `v_avif_grid_rgb8.avif` 比較：樣本 `equal`、`avif_pixel_verified_lossless`；strict `indeterminate`（`color_unverified`：PNG 沒有色彩資訊，AVIF 是 nclx），退出碼 2 |
| `v_avif_grid_ref_rgba8.png` | 和上面相同的 RGBA 樣本 | 和 `v_avif_grid_rgba8.avif` 比較：同上（alpha 0 底下的 RGB 也相同） |

Rust 版（rav1d）四個案例都和 C++ 版一致，只有允許的 `decode_path` 名稱不同。
