# CLI 與 JSON 契約（階段 0 草案，階段 3、0.5.0、1.0.0 更新）

階段 1 依此實作；欄位若需變動，先改本檔並記錄於 `docs/decisions.md`。

## 指令

```text
iat compare <original> <output> [--report-md <path>] [--report-json <path>] [--stdout md|json]
            [--full-paths] [--temp-dir <dir>] [--limits <name>=<value> ...]
iat inspect <file> [--report-md <path>] [--report-json <path>] [--stdout md|json] [--full-paths]
            [--temp-dir <dir>] [--limits <name>=<value> ...]
iat --version
```

- `--temp-dir`（階段 2）：解碼後樣本需要暫存時（PNG Adam7、TIFF 一排 tile 超過 `buffer_mib`/2）使用的目錄，必須已存在，否則為參數錯誤（退出碼 5）。未指定時用系統暫存目錄（`GetTempPath2W`）。暫存檔為 delete-on-close，總量受 `temp_gib` 限制，並在解碼前預留。

- 至少要指定一種報告輸出（檔案或 `--stdout`），否則為參數錯誤，退出碼 5。
- 路徑以 Windows Unicode 解析（`wmain`），支援空白與中文。
- 報告目標必須不存在，且不得與任一輸入或另一份報告同路徑；比對方式為正規化完整路徑，大小寫不敏感。既有檔案一律拒絕，不覆寫。
- 每份報告先寫入同目錄的暫存檔（`CREATE_NEW`），`FlushFileBuffers` 後以不取代既有檔的 rename 落盤（`SetFileInformationByHandle(FileRenameInfo, ReplaceIfExists=FALSE)`）。兩份報告各自原子落盤，並帶相同的 `run_id`。
- stderr 輸出診斷；stdout 只在指定 `--stdout` 時輸出報告。
- `--limits` 可調整的項目：`job_commit_mib`（預設 1024，範圍 16–1048576）、`buffer_mib`（64，1–65536）、`metadata_item_mib`（8）、`metadata_file_mib`（32）、`temp_gib`（8）、`deadline_s`（600，1–604800）。不提供「無上限」選項，0 或超出範圍是參數錯誤。
- 整圖型解碼器（JXL、WebP，階段 3）的硬上限＝`job_commit_mib` − 64 MiB（D10）。解碼前的預估值加兩側列緩衝超過硬上限，或 libjxl 配置器在硬上限拒絕配置，都判為 `not_checked`／`resource_limit`（退出碼 4）。
- `--test-codec-estimate-pct 1..100`（D13）：**僅供測試**，不在說明文字、不屬於 `--limits`。把整圖型解碼器的預估值按比例壓低，用來觸發配置器硬上限，或讓 WebP 改由 Job 上限擋下。使用時報告記 `settings.test.codec_estimate_pct`，並列 `test_option_used`（warning）。一般使用不應指定。
- 被預算或期限擋下的檔案：`decode_status=not_checked`，`decode_status_reason` 為 `resource_limit`／`deadline`／`cancelled`；Markdown 顯示「未檢查」；退出碼 4；strict 為 `not_evaluated`。

## 退出碼

| 碼 | 意義 |
|---|---|
| 0 | compare：嚴格一致；inspect：支援範圍內完整檢查成功 |
| 1 | 可確認的嚴格差異 |
| 2 | 不可判定，或超出支援範圍 |
| 3 | 解碼或結構失敗 |
| 4 | 資源上限或取消 |
| 5 | 參數、I/O、worker 或環境錯誤 |

多種狀態並存時取優先序 5 > 4 > 3 > 1 > 2 > 0；報告保留所有原因。任務已開始且報告路徑可用時，失敗也產出報告。

報告寫入失敗：報告內容（含 `exit.code`）在寫檔前就已定案，寫入失敗不會回寫進已寫成的報告。只要有任一份報告寫入失敗，程序退出碼一律為 5，**程序退出碼優先於報告內的 `exit.code`**；失敗的路徑與原因只寫在 stderr（`iat: report NOT written: …`）。不做「全部寫完再一起改名」的交易式寫入，已寫成的報告保留。

