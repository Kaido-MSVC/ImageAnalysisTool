# iat Rust 版實驗規劃書

Nov 19, 2026 · @YU SIANG CHEN

## 目標與範圍

以純 Rust 重寫 iat 的 core、CLI、GUI，當作實驗專案。C++ 版 1.x 照常維護，在 Rust 版追上之前就是它的對照組。

- **位置**：新的獨立 repo，位置為 `D:\projects\ImageAnalysisTool-rs`，不放進現有 repo。
- **授權**：MIT，作者 Kaido-MSVC，與 C++ 版相同。

**目標**

1. **判定一致**：同一組輸入，Rust 版輸出的 JSON 報告（schema 1.1）要和 C++ 版相同；允許的差異必須逐條寫明。
2. **CLI 相容**：參數、退出碼、JSON 結構都和 C++ 版一致，IMB-C 可以直接把 `iat.exe` 換成 Rust 版。
3. **GUI**：用 Slint 寫，直接呼叫 core 函式庫，不透過子程序。
4. **純 Rust、寬鬆授權**：執行檔不連結任何 C 或 C++ 函式庫，依賴的授權一律寬鬆。

**這次不做**

- 不新增格式，不改報告 schema。
- 不改 C++ 版的程式碼；IMB-C 維持唯讀。
- 首版不做縮圖金字塔：Skia 後端已經能順暢顯示 16K×8K 的大圖。

**沿用 C++ 版的專案規則**

- 輸入圖片一律唯讀，報告不覆寫。
- 不做 8-bit 便利解碼，也不做隱式的色彩或 Alpha 轉換；16-bit 完整保留。
- 測試圖要有人工指定的獨立真值，產生器不能依賴受測的解碼器。
- 依階段小步提交，每個階段完成後停下回報；commit 訊息不加 Co-Authored-By。

## 技術選型

一般格式用現成的純 Rust crate；PSD/PSB 和 AVIF 的容器解析移植 C++ 版自己的程式碼，AV1 直接用 rav1d。表中有寫版本的，是 2026-11-19 查證過的版本；其餘以實作時你電腦上的 `Cargo.lock` 為準。

| 格式 | 主力 | 授權 | 測試對照 | 實作重點 |
| --- | --- | --- | --- | --- |
| PNG | `png` crate 0.18.1（image-rs）加移植 `png_scan` | MIT/Apache-2.0 | C++ 版 iat | 關閉所有轉換，實測輸出是否為原始樣本；CRC 等結構檢查由 `png_scan` 負責 |
| WebP | `image-webp` 0.2.4（image-rs）加移植 `webp_scan` | MIT/Apache-2.0 | C++ 版 iat | 驗證 VP8L 截斷、Alpha 0 底下的 RGB |
| TIFF | `tiff` crate 0.11.3 加移植 `tiff_scan` | MIT | C++ 版 iat | 分平面影像要檢查 `complete_len`；解碼上限要自訂（預設 256 MiB） |
| JXL | jxl-rs（crate 名稱 `jxl`，0.8.0）加移植 `jxl_scan` | BSD-3-Clause | C++ 版 iat | `adjust_orientation: false`；有損影像的判定可能要容許誤差 |
| PSD/PSB | 移植 `psd_scan` 和 `psd_side` | 自己的程式碼 | apeireth-image-psd 2.0.0-rc.5（只放 dev-dependencies） | 兩套獨立實作互相比對 |
| AVIF 容器 | 移植 `avif_scan` | 自己的程式碼 | oxideav-avif 或 avif-parse | ICC、nclx、pixi、irot/imir、auxl |
| AV1 解碼 | rav1d 1.1.0 | BSD-2-Clause | C++ 版 iat（libavif 1.4.2 + aom） | 只有 `dav1d_*` 形式的 API，要自己包一層安全介面 |
| AVIF 色彩轉換 | 自己寫 | 自己的程式碼 | C++ 版 iat | 無損：identity 4:4:4；有損：各標準矩陣 |
| GUI | Slint（最新版，測試時為 1.19.0）、rfd（測試時為 0.18.0） | Slint：免版稅桌面授權；rfd：MIT | — | 見「GUI（Slint）設計要點」 |
| JSON | serde、serde\_json | MIT/Apache-2.0 | C++ 版的輸出 | 欄位順序不在相容性保證內 |

**排除的套件**

