# 依賴鎖定紀錄

狀態：**已確認**（2026-11-07 使用者核准；libpng 採 1.6.59#1）。全部版本已用實際解析確認。

紀錄日期：2026-11-07。用途：第三方清單、THIRD_PARTY_NOTICES 與授權材料的來源依據。

## 解析環境

| 項目 | 值 |
|---|---|
| vcpkg 工具 | 2026-07-27-98d7cb0cf1f4686a3e43aa5672b6230c1d56bce8（VS 2026 隨附，未升級／切換）；2026-11-17 起 VS 更新為 2026-09-26-51bf87ca6e9bf3e622d84ff323bd202ab1ca0c0b |
| `builtin-baseline` | `1460b31b08c42cc2e9ac2c79f45ec8707e2675e2`（本機 `vcpkg-bundle.json` 的 `embeddedsha`，microsoft/vcpkg 2026-07-27）。VS 更新後 `embeddedsha` 變為 `6ade29bb…`，**baseline 依使用者決定維持不變**（git registry 模式仍可解析） |
| registry | 隨附版的 git registry 模式（`usegitregistry: true`），`https://github.com/microsoft/vcpkg` |
| triplet | `x64-windows-static-iat`（overlay；static CRT、static lib、release only、v145 / 14.52；2026-11-17 前為 14.51） |
| 編譯器 | MSVC 14.52.37025，`cl.exe` 19.52.37029（2026-11-17 起；之前為 14.51.36231／19.51.36260，該工具集已從本機移除） |
| 驗證方式 | `vcpkg install --dry-run`，在暫存目錄以草稿 manifest 實際解析，未建置、未寫入專案 |

`vcpkg-configuration.json` 不得設定 `default-registry`。vcpkg-tool 2026-07-27 的訊息 `AttemptingToSetBuiltInBaseline` 指出，兩者並存時會改用 `default-registry` 而忽略 `builtin-baseline`。設定檔只保留 `overlay-triplets`。

## 直接依賴（overrides 鎖定）

| 埠 | baseline 版本 | 鎖定版本 | port git-tree | 上游來源 | 上游 tag commit | 來源 SHA512 | 授權（port 宣告） | port patches | 啟用 features |
|---|---|---|---|---|---|---|---|---|---|
| libjxl | 0.11.2 | **0.12.0#1** | `30481191c0e65e92a71840803a1d0c90191ce2c1` | github.com/libjxl/libjxl `v0.12.0` | `a7a9c787341cf703dede03c2009fa460cae5e5df` | `71d45123e47588c8028c0e01a7820c7a18d98df05c29b5318ab6858eaff5f249879763907201ba19a93f817c6ef75c393c1974d2a0cc8e461c4e2709ca6fa45f` | BSD-3-Clause | fix-dependencies.patch、fix-jxl-static.diff、fix-pkgconfig.diff（2026-11-17 更正：先前只記了第一個） | core（不開 tools） |
| tiff | 4.7.2 | 4.7.2 | `a9a8ba148c5c2295dbcf22fe4f6ee5de135b0e33` | gitlab.com/libtiff/libtiff `v4.7.2` | `d01a94be176f5f6a87f7ee1c0b32e65416aa2b4d` | `c4dcde3c79e5d69c7231f8862e2e5a83d90d9cce694fb2a4804800b2f8f1bc9db504b9252d81dce872eec8358b33a3a1dbdddcbb6181f6fb8d1d7fc0e9a9fc6a` | libtiff | FindCMath.patch、jpeccodec.patch | core、zip |
| libwebp | 1.6.0#2 | 1.6.0#2 | `5366c6ab4c6a5ece31ab06309b9729d29a5d4da1` | github.com/webmproject/libwebp `v1.6.0` | `4fa21912338357f89e4fd51cf2368325b59e9bd9` | `298e0ad4c09392213baf5abb69d330c6203b618800073fe2df91d01d35034197c5d3e29a74573b06971473c52c74514f0e6e0f6c8162f923e2dd15cb1a692aef` | BSD-3-Clause | 0002-cmake-config、0003-simd、0008-sdl、0009-cpufeatures-android | core、simd、unicode（見備註） |
| libpng | 1.6.58 | **1.6.59#1** | `129ad6200060f2e0cce22d36b954d7dff9dc4f8a` | github.com/pnggroup/libpng `v1.6.59` | `cd952f49f95bb27154ae77dbb103032d95f6e580` | `2d753e764846a0a75a10f2a00c6f4ae23d2ae37237c69d59d5e619da3f143d59f4ea9ce29c1fb39435df1cef77877c83b042a2275934c353a6603061da2d2dd9` | libpng-2.0 | cmake.patch、genout-compiler-target.patch | core（不開 apng、tools） |