## JSON 頂層

```jsonc
{
  "schema_version": "1.1",
  "run_id": "<UUID>",
  "tool": { "name": "iat", "version": "…", "compiler": "MSVC …" },   // 1.0.0 文件更正：實際欄位為 compiler（0.x 起即如此）
  "codecs": { "libpng": "1.6.59", "libtiff": "4.7.2", "libjxl": "0.12.0", "libwebp": "1.6.0", "zlib": "…" },
  "settings": { "limits": { … }, "codec_threads": 1, "full_paths": false,
                "temp": { "location": "system_temp|user_specified", "free_bytes_at_start": "…", "delete_on_close": true,
                          "dir": "…" } },   // dir 只在 --full-paths 時出現
  "mode": "compare" | "inspect",
  "original": <FileReport>,
  "output": <FileReport>,             // compare only
  "geometry": { "status": "equal|differs|unknown", … },
  "samples": <SampleComparison>,
  "color": { "status": "equal|differs|unverified|unknown", "original": <EffectiveColor>, "output": <EffectiveColor>, "basis": "…" },
  "icc": { "status": "equal|differs|missing_on_output|missing_on_original|both_absent|unreadable|over_budget|embedded_vs_synthesized|both_synthesized|synthesized_vs_absent", … },
  "metadata": { "fields": [ <MetadataField> ] },
  "size": { "original_bytes": "123", "output_bytes": "456", "output_ratio": 0.37, "saving_ratio": 0.63, "compression_factor": 2.7, "comparable": true },
  "metrics": { "time_ms": { … }, "memory_bytes": { … }, "io_read_bytes": "…", "timing_note": "…" },
  "strict": { "result": "match|mismatch|indeterminate|not_evaluated", "reasons": [ "<code>" ] },
  "issues": [ { "code": "…", "severity": "error|warning|info", "scope": "original|output|pair|run", "message": "…" } ],
  "exit": { "code": 0, "reason": "…" }   // 1.0.0 文件補記：0.x 起即輸出
}
```

- 超出 2^53 的 64-bit 計數（bytes、offset、樣本數）一律用十進位字串。
- 所有字串都是有效 UTF-8。來自檔案的文字依其規格轉碼（PNG 文字關鍵字為 Latin-1），無法解讀的位元組與控制字元顯示為 U+FFFD；雜湊一律以檔案中的原始位元組計算（D20）。
- `metrics.memory_bytes`（實際鍵名）：`job_commit_limit`、`job_peak_commit`、`worker_peak_commit`、`worker_peak_working_set`、`row_budget_estimate`、`tool_row_buffers`、`libpng_alloc_peak`；階段 2 新增 `temp_reserved_peak`、`temp_written`、`temp_read`。`metrics.time_ms` 階段 2 新增 `temp_io`（只在用到暫存時出現）。階段 3 新增（整圖型解碼器）：`codec_estimate`（解碼前預估）、`codec_need_estimate_peak`（預估加列緩衝的峰值，只在檢查過時出現）、`coverage_state`（依座標寫入的覆蓋追蹤）、`libjxl_alloc_peak`、`libjxl_alloc_refusals`（只在有拒絕時出現）。
- 浮點統計（MAE、RMSE）只供呈現；相等與否由整數比較決定。

### FileReport

