# 開發說明

本檔是 0.5.1 以前的 `README.md`，1.0.0 起移到這裡（D35）；對外介紹改在根目錄 `README.md`。規格修訂與設計決定見 `docs/decisions.md`（開發規劃書本身未公開，見 `docs/README.md`），0.x 的變更紀錄見 `docs/changelog-0.x.md`。

程式結構（1.0.0 起，D34）：`iat-core`（靜態庫，`src/core/`、`src/model/`、`src/common/`）負責一次分析的完整流程與報告；`iat.exe`（`src/iat_main.cpp`）只解析參數、呼叫 core、輸出報告並決定退出碼；`iat_engine`（`src/engine/`，所有解碼器）只連進 `iat-worker.exe`，在 Job Object 內解碼。

授權：MIT（`LICENSE`）；第三方元件見 `THIRD_PARTY_NOTICES.txt`、`licenses/` 與 `docs/third-party.md`。

## 使用

```powershell
iat compare 原檔.png 輸出檔.png --report-md 報告.md --report-json 報告.json
iat inspect 檔案.png --stdout md
iat compare a.png b.png --stdout json --limits deadline_s=1200 buffer_mib=128
iat compare 原檔.png 輸出檔.tif --report-json 報告.json --temp-dir D:\iat-temp
```

- 支援範圍：PNG（非交錯與 Adam7）；TIFF／BigTIFF 的 contiguous 8/16-bit Gray／GA／RGB／RGBA，壓縮限 none、LZW、Deflate、PackBits；JPEG XL 的原生樣本路徑（`uses_original_profile=true`，1–16-bit，裸 codestream 或容器，非動畫）；WebP 靜態 VP8L（每邊最多 16383）。XYB、VP8 有損、動畫等明確回報不支援，詳見 `docs/capability-matrix.md`。
- 色彩：JXL 的色彩 enum 只與明示相同 CICP 的檔案，或與 libjxl 合成 ICC 逐 byte 相同的嵌入 ICC 判為相等；WebP 沒有 ICCP 時只當作規格預設 sRGB，不與其他格式判為相等（`docs/decisions.md` D12）。
- 暫存：Adam7、過寬的 tile 排與 JXL／WebP 的整圖解碼會把解碼後樣本寫入 delete-on-close 暫存檔，預設放在系統暫存目錄，可用 `--temp-dir` 指定；總量受 `temp_gib` 限制。
- JXL／WebP 的解碼器記憶體硬上限是 `job_commit_mib` − 64 MiB（預設 960 MiB，約 69 Mpx RGB 或 52 Mpx RGBA 的 JXL）；超過時為「未檢查」，退出碼 4。

- 輸入只以唯讀方式開啟；報告只寫入新檔，既有檔案一律拒絕。
- 退出碼：0 嚴格一致或檢查成功、1 嚴格差異、2 不可判定或不支援、3 解碼或結構失敗、4 資源上限／期限／取消（未檢查）、5 參數／I/O／worker／環境錯誤。
- 限額：`job_commit_mib`（1024）、`buffer_mib`（64）、`metadata_item_mib`（8）、`metadata_file_mib`（32）、`temp_gib`（8）、`deadline_s`（600）。
- 契約詳見 `docs/cli-json-contract.md`；支援範圍與對應測試見 `docs/capability-matrix.md`。

## 測試

`ctest --preset release`（本機 preset 則用同名的 test preset，例如 `ctest --preset local`）依序執行 `unit`、`fixtures`（產生下方的 `tests/fixtures/generated`）與 `e2e`（全部小檔案例）。`fixtures` 與 `e2e` 需要 PATH 上有 Python launcher `py`（Python 3.13）；找不到時 CMake 設定階段會發出警告，只註冊 `unit`。個別步驟如下：

```powershell
# --codec-bin 指向建置輸出（含 gen_codec_fixture.exe）。省略時不產生 JXL／WebP／AVIF 測試圖，相關配對會失敗。
py -3.13 -I tools/gen_fixtures.py tests/fixtures/generated --codec-bin build/release/bin
build/release/bin/iat_unit.exe
py -3.13 -I tests/e2e/run_e2e.py build/release/bin tests/fixtures/generated build/e2e
# 大圖（PNG、Adam7、超大 TIFF/BigTIFF、稀疏 >4 GiB BigTIFF；約 3 GB 磁碟空間）：
py -3.13 -I tests/e2e/run_e2e.py build/release/bin tests/fixtures/generated build/e2e-big --big build/iat-big --only "^(big_|tiffbig_)" --groups big,tiffbig
# 大圖（JXL 4096×4096 RGB16、WebP 8192×8192 RGBA8；約 170 MB 磁碟空間，第一次會產生輸入）：
py -3.13 -I tests/e2e/run_e2e.py build/release/bin tests/fixtures/generated build/e2e-s3big --big build/iat-big/s3 --only "^(jxlbig|webpbig)" --groups jxlbig,webpbig
```

```powershell
# 真實樣本（0.5.0）：唯讀檢查一個資料夾內的 PSD／PSB 與最大的 PNG，並以 IMB-C 的 ImageMagick（唯讀呼叫）
# 產生對照 PNG 與 AVIF。資料夾與 IMB-C runtime 在前後各算一次 SHA-256，輸出只寫到最後一個參數的資料夾。
py -3.13 -I tests/e2e/run_real_samples.py build/release/bin <本機測試圖片集> <IMB-C>\target\release\ImageMagick\magick.exe build/real [--full-avif]
```