| 套件 | 排除原因 |
| --- | --- |
| zenavif、rav1d-safe 0.6.0 | AGPL-3.0 或 Imazen 商業授權，和 MIT 不相容 |
| avif-decode | 只輸出最後的 RGB，拿不到原始平面；1.0.2 還依賴 C 的 libaom |
| avif-rust | 沒有聲稱支援 4:4:4，而這正是無損驗證的核心案例 |
| libavif-sys | 要編譯 C；預設解碼器 dav1d 在 MSVC 14.52 編不過 |
| zune-png | 會把低於 8 bpp 的影像自動展開成 8 bit |
| ag-psd | 只輸出 RGBA8，而且拒絕 CMYK |

## 專案結構

整個專案是一個 cargo workspace，三個 crate 共用同一個版本號。CLI 和 GUI 都只依賴 `iat-core`，`iat-core` 則不依賴任何 GUI 套件。

```
ImageAnalysisTool-rs/
├─ Cargo.toml          workspace：成員、共用版本、rust-version、[workspace.dependencies]
├─ Cargo.lock          在你的電腦上產生並提交
├─ deny.toml           cargo-deny：授權白名單
├─ .cargo/config.toml  靜態 CRT
├─ crates/
│  ├─ iat-core/        函式庫：格式掃描、解碼、比對、報告模型
│  ├─ iat-cli/         執行檔 iat.exe：參數與輸出和 C++ 版相同
│  └─ iat-gui/         執行檔 iat-gui.exe：Slint
├─ tests/
│  ├─ fixtures/        從 C++ 版複製的測試圖及其 SHA-256
│  └─ compare/         和 C++ 版 iat 比對的腳本
└─ docs/               decisions.md、dependencies.md
```

**工具鏈與版本鎖定**

- Rust 工具鏈以你電腦上的 1.100.0 為準：workspace 設 `rust-version = "1.100"`，edition 2024。
- `Cargo.lock` 只以你電腦上產生的為準。其他環境解析出的版本可能不同，不算數。
- 依賴一律在 `[workspace.dependencies]` 統一宣告。版本變更要同步記在 `docs/dependencies.md`。
- Slint 可以跟最新版，但每次升級都要重測拖放，因為拖放用的是不保證相容的 `unstable-winit-030` feature。
- 沿用 C++ 版「靜態 CRT、不附 VC++ 執行階段」的政策：在 `.cargo/config.toml` 的 `[target.x86_64-pc-windows-msvc]` 設 `rustflags = ["-C", "target-feature=+crt-static"]`。
- release profile 設 `lto = true`、`codegen-units = 1`。

## core API 草案

core 對外提供同步 API，從第一版就支援進度回報和取消。下面只是方向；實際的型別和選項要對照 C++ 版的 `src/core/core.hpp`、`src/model/report.hpp`、`src/common/limits.hpp` 和 `docs/cli-json-contract.md` 來定，這份規劃並未逐行讀過這些檔案。

```rust
// 比對原檔與輸出檔
pub fn analyze(req: &AnalyzeRequest, progress: &dyn Progress, cancel: &CancelToken) -> Result<Report, FatalError>;
// 單檔檢查（對應 C++ 版的 inspect）
pub fn inspect(path: &Path, opts: &Options, progress: &dyn Progress, cancel: &CancelToken) -> Result<Report, FatalError>;
// 給 GUI 檢視用：解碼後的原始樣本，不做任何轉換
pub fn decode_for_view(path: &Path, opts: &Options, cancel: &CancelToken) -> Result<DecodedImage, FatalError>;

pub trait Progress: Send + Sync {
    fn report(&self, stage: Stage, done: u64, total: u64);
}
#[derive(Clone)]
pub struct CancelToken(Arc<AtomicBool>);

pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub samples: Samples,   // U8 / U16 / F32，保留原始位元深度
    pub channels: Channels, // Gray / GrayAlpha / Rgb / Rgba / Cmyk…
    pub alpha: AlphaKind,   // 無 / 直通 / 預乘
    pub color: ColorInfo,   // ICC、CICP 等，原樣交出
}
```

**設計原則**

- **報告模型就是契約**：`Report` 用 serde 序列化，欄位對應 schema 1.1。Markdown 報告的輸出也放在 core，CLI 只負責解析參數、寫檔和退出碼。
- **兩種錯誤分開**：檔案截斷、CRC 錯誤這類發現寫進 `Report`；只有無法產出報告的情況（例如讀不到檔案）才回傳 `FatalError`。退出碼的對應規則跟 C++ 版一致。
- **取消**：在每列、每塊（tile、strip）、每段雜湊之間檢查 `CancelToken`；被取消時的報告內容跟 C++ 版的 `cancel_during_hash` 案例一致。
- **記憶體上限**：沿用 C++ 版的上限與預算選項，包括超過上限時建議設定值的行為。
- **執行緒**：core 是同步的；GUI 在背景執行緒呼叫，結果用 `upgrade_in_event_loop` 送回 UI。
- **不 panic、不用 unsafe**：解析輸入一律回傳 `Result`。`iat-core` 設 `#![deny(unsafe_code)]`，只有 rav1d 包裝那一個模組允許 unsafe。

