# 測試圖（從 C++ 版複製）

- 來源：`D:\projects\ImageAnalysisTool\tests\fixtures\`（C++ 版 iat 1.0.0，commit `2dbb4de`），2026-11-19 複製。
  - `generated/`：C++ repo 的 `tests/gen/gen_fixtures.py` 產生的測試圖、`.truth` 獨立真值、`pairs.json`（e2e 案例定義）與 `manifest.json`。空的 `_s3_raw/` 目錄沒有複製。
  - `imbc/`：IMB-C 的固定樣本複製品，來源與授權見 `imbc/PROVENANCE.md`。
- `SHA256SUMS`：所有檔案的 SHA-256（`sha256sum` 格式，路徑相對於本目錄）。複製後已和 C++ repo 的原檔逐一核對一致。
  驗證：在本目錄執行 `sha256sum -c SHA256SUMS`。
- 產生器不移植；要加新測試圖時照舊在 C++ repo 產生，再複製過來並重產清單。
- 例外：`jxl_generated/` 用 C++ 建置裡的 libjxl 0.12.0 產生（階段 5，D-R18），說明見該資料夾的 README。
- 例外：`tiff_variants/` 是 Rust 版自己產生的 TIFF 變形檔（階段 4，D-R17），連同產生腳本一起提交，改法與 C++ 1.0.0 的結果見該資料夾的 README。雜湊同樣記在 `SHA256SUMS`。
- `.gitattributes` 對本目錄關閉換行轉換，避免 checkout 後雜湊改變。本檔（README.md）不在清單內。