| 欄位 | 說明 |
|---|---|
| `name` / `path` | 預設只有檔名；`--full-paths` 時才含完整路徑 |
| `size_bytes`、`sha256` | 開檔時以同一 handle 計算 |
| `format` | `png`、`tiff`、`bigtiff`、`jxl_codestream`、`jxl_container`、`webp`、`psd`、`psb`、`avif`（0.5.0）、`unknown`；`extension_mismatch` 另列 |
| `container_status` | `valid`、`warning`、`corrupt`、`unverified`、`over_limit` |
| `decode_status` | `complete`、`partial`、`failed`、`unsupported`、`not_attempted`、`cancelled`、`resource_limit` |
| `metadata_status` | 同上列舉 |
| `coverage` | `rows_decoded`、`rows_total`、`bytes_consumed`、`trailing_bytes` |
| `sample_model` | `model`（gray、gray_alpha、rgb、rgba）、`container_bits`、`channel_bits[]`、`compare_type`、`alpha`（none、straight、associated、unspecified、unknown；0.5.0 新增 `matted_white`＝PSD 合成透明度，色彩已與白色混合，strict 最多 indeterminate）、`expansion`（如 `palette+tRNS→rgba8`） |
| `decode_path` | `libpng-rows-no-transform`、`libpng-adam7-passes-no-transform+temp`、`libtiff-strips-encoded-no-rgba`、`libtiff-tiles-encoded-no-rgba`（一排 tile 走暫存時加 `+temp`）、`libjxl-float-pipeline`（多層時加 `+coalesced-N-layers`）、`libwebp-vp8l-rgba`；0.5.0：`psd-composite-raw-rows`、`psd-composite-rle-rows`、`libavif-aom-planes-identity`、`libavif-aom-planes-400`（皆為讀取原生平面，AVIF 加 `+temp`） |
| `color` | `kind`：`cicp`、`icc`、`srgb`、`chrm_gama`、`partial`、`icc_unreadable`、`none`；階段 3 新增 `jxl_enum`（JXL 色彩 enum）、`webp_default_srgb`（WebP 沒有 ICCP：規格預設，不是檔案的明示）。`key`、`description`、`signals[]` |
| `icc` | `status`：`embedded`、`absent`、`unreadable`、`over_budget`；階段 3 新增 `synthesized`（JXL enum 由 libjxl 產生的 ICC，不是檔內資料，`profile_name` 寫明 libjxl 版本）。`bytes`、`sha256` |
| `dimension_policy` | 只在有尺寸政策的格式出現（WebP）：`spec_max`（規格可描述的上限，VP8L 為 16384）、`policy_max`（工具政策，16383）、`within_spec`、`within_policy`。超過政策但在規格內為 `unsupported_dimension`，不是檔案錯誤 |
| `orientation` | `source`（`exif`、`default`、`unparseable`、`unknown`、`jxl_codestream`；0.5.0 新增 `avif_irot_imir`：AVIF 的 irot 後接 imir 對應到 EXIF 1–8，見 D30）、`value`。`unknown`：EXIF 超過 `metadata_item_mib` 未讀取，`value` 為 0，列 `orientation_unknown`（warning），compare 的 strict 最多 `indeterminate`（D21） |
| `psd` | 0.5.0，只在 PSD／PSB 出現：`version`（1 PSD、2 PSB）、`color_mode`、`depth`、`channels`、`compression`（raw、rle、zip、zip_prediction）、`composite`（`saved`：Version Info 0x0421 標示有真正合成影像；`not_saved`：標示沒有，不比較；`flattened`：沒有圖層；`flag_absent`：有圖層但沒有 0x0421）、`composite_uniform`、`layer_count_field`（有號；負值表示第一個額外通道是合成透明度）、`layer_count`、`layers_known`、`alpha` |
| `avif` | 0.5.0，只在 AVIF 出現：`yuv_format`（444、422、420、400）、`depth`、`color_primaries`、`transfer_characteristics`、`matrix_coefficients`、`full_range`、`cicp_source`（`colr_nclx` 或 `av1_sequence_header_or_default`）、`alpha`、`alpha_premultiplied`、`image_count`；**兩種無損判定分開**：`lossless_metadata`（`consistent`／`inconsistent`，只是必要條件）與 `lossless_metadata_reasons[]`（`chroma_subsampled`、`matrix_not_identity`、`matrix_ycgco_r_not_supported`、`limited_range`、`alpha_premultiplied`）；`pixel_verified`（true／false／null，只有 compare 且雙方完成逐樣本比較時才有值）與 `pixel_verified_basis` |
| `resource_need` | 0.5.0：因本工具的預估超過上限而未檢查時出現（`decode_status=not_checked`、`decode_status_reason=resource_limit`，問題代碼 `resource_limit`，退出碼 4）。`limit`（`job_commit_mib`：整圖型解碼器對硬上限；`buffer_mib`：列緩衝，兩側共用同一預算）、`needed_bytes`（檢查用的預估，十進位字串）、`allowed_bytes`（目前設定下可用的位元組）、`current_value`、`suggested_value`（能容納預估的最小值；`job_commit_mib` 取 256 的倍數並加回 64 MiB 保留，`buffer_mib` 取 16 的倍數；超過可設定的最大值時為 null）、`suggested_argument`（例如 `--limits job_commit_mib=2048`）。預估通過但被 Job 上限或解碼器配置失敗擋下時沒有此欄位，因為沒有可靠的數字 |
| `warnings`、`errors` | 解碼器原始訊息（有長度限制） |

