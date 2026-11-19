# 規格決定與修訂

開發規劃書（下稱規劃書；內部文件，未收錄於公開 repo）本身不修改；以下為使用者確認的釐清、保守預設，以及階段 0 實測得出的實作約束。後列條目優先於規劃書中衝突的文字。

## 使用者確認的釐清（2026-11-07）

| 編號 | 主題 | 決定 | 規劃書出處 |
|---|---|---|---|
| D1 | JXL 原生樣本 | libjxl 0.12.0 沒有整數直出路徑，所有整數輸出都經 float 管線再乘回 2^n−1 取整（`dec_modular.cc`、`stage_write.cc`）。改為**窮舉驗證後放行**：僅限 `uses_original_profile=true`，XYB 一律不可比較；報告記 `decode_path=libjxl-float-pipeline`。8-bit 改請求 UINT16 + `JXL_BIT_DEPTH_FROM_CODESTREAM`，避開 UINT8 必經的 ±0.492 blue-noise dither。階段 0 實測：8-bit 與 16-bit 全值 ramp 逐值一致（見 `stage0-measurements.md`）。 | §3.1 第 3 點 |
| D2 | ICC 差異 | 樣本相同時，一端 ICC 遺失或兩端 ICC 不同，strict 都是 `indeterminate`（退出碼 2）；以 `icc_missing_on_output`、`icc_missing_on_original`、`icc_differs` 等穩定代碼區分。 | §3.2、§7 階段 1 |
| D3 | Alpha 增減 | 無 Alpha 的一端視為同位深全不透明後比較。相等時 `samples=equal`，strict 可為 `match`，另列 `alpha_channel_added`／`alpha_channel_removed`。任一 Alpha 非最大值即為 `mismatch`。 | §3.1 第 4 點 |
| D4 | PNG 色彩優先序 | 依 cICP > iCCP > sRGB > cHRM/gAMA 取有效描述。有效描述相同即 `color=equal`；低優先訊號的差異只列警告，不影響 strict。 | §3.2 |

## 使用者確認（2026-11-07，階段 0 回報後）

| 編號 | 主題 | 決定 |
|---|---|---|
| D5 | Job commit 預設 | **1 GiB**（`job_commit_mib=1024`）；超大檔用 `--limits` 調高。 |
| D6 | 超大圖驗收 | 階段 1 加入私人樣本 big-png-261mp（12949×20167，8-bit RGB）：只限本機、唯讀、不進 git、文件只用代號，只記尺寸、位深與 SHA-256，測試前後核對雜湊。另以產生器建立同尺寸的公開合成 PNG 供重現。PNG 預算必須按列估算；被預算或期限擋下的檔案標為「未檢查」（`decode_status=not_checked`，退出碼 4），不得算通過。 |

## 使用者確認（2026-11-07，階段 2）

| 編號 | 主題 | 決定 |
|---|---|---|
| D7 | TIFF 色彩 | 沒有 ICC 的 TIFF：色彩為未知（`color_unknown`），不假設 sRGB。PNG 的 sRGB chunk 對上帶 ICC 的 TIFF：維持 `unverified`，不放寬。 |
| D8 | 暫存位置 | 預設放在系統暫存目錄（`GetTempPath2W`），不在儲存庫內；可用 `--temp-dir` 指定。 |
| D9 | 單一 strip／tile 超過預算 | 未檢查，退出碼 4（與 D6 相同）。 |

## 使用者確認（2026-11-07，階段 3）

