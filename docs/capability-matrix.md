# 支援矩陣（階段 0 定稿，階段 3、4、0.5.0 更新）

每一列對應的測試列在表格最右欄；e2e 案例名稱對應 `tests/e2e/run_e2e.py`，測試圖由 `tools/gen_fixtures.py` 產生。

「階段 0 已驗證」只代表解碼器在鎖定版本下的行為已用探針確認，不等於 CLI 已實作。實作與驗收依開發階段（階段 0–5）進行。

CLI 實作狀態：PNG（含 Adam7）、TIFF／BigTIFF、JPEG XL、WebP、PSD／PSB、AVIF 都已實作並驗收。其他格式（例如 JPEG）回報 `format_not_supported_in_this_build`（退出碼 2）。

## PNG（libpng 1.6.59，階段 1）

| 路徑 | 嚴格比較 | 階段 0 證據 |
|---|---|---|
| 非交錯 Gray／GA／RGB／RGBA，8/16-bit | 是 | 16-bit 低位逐值正確；`png_read_update_info` 前後 rowbytes 相同（未套任何 transform） |
| Gray 1/2/4-bit | 是，保留原生整數碼 | `png_gray1`、`png_gray2` 逐值正確；階段 4 e2e：`gray1_one_code_differs`、`gray4_one_code_differs` |
| 色盤（1/2/4/8-bit）+ PLTE／tRNS | 是，以明確規則展開為 RGB8／RGBA8 | `png_pal4_trns` 逐值正確 |
| Gray／RGB + tRNS 色鍵 | 是，展開為同位深 GA／RGBA | `png_gray16_trns`、`png_rgb8_trns`；階段 4 e2e：`rgb8_trns_vs_rgba8` |
| Adam7 交錯 | 是（階段 2）：逐 pass 解碼寫入有額度的暫存，再依 raster 順序比較；`decode_path=libpng-adam7-passes-no-transform+temp`。兩側交錯不同只列 `interlace_differs`（info） | 階段 0 讀完 IHDR 即識別；階段 2 驗收含 12949×20167 RGB8（747 MiB 暫存） |
| APNG（acTL／fcTL） | 否，回報 `animation_not_supported` | 本 libpng 組態未開 APNG，要靠自有 chunk 掃描識別 |
| 截斷、CRC 錯誤（含 IDAT 之後的 ancillary chunk）、缺 IEND、Adler-32 錯誤 | 結構或解碼失敗 | 全部回報錯誤，且錯誤發生在正確階段；階段 4 e2e：`ancillary_crc_after_idat_output`、`adler32_error_output` |
| IEND 之後的額外資料 | 解碼可完成，另報 `trailing_data` | libpng 不會察覺，要靠讀取位置比對（S2） |
| IDAT 之後的 zTXt 解壓不完整 | 影像可完成，metadata 列 `parse_failed` | libpng 只發警告 `zTXt: truncated`（S3） |
| 色彩訊號 | iCCP（解壓後 bytes）、cICP、sRGB、gAMA、cHRM、sBIT、eXIf 可讀 | IMB-C 樣本：iCCP 4324 bytes；cICP 為 (9,16,0,1) |

## TIFF／BigTIFF（libtiff 4.7.2，階段 2）