### SampleComparison

| 欄位 | 說明 |
|---|---|
| `status` | `equal`、`differs`、`not_comparable`、`incomplete` |
| `reason` | 不可比較時的代碼（`bit_depth_changed`、`sample_model_changed`、`geometry_differs`、`decode_incomplete`、`xyb_not_native` 等） |
| `pixels_compared`、`pixels_differing` | 十進位字串 |
| `per_channel[]` | `name`、`differing`、`max_abs_error`、`mae`、`rmse` |
| `first_differences[]` | 依 raster 順序最多 16 筆：`x`、`y`、`original[]`、`output[]` |
| `includes_alpha`、`unit` | 統計是否含 Alpha；樣本單位（例如 `uint16 code value`） |

### 穩定問題代碼（起始集合）

0.5.0 新增：`psd_composite_not_saved`（error；不比較，退出碼 2）、`psd_composite_flag_absent`（warning；不影響 strict）、`psd_composite_uniform`（warning；strict 原因加 `original:`／`output:` 字首，最多 indeterminate）、`psd_container_warning`（warning）、`alpha_semantics_differ`（warning，pair）、`avif_not_lossless_by_metadata`（error；不解碼，退出碼 2）、`avif_lossless_metadata_only`（info；metadata 符合必要條件）、`avif_pixel_verified_lossless`（info；compare 中樣本逐值相等）、`avif_clap_not_supported`（error）。strict 原因另可為字首加 `alpha_matted_white`。

階段 3 新增：`lossy_not_supported`（WebP VP8）、`xyb_not_native`、`extra_channel_not_supported`（warning，strict 原因加 `original:`／`output:` 字首）、`layered_image`（info）、`alpha_hint_mismatch`（warning）、`webp_container_warning`、`jxl_container_warning`（warning）、`webp_policy_cannot_hold_original`（info）、`rendering_intent_differs`（info）、`test_option_used`（warning）。`unsupported_dimension` 開始使用（WebP 16383 政策）。

階段 2 新增：`interlace_differs`（info）、`compression_not_supported`、`multi_image_not_supported`（TIFF 多頁與 SubIFD）、`temp_cleanup_failed`（warning）、`temp_stale_removed`（info）、`tiff_tag_order`／`tiff_unknown_type`（warning）。strict 原因另可為 `container_warning`（非 PNG 的容器警告）與 `original:`／`output:` 字首的 `alpha_associated`、`alpha_unspecified`。

`input_open_failed`、`input_changed_during_read`、`extension_mismatch`、`truncated`、`crc_error`、`trailing_data`、`missing_end_marker`、`animation_not_supported`、`multi_image_not_supported`、`interlace_not_supported`、`unsupported_sample_model`、`unsupported_dimension`、`bit_depth_changed`、`sample_model_changed`、`alpha_channel_added`、`alpha_channel_removed`、`orientation_differs`、`orientation_unknown`、`icc_missing_on_output`、`icc_missing_on_original`、`icc_differs`、`color_unverified`、`low_priority_color_signal_differs`、`metadata_parse_failed`、`resource_limit`、`cancelled`、`deadline_exceeded`、`worker_crashed`、`report_exists`、`report_path_conflict`、`report_write_failed`、`environment_error`。