| 編號 | 主題 | 決定與依據 |
|---|---|---|
| D10 | 整圖型解碼器的硬上限 | 沿用 `job_commit_mib`，不新增參數：硬上限＝Job 上限 − 64 MiB（`Limits::codec_cap_bytes()`）。預設 1024 MiB 時為 960 MiB。依據：階段 3 實測（`stage3-measurements.md`）程序 commit 只比 libjxl 配置器記到的多 0.1–1.2 MiB，64 MiB 足以涵蓋 worker 本身、列緩衝與報告；用同一個參數，使用者只需調一個值。解碼前以預估值檢查，再加上兩側的列緩衝，超過硬上限即為「未檢查」／`resource_limit`（退出碼 4）。libjxl 另以自訂 `JxlMemoryManager` 在累計配置超過硬上限時拒絕配置；libwebp 沒有配置掛鉤，只靠預估、Job 上限與 `VP8_STATUS_OUT_OF_MEMORY`。 |
| D11 | 亂序輸出的暫存 | libjxl 的 callback 以 256 寬的 group 為單位、不依 raster 順序輸出（實測），因此暫存檔新增「依座標寫入」模式。覆蓋追蹤是每列不重疊的區間：插入時重疊即失敗，結束時每列必須恰為 [0, width)；重複與漏寫不能互相抵消。證明完整覆蓋並 seal 之前，暫存不可讀；依座標寫入與循序附加不可混用。這修改了階段 2「只能循序附加」的保守預設，符合規劃書 §4.1 第 3 點。 |
| D12 | 跨格式色彩等價 | 報告分開記錄「明示」與「規格預設」。WebP 沒有 ICCP 時記為「規格預設 sRGB」，但不與其他格式的明示色彩判為相等（`unverified`）。JXL 的 enum 只在對方明示同一組 CICP 時相等：sRGB enum 對 PNG sRGB chunk 或 cICP (1,13,0,1)；PQ／HLG enum 對 PNG cICP。PNG sRGB 與 JXL enum 的 rendering intent 不同時，記錄差異但不影響相等。另外，JXL 為 enum 且 libjxl 合成的 ICC 與對方嵌入的 ICC 逐 byte 相同時，判為相等，依據另記為「JXL enum 的合成 ICC 與對方嵌入 ICC 相同（libjxl 0.12.0）」；報告仍標明該 ICC 是合成的，不當作嵌入資料。依據：IMB-C 的 PQ／HLG／ICC PNG 所嵌入的 iCCP，與 libjxl 由對應 enum 合成的 ICC 逐 byte 相同（實測）。其他組合一律 `unverified`。 |
| D13 | 測試用的預估係數 | 隱藏旗標 `--test-codec-estimate-pct 1..100`（不在說明文字、不屬於 `--limits`），把整圖型解碼器的預估值按比例壓低，讓 e2e 能觸發配置器硬上限，以及（2026-11-08 起）讓 WebP 的預估通過、改由 Job 上限擋下。使用時報告的 `settings.test` 會記錄係數，並列 `test_option_used`（warning）。 |
| D14 | WebP 預估的安全係數（2026-11-08，階段 3 待辦） | WebP 預估改為 (8 B/px + 像素數/8 + 檔案大小 + 4 MiB) × 1.15。依據：原式在 8192×8192 RGBA8 只比實測 Job 峰值多約 1.4%（524.1 對 516.9 MiB），而 libwebp 沒有配置器可以兜底。e2e `webpbig_job_limit` 已證實預估偏低時由 Job 上限乾淨擋下（`OUT_OF_MEMORY` → `resource_limit`、退出碼 4、沒有 `worker_crashed`），所以 1.15 是為了讓預估先擋下、留下可讀的原因，不是唯一防線。代價：預設硬上限 960 MiB 下可處理的 WebP 從約 117 Mpx 降到約 102 Mpx。 |

同意的保守預設：
- 非動畫的多層 JXL 以 libjxl 合成後的結果作為影像，報告列出層數與各層資訊（info）。
- WebP 的 EXIF／XMP 只記雜湊與大小，與 TIFF／PNG 相同。

## 使用者確認（2026-11-08，階段 4）

| 編號 | 主題 | 決定 |
|---|---|---|
| D15 | 交付形式 | zip 免安裝，不做安裝程式。 |
| D16 | VC++ 執行階段 | 維持靜態 CRT（`/MT`、triplet `VCPKG_CRT_LINKAGE static`），不附 redistributable。 |
| D17 | 專案授權 | MIT，從本機 SPDX 授權範本庫的 `MIT.txt` 複製為專案根目錄 `LICENSE`，版權人為 `2026 Kaido-MSVC`。 |
| D18 | 交付版本 | `0.4.0`。 |
| D19 | 最終回歸 | Release 建置後跑一次完整回歸：單元測試、全部小檔 e2e、大圖群組 big／tiffbig／jxlbig／webpbig，以及私人樣本 big-png-261mp。 |

## 使用者確認（2026-11-08，0.4.1 審查修正）

| 編號 | 主題 | 決定 |
|---|---|---|
| D20 | 來自檔案的字串 | PNG tEXt／zTXt／iTXt 的關鍵字依規格為 Latin-1，顯示時轉成 UTF-8（控制字元顯示為 U+FFFD）；雜湊仍用檔案中的原始位元組。序列化層另加防護：JSON 以 `error_handler_t::replace` 輸出，Markdown 全文過濾無效 UTF-8，任何漏網字串都變成 U+FFFD，不再讓 worker 序列化失敗。盤點結果見下。 |
| D21 | EXIF 超過預算時的方向 | PNG eXIf／WebP EXIF 超過 `metadata_item_mib` 而未讀取時，方向記為 `unknown`（`value` 0），不再當成預設方向 1；列 `orientation_unknown`（warning），compare 的 strict 為 `indeterminate`（原因 `orientation_unknown`），geometry 為 `unknown`。JXL 方向取自 codestream、TIFF 取自 IFD，不受影響。 |
| D22 | 殘留暫存檔辨識 | 仍在建立暫存前清理先前執行留下的殘檔（當機、斷電時 delete-on-close 沒有執行），不改為只清本次檔案；但檔名必須是 `iat-tmp-<UUID>-a.bin`／`-b.bin`，中間 36 字元須為 8-4-4-4-12 十六進位，其他同字首檔名一律不動。 |
| D23 | 修正版本 | `0.4.1`：D20–D22、輸入雜湊可取消、報告寫入失敗時退出碼說明。 |

D20 盤點（會直接放進報告、源自檔案內容的字串）：

