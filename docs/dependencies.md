# 依賴與工具版本

版本以本機產生的 `Cargo.lock` 為準；依賴一律在根目錄 `Cargo.toml` 的 `[workspace.dependencies]` 宣告。版本變更時同步更新本檔。

## 工具鏈（2026-11-19）

| 工具 | 版本 | 備註 |
|---|---|---|
| rustc | 1.100.0 (0ef1bc386 2026-11-09) | `stable-x86_64-pc-windows-msvc`；workspace `rust-version = "1.100"`、edition 2024 |
| cargo | 1.100.0 (3acc79c0c 2026-11-05) | |
| cargo-deny | 0.21.0 | 2026-11-19 經同意以 `cargo install --locked cargo-deny` 安裝到 `%USERPROFILE%\.cargo\bin` |
| Python | 3.13（`py -3.13 -I`） | 只跑比對腳本，只用標準函式庫 |

建置設定：`.cargo/config.toml` 對 `x86_64-pc-windows-msvc` 設 `+crt-static`；release profile `lto = true`、`codegen-units = 1`。release 版 `iat.exe` 只匯入 KERNEL32、ntdll、bcrypt、dbghelp 與 api-ms-win-core-synch，沒有 VC++ 或 UCRT 執行階段 DLL。

## 直接依賴

| crate | 宣告 | Cargo.lock | 授權 | 用途 |
|---|---|---|---|---|
| serde（derive） | 1.0.229 | 1.0.229 | MIT OR Apache-2.0 | 報告模型序列化 |
| serde_json | 1.0.154 | 1.0.154 | MIT OR Apache-2.0 | JSON 報告（鍵排序靠預設的 BTreeMap，不開 `preserve_order`） |
| sha2 | 0.11.1 | 0.11.1 | MIT OR Apache-2.0 | 輸入檔 SHA-256 |
| windows-sys | 0.61.2 | 0.61.2 | MIT OR Apache-2.0 | Win32 繫結，只在 `iat-core/src/win.rs` 使用（D-R03） |
| png | 0.18.1 | 0.18.1 | MIT OR Apache-2.0 | 階段 1：PNG 的 IDAT 解壓、反濾波與 Adam7 列順序，`Transformations::IDENTITY`（D-R11） |
| miniz_oxide | 0.8.9 | 0.8.9 | MIT OR Zlib OR Apache-2.0 | 階段 1：iCCP／zTXt／iTXt 解壓與 IDAT 串流檢查（區分 Adler-32、損壞、截斷）；與 png 用的是同一版 |
| crc32fast | 1.5.5 | 1.5.5 | MIT OR Apache-2.0 | 階段 1：PNG chunk CRC（png 也使用） |
| image-webp | 0.2.4 | 0.2.4 | MIT OR Apache-2.0 | 階段 3：WebP 的 VP8L 解碼；只交 VP8L chunk 的內容（D-R15） |
| weezl | 0.1.12 | 0.1.12 | MIT OR Apache-2.0 | 階段 4：TIFF 的 LZW 解壓（TIFF 早換碼寬、輸出滿了就停；舊式碼用 LSB 順序），照 libtiff 的行為包裝（D-R17） |

windows-sys 啟用的 feature：`Win32_Foundation`、`Win32_Globalization`、`Win32_Storage_FileSystem`、`Win32_System_Console`、`Win32_System_Diagnostics_Debug`、`Win32_Security_Cryptography`。

## 間接依賴（Cargo.lock）

block-buffer 0.12.1、cfg-if 1.0.5、const-oid 0.10.3、cpufeatures 0.3.1、crypto-common 0.2.2、digest 0.11.3、hybrid-array 0.4.15、itoa 1.0.19、libc 0.2.191、memchr 2.8.3、proc-macro2 1.0.107、quote 1.0.47、serde_core 1.0.229、serde_derive 1.0.229、syn 3.0.10、typenum 1.20.1、unicode-ident 1.0.27、windows-link 0.2.1、zmij 1.0.23。

階段 1 新增（2026-11-20，png 0.18.1 帶入）：adler2 2.0.1（0BSD OR MIT OR Apache-2.0）、bitflags 2.13.2、fdeflate 0.3.7、flate2 1.1.11（MIT OR Apache-2.0）、miniz_oxide 0.9.1（經 flate2）、simd-adler32 0.3.10（MIT）。Cargo.lock 裡的 zlib-rs 是 flate2 的選用功能，沒有啟用、不會編譯。

