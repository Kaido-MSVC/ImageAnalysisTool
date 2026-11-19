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

windows-sys 啟用的 feature：`Win32_Foundation`、`Win32_Globalization`、`Win32_Storage_FileSystem`、`Win32_System_Console`、`Win32_System_Diagnostics_Debug`、`Win32_Security_Cryptography`。

## 間接依賴（Cargo.lock）

block-buffer 0.12.1、cfg-if 1.0.5、const-oid 0.10.3、cpufeatures 0.3.1、crypto-common 0.2.2、digest 0.11.3、hybrid-array 0.4.15、itoa 1.0.19、libc 0.2.191、memchr 2.8.3、proc-macro2 1.0.107、quote 1.0.47、serde_core 1.0.229、serde_derive 1.0.229、syn 3.0.10、typenum 1.20.1、unicode-ident 1.0.27、windows-link 0.2.1、zmij 1.0.23。

`cargo deny check`（advisories、bans、licenses、sources）全部通過。

## 尚未加入（規劃書已選定）

png、image-webp、tiff、jxl、rav1d、slint、rfd 依各階段加入；apeireth-image-psd、avif-parse 等對照用 crate 只放 dev-dependencies。