## GUI（Slint）設計要點

GUI 的做法沿用 `D:\projects\slint-probe` v4 已在你電腦上測過的設定。測試時過程有兩個坑：Slint 的 `DropArea` 接不到檔案總管的拖放，預設的 FemtoVG 後端縮放大圖會卡。

| 項目 | 做法 | 測試結果 |
| --- | --- | --- |
| 拖放檔案 | 啟用 `unstable-winit-030`，接 winit 的 `HoveredFile`、`DroppedFile` 事件；一次拖多個檔案時，用 50 ms 計時器等同一批到齊 | 通過，包括中文路徑 |
| 繪圖後端 | 啟用 `renderer-skia`；沒指定 `SLINT_BACKEND` 時 Slint 會優先用 Skia | 8K 和 16K×8K 的縮放、平移都順暢 |
| 大圖顯示 | `SharedPixelBuffer<Rgba8Pixel>` 搭配 `Image::from_rgba8`，放在 `Flickable` 裡縮放；200% 以上改用 `image-rendering: pixelated` | 16K×8K 可以顯示，約佔 640 MB 記憶體 |
| 開檔對話框 | rfd 的 `FileDialog` | 通過 |

**首版的畫面**

1. **選檔**：原檔和輸出檔各有一個拖放區，也可以按「開啟…」。一次拖入兩個檔案時，依拖入順序填入原檔和輸出檔，並提供「對調」按鈕。
2. **執行**：進度條顯示目前階段，可以隨時取消。
3. **結果**：最上方是判定結論，下面是問題清單；可以匯出 JSON 和 Markdown 報告，內容和 CLI 產出的相同。
4. **影像檢視**：可以切換原檔、輸出、差異熱圖，兩邊同步縮放和平移；游標所在的像素會顯示兩邊的原始樣本值。

**顏色與位元深度**

- 16-bit 和浮點影像只為了顯示才轉成 8-bit，這個轉換只在 `iat-gui` 裡做。像素檢視和差異判定一律用原始值。
- 首版不做色彩管理，畫面上標示「未套用 ICC」，避免誤導。

**多語系（繁中、英文、日文）**

- `.slint` 裡的字串一律用 `@tr()` 包起來，原文寫英文；`en` 就是 Slint 的預設語言。
- 繁中和日文各一份 gettext `.po` 檔，放在 `crates/iat-gui/lang/<語系>/LC_MESSAGES/` 底下，檔名是 crate 名稱。
- 在 `build.rs` 用 `slint_build::CompilerConfiguration::with_bundled_translations()` 把翻譯編進執行檔，不另外附翻譯檔。
- 啟動時依 Windows 顯示語言預選，介面上也可以手動切換；切換用 `select_bundled_translation()`，必須在建立第一個元件之後呼叫。取得系統語言的做法實作時再確認。
- 匯出的 Markdown 報告跟 CLI 一樣只有繁中，不跟著介面語言變；JSON 報告本來就和語言無關。
- 三種語言都要實測字型顯示，特別是日文和繁中的漢字有沒有用到對方語言的字形。

## 開發階段與驗收標準

共分八個階段，明天從階段 0 開始。每個格式階段的驗收標準都一樣：C++ 版 e2e 中該格式的案例，兩邊的 JSON 比對一致。每個階段完成後停下回報。

0. **骨架**：workspace、`Report` 型別、CLI 參數解析、`deny.toml`、比對腳本，以及從 C++ 版複製測試圖。
   - 驗收：`cargo deny check` 通過；`missing_input`、`inspect_unknown_format` 這類不用解碼的案例和 C++ 版一致。
1. **PNG**：移植 `png_scan`，接上 `png` crate，完成比對引擎。
   - 驗收：所有 PNG 案例一致，包括 Adam7、gray1/2/4、tRNS、CRC、zTXt、iCCP/cICP。
2. **GUI 第一版**：只支援 PNG，四個畫面都要能用，用來及早驗證 core API 好不好用。
   - 驗收：拖放、取消、匯出報告都正常；匯出的 JSON 和 CLI 相同。