| 路徑 | 嚴格比較 | 證據 |
|---|---|---|
| Contiguous、無號整數 8/16-bit Gray（MinIsBlack／MinIsWhite）、GA、RGB、RGBA；strip 或 tile；II／MM；Predictor 1/2 | 是（階段 2 實作）。MinIsWhite 同位深反轉；`decode_path=libtiff-strips-encoded-no-rgba` 或 `libtiff-tiles-encoded-no-rgba` | 階段 0：11 種組合逐值正確；階段 2：PNG↔TIFF 跨格式 16-bit、tile 填充區排除、低位差異 |
| 壓縮：none、LZW、Deflate（8、32946）、PackBits | 是 | 階段 0：`TIFFIsCODECConfigured` 只對這四種回傳 1 |
| 其他壓縮：JPEG、LZMA、ZSTD、WebP、LERC、JPEG XL 等 | 否，`not_checked`／`compression_not_supported`（退出碼 2），完全不解碼 | 階段 2 e2e：JPEG（7）、LZMA（34925） |
| BigTIFF（64-bit offset） | 是 | 階段 2：資料與 IFD 位於 5 GiB 之後的稀疏 BigTIFF 通過（只驗證偏移處理） |
| 一排 tile 超過 `buffer_mib`/2 | 是，逐 tile 寫入暫存後依列重組（`+temp`） | 階段 2：4096×300 小圖與 16384×8192 RGB16（768 MiB 暫存） |
| 單一 strip／tile 超過預算、libtiff 配置上限 | 未檢查，`resource_limit`（退出碼 4） | 階段 2 e2e（D9、S6） |
| Associated／未指定 Alpha（ExtraSamples=1／0） | 樣本比較，strict 最多 indeterminate | 階段 2 e2e |
| 沒有 ICC | 色彩未知（D7） | 階段 2 e2e |
| Strip／tile 截斷、offset 超出檔案、IFD 遺失、next IFD 懸空、Deflate 損壞 | 失敗（退出碼 3） | 階段 2 e2e，另有掃描器單元測試 |
| 多頁（下一個 IFD）、SubIFD | 否，`multi_image_not_supported`（退出碼 2） | IMB-C `multipage-v12.tiff` 被識別；階段 2 e2e；SubIFD 為階段 4 e2e `tiff_subifd` |
| Separate planes、浮點、帶號、CMYK、YCbCr、非 Alpha extra samples | 否，`unsupported_sample_model`（退出碼 2） | 列為後續項目；階段 2、4 e2e 各一例（planar、浮點、帶號、CMYK、YCbCr、RGBA＋額外 sample） |
| 交錯 | TIFF 沒有交錯欄位（`interlace=-1`），不會觸發 `interlace_differs` | — |

## JXL（libjxl 0.12.0，階段 3）

整圖型解碼：像素經 `JxlDecoderSetImageOutCallback` 依座標寫入暫存（D11），`decode_path=libjxl-float-pipeline`。自有 box 掃描先檢查容器，再以 coalescing 關閉的標頭掃描讀出 basic info、extra channel、色彩與各層 frame header（不解像素）。

| 路徑 | 嚴格比較 | 證據 |
|---|---|---|
| `uses_original_profile=true`（modular／無損），1–16-bit 整數，Gray／GA／RGB／RGBA | 是，經窮舉驗證的 float 管線（D1），輸出位深取自 codestream | 階段 0：全值 ramp 逐值一致；階段 3 e2e：600×40 RGB16（寬度 > 256，亂序 callback）、Gray8、RGBA8 隱藏 RGB、末像素 1 LSB、12-bit 對 16-bit 為 `bit_depth_changed` |
| 裸 codestream、容器（`jxlc`、`jxlp` 分段、Exif、xml、`jxll`） | 是 | 階段 3 e2e 與 box 掃描單元測試 |
| 非動畫的多層靜態圖 | 是，比較 libjxl 合成後的結果；列 `layered_image`（info），`decode_path` 加 `+coalesced-N-layers` | 階段 3 e2e：REPLACE 層對手工合成的 PNG |
| XYB（`uses_original_profile=false`） | 否，`xyb_not_native`（退出碼 2） | 階段 3 e2e |
| 動畫 | 否，`animation_not_supported` | 階段 3 e2e |
| Premultiplied Alpha | 樣本比較，strict 最多 indeterminate（`alpha_associated`） | 階段 3 e2e |
| 非 Alpha extra channel（depth 等） | 樣本比較只含色彩與 Alpha，strict 最多 indeterminate（`extra_channel_not_supported`） | 階段 3 e2e |
| 浮點樣本、> 16-bit | 否，`unsupported_sample_model` | 階段 4 e2e：`jxl_float32_samples`、`jxl_24bit_samples` |
| 方向 | 保留儲存方向，方向另列（`orientation.source=jxl_codestream`） | 階段 3 e2e：方向 6 為 `orientation_differs` |
| 截斷（裸 codestream 或容器）、box 超出檔案、jxlp 缺最後一段、box 標頭不完整 | 失敗（退出碼 3），`truncated` | 階段 3 e2e |
| 裸 codestream 之後的多餘資料 | 解碼完成，`trailing_data`，strict 最多 indeterminate | 階段 3 e2e：位元組數精確（完整解碼後量測） |
| 色彩 | enum 與嵌入 ICC 分開記錄；enum 的 ICC 是 libjxl 合成（`icc.status=synthesized`）；等價規則見 D12 | 階段 3 e2e 與 `jxl_equivalence` 單元測試 |
| 記憶體 | 預估 (4 × 全部通道 + 1) B/px + 8 MiB 先擋；libjxl 配置器在硬上限（`job_commit_mib` − 64 MiB）拒絕配置，皆為 `resource_limit`（退出碼 4） | 階段 3 大圖 e2e：4096×4096 RGB16 |
| IMB-C v1.3.0 輸出 | PQ／HLG／ICC 16-bit 嚴格相等；8-bit enum 輸出對無色彩 PNG 為 unverified | 階段 3 e2e `imbc_*` |

