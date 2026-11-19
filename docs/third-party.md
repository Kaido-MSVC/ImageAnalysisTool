# 第三方元件清單

由 `tools/gen_third_party.py` 依 vcpkg 安裝目錄（`share/<port>/copyright`、`vcpkg.spdx.json`、`vcpkg/info/*.list`）與 Release 連結的 linker map（`build/release/bin/iat.map`、`iat-worker.map`）產生，請勿手改；重跑產生器更新。
「散布」以 map 為準：該 port 的 lib 至少有一個物件連進 `iat.exe` 或 `iat-worker.exe`，header-only 則看符號。

| 元件 | 版本 | 授權 | 用途 | 連結方式 | 散布 | 隨附材料 |
|---|---|---|---|---|---|---|
| aom | 3.15.2 | BSD-2-Clause；AOM Patent License 1.0（已含在 LICENSE.txt） | 執行檔 | 靜態連結：aom → iat-worker.exe | 是 | `licenses/aom/LICENSE.txt`、（PATENTS 已含在 LICENSE.txt） |
| brotli | 1.2.0 | MIT | 執行檔 | 靜態連結：brotlicommon → iat-worker.exe；brotlidec → iat-worker.exe（未使用：brotlienc） | 是 | `licenses/brotli/LICENSE.txt`、`licenses/brotli/NOTICES-files.txt` |
| doctest | 2.5.3 | MIT | 僅測試（iat_unit.exe） | 靜態，只連進測試執行檔 | 否 | — |
| highway | 1.4.0 | Apache-2.0 OR BSD-3-Clause（雙授權；hwy/contrib/random 為 CC0-1.0，未連結）。本工具依 BSD-3-Clause 散布 | 執行檔 | 靜態連結：hwy → iat-worker.exe | 是 | `licenses/highway/LICENSE.txt`、`licenses/highway/NOTICES-files.txt` |
| lcms | 2.19.1#1 | MIT | 未使用 | 連結命令列上有，但 map 中沒有任何物件 | 否 | — |
| libavif | 1.4.2#1 | BSD-2-Clause（port 宣告 BSD-2-Clause AND Apache-2.0；Apache-2.0 只涵蓋未連結的 android_jni/gradlew*） | 執行檔 | 靜態連結：avif → iat-worker.exe | 是 | `licenses/libavif/LICENSE.txt`、`licenses/libavif/NOTICES-files.txt` |
| libjpeg-turbo | 3.2.0 | BSD-3-Clause | 未使用 | 連結命令列上有，但 map 中沒有任何物件 | 否 | — |
| libjxl | 0.12.0#1 | BSD-3-Clause | 執行檔 | 靜態連結：jxl → iat-worker.exe（未使用：jxl_cms、jxl_threads） | 是 | `licenses/libjxl/LICENSE.txt`、`licenses/libjxl/PATENTS.txt`、`licenses/libjxl/AUTHORS.txt` |
| libpng | 1.6.59#1 | libpng-2.0 | 執行檔 | 靜態連結：libpng16 → iat-worker.exe | 是 | `licenses/libpng/LICENSE.txt` |
| libwebp | 1.6.0#2 | BSD-3-Clause | 執行檔 | 靜態連結：libwebp → iat-worker.exe（未使用：libsharpyuv、libwebpdecoder、libwebpdemux） | 是 | `licenses/libwebp/LICENSE.txt`、（PATENTS 已含在 LICENSE.txt） |
| libyuv | 1916 | BSD-3-Clause（port 未宣告 license，依上游 LICENSE）；附 PATENTS | 執行檔 | 靜態連結：yuv → iat-worker.exe | 是 | `licenses/libyuv/LICENSE.txt`、`licenses/libyuv/PATENTS.txt` |
| nlohmann-json | 3.12.0#2 | MIT | 執行檔 | header-only，編進 iat.exe、iat-worker.exe | 是 | `licenses/nlohmann-json/LICENSE.txt` |
| tiff | 4.7.2 | libtiff | 執行檔 | 靜態連結：tiff → iat-worker.exe | 是 | `licenses/tiff/LICENSE.txt`、`licenses/tiff/NOTICES-files.txt` |
| zlib | 1.3.2#1 | Zlib | 執行檔 | 靜態連結：zs → iat-worker.exe | 是 | `licenses/zlib/LICENSE.txt` |
| MSVC 執行階段（UCRT、vcruntime、C++ 標準程式庫） | MSVC 14.52 | Visual Studio 授權條款 | 執行檔 | 靜態 CRT（`/MT`；map：libcmt、libcpmt、libucrt、libvcruntime） | 是（編入執行檔） | — |
| Windows 系統程式庫 | — | 作業系統元件 | 執行檔 | 匯入 DLL（bcrypt、kernel32） | 否 | — |
| 測試圖、IMB-C 樣本 | — | — | 僅測試 | — | 否 | — |