libpng 的 portfile 另列一個 SHA512 給 APNG patch。本專案不開 `apng`，因此不會下載該 patch；上表只列原始碼封存檔的雜湊。

## 傳遞依賴（由 baseline 解析，未 override）

| 埠 | 版本 | port git-tree | 上游 tag commit | 來源 SHA512 | 授權（port 宣告） | 引入者 | 是否進入執行檔 |
|---|---|---|---|---|---|---|---|
| zlib | 1.3.2#1 | `f49b05c666425471a7f6dc8b6156e9b3675a3c90` | madler/zlib `v1.3.2` → `da607da739fa6047df13e66a2af6b8bec7c2a498` | `16fea4df307a68cf0035858abe2fd550250618a97590e202037acd18a666f57afc10f8836cbbd472d54a0e76539d0e558cb26f059d53de52ff90634bbf4f47d4` | Zlib | libpng、tiff[zip] | 是 |
| brotli | 1.2.0 | `0fb79bb13588702a9e06057745b4aed95ce5c861` | google/brotli `v1.2.0` → `028fb5a23661f123017c060daa546b55cf4bde29` | `f94542afd2ecd96cc41fd21a805a3da314281ae558c10650f3e6d9ca732b8425bba8fde312823f0a564c7de3993bdaab5b43378edab65ebb798cefb6fd702256` | MIT | libjxl | 是 |
| highway | 1.4.0 | `4a44183703a0d4f07d01e3731d7bb50ab11d4f6c` | google/highway `1.4.0` → `2607d3b5b0113992fe84d3848859eae13b3b52c1` | `819422857d6a74e3a936c402698e078db5b7b88fb43767e62429ec7bd954fe93b017e75029a4df4a1a97ef3a2486107eef5247da751eb487640dea409f3f2fa2` | Apache-2.0 | libjxl | 是 |
| lcms | 2.19.1#1 | `18ab2c77128f44e6439741effc3f62ccaa9f36f7` | mm2/Little-CMS `lcms2.19.1` → `21c582a594fe5279f90c0b93437c398f93bf62b0` | `1b2781ed8898e65f15be17cf0130a1500ec0bf5ca5159f871dff5692e387747be9526feef0bc7c370200656fc0aabe3036746041285a3978e90adec200d685f2` | MIT | libjxl | 否（在連結命令列上，但 linker map 中沒有任何物件：本工具不設定 CMS；2026-11-08 確認，見 `third-party.md`） |
| nlohmann-json | 3.12.0#2 | `ac3b8821cf486e45dd543bef2a4ba1d1ba230258` | nlohmann/json `v3.12.0` → `55f93686c01528224f448c19128836e7df245f72` | `6cc1e86261f8fac21cc17a33da3b6b3c3cd5c116755651642af3c9e99bb3538fd42c1bd50397a77c8fb6821bc62d90e6b91bcdde77a78f58f2416c62fc53b97d` | MIT | 專案直接使用 | 是（header-only） |
| doctest | 2.5.3 | `0803364e7e4a64135c72f47bf4670ca89a964bc2` | doctest/doctest `v2.5.3` → `2d0a9359a60c51affe2a9bebb1be1dca47868151` | `d7222ae62cf2ac0f858567af0e539bd34cef47827004f805261007943e8e02370fcf16cbf0302581766353bf28317dd280a885fdc775baaa3e519aa2cf2f2eca` | MIT | 測試 | 否（僅測試執行檔） |

### 僅建置期使用的 vcpkg 輔助埠（不散布）

| 埠 | 版本 | port git-tree | 引入者 |
|---|---|---|---|
| vcpkg-cmake | 2024-04-23 | `e74aa1e8f93278a8e71372f1fa08c3df420eb840` | 多數 port |
| vcpkg-cmake-config | 2026-07-21 | `51befcb03097f8d19c1c59f9ec3625e69d85322a` | 多數 port |
| vcpkg-cmake-get-vars | 2025-05-29 | `d6510f888dd526c7828f6b973349e7f30be51254` | 多數 port |
| vcpkg-tool-meson | 1.9.0#10 | `86a0d388e3e72719eadc392e359ee80f2f8d8a16` | lcms（以 meson 建置） |