| 來源 | 處理 |
|---|---|
| PNG 文字 chunk 關鍵字（summary `keys=[…]`、`metadata_parse_failed` 訊息） | Latin-1 → UTF-8（`latin1_text`）；`values_sha256` 用原始位元組。 |
| PNG iTXt 語言標籤、翻譯關鍵字 | 只跳過，不放進報告。 |
| PNG 文字內容、eXIf、XMP | 只放大小與雜湊。 |
| PNG chunk 類型 | 掃描器只接受 ASCII 字母（`valid_type`），否則判為結構錯誤。 |
| TIFF DateTime（306，ASCII） | `file_text`：有效 UTF-8 保留，其餘與控制字元為 U+FFFD。 |
| TIFF 其他 ASCII 欄位（ImageDescription、Make、Model、Software、Artist、Copyright） | 只放欄位名稱與雜湊。 |
| libtiff 錯誤／警告訊息（可能含檔案內字串，且 512 bytes 截斷可能切斷多位元組序列） | `file_text`。 |
| WebP FourCC、JXL box 類型 | 原本就以 `?` 取代非可列印 ASCII。 |
| worker stderr（截前 2000 bytes 可能切斷序列） | 過濾無效 UTF-8，保留換行。 |

## 使用者確認（2026-11-17，0.5.0 升級）