## WebP（libwebp 1.6.0，階段 3）

整圖型解碼：自有 RIFF 掃描判定容器與旗標，libwebp 解成直式 RGBA8，`decode_path=libwebp-vp8l-rgba`。VP8L 的 `alpha_is_used` 只是提示，實際 Alpha 決定以 RGB 或 RGBA 比較。

| 路徑 | 嚴格比較 | 證據 |
|---|---|---|
| 靜態 VP8L（簡單格式或 VP8X＋ICCP／EXIF／XMP） | 是 | 階段 3 e2e：A=0 隱藏 RGB、opaque RGB、1 LSB 差異座標 |
| VP8（有損，含 ALPH） | 否，`lossy_not_supported`（退出碼 2） | 階段 3 e2e、IMB-C #9 |
| 動畫（ANIM／ANMF） | 否，`animation_not_supported`；尺寸取 VP8X canvas | IMB-C `animation.webp` |
| RIFF 長度大於檔案、chunk 超出、缺補齊 byte、VP8L 截斷 | 失敗（退出碼 3） | 階段 3 e2e；libwebp 對缺補齊 byte 仍回報 OK（S4），由 RIFF 掃描判定 |
| RIFF 之後的多餘資料、VP8X 旗標與 chunk 不符 | 解碼完成，strict 最多 indeterminate（`trailing_data`、`webp_container_warning`） | 階段 3 e2e |
| Alpha 提示與資料或 VP8X 旗標不符 | 以資料為準，列 `alpha_hint_mismatch`，strict 最多 indeterminate | 階段 3 e2e（手工 VP8L） |
| 寬或高大於 16383 | 否，`unsupported_dimension`（工具政策）；`dimension_policy` 另記規格上限 16384 與 `within_spec` | 階段 3 e2e：16383×1、1×16383 通過；16384×1、1×16384 為政策拒絕、`within_spec=true` |
| 沒有 ICCP | 記為規格預設 sRGB（`webp_default_srgb`），只與另一個 WebP 規格預設相等（D12） | 階段 3 e2e |
| 記憶體 | 預估 (8 B/px＋像素數/8＋檔案大小＋4 MiB) × 1.15（D14），超過硬上限即解碼前停止；libwebp 沒有配置掛鉤，預估偏低時由 Job 上限擋下，回報 `resource_limit`（退出碼 4），不會崩潰 | 階段 3 大圖 e2e：8192×8192 RGBA8（預估 602.7 MiB、實測 516.9 MiB）；`webpbig_job_limit` |

## PSD／PSB（自寫 parser，0.5.0，D27、D28）

逐列解碼合成影像（merged image data），`decode_path=psd-composite-raw-rows` 或 `psd-composite-rle-rows`，不用暫存。