3. **WebP 和 PSD/PSB**
   - 驗收：WebP、PSD、PSB 的案例一致；PSD/PSB 和 apeireth-image-psd 的交叉比對通過。
4. **TIFF**
   - 驗收：所有 TIFF 案例一致，包括 strip/tile、predictor、BigTIFF、多頁、記憶體預算。
5. **JXL**
   - 驗收：無損案例一致；有損案例的差異寫進允許清單，並注明原因。
6. **AVIF**：rav1d 包裝、移植 `avif_scan`、自己寫色彩轉換。
   - 驗收：無損案例一致；這個階段的 AVIF 支援先標為實驗性。
7. **收尾**：封包、產生第三方授權清單，並在 IMB-C 不改程式的情況下改用 Rust 版 `iat.exe` 實跑一次。
   - 驗收：IMB-C 的驗證結果跟用 C++ 版時相同。

## 驗證方法

驗證的基準固定為 C++ 版 iat 1.0.0 的發布檔 `iat.exe`。同一組輸入分別跑兩個版本，比對兩邊的 JSON。

1. **固定基準**：使用 `dist\ImageAnalysisTool-1.0.0-win-x64\iat.exe`，把它的 SHA-256 記在 `docs/decisions.md`。C++ 版之後改版時，基準不跟著換，要換就另外記一筆決定。
2. **測試圖**：從 C++ repo 複製 `tests/fixtures` 已產生的測試圖，並以 SHA-256 清單記錄。測試圖產生器先不移植，要加新測試圖時照舊在 C++ repo 產生。
3. **案例清單**：沿用 `tests/e2e/run_e2e.py` 的案例定義，只把要執行的執行檔改成參數。
4. **比對腳本**：以 `tools/regress/compare_runs.py` 為基礎改寫。先拿掉工具名稱、版本、耗時這類必然不同的欄位，再逐欄比對其餘內容，輸出一份差異表。
5. **允許的差異**：只能有一份清單，寫在 `docs/decisions.md`，逐條列出案例、欄位和原因。比對腳本讀這份清單，清單以外的差異一律算失敗。
6. **私人樣本**：用 `tools/regress/cases.local.json` 裡的真實樣本在本機加跑一輪；樣本與結果都不進 git。
7. **單元測試**：只移植 `tests/unit/test_core.cpp` 裡測試解析邊界、上限和判定邏輯的部分，其餘交給 e2e 比對。

rust-anything 的 `app` 模式可以當額外的比對工具：階段 0 拿 `iat.exe` 試一次，能用就留著；不能用就只靠上面的比對腳本。它不支援 C++，所以函式層級的比對用不上。

## 建置、授權與發布

建置只需要 `cargo build --release`，不再需要 vcpkg、CMake 或 MSVC 工具集鎖定。授權由 cargo-deny 在建置時自動把關。

**建置**

- Skia 第一次建置時會從網路下載預編好的二進位檔，之後用快取。
- 需要的工具：cargo-deny（授權檢查）和 cargo-about（產生第三方授權清單）。兩者都用 `cargo install` 裝進 `%USERPROFILE%\.cargo\bin`，會動到全域環境，安裝前先問你。

**授權白名單（`deny.toml`）**

- 允許：MIT、Apache-2.0（含 WITH LLVM-exception）、BSD-2-Clause、BSD-3-Clause、ISC、Zlib、BSL-1.0、Unicode-3.0、CC0-1.0、Unlicense、0BSD、NCSA。
- MPL-2.0：只允許用在測試對照的 dev-dependency，例如 avif-parse。
- Slint：宣告的授權是 `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`，只對 Slint 的 crate 開例外，採用免版稅授權。
- 完全禁止：GPL、AGPL、所有商業授權（例如 Imazen 的套件）。