| 編號 | 主題 | 決定 |
|---|---|---|
| D24 | 工具集 | MSVC 14.51 已從本機移除，改鎖 **14.52**（triplet `VCPKG_PLATFORM_TOOLSET_VERSION 14.52`、`vcpkg-env.cmd` 的 `-vcvars_ver=14.52`）。`vcvarsall` 在工具集不存在時仍回傳 0，所以 `vcpkg-env.cmd` 與 `build.ps1` 載入後都檢查實際的 `VCToolsVersion`（須以 `14.52.` 開頭）與 `cl.exe`，不符即退出碼 1。vcpkg `builtin-baseline` 維持 `1460b31b`，不跟隨 VS 更新後的 `embeddedsha`。 |
| D25 | 語言標準 | C++20 → **C++23**，只升標準、不採用 C++23 新語言或程式庫特性。CMake 4.5.0 對 MSVC 19.52 產生 `-std:c++23`（`__cplusplus`＝202302）。加 `CMAKE_CXX_SCAN_FOR_MODULES OFF`：專案不使用模組，避免 CMake 4.5 對 C++20 以上自動掃描並合成 `import std` 目標。C 標準維持 C17。 |
| D26 | 14.52 的 C4244 | `tiff_scan.cpp` 的 TIFF 標籤表原為 `std::pair<uint16_t, const char*>`，以 `int` 字面值經 pair 的轉發建構子初始化，14.52 STL 對此發出 C4244。所有值（270–50706）都在 uint16_t 範圍內，**實際不會截斷**。改為聚合結構 `NamedTag`，大括號初始化在編譯期做窄化檢查，超出範圍的常數會直接編譯失敗。 |
| D27 | PSD／PSB 解碼範圍 | 自寫唯讀 parser（使用者決定 Q3），只讀合成影像，不解圖層。Bitmap（1 = 黑，同位深反轉為 Gray 1-bit，記錄於 `expansion`）、Grayscale、Indexed（768 bytes 調色盤；0x0416 色數、0x0417 透明索引，展開規則同 PNG PLTE／tRNS，索引超出色數為解碼失敗）、RGB；8／16-bit。CMYK、Lab、Multichannel、Duotone、32-bit 浮點：讀標頭、ICC、資源後 `unsupported_sample_model`（退出碼 2）。ZIP（2、3）：`compression_not_supported`。Raw 與 RLE 逐列解碼，各通道平面以 1 MiB 緩衝串流定位讀取；RLE 每列必須剛好解出列寬且用完該列的位元組數，超出 PackBits 理論上限（列寬 + ⌈列寬/128⌉）也是 `structure_error`（0.5.1 由 D33 取消此上限）。PSD 每邊上限 30000、PSB 300000，超出為結構錯誤。色彩只認嵌入 ICC（0x040F），沒有 ICC 為 `none`（同 TIFF D7）。方向取 EXIF（0x0422），否則為預設。圖層數欄位為負時，第一個額外通道是合成透明度，`alpha=matted_white`（Photoshop 與白色混合，不反混合），strict 最多 indeterminate；其他額外通道（儲存的選取範圍、特別色）不比較，列 `extra_channel_not_supported`。 |
| D28 | PSD 合成影像有效性 | 0x0421 `hasRealMergedData=0`：`psd_composite_not_saved`，不解碼、不比較（退出碼 2），與 IMB-C 拒絕無合成圖檔案一致。有圖層但沒有 0x0421：`psd_composite_flag_absent`（warning），**不降級**（使用者決定 Q4；本機測試圖片集的 9 個 PSD／PSB 樣本都屬此類）。沒有圖層（含 Lr16／Lr32／Layr 區塊都沒有）為 `flattened`。任何情況下合成影像整張單一顏色：`psd_composite_uniform`（warning），strict 最多 indeterminate。layer 區段的 tagged block 走不完時 `layers_known=false`，不會判為 `flattened`。 |
| D29 | 測試圖：libjxl 非 8／16-bit 的輸入 | libjxl 0.12.0 編碼器預設（`JXL_BIT_DEPTH_FROM_PIXEL_FORMAT`）會把 UINT16 輸入乘上 65535/(2ⁿ−1) 縮放（`encode.h`，`JxlEncoderSetFrameBitDepth` 說明）。`gen_codec_fixture` 改為 `JXL_BIT_DEPTH_FROM_CODESTREAM`，真值才會原樣寫入。只影響測試圖；階段 3 的 12-bit 對 16-bit 案例只檢查 `bit_depth_changed`，不受影響，重跑仍通過。 |
| D30 | AVIF 無損判定與解碼 | 兩欄分開。`lossless_metadata=consistent` 的條件：4:4:4 且 matrix 0（identity），或 4:0:0；full range；Alpha 非 premultiplied。這只是必要條件。不符時不解碼（`avif_not_lossless_by_metadata`，退出碼 2，使用者決定 Q7）。YCgCo-Re／Ro 列 `matrix_ycgco_r_not_supported`。解碼不呼叫 `avifImageYUVToRGB`，直接讀平面：identity 下 R＝V、G＝Y、B＝U，4:0:0 下 Gray＝Y。`pixel_verified` 只在 compare 且樣本完成比較時設定。色彩：有 ICC 用 ICC；只有 nclx 時以 `cicp:P/T/0/1` 為比較鍵（identity 解碼後是 RGB，可與 PNG cICP 或 JXL enum 比較），沒有 colr 為 `none`。方向：displayed = imir(irot(stored))，`avif_orientation()` 對照表（irot 0–3 × 無／axis 0／axis 1）＝ {1,4,2}、{8,5,7}、{3,2,4}、{6,7,5}；libavif 1.4.2 標頭寫明 axis 0 對應 EXIF 4、axis 1 對應 EXIF 2。`clap` 為 `avif_clap_not_supported`，影像序列為 `animation_not_supported`。AV1 規格不允許 4:0:0 搭配 identity，灰階測試圖用 matrix 2。 |
| D31 | AVIF 解碼器與記憶體 | 解碼器 aom 3.15.2（使用者決定 Q2，與 IMB-C 同版；dav1d 1.5.3／1.5.4 在 MSVC 14.52／14.53 編譯失敗，日後再評估）。`maxThreads=1`、`strictFlags=AVIF_STRICT_ENABLED`、`imageContentToDecode=COLOR_AND_ALPHA`、自訂 `avifIO` 從唯讀 handle 定位讀取。整圖型解碼，沿用 `WholeImageDecoder` 寫暫存。預估：像素數 × 樣本數 × 6 B ＋ 檔案大小 ＋ 48 MiB。實測 12000×8637 8-bit RGB（110 MB 無損 AVIF）：預估 1932 MiB、worker peak commit 1707 MiB，預估高出約 13%；libavif／aom 沒有配置器掛鉤，預估偏低時由 Job 上限兜底。測試圖產生：aom 已在發佈樹且版本相同，`gen_codec_fixture` 直接連結發佈樹的 libavif[aom]，沒有另建測試用 vcpkg 樹（偏離 Q5 的原始方案）。 |
| D32 | 超過上限時的建議值 | 預設上限不變（使用者決定）。本工具的預估超過上限時（列緩衝對 `buffer_mib`，整圖型解碼器對 `job_commit_mib` − 64 MiB），`resource_limit` 訊息寫出需要的位元組（與 MiB）、目前的設定，以及 `rerun with --limits <名稱>=<值>`；同時在該側 `FileReport.resource_need` 放結構化欄位（契約見 `cli-json-contract.md`）。建議值為能容納預估的最小值：`job_commit_mib` 取 256 的倍數（含 64 MiB 保留），`buffer_mib` 取 16 的倍數；超過可設定的最大值時 `suggested_value` 為 null。Job 上限或解碼器配置失敗擋下的情況沒有可靠數字，不給建議。回歸：單元測試 `suggest_limit_mib`、e2e `avif_job_limit_suggests_value`（建議 256）與 `avif_job_limit_suggested_value_works`（以建議值重跑得到 match）。 |
| D33 | PSD／PSB PackBits 列長（0.5.1，修訂 D27） | 使用者決定（2026-11-17）：放寬 D27 的「超出 PackBits 理論上限（列寬 + ⌈列寬/128⌉）即 `structure_error`」。ImageMagick 7.1.2-35 寫出的 RLE 每列結尾有 0x80 空指令、literal 會拆段，是合法但非最佳化的 PackBits，0.5.0 以 `rle_row_too_long` 誤判（實測見 `imbc-v1.4-codex-guide.md` §7）。0.5.1 起 0x80 可出現在任何位置，也接受非最佳化的編碼；列長不再設上限，解碼以固定大小的緩衝分段讀入（`PackBitsRow`），記憶體不隨列長增加。仍然嚴格：每列必須剛好用完 byte count 表給的位元組數並解出剛好的列寬；run 寫超過該列、最後一個 run 被 byte count 截斷、解出不足，皆為 `structure_error`（`rle_row_invalid`）；byte count 加總超出檔案為 `truncated`（不變）。`rle_row_too_long` 不再產生。 |