| 路徑 | 嚴格比較 | 證據 |
|---|---|---|
| RGB／Grayscale 8／16-bit，Raw 或 RLE；PSD 與 PSB（64-bit 區段長度、32-bit RLE 列長） | 是 | e2e：`psd_rgb8_rle_vs_png`、`psd_rgb8_raw_vs_png`、`psd_rgb16_rle_vs_png`、`psb_rgb16_rle_vs_png`、`psb_rgb16_raw_vs_png`、`psd_gray8_vs_png`；1 LSB 差異座標 `psd_rgb16_last_bit_differs` |
| Bitmap 1-bit（1 = 黑） | 是，反轉為 Gray 1-bit（0 = 黑） | e2e：`psd_bitmap_vs_png_gray1` |
| Indexed＋透明索引（0x0416／0x0417） | 是，展開為 RGB8／RGBA8 | e2e：`psd_indexed_vs_png_palette` |
| 合成透明度（圖層數為負） | 樣本比較，`alpha=matted_white`，strict 最多 indeterminate | e2e：`psd_merged_alpha_vs_png_rgba` |
| 其他額外通道（選取範圍、特別色） | 不比較，`extra_channel_not_supported`，strict 最多 indeterminate | — |
| 0x0421 `hasRealMergedData=0` | 否，`psd_composite_not_saved`（退出碼 2） | e2e：`psd_composite_not_saved` |
| 有圖層、沒有 0x0421 | 是，只列 `psd_composite_flag_absent`（warning） | e2e：`psd_composite_flag_absent`；真實樣本 9 個（CLIP Studio 匯出）皆屬此類 |
| 合成影像整張單色 | 樣本比較，strict 最多 indeterminate（`psd_composite_uniform`） | e2e：`psd_composite_uniform` |
| CMYK、Lab、Multichannel、Duotone、32-bit 浮點 | 否，`unsupported_sample_model`（退出碼 2） | inspect：`inspect_psd_cmyk8`、`inspect_psd_cmyk16`、`inspect_psd_rgb32` |
| ZIP 壓縮 | 否，`compression_not_supported`（退出碼 2） | inspect：`inspect_psd_zip` |
| 截斷、區段超出檔尾、RLE 列解碼長度不符 | 失敗（退出碼 3） | inspect：`inspect_psd_truncated`、`inspect_psd_header_only`、`inspect_psd_resources_past_eof`、`inspect_psd_rle_row_overflow` |
| 影像資料之後的多餘資料 | 解碼完成，`trailing_data`，strict 最多 indeterminate | `inspect_psd_trailing`、`psd_trailing_vs_png` |
| 真實樣本 | 合成影像與 IMB-C ImageMagick 解出的 PNG 逐值相等 | `run_real_samples.py`：7 個 PSD、2 個 PSB |

## AVIF（libavif 1.4.2 ＋ aom 3.15.2，0.5.0，D30、D31）

整圖型解碼，直接讀平面，`decode_path=libavif-aom-planes-identity+temp` 或 `libavif-aom-planes-400+temp`。

| 路徑 | 嚴格比較 | 證據 |
|---|---|---|
| 4:4:4 identity、full range，8／10／12-bit，RGB／RGBA（含 A=0 隱藏 RGB） | 是；metadata 判定 `consistent`，compare 中 `pixel_verified` | e2e：`avif_rgb8_lossless_vs_png`、`avif_rgba8_hidden_rgb_vs_png`、`avif_rgb10/12_lossless_vs_jxl`、`avif_rgba10/12_lossless_vs_jxl`；真實樣本裁切與全尺寸 |
| 4:0:0 灰階 | 是 | e2e：`avif_gray8_lossless_vs_png` |
| metadata 符合但實際有損 | 樣本不同，`pixel_verified=false`（退出碼 1） | e2e：`avif_identity_lossy_vs_png` |
| 4:2:0／4:2:2、matrix ≠ 0（含 IMB-C 現行 BT.601）、limited range、premultiplied | 否，不解碼，`avif_not_lossless_by_metadata`（退出碼 2） | e2e：`avif_not_lossless_*` 4 組；真實樣本 IMB-C q90／q100 |
| 位深不同（10-bit 對 16-bit） | `bit_depth_changed` | e2e：`avif_rgb10_vs_png16` |
| irot／imir | 對應為有效方向；不同即 `orientation_differs` | e2e：`avif_irot_vs_png`；單元測試對照表 |
| 截斷、box 超出檔案 | 失敗（退出碼 3） | inspect：`inspect_avif_truncated`、`inspect_avif_box_past_eof` |
| 影像序列、clap | 否（退出碼 2） | — |
| 記憶體 | 預估 像素 × 樣本數 × 6 B ＋ 檔案 ＋ 48 MiB；超過硬上限即 `resource_limit` | 全尺寸 12000×8637：預估 1932 MiB、實測 peak commit 1707 MiB |