**Slint 免版稅授權的義務**：散布時二擇一，在 GUI 的「關於」畫面顯示 `AboutSlint` 元件，或在公開網頁（例如 GitHub README 或發布頁）放 Slint 標誌。建議直接在關於畫面放 `AboutSlint`。依據：[Slint Royalty-free License 2.0](https://slint.dev/agreements/slint-royalty-free-license.pdf)。

**發布**

- zip 內容照 C++ 版：`iat.exe`、`iat-gui.exe`、README、CHANGELOG、LICENSE、THIRD\_PARTY\_NOTICES、`licenses\`、SHA256SUMS。
- 封包前檢查兩個執行檔都沒有匯入 VC++ 或 UCRT 的執行階段 DLL，和 C++ 版的 `package.ps1` 做同樣的檢查。
- 實驗階段不公開發布；要公開時再決定 repo 和版本號。

## 風險與未決事項

風險最大的是 JXL 和 AVIF 有損影像的判定一致性，兩者都靠「允許差異清單」控制。

| 風險 | 影響 | 對策 |
| --- | --- | --- |
| jxl-rs 的有損輸出和 libjxl 不完全相同 | 有損 JXL 的差異判定可能不同 | 無損要求完全一致；有損的差異寫進允許清單 |
| rav1d 只有 C 形式的 API | 需要 unsafe 包裝 | unsafe 只在一個模組；用截斷、損壞的 e2e 案例驗證 |
| 有損 AVIF 的色度升取樣和色彩轉換跟 libavif 不同 | 有損 AVIF 的像素比對不一致 | 照 libavif 的做法自己寫；差異寫進允許清單 |
| `tiff` crate 的涵蓋範圍比 libtiff 窄 | YCbCr、SubIFD、有號或浮點樣本可能解不出來 | C++ 版標為未檢查的情況維持相同判定；其餘不一致的案例逐一評估 |
| `png` crate 未必能交出未經轉換的樣本 | 階段 1 的做法要改 | 階段 1 第一步就實測；不行就自己寫濾波與 Adam7 還原，只用 zlib 解壓 |
| `unstable-winit-030` 沒有相容性保證 | 升級 Slint 後拖放可能壞掉 | 每次升級重測；Slint 的 `DropArea` 支援 Windows 後改回正規做法 |
| GUI 同時持有原檔、輸出、差異熱圖 | 16K×8K 的圖約需 3 倍記憶體 | 熱圖在需要時才產生；不夠再做縮圖金字塔 |
| 依賴改用不相容的授權 | 執行檔的授權出問題 | `cargo deny check` 失敗時建置即停止 |

**未決事項**

- [ ] 專案名稱和位置（已定為 `D:\projects\ImageAnalysisTool-rs`）
- [ ] 實驗階段的版本號（已定從 0.1.0 起算，和 C++ 版的 1.x 區分）
- [ ] GUI 介面的語言（已定為繁中、英文、日文）
- [ ] rust-anything 的 `app` 模式能不能對 `iat.exe` 使用

## 交給 Claude Code 的起始指示

Claude Code 讀不到這份線上文件。開工前先把它匯出成 Markdown，放在新專案的 `docs/plan.md`，再把下面兩段內容放進去。

**新專案的 `CLAUDE.md`**

```markdown
# 專案規則

- 規格以 `docs/plan.md` 為準，修訂記在 `docs/decisions.md`，規劃書本身不改。
- 對照組是 C++ 版 iat 1.0.0：`D:\projects\ImageAnalysisTool` 只讀不寫；測試圖一律複製過來，並記錄 SHA-256。
- `D:\projects\IMB-C` 只讀不寫。
- 工具鏈以本機實際版本為準（rustc 1.100.0）；不自行升級、切換或移除全域工具，要安裝 cargo-deny 或 cargo-about 先問。
- 依賴在 `[workspace.dependencies]` 統一宣告，變更同步記在 `docs/dependencies.md`；`cargo deny check` 必須通過。
- 輸入圖片一律唯讀；不做 8-bit 便利解碼、隱式色彩或 Alpha 轉換；16-bit 完整保留；報告不覆寫。
- `iat-core` 不用 unsafe，只有 rav1d 包裝模組例外。
- 測試要和小型個人工具的規模相稱：涵蓋主要流程與常見錯誤，已通過的不重跑。
- 依階段小步提交；commit 訊息不加 Co-Authored-By 或 Claude-Session。每個階段完成後停下回報。
```

**第一次的指示（階段 0）**

```text
請先讀 CLAUDE.md 和 docs/plan.md，再讀 C++ 版的 src/core/core.hpp、src/model/report.hpp、
src/common/limits.hpp、docs/cli-json-contract.md 和 tests/e2e/run_e2e.py（都在
D:\projects\ImageAnalysisTool，只讀）。

接著完成規劃書的階段 0：
1. 建立 cargo workspace（iat-core、iat-cli、iat-gui）、.cargo/config.toml、deny.toml。
2. 依 C++ 版定義 Report 型別（serde）和 CLI 參數；格式解碼先留空。
3. 複製測試圖並產生 SHA-256 清單。
4. 寫比對腳本，讓 missing_input、inspect_unknown_format 這類不用解碼的案例
   和 C++ 版 iat 1.0.0 的 JSON 一致。

完成後停下，回報：實際使用的套件版本、比對結果、和規劃書不同的地方。
```
