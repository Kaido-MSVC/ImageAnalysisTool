# libavif 輔助程式（階段 6，D-R19）

連結 C++ 版 iat 1.0.0 建置裡的 libavif 1.4.2 + aom 3.15.2 靜態庫（唯讀，不修改 C++ repo），只用於驗證，不產生 Rust 版要用的資料。

| 程式 | 用途 |
|---|---|
| `dump_avif_planes.cpp` | 用和 C++ iat 相同的解碼設定解出 AVIF，傾印 Y／U／V／A 平面並印出解碼時間；格式見檔頭註解 |
| `encode_avif.cpp` | 把 8-bit PNG 編成無損（4:4:4 identity）或有損（4:2:0 BT.601 q60）AVIF，只供量測大圖解碼時間，不產生測試圖 |

建置：`pwsh -File tools/libavif_gen/build.ps1 [-Out <dir>]`（需要 VS 2026 的 MSVC 14.52 工具集與 C++ 建置目錄；可用 `-CppBuild` 指定位置）。加上 `-Dump <輸出資料夾> <檔案.avif>...` 會接著傾印平面。

和 rav1d 比對：

```
cargo run --release -p iat-av1 --example verify_planes -- <輸出資料夾> <檔案.avif>...
```

libavif 是 BSD-2-Clause、aom 是 BSD-2-Clause（另有 AOM 專利授權），只在本機驗證時連結，不隨 iat 發布。
