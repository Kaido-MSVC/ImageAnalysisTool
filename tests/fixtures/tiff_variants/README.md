# TIFF 變形檔（Rust 版自己產生）

階段 4 驗證 TIFF 前端用的測試圖（docs/decisions.md D-R17）。不是從 C++ repo 複製的：由本資料夾的 `gen_tiff_variants.py`（只用標準函式庫）產生，多數是 `../generated/` 測試圖的複本，只改一個欄位或一個位元組；另有 8 張從頭寫成的小檔。

- 重新產生：`py -3.13 -I tests/fixtures/tiff_variants/gen_tiff_variants.py`。2026-11-21 以 Python 3.13.17（zlib 1.3.1）產生，結果和驗證時用的檔案逐位元組相同。
- SHA-256 記在 `../SHA256SUMS`（腳本與本檔不在清單內）。
- **預期結果以 C++ 版 iat 1.0.0 的實際輸出為準**。`tests/compare/compare_iat.py` 會用兩個版本跑這些檔案並比對，下表的「C++ 1.0.0」欄是 2026-11-21 執行 `iat inspect` 的結果。
- 掃描階段就會擋下的錯誤（截斷、offset 超出檔案）已有 `../generated/` 的測試圖，這裡只收進得了解碼器的情況。

## 解碼器錯誤（容器掃描通過，解碼時才失敗）

| 檔案 | 來源與改法 | C++ 1.0.0 |
|---|---|---|
| `v_deflate_short_mid.tif` | `s2_tif_rgb16_pred2_le_deflate.tif`，strip 2 的 byte count 減 20 | partial，rows 12/29；`ZIPDecode: ZLib error: ` |
| `v_deflate_no_adler.tif` | 同上，strip 2 的 byte count 減 4（拿掉 Adler-32） | complete |
| `v_deflate_corrupt.tif` | 同上，strip 2 第 30 個位元組反轉 | partial，rows 12/29；`invalid distance too far back` |
| `v_deflate_last_short.tif` | 同上，最後一條 strip（4）的 byte count 減 20 | partial，rows 24/29；`ZIPDecode: ZLib error: ` |
| `v_lzw_short_mid.tif` | `s2_tif_rgb16_be_strip_lzw.tif`，strip 3 的 byte count 減 100 | partial，rows 12/29；`Strip 3 not terminated with EOI code` |
| `v_lzw_corrupt.tif` | 同上，strip 3 第 400 個位元組反轉 | partial，rows 12/29；`Using code not yet in table` |
| `v_lzw_no_eoi.tif` | 同上，strip 3 的 byte count 減 2 | partial，rows 12/29；`Strip 3 not terminated with EOI code` |
| `v_lzw_tile_short.tif` | `s2_tif_rgba8_tile_lzw.tif`，tile 1 的 byte count 減 50 | partial，rows 0/29；tile 1 失敗 |
| `v_packbits_short_mid.tif` | `s2_btf_rgb16_le_strip_packbits.tif`（BigTIFF），strip 1 的 byte count 減 100 | partial，rows 7/29；`decoder_warning`（Terminating PackBitsDecode due to lack of data.） |
| `v_packbits_short_last.tif` | `tif_rgb8_le_strip_packbits.tif`，最後一條 strip（5）的 byte count 減 3 | partial，rows 15/18；`decoder_warning` 同上 |
| `v_none_short_count.tif` | `s2_tif_rgb16_le_strip_none.tif`，strip 2 的 byte count 改成一半 | complete；樣本和原檔相同（未壓縮時 libtiff 從 offset 讀整條，不看 byte count） |
| `v_none_short_count_last.tif` | 同上，最後一條 strip（5）的 byte count 減 100 | complete；樣本和原檔相同 |

## 樣本版面（測試圖沒有涵蓋的）

13×7 像素，從頭寫成的 classic TIFF（單一 IFD、strip）。

| 檔案 | 內容 | C++ 1.0.0 |
|---|---|---|
| `v_gray8a_miniswhite.tif` | 灰階＋alpha 8 位元，MinIsWhite，ExtraSamples=2，未壓縮 | complete；和 `v_gray8a_miniswhite_expected.tif` 樣本相同 |
| `v_gray8a_miniswhite_expected.tif` | 上一張的對照：MinIsBlack，灰階存 255−v，alpha 不變 | complete |
| `v_gray16_miniswhite_deflate.tif` | 灰階 16 位元（小端），MinIsWhite，Deflate，每條 3 列 | complete |
| `v_rgb8_fill2.tif` | RGB 8 位元，FillOrder=2，未壓縮 | complete；和 `v_rgb8_fill2_expected.tif` 樣本相同（libtiff 反轉每個位元組的位元） |
| `v_rgb8_fill2_expected.tif` | 上一張的對照：FillOrder=1，每個位元組已位元反轉 | complete |
| `v_rgb8_fill2_deflate.tif` | RGB 8 位元，FillOrder=2，Deflate（資料沒有反轉） | failed，rows 0/7；`incorrect header check`（libtiff 先反轉壓縮資料） |
| `v_rgb8_none_ref.tif` | RGB 8 位元，未壓縮 | complete |
| `v_rgb8_pred2_deflate_be.tif` | RGB 8 位元，big-endian，Predictor 2，Deflate，每條 4 列 | complete；和 `v_rgb8_none_ref.tif` 樣本相同 |

驗證時另外寫了 18 張；`v_gray8a_miniswhite_expected.tif` 與 `v_rgb8_fill2_expected.tif` 是後來為了確認 libtiff 的樣本值而加的對照檔，所以共 20 張。