## 來源與雜湊

| 元件 | 上游來源 | 來源封存檔 SHA-512 | vcpkg port |
|---|---|---|---|
| aom | git+https://aomedia.googlesource.com/aom@af3dc9aadc793c1b00edc003e3fc19a44fdeb574 | `876673d02a4ed4d7c24ec95ebd83cd720b15fb8871c35f94c36d9c966d077282f55bacd8b5f27c25470a48ba28d47a3f0936b87247d5915ad01f18cecabcc0fc`（vcpkg 依 git commit af3dc9aadc793c1b00edc003e3fc19a44fdeb574 匯出的封存檔） | git+https://github.com/microsoft/vcpkg@af714ed6f6df20bc728bd916954f4a50e99a9aef |
| brotli | git+https://github.com/google/brotli@v1.2.0 | `f94542afd2ecd96cc41fd21a805a3da314281ae558c10650f3e6d9ca732b8425bba8fde312823f0a564c7de3993bdaab5b43378edab65ebb798cefb6fd702256` | git+https://github.com/microsoft/vcpkg@0fb79bb13588702a9e06057745b4aed95ce5c861 |
| doctest | git+https://github.com/doctest/doctest@v2.5.3 | `d7222ae62cf2ac0f858567af0e539bd34cef47827004f805261007943e8e02370fcf16cbf0302581766353bf28317dd280a885fdc775baaa3e519aa2cf2f2eca` | git+https://github.com/microsoft/vcpkg@0803364e7e4a64135c72f47bf4670ca89a964bc2 |
| highway | git+https://github.com/google/highway@1.4.0 | `819422857d6a74e3a936c402698e078db5b7b88fb43767e62429ec7bd954fe93b017e75029a4df4a1a97ef3a2486107eef5247da751eb487640dea409f3f2fa2` | git+https://github.com/microsoft/vcpkg@4a44183703a0d4f07d01e3731d7bb50ab11d4f6c |
| lcms | git+https://github.com/mm2/Little-CMS@lcms2.19.1 | `1b2781ed8898e65f15be17cf0130a1500ec0bf5ca5159f871dff5692e387747be9526feef0bc7c370200656fc0aabe3036746041285a3978e90adec200d685f2` | git+https://github.com/microsoft/vcpkg@18ab2c77128f44e6439741effc3f62ccaa9f36f7 |
| libavif | git+https://github.com/AOMediaCodec/libavif@v1.4.2 | `cbf31827884058acc54d3b1fa0f2059f022691609a76e0f913981d5b8c1be60f52069a25b61c888c09e963161d1f4c9bf692c0716ab0ee067a4ccaa4e36d9ce1` | git+https://github.com/microsoft/vcpkg@cad3f5196a32c420fb9aebfc2a8a5876e5c36129 |
| libjpeg-turbo | git+https://github.com/libjpeg-turbo/libjpeg-turbo@3.2.0 | `1ce063e9e126d55019385da3a6ff4521a9a3958edeab15e2465ae3435026dac6e598277eea066173ada47da37159d0f7812b2227869c10dd141dcfe2ddeab720` | git+https://github.com/microsoft/vcpkg@c7d2d8cbb868863a698862a0da02b4af225f3c72 |
| libjxl | git+https://github.com/libjxl/libjxl@v0.12.0 | `71d45123e47588c8028c0e01a7820c7a18d98df05c29b5318ab6858eaff5f249879763907201ba19a93f817c6ef75c393c1974d2a0cc8e461c4e2709ca6fa45f` | git+https://github.com/microsoft/vcpkg@30481191c0e65e92a71840803a1d0c90191ce2c1 |
| libpng | git+https://github.com/pnggroup/libpng@v1.6.59 | `2d753e764846a0a75a10f2a00c6f4ae23d2ae37237c69d59d5e619da3f143d59f4ea9ce29c1fb39435df1cef77877c83b042a2275934c353a6603061da2d2dd9` | git+https://github.com/microsoft/vcpkg@129ad6200060f2e0cce22d36b954d7dff9dc4f8a |
| libwebp | git+https://github.com/webmproject/libwebp@v1.6.0 | `298e0ad4c09392213baf5abb69d330c6203b618800073fe2df91d01d35034197c5d3e29a74573b06971473c52c74514f0e6e0f6c8162f923e2dd15cb1a692aef` | git+https://github.com/microsoft/vcpkg@5366c6ab4c6a5ece31ab06309b9729d29a5d4da1 |
| libyuv | git+https://chromium.googlesource.com/libyuv/libyuv@d98915a654d3564e4802a0004add46221c4e4348 | `b1d76b6ea0804f35c2c2ae83b6f0284baecd2f4bf1e49af29d930eaa28e0d1a1639cfb882e71e30641ceaeacbdbe6128899c50445ed24cfe7f3ed128ccfb5c34`（vcpkg 依 git commit d98915a654d3564e4802a0004add46221c4e4348 匯出的封存檔） | git+https://github.com/microsoft/vcpkg@557f40e7241d91a80b55f849a18376613c45f13a |
| nlohmann-json | git+https://github.com/nlohmann/json@v3.12.0 | `6cc1e86261f8fac21cc17a33da3b6b3c3cd5c116755651642af3c9e99bb3538fd42c1bd50397a77c8fb6821bc62d90e6b91bcdde77a78f58f2416c62fc53b97d` | git+https://github.com/microsoft/vcpkg@ac3b8821cf486e45dd543bef2a4ba1d1ba230258 |
| tiff | git+https://gitlab.com/libtiff/libtiff@v4.7.2 | `c4dcde3c79e5d69c7231f8862e2e5a83d90d9cce694fb2a4804800b2f8f1bc9db504b9252d81dce872eec8358b33a3a1dbdddcbb6181f6fb8d1d7fc0e9a9fc6a` | git+https://github.com/microsoft/vcpkg@a9a8ba148c5c2295dbcf22fe4f6ee5de135b0e33 |
| zlib | git+https://github.com/madler/zlib@v1.3.2 | `16fea4df307a68cf0035858abe2fd550250618a97590e202037acd18a666f57afc10f8836cbbd472d54a0e76539d0e558cb26f059d53de52ff90634bbf4f47d4` | git+https://github.com/microsoft/vcpkg@f49b05c666425471a7f6dc8b6156e9b3675a3c90 |