## Feature 核對（鎖定的 port 版本）

- **tiff 4.7.2 `zip`**：存在。說明為 "Support ZIP/deflate compression in TIFF image files"，依賴 zlib；portfile 對應 `zip → zlib` 並要求 `CMAKE_REQUIRE_FIND_PACKAGE_ZLIB`。預設 features 為 jpeg、lzma、zip；設 `default-features:false` 後，實際解析為 `tiff[core,zip]`。lzma、jpeg、webp、zstd、lerc、libdeflate 均未啟用。LZW 與 PackBits 屬 libtiff 內建 codec，不受 feature 控制，建置後要以 `TIFFIsCODECConfigured` 實測。
- **libwebp 1.6.0#2 `simd`**：存在。說明為 "Enable any SIMD optimization."；portfile 對應 `simd → WEBP_ENABLE_SIMD`，另有 `0003-simd.patch`。預設 features 為 libwebpmux、nearlossless、simd；設 `default-features:false` 後未啟用 libwebpmux、nearlossless。
- **libwebp `unicode`（非本專案要求）**：port 在 `platform: windows` 時自我依賴 `libwebp[unicode]`，所以 Windows 一律帶出這個 feature。它只對應 `WEBP_UNICODE`，影響 libwebp 自帶的範例與工具程式；本專案不建置那些程式，對函式庫解碼路徑沒有作用。
- **libjxl 0.12.0#1**：只有 `tools` 一個 feature，不開。
- **libpng**：只有 `apng`、`tools`，兩者都不開。

## 與原計畫的差異

| 項目 | 原計畫（2026.10.16 baseline） | 現況（本機 vcpkg baseline + overrides） | 原因 |
|---|---|---|---|
| baseline | `9eec002d…`（2026.10.16） | `1460b31b…`（本機 embeddedsha） | 使用者要求用本機實際 commit |
| libjxl | 0.12.0#0（兩個 patch） | 0.12.0#1（fix-dependencies.patch、fix-jxl-static.diff、fix-pkgconfig.diff；2026-11-17 以 port git-tree 核對更正） | baseline 只有 0.11.2，因此 override 到 0.12.0。選 #1 是因為 vcpkg 2026-10-27 修了 pkg-config（#54570），並移除 msvc-remove-libm.patch；上游原始碼與 SHA512 和 #0 相同 |
| libpng | 1.6.59#0 | 1.6.59#1 | baseline 只有 1.6.58，以 override 取得 1.6.59，原因見「libpng：1.6.58 與 1.6.59 的差異」 |
| libwebp | 1.6.0#3 | 1.6.0#2 | baseline 版本；上游原始碼與 SHA512 相同，只差 port 修訂 |
| zlib | 1.3.2#2 | 1.3.2#1 | baseline 版本 |
| 原計畫的 libpng「commit」 | `a543b51f…` | — | 那其實是 annotated tag 物件，不是 commit；正確 commit 見上表 |

### libjxl：0.11.2 與 0.12.0 的差異

- baseline 解析出的 libjxl 是 **0.11.2**，不是目標 0.12.0，因此必須 override。實測 override 可以取得 baseline 之後才加入的版本，解析成功。
- 0.12.0 與 IMB-C v1.3.0 驗收紀錄的 libjxl 同版，也是 2026-11-07 上游最新版。0.11.2 同樣有本專案需要的 decode API（`SetImageOutCallback`、`SetImageOutBitDepth`、`SetKeepOrientation`、`SetRenderSpotcolors`），但先前的原始碼核對是針對 0.12.0 做的，所以採用 0.12.0。
- 依賴需求相同（brotli、highway、lcms），解析時沒有版本衝突。

### libpng：1.6.58 與 1.6.59 的差異

baseline 解析出 1.6.58。1.6.59（2026-09-28）修正了：

- **CVE-2026-46675（medium）**：zTXt、iTXt 或 iCCP 解壓不完整時，`png_read_end` 使用已釋放的 zlib 輸入（use-after-free）。
- 配置失敗時 `png_struct` 成員被重複釋放。
- 1.6.47 起誤拒位置正確（PLTE 之後）的 hIST chunk。