## 使用者要求（2026-11-19，1.0.0 公開發布準備）

使用者指示：拆分 core／CLI、CLI 行為完全不變、版本 1.0.0、README 定位為通用驗證器、zip 內含 README。下列為依指示所做、回報時列出的實作決定。

| 編號 | 主題 | 決定 |
|---|---|---|
| D34 | core／CLI 拆分 | 新靜態庫 `iat-core`（`src/core/`，併入原 `iat_model` 的 `src/model/`、`src/common/win_util.cpp`）提供 `run_analysis(RunRequest, cancel) → Report`、`validate_report_targets`、`report_json_text`／`report_markdown_text`、`write_new_file`、`version()`；不寫 console、不呼叫 exit，所有結果（含取消、worker 異常、環境錯誤）都在 `Report` 內，`exit_code` 由 `decide_exit` 決定後放在報告裡。`iat.exe` 只解析參數、呼叫 core、寫出／印出報告、決定程序退出碼（報告寫入失敗時為 5）。**解碼器不放進 `iat-core`**：`iat_engine`（所有 codec）仍只連進 `iat-worker.exe`，在 Job Object 內解碼，這是規劃書的隔離設計，CLI 行為（資源上限、`worker_crashed` 等）也依賴它；`iat-core` 驅動 worker，因此 core 的 API 涵蓋解碼、比對與報告產生，但程序內不含解碼器。worker 位置維持「目前執行檔旁的 `iat-worker.exe`」，不為 GUI 預先加設定。版本號只在 `project(VERSION)`，以 `IAT_VERSION` 傳給 `iat-core`；`iat_unit` 另以 `IAT_EXPECTED_VERSION` 核對。`tools/gen_third_party.py` 的自有庫名改為 `iat-core`、`iat_engine`。回歸：0.5.1 基準 62 案例逐一比對一致（遮蔽 run_id 與量測值）、e2e 240／240、單元 42／42。 |
| D35 | 1.0.0 發布文件 | 版本 `1.0.0`（`schema_version` 不變，1.1）。根目錄 `README.md` 改為對外說明（支援格式、用法、CLI 介面規格、IMB-C 整合範例）；原 README 的開發內容移到 `docs/development.md`，原 `CHANGELOG.md` 完整移到 `docs/changelog-0.x.md`，新 `CHANGELOG.md` 從 1.0.0 開始並摘要 0.x。README 寫明 1.x 的相容性承諾：同一 `schema_version` 主版本內只新增欄位與列舉值。zip 加入 `README.md`、`CHANGELOG.md`；`package.ps1` 封包前檢查兩個執行檔不含 VC++／UCRT 執行階段 DLL 的匯入名稱（D16 靜態 CRT）。 |
| D36 | 公開 repo（1.0.0，修訂 D35） | 使用者決定（2026-11-19）：公開用 repo 以全新歷史建立（單一 initial commit，分支 `main`），原 repo 保留完整歷史作為本機封存。公開前清理：`CMakePresets.json` 不含本機路徑（Ninja 依 PATH、vcpkg 依 `VCPKG_ROOT`），本機設定放 `CMakeUserPresets.json`（不進 git）；文件中的本機路徑改為一般描述，私人樣本檔名改為代號。`README.md` 改為英文主版、繁中內容移到 `README.zh-TW.md`，註明 Markdown 報告目前只有繁中、JSON 與語言無關。相容性承諾改為：1.x 期間同一 `schema_version` 主版本內只新增欄位，不刪除也不改名；破壞性變更同時提升 `schema_version` 與 iat 的主版本；JSON 的空白、縮排與欄位順序不在保證範圍內。公開 repo 不收：階段 0 探針（`probes/`、`tools/run_probes.py`、`tools/measure_stage3.py`）、`docs/stage*-progress.md`、`docs/stage*-measurements.md`、`docs/imbc-v1.4-codex-guide.md`、`CLAUDE.md`，以及開發規劃書、`docs/upgrade-0.5.0-assessment.md`、`docs/stage*-acceptance.md`（私人樣本的 SHA-256 只記在這些內部文件；公開文件中對它們的引用改為指向公開文件或刪除）。e2e 需要的測試圖產生器（`gen_codec_fixture`、`gen_large_png`、`gen_large_tiff`）移到 `tests/gen/`。回歸比對腳本納入 `tools/regress/`，私人樣本案例只放本機設定檔。 |

## 保守預設（未逐項詢問，回報時列出）