階段 3 新增（2026-11-20，image-webp 0.2.4 帶入）：byteorder-lite 0.1.0（Unlicense OR MIT）、quick-error 2.0.1（MIT OR Apache-2.0）。

階段 4（2026-11-21）：weezl 0.1.12 只用預設 feature（`std`），沒有其他依賴；它原本就在 Cargo.lock 裡（Slint 經 resvg → gif 帶入），iat-core 與 iat-cli 因此多了這一個 crate，Cargo.lock 沒有新增項目。TIFF 的 Deflate 沿用 miniz_oxide 0.8.9。驗證時用過的 tiff crate 0.11.3 沒有加入（D-R17）。`cargo deny check` 通過。

`cargo deny check`（advisories、bans、licenses、sources）全部通過。唯一的警告是 miniz_oxide 有兩個版本（png 直接用 0.8.9，png 依賴的 flate2 用 0.9.1），不改 png 就無法消除，屬於預期內。

**miniz_oxide 維持 0.8.9（2026-11-21 確認）**：png 0.18.1 的 `Cargo.toml` 直接宣告 `miniz_oxide = "0.8"`（解碼用），0.9.1 只來自 png → flate2 1.1.11（編碼用）。iat-core 的直接依賴本來就和 png 同為 0.8.9；改成 0.9.x 不會少掉任何一版，反而變成 iat-core 用 0.9、png 用 0.8。等 png 改用 0.9 時再一起升。

## dev-dependencies

| crate | 宣告 | Cargo.lock | 授權 | 用途 |
|---|---|---|---|---|
| apeireth-image-psd | 2.0.0-rc.5（`default-features = false`） | 2.0.0-rc.5 | Apache-2.0 OR MIT | 只在 iat-core 的測試使用：PSD／PSB 合成影像的編碼與交叉比對（D-R16）；關掉 default feature 後沒有其他依賴 |

## GUI（階段 2，2026-11-20，只有 iat-gui 使用）

| crate | 宣告 | Cargo.lock | 授權 | 用途 |
|---|---|---|---|---|
| slint | 1.19.0 | 1.19.0 | GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0，採用免版稅授權 | GUI。`default-features = false`，feature：`std`、`compat-1-2`、`backend-winit`、`renderer-femtovg-wgpu`、`accessibility`、`unstable-winit-030`（D-R14） |
| slint-build | 1.19.0 | 1.19.0 | 同 slint | build.rs 編譯 `ui/app.slint`，內嵌翻譯（bundled translations） |
| rfd | 0.18.0 | 0.18.0 | MIT | 開檔／存檔對話方塊 |
| sys-locale | 0.3.2 | 0.3.2 | MIT OR Apache-2.0 | 依 Windows 顯示語言預選介面語言；Slint 本來就依賴同一版，沒有多拉新 crate |

GUI 帶入約 290 個間接依賴（全部是純 Rust，沒有 C/C++ 預建檔），主要是 wgpu 30.0.2、femtovg 0.28.0、winit 0.30.13 與 i-slint-* 子 crate，完整清單以 Cargo.lock 為準。iat-core 與 iat-cli 的依賴沒有變。

`cargo deny check` 的調整（deny.toml）：
- 授權：slint、slint-build、slint-macros 與 7 個 i-slint-* crate 逐一列為例外，只允許 `LicenseRef-Slint-Royalty-free-2.0`；義務是關於畫面的 `AboutSlint`。
- advisories：忽略兩筆「不再維護」通知（不是漏洞），都是 Slint 1.19.0 的間接依賴、沒有可升級的版本：RUSTSEC-2026-0441（pin-utils，經 spin_on，只在建置時使用）、RUSTSEC-2026-0543（chrono，經 i-slint-core）。Slint 升級時重新檢查。
- bans：多版本警告增加 glow、hashbrown、smol_str、syn、windows-sys，都來自 Slint／wgpu 的依賴樹，屬於預期內。

## 尚未加入（規劃書已選定）

jxl、rav1d 依各階段加入；avif-parse 等對照用 crate 只放 dev-dependencies。規劃書選定的 tiff crate 經階段 4 驗證後不採用（D-R17）。