本工具會對截斷或損壞的 PNG 呼叫 `png_read_end`，正好落在 CVE 的觸發路徑上，因此以 override 鎖 1.6.59#1（已實測可解析）。#1 比 #0 多一個 genout-compiler-target.patch（OHOS 支援，Windows 不受影響）。

## 0.5.0 新增（2026-11-17，使用者確認，D31）

`builtin-baseline` 不變；解析結果以 `vcpkg install`（MSVC 14.52 triplet）實測。既有依賴的版本與 git-tree 全部不變，只因工具集改為 14.52 而重建。

| 埠 | 版本 | port git-tree | 上游來源 | 來源雜湊 | 授權 | 引入方式 | 進入執行檔 |
|---|---|---|---|---|---|---|---|
| libavif | 1.4.2#1（baseline，不需 override） | `cad3f5196a32c420fb9aebfc2a8a5876e5c36129` | github.com/AOMediaCodec/libavif `v1.4.2` | SHA-512 `cbf31827884058acc54d3b1fa0f2059f022691609a76e0f913981d5b8c1be60f52069a25b61c888c09e963161d1f4c9bf692c0716ab0ee067a4ccaa4e36d9ce1` | BSD-2-Clause（port 宣告另含 Apache-2.0，只涵蓋未連結的 android 檔案） | 直接，`default-features:false`、`features:[aom]` | 是 |
| aom | **3.15.2**（override；baseline 為 3.13.3） | `af714ed6f6df20bc728bd916954f4a50e99a9aef` | aomedia.googlesource.com/aom commit `af3dc9aadc793c1b00edc003e3fc19a44fdeb574`（`vcpkg_from_git`） | vcpkg 匯出封存檔 SHA-512 `876673d0…`（全文見 `third-party.md`） | BSD-2-Clause ＋ AOM Patent License 1.0 | libavif[aom]；版本與 IMB-C 1.3.3 的 libaom 相同 | 是（含編碼器：libavif 的 codec 表同時引用，`/OPT:REF` 無法移除） |
| libyuv | 1916（傳遞） | `557f40e7241d91a80b55f849a18376613c45f13a` | chromium.googlesource.com/libyuv commit `d98915a654d3564e4802a0004add46221c4e4348`（`vcpkg_from_git`） | vcpkg 匯出封存檔 SHA-512 `b1d76b6e…` | BSD-3-Clause ＋ PATENTS（port 的 `license` 欄位為 null，以上游為準） | libavif 必帶 | 是（map 中 10 個物件） |
| libjpeg-turbo | 3.2.0（傳遞） | `c7d2d8cbb868863a698862a0da02b4af225f3c72` | github.com/libjpeg-turbo `3.2.0` | SHA-512 `1ce063e9…` | IJG／BSD-3-Clause／Zlib | libyuv 的 port 強制依賴 | 否（map 中沒有物件，不散布） |

建置期另外由 vcpkg 取得 nasm 3.01（aom 的組譯器），不散布。

**沒有採用 dav1d**：dav1d 1.5.3（baseline）與 1.5.4 在 MSVC 14.52 與 14.53 都編譯失敗：`include/compat/msvc/stdatomic.h` 第 35 行 `#if defined(__clang__) && __has_include_next(<stdatomic.h>)`，14.52 起把 `#if` 裡未定義的函式型巨集視為錯誤 C7780。使用者決定不維護 overlay port 修補（Q2）；上游修好後可再評估改用 dav1d（worker 約可少 7 MB）。

## 散布清單與授權材料（階段 4）

實際散布哪些元件、各自的連結方式與隨附材料，以 `third-party.md` 為準。該檔、專案根目錄的 `THIRD_PARTY_NOTICES.txt` 與 `licenses/` 都由 `tools/gen_third_party.py` 產生，依據是 vcpkg 安裝目錄的 `share/<port>/copyright`、`vcpkg.spdx.json`，以及 Release 連結時輸出的 linker map（`build/release/bin/iat.map`、`iat-worker.map`）。依賴或建置設定變更後要重跑產生器；封包前以 `--check` 確認沒有落差。

2026-11-08 的結果：lcms、jxl_cms、jxl_threads、brotlienc、libsharpyuv、libwebpdecoder、libwebpdemux 在連結命令列上或已安裝，但 map 中沒有物件，不散布；doctest 只連進 `iat_unit.exe`，不散布。