- 預算：`buffer_mib` 64 MiB 指工具自管的列／帶緩衝；解碼器必要的整圖配置計入 Job commit 上限（預設 1 GiB，D5）。
- 輸入：父程序以 `GENERIC_READ` + `FILE_SHARE_READ` 開啟，透過 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 只把該 handle 交給 worker；worker 不重新解析路徑。檔案被其他程序以寫入模式開啟時，回報 I/O 錯誤（退出碼 5）。
- 只有 gAMA 而無 cHRM：primaries 未知，`color=unverified`。
- 寬高或有效方向不同：`strict=mismatch`，`samples=not_comparable`。
- ImageMagick 私有 `orNT` chunk 只列 metadata，不作為方向來源。
- 沒有指定任何報告輸出：參數錯誤（退出碼 5）。
- 取消：Ctrl+C／Ctrl+Break 由父程序處理並終止 Job，worker 以 `CREATE_NEW_PROCESS_GROUP` 啟動，不直接接收主控台訊號；之後仍寫出未完成報告。
- 專案自有授權：待使用者決定（階段 4 前）。

### 階段 1 新增

- IEND 之後有多餘資料：容器狀態為 `warning`。樣本即使相等，strict 也只能是 `indeterminate`（`trailing_data`），因為規劃書要求「結構檢查符合政策」才能 `match`。
- inspect 的退出碼 0 要求三者都成立：解碼完整、容器 `valid`、metadata `valid`。任一不符為 2，解碼或結構失敗為 3。
- compare 中 metadata 解析失敗（例如 zTXt 損壞）只影響 metadata 欄位與問題清單，不影響 samples 與 strict。
- 色盤索引超出 PLTE 項數時判為解碼失敗，不以任何值替代。
- 被 libpng 拒絕的 iCCP（例如 S10 的長度條件）：有效色彩為 `icc_unreadable`，color 為 `unverified`。
- libpng 自身的配置受單側上限約束（2 × 列大小 + metadata 單項上限 + 1 MiB）。超限時該側為 `not_checked`／`resource_limit`，不會判為檔案損壞。
- 同一檔案同時作為原檔與輸出檔：允許執行，另列 `same_file`（info）。
- compare 時兩側在同一個 worker 內逐列同步解碼。worker 的期限涵蓋整個 worker 執行時間；期限到時兩側都標為 `not_checked`／`deadline`。

### 階段 2 新增

暫存：
- 暫存檔由父程序建立：`CREATE_NEW`、share 0、`FILE_FLAG_DELETE_ON_CLOSE`、可繼承的 handle，檔名 `iat-tmp-<run_id>-a|b.bin`。worker 只拿到 handle。worker 結束後父程序關閉 handle，並確認檔案已不存在；仍存在時列 `temp_cleanup_failed`（warning）。
- 啟動時只刪除名稱完全符合上述格式、且沒有被其他程序開著的殘留檔（列 `temp_stale_removed`，info）；其他檔案一律不動。
- 暫存一律在解碼前預留（受 `temp_gib` 限制，並保留 64 MiB 磁碟餘量），以 `FileAllocationInfo` 預先配置。寫入只能循序附加，讀取不能超過已寫入範圍。
- 額度或空間不足判為 `not_checked`／`resource_limit`（退出碼 4）；其他暫存錯誤列為 `environment_error`。
- 報告中的暫存目錄只在 `--full-paths` 時列出。

PNG Adam7：
- 經暫存解碼：逐 pass 讀出子圖列，以原生寬度寫入暫存（≤8 bit 每樣本 1 byte），再依 raster 順序讀回比較。12949×20167 RGB8 剛好 747 MiB，預設 `temp_gib=8` 足夠，未調整。
- 交錯只是儲存順序；兩側不同時只列 `interlace_differs`（info），不影響 strict。TIFF 沒有交錯欄位，不會觸發這個提示。
- 只設交錯旗標、資料卻不是 Adam7 的 PNG：從「不支援」（階段 1，退出碼 2）改為「解碼失敗」（退出碼 3）。階段 1 的配對已改名為 `interlace_flag_without_adam7_data`。

TIFF／BigTIFF：
- 嚴格支援範圍：contiguous、無號整數、每通道 8/16-bit、MinIsWhite／MinIsBlack／RGB、最多一個 Alpha、Predictor 1/2；壓縮只支援 none、LZW、Deflate（8 與 32946）、PackBits。
- 其他壓縮（JPEG、LZMA、ZSTD、WebP 等）：`not_checked`／`compression_not_supported`，退出碼 2，完全不解碼。
- 多頁、SubIFD、separate planes、浮點：`unsupported`，退出碼 2（`multi_image_not_supported`、`unsupported_sample_model`）。
- MinIsWhite 在同位深反轉為 MinIsBlack 後比較（Alpha 不動）。
- Alpha 語意為 associated 或未指定（ExtraSamples=1／0）時，樣本仍會比較，但 strict 最多只能是 indeterminate（原因 `original:alpha_associated`、`output:alpha_unspecified` 等）。
- 非 PNG 格式的容器警告，strict 原因寫成 `container_warning`（PNG 維持 `trailing_data`）。目前 TIFF 只有 tag 順序錯或重複（`tiff_tag_order`）與未知 tag 型別（`tiff_unknown_type`）會產生警告。
- 自有掃描器在 libtiff 之前檢查標頭、第一個 IFD、tag 值範圍、每個 strip／tile 的 offset＋byte count 與檔案大小、IFD 鏈；libtiff 讀到的目錄必須與掃描結果一致，否則判為解碼失敗（`decoder_disagrees_with_scan`）。區塊起點在檔內但超出檔尾判為 `truncated`，起點在檔外判為 `structure_error`。
- libtiff 開檔上限：單筆配置不超過 max(`buffer_mib`, `metadata_item_mib`)，累計不超過 `buffer_mib + metadata_file_mib`。觸發時（例如極多 tile 的 offset 表）判為 `not_checked`／`resource_limit`，不判為損壞。
- strip 逐條解碼；tile 以一整排為單位：一排的緩衝 ≤ `buffer_mib`/2 時放記憶體，否則逐 tile 寫入暫存，再依列重組（`decode_path` 加 `+temp`）。
- 預算估算使用 libtiff 開檔後的 strip 大小，因為 strip chopping 有開（`STRIPCHOP_DEFAULT`）。
- 階段 1 的 inspect 案例 `inspect_not_png` 以 TIFF 當「不支援的格式」；TIFF 支援後改用 WebP 魔數與未知格式兩例。