CLI 回歸比對（與先前保存的執行結果逐案例比對 stdout、stderr、JSON、Markdown、退出碼）見 [`tools/regress/README.md`](../tools/regress/README.md)。

測試圖產生器 `gen_codec_fixture`、`gen_large_png`、`gen_large_tiff` 的原始碼在 `tests/gen/`，隨主建置一起產生在 `build/release/bin/`。

可以刪除、需要時重產的資料（都在 `.gitignore` 的範圍內）：

| 資料 | 重產方式 |
|---|---|
| `tests/fixtures/generated/` | `tools/gen_fixtures.py`（上方第一行） |
| `--big` 指定的資料夾（大圖輸入，約 2.7 GB；0.5.0 驗收時用 `build/iat-big`） | `run_e2e.py --big <資料夾>` 在檔案不存在時自動以 `gen_large_png.exe`、`gen_large_tiff.exe`、`gen_codec_fixture.exe` 產生，之後重複使用 |
| `run_real_samples.py` 的輸出資料夾（報告、裁切 PNG、AVIF） | 重跑同一指令；需要原樣本資料夾與 IMB-C runtime |
| `build/e2e*` | 每次執行 `run_e2e.py` 時先清空再寫入 |

PSD／PSB 測試圖由 Python 依 Adobe 檔案格式規格逐位元組寫出。JXL／WebP／AVIF 測試圖的樣本與 PNG 對照由 Python 產生；`gen_codec_fixture` 只用 libjxl／libwebp／libavif 的編碼 API 把同一份樣本編碼，從不解碼（無損 AVIF 直接把樣本寫入 Y／U／V 平面，不經 RGB→YUV 轉換）。容器與損壞也由 Python 依規格組出。

## 建置

```powershell
pwsh -File scripts/build.ps1                  # preset "release"（CMakePresets.json）
pwsh -File scripts/build.ps1 -Preset local    # 本機 preset（CMakeUserPresets.json）
```

- 鎖定 MSVC 14.52 與 Windows SDK 10.0.26100.0，環境只注入子程序。`build.ps1` 載入 vcvars 後會檢查實際的 `VCToolsVersion`（必須是 14.52.x）與 `cl.exe`，不符合即以退出碼 1 停止（`vcvarsall` 在工具集不存在時仍回傳 0，不能依賴）。
- Ninja 從 PATH 取得；vcpkg 由 MSVC 開發環境設定的 `VCPKG_ROOT`（VS 隨附的 vcpkg）提供，`builtin-baseline` 對應這份 vcpkg。
- 依賴由 `vcpkg.json` 的 `builtin-baseline` 與 `overrides` 鎖定；版本、來源、雜湊見 `docs/dependencies.md`。首次建置需要連網。
- 本機設定：
  - `CMakeUserPresets.json`（不進 git；範本 `CMakeUserPresets.example.json`）：例如把 vcpkg 的 installed、buildtrees、packages 放到固定的快取資料夾，避免每個 build 資料夾各裝一份。
  - 環境變數 `IAT_VCPKG_CACHE`：vcpkg 的二進位快取（`archives`）、下載（`downloads`）與 registries 快取的根資料夾。未設定時用 vcpkg 的預設位置。
  - 環境變數 `IAT_VS`（Visual Studio 根目錄）、`IAT_CMAKE`（`cmake.exe`）：安裝在非預設位置時使用。

## 封包

```powershell
pwsh -File scripts/build.ps1
pwsh -File scripts/package.ps1 -SpdxTemplates <SPDX 授權範本資料夾> [-Downloads <vcpkg downloads 資料夾>]
```

`package.ps1` 先以 `tools/gen_third_party.py --check` 確認第三方清單與目前的建置（linker map）一致，再產生 `dist/ImageAnalysisTool-<版本>-win-x64.zip`。vcpkg 的安裝樹從 `build/release/CMakeCache.txt` 的 `VCPKG_INSTALLED_DIR` 讀取；`-Downloads` 預設依序為 `VCPKG_DOWNLOADS`、`IAT_VCPKG_CACHE\downloads`、`%LOCALAPPDATA%\vcpkg\downloads`（需含 libjxl 的原始碼封存檔）；`-SpdxTemplates` 也可用環境變數 `IAT_SPDX_TEMPLATES` 指定（內含 `MIT.txt`、`BSD-3-Clause.txt` 等 SPDX 範本）。內容：`iat.exe`、`iat-worker.exe`、`README.md`、`README.zh-TW.md`、`USAGE.md`、`CHANGELOG.md`、`LICENSE.txt`、`THIRD_PARTY_NOTICES.txt`、`licenses/`、`SHA256SUMS.txt`（1.0.0 起加入 README 與 CHANGELOG，D35、D36）。執行階段為靜態 CRT，不附 redistributable（D16）；封包前檢查兩個執行檔沒有匯入 VC++／UCRT 執行階段 DLL。依賴或建置設定變更後，先重跑 `py -3.13 -I tools/gen_third_party.py . --installed <VCPKG_INSTALLED_DIR> --bin build/release/bin --downloads <downloads> --spdx-templates <SPDX 範本資料夾>`。

## 測試圖產生器

測試圖產生器（`tools/gen_fixtures.py`）只用 Python 標準函式庫，不需要 `.venv`（本機以 Python 3.13 執行）。階段 0 的可行性探針（`probes/`、`tools/run_probes.py`）是內部紀錄，未收錄於公開 repo；結論見 `docs/capability-matrix.md`、`docs/decisions.md`。