僅建置期使用、不散布的 vcpkg 輔助埠：vcpkg-cmake、vcpkg-cmake-config、vcpkg-cmake-get-vars、vcpkg-tool-meson。

## 個別檔案聲明（`NOTICES-files.txt`）

對每個散布的 port，以 SPDX 的 SHA-512 找到鎖版原始碼封存檔，取 map 中實際連進的物件所對應的原始檔（依檔名比對，同名檔一併納入），擷取含版權宣告（`Copyright (c)`／`©`／`Copyright <年份>`）且版權人未出現在該 port `LICENSE.txt` 的註解區塊，原文照錄。範圍是編譯單元；header 經由 inline／template 編入的部分不另掃描（libjxl、highway 的 header 與其編譯單元同屬一個專案與授權）。highway 的 `hwy/contrib/random`（CC0-1.0）未被 libjxl 0.12.0 或本工具引用。aom、libyuv 由 vcpkg_from_git 取得，SPDX 沒有上游封存檔的 SHA-512，改以 portfile 鎖定的 commit 找到 vcpkg 匯出的封存檔。

## 通用條文核對（SPDX 範本）

散布元件的授權條文一律取自 vcpkg 安裝的上游 `copyright`（含實際版權行）。產生器另以 SPDX 授權範本庫（`--spdx-templates`）核對：範本中占位符以外的條款文字必須逐字出現在上游條文中，否則中止。比照 SPDX 比對準則，忽略標點、換行與條列記號，BSD-3 第三條「Neither the name of …」的權利人名稱視為可替換文字。libpng、libtiff 的授權不在 SPDX 範本庫中，只照錄上游原文。

| 元件 | SPDX 範本 | 結果 |
|---|---|---|
| aom | `BSD-2-Clause.txt` | 條款一致 |
| brotli | `MIT.txt` | 條款一致 |
| highway | `BSD-3-Clause.txt` | 條款一致 |
| libavif | `BSD-2-Clause.txt` | 條款一致 |
| libjxl | `BSD-3-Clause.txt` | 條款一致 |
| libwebp | `BSD-3-Clause.txt` | 條款一致 |
| libyuv | `BSD-3-Clause.txt` | 條款一致 |
| nlohmann-json | `MIT.txt` | 條款一致 |
| zlib | `Zlib.txt` | 條款一致 |