### 階段 3 新增

JPEG XL：
- 支援範圍：`uses_original_profile=true`、1–16-bit 整數、Gray／GA／RGB／RGBA；裸 codestream 與容器（`jxlc`、`jxlp`）。XYB 為 `xyb_not_native`、動畫為 `animation_not_supported`、浮點與 > 16-bit 為 `unsupported_sample_model`，都是退出碼 2，不解碼。
- 自有 box 掃描先於 libjxl：box 超出檔案、box 標頭不完整、jxlp 缺最後一段判為 `truncated`；jxlp 序號錯、jxlc 與 jxlp 並存、缺 ftyp 判為 `structure_error`。
- 裸 codestream 的尾端資料以完整解碼後未消耗的位元組數判定（`trailing_data`，warning），容器的尾端由 box 掃描涵蓋。
- 截斷：檔案全部送入後 libjxl 仍回 `JXL_DEC_NEED_MORE_INPUT`，即判為 `truncated`；不呼叫 `JxlDecoderCloseInput`（S12，2026-11-08 修訂）。前提是不訂閱 `JXL_DEC_BOX` 與 `JXL_DEC_JPEG_RECONSTRUCTION`；日後要訂閱任一個，必須重審這條截斷判定。
- Premultiplied Alpha 與非 Alpha extra channel：樣本照樣比較（extra channel 不比較），strict 最多 indeterminate。
- 輸出保留儲存方向；方向不同為 `orientation_differs`。

WebP：
- 只支援靜態 VP8L。VP8 有損（含 ALPH）為 `lossy_not_supported`，動畫為 `animation_not_supported`（尺寸取 VP8X canvas），都是退出碼 2。
- RIFF 長度大於檔案、chunk 超出 RIFF、缺補齊 byte、VP8L 截斷判為失敗（退出碼 3）。RIFF 之後的資料、VP8X 旗標與 chunk 不符、Alpha 提示不符只是警告，strict 最多 indeterminate（`container_warning`）。
- 解碼一律輸出 RGBA；VP8L 提示為 0 且 Alpha 全為 255 時以 RGB 比較，否則以 RGBA 比較（提示不可信，見階段 3 實測）。
- 16383 政策：寬或高超過 16383 為 `unsupported_dimension`（退出碼 2）；`dimension_policy` 另記規格上限 16384 與 `within_spec`，並說明這不是檔案錯誤。原檔超過 16383、輸出為 WebP 時列 `webp_policy_cannot_hold_original`（info）。

測試圖：
- JXL／WebP 測試圖的樣本、PNG 對照、容器包裝與各種損壞都由 `gen_fixtures.py` 產生；libjxl／libwebp 只用於編碼（`gen_codec_fixture`），從不用於產生真值。需要 `--codec-bin`，沒有時不產生階段 3 測試圖。
- 邊界尺寸與 Alpha 提示不符的 VP8L 依位元流規格手寫，不經 libwebp。

## 階段 0 實測得出的實作約束

| 編號 | 發現 | 約束 |
|---|---|---|
| S1 | 本機 PATH 含 `C:\strawberry_msvc_x86_64_release\bin`。CMake 曾經從 PATH 推得的 prefix 路徑找到 Strawberry 的 `TiffConfig.cmake`，執行期改載入 Strawberry 的 `tiff.dll`。 | `project()` 之後關閉 `CMAKE_FIND_USE_SYSTEM_ENVIRONMENT_PATH` 等搜尋；`cmake/IatDeps.cmake` 檢查每個依賴都位於 vcpkg 安裝目錄。交付前以 `dumpbin /dependents` 確認只依賴系統 DLL。 |
| S2 | libpng 不會察覺 IEND 之後的額外資料，`png_read_end` 照樣成功。 | 需要自有 chunk 掃描器核對 EOF；探針以讀取位置比對檔案長度取得 `trailing_bytes`。 |
| S3 | IDAT 之後的 zTXt 解壓不完整時（CVE-2026-46675 路徑），libpng 1.6.59 只發出警告 `zTXt: truncated`，影像仍成功。 | libpng 警告必須逐條保留，並對應到 metadata 的 `parse_failed`，不可丟棄。 |
| S4 | WebP 少了最後的 RIFF 補齊 byte 時，libwebp 仍回報 OK 並輸出全部列。 | WebP 的完整性不能只看解碼器狀態；RIFF 長度、chunk 長度與補齊要由容器掃描獨立判定。 |
| S5 | `TIFFOpenW` 以 `FILE_SHARE_WRITE` 開檔並可能使用 mmap。 | 一律用 `TIFFClientOpenExt` 搭配自有唯讀 handle 與 `"rm"` 模式；`on_map` 回傳 0。 |
| S6 | libtiff 的 `MaxSingleMemAlloc`／`MaxCumulatedMemAlloc` 在 4.7.2 實際生效，錯誤訊息可辨識（"beyond the ... byte limit defined in open options"）。 | 可用來把 codec 超限辨識為 `resource_limit`。 |
| S7 | libjxl 的 `JxlDecoderSetRenderSpotcolors` 預設為 true。 | worker 一律關閉；extra channel 不是 Alpha 時不給嚴格通過。 |
| S8 | libjxl 編碼器不接受「UINT16 輸入搭配 8-bit 位元流」（錯誤碼 GENERIC）。 | 只影響測試圖產生：8-bit 真值以 UINT8 傳入。 |
| S9 | Job 記憶體超限時，completion port 收到 `PROCESS_MEMORY_LIMIT`；worker 的配置失敗與這個訊息可對應。 | `resource_limit` 只在 worker 自己回報配置失敗時成立；Job 訊息作為佐證寫入說明。worker 異常結束時一律回報 `worker_crashed`，即使收到 Job 訊息也不猜測是資源問題。 |
| S10 | libpng 1.6.59 的 `png_handle_iCCP` 先讀 81 bytes 關鍵字區，剩餘不足 11 bytes 時以 "too short" 拒絕，所以 **iCCP 內容少於 92 bytes 會被拒**（`pngrutil.c` 第 1350–1366 行）。 | 自有掃描器仍會解壓並記錄 ICC 雜湊，但 libpng 拒絕時有效色彩為 `icc_unreadable`、color 為 `unverified`，並列 `icc_rejected_by_decoder`。測試用的合成 ICC 以 zlib 不壓縮方式儲存，避開這個條件。 |
| S11 | （階段 2）libtiff 4.7.2 預設開啟 strip chopping（`STRIPCHOP_DEFAULT`），未壓縮的單一 strip 會被切成多條。 | 預算與幾何一律以 libtiff 開檔後的 `TIFFStripSize64`／`TIFFNumberOfStrips` 為準，並與自有掃描交叉核對。 |
| S12 | （階段 3）libjxl 0.12.0 的標頭掃描（只訂閱 BASIC_INFO／COLOR_ENCODING／FRAME）跳過 frame 資料時，那些位元組不算已消耗，`JxlDecoderReleaseInput` 會回報幾乎整個檔案未消耗。另外，先呼叫 `JxlDecoderCloseInput` 再遇到截斷，回傳的是一般的 `JXL_DEC_ERROR`，不是 `NEED_MORE_INPUT`。2026-11-08 核對：`decode.cc` 第 2194 行在輸入已關閉時把 `NEED_MORE_INPUT` 轉成 `JXL_INPUT_ERROR("premature end of input")`，但 `decode.h` 沒有保證這個回傳值，而 `JXL_DEC_ERROR` 與資料損壞無法區分。`decode.h` 有寫明的是：`NEED_MORE_INPUT` 表示需要更多位元組；不訂閱 `JXL_DEC_BOX` 時 `JxlDecoderCloseInput` 可省略，解碼器自行判斷 frame 結尾。 | 尾端資料改由完整解碼後量測。截斷判定（2026-11-08 修訂）：整個檔案都送入後仍回 `NEED_MORE_INPUT` 即為 `truncated`，不呼叫 `JxlDecoderCloseInput`，只依賴文件化行為。本工具不訂閱 `JXL_DEC_BOX`、`JXL_DEC_JPEG_RECONSTRUCTION`，所以完整檔案不會在結尾要求資料。**這是新判定的前提**：訂閱 `JXL_DEC_BOX` 時 `decode.h` 要求呼叫 `JxlDecoderCloseInput`，訂閱 JPEG 重建時解碼器可能為了 jbrd 所需的 Exif／XMP box 在結尾要求資料（`decode.cc` 的 `JbrdNeedMoreBoxes`），兩種情況下完整檔案都可能回 `NEED_MORE_INPUT`。日後訂閱任一個，必須重審本條並重跑 JXL 截斷與完整檔案配對。修訂前的做法（用盡才關閉、之後的錯誤判為截斷）在 0.12.0 上結果相同；修訂後 JXL 配對、inspect 與 `jxlbig` 全部重跑通過。 |
| S13 | （階段 3）libjxl 編碼器處理 > 12-bit 的無損影像時，自動輸出含 `jxll`（level 10）的容器；多 frame 時用 `jxlp` 分段。 | 只影響測試圖產生：`gen_fixtures.py` 取出 codestream，容器一律由 Python 自行組出。 |
