//! 比對引擎：格式分派、預算、逐列同步比較，以及色彩／metadata／strict 判定。
//! 對照 C++ 版 `src/engine/analyze.cpp` 的 `analyze()` 與 `side.hpp`。
//!
//! 階段 1 接上 PNG；其他格式仍回報「不支援」（見 docs/decisions.md D-R05）。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::time::Instant;

use crate::input::Input;
use crate::limits::{CODEC_RESERVE, Limits};
use crate::report::{
    ChannelStat, Count, DiffEntry, FileReport, Issue, MetaItem, MetadataField, PairStatus, Report, ResourceNeed, code,
};
use crate::sniff::sniff_format;
use crate::temp::{TempError, TempErrorKind, TempFile, TempQuota};
use crate::text::short_hash;
use crate::{CancelToken, Mode, Progress, Stage, ms_since};

/// 未知格式訊息中列出的支援格式。沿用 C++ 版 1.0.0 的文字（最終目標的支援範圍），見 D-R05。
const SUPPORTED_FORMATS: &str = "PNG, TIFF/BigTIFF, WebP, JPEG XL, PSD/PSB, AVIF";

const METADATA_FIELDS: [&str; 9] =
    ["exif", "xmp", "text", "resolution", "time", "hdr_signals", "sbit", "ornt_private", "other_chunks"];

pub struct AnalysisInput<'a> {
    pub mode: Mode,
    /// 開檔階段已取得大小與檔頭的輸入
    pub original: &'a Input<'a>,
    pub output: Option<&'a Input<'a>>,
    pub limits: Limits,
    /// 兩側各一個暫存檔（delete-on-close）；建立失敗時外層已經停止
    pub temp: [Option<&'a File>; 2],
    pub temp_free_bytes: u64,
    pub cancel: &'a CancelToken,
    /// 解碼進度（Stage::Analyze，done／total 為已解出的列數與兩側總列數）
    pub progress: &'a dyn Progress,
    /// 整次執行的期限（deadline_s）
    pub deadline: Instant,
    /// 已知 rav1d 會 panic 的一側（[original, output]）：不再解碼，判為解碼錯誤（見 `worker`）
    pub av1_failed: [bool; 2],
    /// 先前的 worker 在這一側解碼時配置失敗（Job 的 commit 上限）與失敗的階段：不再解碼，照 C++ 版
    /// 該格式解碼器的形式判為 `resource_limit`（見 [`job_limit_failure`]）
    pub alloc_failed: [Option<AllocStage>; 2],
}

/// 配置失敗發生在解碼的哪個階段；決定照 C++ 版哪一種訊息回報（D-R02 第四次修訂）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocStage {
    /// 解碼器本身（C++ 版的 libpng／libwebp／libjxl／libavif 配置失敗）
    Decoder = 0,
    /// 解碼前配置的工具緩衝（C++ 版 WebP 的檔案與 RGBA 緩衝）
    ToolBuffers = 1,
}

impl AllocStage {
    /// 通道紀錄與命令列用的數值。
    pub(crate) fn from_code(v: u64) -> Option<Self> {
        match v {
            0 => Some(Self::Decoder),
            1 => Some(Self::ToolBuffers),
            _ => None,
        }
    }

    pub(crate) fn code(self) -> u8 {
        self as u8
    }
}

/// 解碼器在這段期間的配置屬於 [`AllocStage::ToolBuffers`]；離開時（含 unwind）回到解碼器階段。
pub(crate) struct ToolBufferStage;

impl ToolBufferStage {
    pub(crate) fn enter() -> Self {
        crate::win::ALLOC_STAGE.store(AllocStage::ToolBuffers.code(), std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for ToolBufferStage {
    fn drop(&mut self) {
        crate::win::ALLOC_STAGE.store(AllocStage::Decoder.code(), std::sync::atomic::Ordering::SeqCst);
    }
}

/// 引擎的結果：被取消或超過期限時，外層照 C++ 版處理 worker 被終止的情況。
pub enum Outcome {
    Done(Box<Report>),
    Cancelled,
    Deadline,
}

/// 一側的正規樣本版面。
#[derive(Clone, Debug, Default)]
pub struct RowLayout {
    /// gray | gray_alpha | rgb | rgba
    pub model: &'static str,
    pub channels: u32,
    /// 每個聲道的正規位元數
    pub bits: u32,
}

impl RowLayout {
    pub fn new(model: &'static str, channels: u32, bits: u32) -> Self {
        Self { model, channels, bits }
    }
}

/// 一個輸入檔在引擎裡、與格式無關的狀態（C++ 版 `Side` 去掉解碼器的部分）。
pub struct SideState<'a> {
    /// original | output
    pub scope: &'static str,
    pub input: &'a Input<'a>,
    pub rep: FileReport,
    pub layout: RowLayout,
    pub width: u32,
    pub height: u32,
    /// 目前這一列的正規樣本
    pub canon: Vec<u16>,
    pub failed: bool,
    pub rows: u64,
    pub scan_ms: f64,
    pub temp: Option<TempFile<'a>>,
    pub quota: Rc<RefCell<TempQuota>>,
    /// 先前的 worker 在這一側的 AV1 解碼中 panic（rav1d 的 C 介面不能 unwind）
    pub av1_failed: bool,
}

impl SideState<'_> {
    /// read_row() 要產生的列號，也就是已完成的列數（read_row() 只在 rows < height 時被呼叫）。
    #[expect(clippy::cast_possible_truncation, reason = "rows < height，height 是 u32")]
    pub fn current_row(&self) -> u32 {
        self.rows as u32
    }
}

/// 一側的解碼器。引擎依序呼叫：open() 一次；每列 read_row() 一次（由上而下），直到失敗或讀完；
/// 最後 finish() 一次。read_row() 成功時，SideState::canon 裡有 width × channels 個正規樣本。
pub trait Decoder<'a> {
    /// 解碼期間工具自管的緩衝（開始前與 buffer_mib 比較）；算術溢位時為 None。
    fn buffer_estimate(&self) -> Option<u64>;
    /// 隨影像變大的解碼器內部記憶體（整圖型解碼器）；逐列解碼器為 0。
    fn codec_estimate(&self) -> Option<u64> {
        Some(0)
    }
    fn open(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, lim: &Limits) -> bool;
    fn read_row(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool;
    fn finish(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>);
    fn memory_metrics(&self, m: &mut BTreeMap<String, u64>);
}

/// 暫存失敗：配額或空間用完是資源上限（未檢查，退出碼 4）；其他是環境問題。
pub fn temp_failure(s: &mut SideState<'_>, issues: &mut Vec<Issue>, e: &TempError) {
    s.failed = true;
    s.rep.decode_status = "not_checked".into();
    if matches!(e.kind, TempErrorKind::Quota | TempErrorKind::Disk) {
        s.rep.decode_status_reason = "resource_limit".into();
        issues.push(Issue::new(code::RESOURCE_LIMIT, "error", s.scope, format!("temporary storage: {}", e.message)));
    } else {
        s.rep.decode_status_reason = format!("temp_{}", e.kind);
        issues.push(Issue::new(code::ENVIRONMENT_ERROR, "error", s.scope, format!("temporary storage: {}", e.message)));
    }
}

/// JXL 色彩列舉值的 `key=value` 欄位（C++ 版 `enum_fields`）。
fn enum_fields(key: &str) -> BTreeMap<&str, &str> {
    let rest = key.split_once(':').map_or("", |(_, r)| r);
    rest.split(';').filter_map(|kv| kv.split_once('=')).collect()
}

/// 訊號清單裡 `marker` 後面的整數（C++ 版 `signal_int`，atoi 語意）。
fn signal_int(signals: &[String], marker: &str) -> i64 {
    for s in signals {
        if let Some(p) = s.find(marker) {
            let digits: String = s[p + marker.len()..].chars().take_while(char::is_ascii_digit).collect();
            return digits.parse().unwrap_or(0);
        }
    }
    -1
}

/// JXL 色彩列舉值和另一側的等價規則（D12，移植 C++ 版 `jxl_equivalence`）：
/// 相等時回傳 (依據, 附註)。JXL 的列舉值在這裡接受的範圍內等於 CICP 代碼：原色 sRGB=1、BT.2100=9；
/// 轉換函數 BT.709=1、linear=8、sRGB=13、PQ=16、HLG=18；白點 D65=1。
fn jxl_equivalence(a: &FileReport, b: &FileReport) -> Option<(String, String)> {
    let (j, o) = if a.color.kind == "jxl_enum" {
        (a, b)
    } else if b.color.kind == "jxl_enum" {
        (b, a)
    } else {
        return None;
    };
    if o.color.kind == "jxl_enum" {
        return None; // 兩個 JXL 列舉值：照一般的 key 比較
    }
    // libjxl 產生的 ICC 和另一側內嵌的 ICC 逐位元組相同（使用者決定，D12）
    if o.color.kind == "icc"
        && o.icc.status == "embedded"
        && j.icc.status == "synthesized"
        && !j.icc.sha256.is_empty()
        && j.icc.sha256 == o.icc.sha256
    {
        return Some((
            format!(
                "JXL colour enum whose ICC ({}) is byte-identical to the other file's embedded ICC (sha256 {})",
                j.icc.profile_name,
                short_hash(&o.icc.sha256)
            ),
            String::new(),
        ));
    }
    let f = enum_fields(&j.color.key);
    let num = |k: &str| -> i64 { f.get(k).and_then(|v| v.parse().ok()).unwrap_or(0) };
    let (space, white, prim, tf) = (num("space"), num("white"), num("primaries"), num("transfer"));
    if (space != 0 && space != 1) || white != 1 {
        return None;
    }
    if o.color.kind == "srgb" && (space == 1 || prim == 1) && tf == 13 {
        let ji = signal_int(&j.color.signals, "intent=");
        let pi = signal_int(&o.color.signals, "sRGB(intent ");
        let note = if ji >= 0 && pi >= 0 && ji != pi {
            format!(
                "rendering intent differs: JXL enum {ji}, PNG sRGB {pi} (recorded only; does not affect colour equality)"
            )
        } else {
            String::new()
        };
        return Some(("JXL sRGB colour enum and PNG sRGB chunk".into(), note));
    }
    if o.color.kind == "cicp" && space == 0 && (prim == 1 || prim == 9) && [1, 8, 13, 16, 18].contains(&tf) {
        let want = format!("cicp:{prim}/{tf}/0/1");
        if o.color.key == want {
            return Some((format!("JXL colour enum equals PNG cICP {}", &want[5..]), String::new()));
        }
    }
    None
}

/// 樣本模型名稱的靜態字串（worker 傳回的檢視標頭用）；不認得的名稱為空字串。
pub(crate) fn model_name(model: &str) -> &'static str {
    match model {
        "gray" => "gray",
        "gray_alpha" => "gray_alpha",
        "rgb" => "rgb",
        "rgba" => "rgba",
        _ => "",
    }
}

pub fn has_alpha(model: &str) -> bool {
    model == "gray_alpha" || model == "rgba"
}

fn color_base(model: &str) -> &'static str {
    if model.starts_with("gray") { "gray" } else { "rgb" }
}

fn channel_names(model: &str) -> &'static [&'static str] {
    match model {
        "gray" => &["Gray"],
        "gray_alpha" => &["Gray", "A"],
        "rgb" => &["R", "G", "B"],
        _ => &["R", "G", "B", "A"],
    }
}

fn compare_item(name: &str, a: Option<&MetaItem>, b: Option<&MetaItem>) -> MetadataField {
    let mut f = MetadataField {
        name: name.into(),
        status: String::new(),
        original: a.map(|x| x.summary.clone()).unwrap_or_default(),
        output: b.map(|x| x.summary.clone()).unwrap_or_default(),
    };
    let pa = a.filter(|x| x.present);
    let pb = b.filter(|x| x.present);
    f.status = match (pa, pb) {
        (None, None) => "both_absent",
        _ if pa.is_some_and(|x| x.status == "parse_failed") || pb.is_some_and(|x| x.status == "parse_failed") => {
            "parse_failed"
        }
        _ if pa.is_some_and(|x| x.status == "over_limit") || pb.is_some_and(|x| x.status == "over_limit") => {
            "over_limit"
        }
        (Some(_), None) => "removed",
        (None, Some(_)) => "added",
        (Some(x), Some(y)) => {
            if x.summary == y.summary {
                "same"
            } else {
                "changed"
            }
        }
    }
    .into();
    f
}

/// 以 step_mib 的倍數向上取整、可容納需要量的最小上限值（MiB）；超過 max_mib 時為 None。
pub fn suggest_limit_mib(needed: u64, reserve: u64, step_mib: u64, max_mib: u64) -> Option<u64> {
    let total = needed.checked_add(reserve)?;
    if step_mib == 0 {
        return None;
    }
    let mib = (total >> 20) + u64::from(total & ((1 << 20) - 1) != 0);
    let rounded = mib.div_ceil(step_mib) * step_mib;
    (rounded >= mib && rounded <= max_mib).then_some(rounded)
}

struct Side<'a> {
    st: SideState<'a>,
    /// 先前的 worker 在這一側配置失敗（見 [`AnalysisInput::alloc_failed`]）
    alloc_failed: Option<AllocStage>,
    /// 仍可解碼的解碼器；格式不支援或已停用時為 None
    dec: Option<Box<dyn Decoder<'a> + 'a>>,
    /// 被預算或 open() 擋下的解碼器，只留著做記憶體統計
    stopped: Option<Box<dyn Decoder<'a> + 'a>>,
}

impl Side<'_> {
    /// 停用解碼器（之後不再解碼），保留它供記憶體統計。
    fn stop(&mut self) {
        if let Some(d) = self.dec.take() {
            self.stopped = Some(d);
        }
    }

    /// 解碼器 panic：判為解碼錯誤（同 C++ 版解碼器回報失敗），停用解碼器。
    fn panicked(&mut self, issues: &mut Vec<Issue>) {
        let st = &mut self.st;
        st.failed = true;
        st.rep.decode_status = if st.rows > 0 { "partial" } else { "failed" }.into();
        st.rep.decode_status_reason = "decode_error".into();
        st.rep.coverage.rows_decoded = st.rows;
        issues.push(Issue::new(code::DECODE_FAILED, "error", st.scope, "decoder panicked"));
        self.stop();
    }

    /// 以下三個呼叫都用 catch_unwind 包住：解碼器裡的 panic 不讓整個 worker 結束。
    /// 呼叫期間標記這一側，worker 配置失敗時父程序才知道是哪一側（[`AllocSideGuard`]）。
    fn open(&mut self, issues: &mut Vec<Issue>, lim: &Limits) -> bool {
        let Some(dec) = self.dec.as_mut() else { return false };
        let st = &mut self.st;
        let _side = AllocSideGuard::new(st);
        match panic::catch_unwind(AssertUnwindSafe(|| dec.open(st, issues, lim))) {
            Ok(ok) => ok,
            Err(_) => {
                self.panicked(issues);
                false
            }
        }
    }

    fn read_row(&mut self, issues: &mut Vec<Issue>) -> bool {
        let Some(dec) = self.dec.as_mut() else { return false };
        let st = &mut self.st;
        let _side = AllocSideGuard::new(st);
        match panic::catch_unwind(AssertUnwindSafe(|| dec.read_row(st, issues))) {
            Ok(ok) => ok,
            Err(_) => {
                self.panicked(issues);
                false
            }
        }
    }

    fn finish(&mut self, issues: &mut Vec<Issue>) {
        let Some(dec) = self.dec.as_mut() else { return };
        let st = &mut self.st;
        let _side = AllocSideGuard::new(st);
        if panic::catch_unwind(AssertUnwindSafe(|| dec.finish(st, issues))).is_err() {
            self.panicked(issues);
        }
    }
}

/// C++ 版解碼器在配置失敗時的回報（只有在 C++ 版是「那一側 `resource_limit`、報告其餘照常」的格式）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct JobLimitFailure {
    /// `resource_limit` 問題的訊息
    pub message: String,
    /// 寫進 `decoder_errors` 的一筆（C++ 版有寫的格式）
    pub decoder_error: Option<String>,
}

/// C++ 1.0.0 在這個格式的解碼器配置失敗時，是否只把那一側判為 `resource_limit`。
fn side_level_alloc_failure(format: &str) -> bool {
    matches!(format, "png" | "webp" | "jxl_codestream" | "jxl_container" | "avif" | "psd" | "psb")
}

/// 這個格式在 C++ 1.0.0 配置失敗時的形式（D-R02 第四次修訂）。TIFF 等 C++ 版沒有攔下配置失敗的
/// 格式回傳 None：worker 不標記那一側，維持執行層級的 `resource_limit`（同 C++ 版 worker 的 bad_alloc）。
pub(crate) fn job_limit_failure(format: &str, stage: AllocStage, lim: &Limits) -> Option<JobLimitFailure> {
    if !side_level_alloc_failure(format) {
        return None;
    }
    let (message, decoder_error) = match (format, stage) {
        // C++ 版先配置整個檔案與 RGBA 輸出（webp_side.cpp），再交給 libwebp
        ("webp", AllocStage::ToolBuffers) => {
            ("buffers for the WebP file and its RGBA output could not be allocated".to_string(), None)
        }
        _ => return decoder_failure(format, lim),
    };
    Some(JobLimitFailure { message, decoder_error })
}

/// 解碼器本身配置失敗時 C++ 1.0.0 的形式。
fn decoder_failure(format: &str, lim: &Limits) -> Option<JobLimitFailure> {
    let (message, decoder_error) = match format {
        // libpng 的配置器（png_shim）拒絕：`pngs_read_info` 失敗
        "png" => ("libpng: Out of memory".to_string(), Some("Out of memory".to_string())),
        "webp" => ("libwebp ran out of memory (OUT_OF_MEMORY)".into(), Some("libwebp: OUT_OF_MEMORY".into())),
        "jxl_codestream" | "jxl_container" => (
            format!(
                "libjxl allocation refused at the hard cap of {} bytes (job_commit_mib - 64 MiB; raise with --limits job_commit_mib=)",
                lim.codec_cap_bytes()
            ),
            None,
        ),
        "avif" => ("libavif: Out of memory".into(), Some("libavif: Out of memory".into())),
        "psd" | "psb" => ("PSD row buffers could not be allocated".into(), None),
        _ => return None,
    };
    Some(JobLimitFailure { message, decoder_error })
}

/// 解碼器呼叫期間把這一側登記給 worker 的配置器（見 `win::ALLOC_SIDE`）；只登記
/// [`job_limit_failure`] 有對應形式的格式。離開時（含 unwind）清除。
struct AllocSideGuard;

impl AllocSideGuard {
    fn new(st: &SideState<'_>) -> Self {
        let v = match (side_level_alloc_failure(&st.rep.format), st.scope) {
            (false, _) => 0,
            (true, "output") => 2,
            (true, _) => 1,
        };
        crate::win::ALLOC_SIDE.store(v, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for AllocSideGuard {
    fn drop(&mut self) {
        crate::win::ALLOC_SIDE.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

fn new_side<'a>(
    scope: &'static str,
    input: &'a Input<'a>,
    temp: Option<&'a File>,
    quota: &Rc<RefCell<TempQuota>>,
) -> Side<'a> {
    Side {
        st: SideState {
            scope,
            input,
            rep: FileReport { name: scope.into(), ..FileReport::default() },
            layout: RowLayout::default(),
            width: 0,
            height: 0,
            canon: Vec::new(),
            failed: false,
            rows: 0,
            scan_ms: 0.0,
            temp: temp.map(|f| TempFile::new(Some(f))),
            quota: Rc::clone(quota),
            av1_failed: false,
        },
        dec: None,
        stopped: None,
        alloc_failed: None,
    }
}

/// 這個版本有前端（掃描與解碼器）的格式。
pub(crate) fn has_frontend(format: &str) -> bool {
    matches!(format, "png" | "webp" | "psd" | "psb" | "tiff" | "bigtiff" | "jxl_codestream" | "jxl_container" | "avif")
}

fn prepare_side<'a>(s: &mut Side<'a>, issues: &mut Vec<Issue>, lim: &Limits) {
    let st = &mut s.st;
    st.rep.size_bytes = Count(st.input.size());
    st.rep.format = sniff_format(st.input.head()).to_string();
    if st.rep.format == "png" {
        s.dec = crate::png_side::prepare_png(st, issues, lim);
    } else if st.rep.format == "webp" {
        s.dec = crate::webp_side::prepare_webp(st, issues, lim);
    } else if st.rep.format == "psd" || st.rep.format == "psb" {
        s.dec = crate::psd_side::prepare_psd(st, issues, lim);
    } else if st.rep.format == "tiff" || st.rep.format == "bigtiff" {
        s.dec = crate::tiff_side::prepare_tiff(st, issues, lim);
    } else if st.rep.format == "jxl_codestream" || st.rep.format == "jxl_container" {
        s.dec = crate::jxl_side::prepare_jxl(st, issues, lim);
    } else if st.rep.format == "avif" {
        s.dec = crate::avif_side::prepare_avif(st, issues, lim);
    } else {
        let t0 = Instant::now();
        st.rep.container_status = "unverified".into();
        st.rep.decode_status = "unsupported".into();
        let unknown = st.rep.format == "unknown";
        st.rep.decode_status_reason =
            if unknown { "unknown_format" } else { "format_not_supported_in_this_build" }.into();
        st.rep.metadata_status = "unverified".into();
        issues.push(Issue::new(
            if unknown { code::UNKNOWN_FORMAT } else { code::UNSUPPORTED_SAMPLE_MODEL },
            "error",
            st.scope,
            format!("format '{}' is not supported by this build ({SUPPORTED_FORMATS})", st.rep.format),
        ));
        st.scan_ms = ms_since(t0);
    }
}

#[derive(Default)]
struct Stats {
    differing: Vec<u64>,
    maxabs: Vec<u32>,
    sumabs: Vec<u128>,
    sumsq: Vec<u128>,
    pixels: u64,
    pixels_diff: u64,
    first: Vec<DiffEntry>,
}

/// 與 C++ 版 U128::approx 相同的換算（只供顯示）。
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "對照 C++ static_cast 截斷：U128 拆成 hi／lo 兩個 uint64 再轉 double"
)]
fn approx(v: u128) -> f64 {
    (v >> 64) as u64 as f64 * 18446744073709551616.0 + v as u64 as f64
}

/// u64 轉 f64（只供顯示的比例與平均）。
#[expect(clippy::cast_precision_loss, reason = "對照 C++ static_cast<double>")]
fn to_f64(v: u64) -> f64 {
    v as f64
}

/// 一次分析，產生 C++ 版 worker 那一半的報告（身分欄位與執行層級資料由 [`crate::run`] 補上）。
pub fn analyze(input: &AnalysisInput<'_>) -> Outcome {
    let t_all = Instant::now();
    let compare = input.mode == Mode::Compare;
    let lim = &input.limits;
    let mut r = Report { mode: input.mode.as_str().into(), ..Report::default() };
    r.codecs.insert(crate::png_side::DECODER_NAME.into(), crate::png_side::DECODER_VERSION.into());
    r.codecs.insert(crate::webp_side::DECODER_NAME.into(), crate::webp_side::DECODER_VERSION.into());
    r.codecs.insert(crate::tiff_side::DECODER_NAME.into(), crate::tiff_side::DECODER_VERSION.into());
    r.codecs.insert(crate::jxl_side::DECODER_NAME.into(), crate::jxl_side::DECODER_VERSION.into());
    r.codecs.insert(crate::avif_side::DECODER_NAME.into(), crate::avif_side::DECODER_VERSION.into());

    let quota = Rc::new(RefCell::new(TempQuota::new(lim.temp_gib << 30, input.temp_free_bytes)));
    let mut issues: Vec<Issue> = Vec::new();
    let mut a = new_side("original", input.original, input.temp[0], &quota);
    a.st.av1_failed = input.av1_failed[0];
    a.alloc_failed = input.alloc_failed[0];
    let mut b = input.output.map(|i| {
        let mut s = new_side("output", i, input.temp[1], &quota);
        s.st.av1_failed = input.av1_failed[1];
        s.alloc_failed = input.alloc_failed[1];
        s
    });
    prepare_side(&mut a, &mut issues, lim);
    if let Some(b) = b.as_mut() {
        prepare_side(b, &mut issues, lim);
    }

    // ---- 預算：只算列緩衝（從不整張圖），64 位元檢查過的算術
    let mut need: u64 = 0;
    let mut overflow = false;
    for s in std::iter::once(&mut a).chain(b.as_mut()) {
        let Some(dec) = s.dec.as_ref() else { continue };
        match dec.buffer_estimate().and_then(|e| need.checked_add(e)) {
            Some(n) => need = n,
            None => {
                overflow = true;
                break;
            }
        }
    }
    if overflow || need > lim.buffer_bytes() {
        // 列緩衝是兩側共用的一個預算：每個被擋下的一側記錄相同的需要量。
        let sug = if overflow { None } else { suggest_limit_mib(need, 0, 16, 1 << 16) };
        let arg = sug.map(|v| format!("--limits buffer_mib={v}")).unwrap_or_default();
        for s in std::iter::once(&mut a).chain(b.as_mut()) {
            if s.dec.is_none() {
                continue;
            }
            s.stop();
            s.st.rep.decode_status = "not_checked".into();
            s.st.rep.decode_status_reason = "resource_limit".into();
            if !overflow {
                s.st.rep.resource_need = Some(ResourceNeed {
                    limit: "buffer_mib".into(),
                    needed_bytes: Count(need),
                    allowed_bytes: Count(lim.buffer_bytes()),
                    current_value: lim.buffer_mib,
                    suggested_value: sug,
                    suggested_argument: arg.clone(),
                });
            }
        }
        let msg = if overflow {
            "row buffer size overflows 64-bit arithmetic".to_string()
        } else {
            let tail = if sug.is_some() {
                format!("rerun with {arg}")
            } else {
                "this exceeds the largest allowed buffer_mib".into()
            };
            format!(
                "row buffers need {need} bytes ({} MiB), budget is {} bytes (buffer_mib={}); {tail}",
                need.div_ceil(1 << 20),
                lim.buffer_bytes(),
                lim.buffer_mib
            )
        };
        issues.push(Issue::new(code::RESOURCE_LIMIT, "error", "run", msg));
    }

    // ---- 整圖型解碼器的記憶體預估與硬上限（D10）。兩側依序解碼，各自與列緩衝合計比較。
    let pct = lim.test_codec_estimate_pct;
    let mut codec_need_peak = 0u64;
    for s in std::iter::once(&mut a).chain(b.as_mut()) {
        let Some(dec) = s.dec.as_ref() else { continue };
        let est = dec.codec_estimate();
        if est == Some(0) {
            continue;
        }
        let scaled = est.and_then(|e| e.checked_mul(pct));
        let with_rows = scaled.and_then(|v| (v / 100).checked_add(need));
        match with_rows {
            Some(w) if w <= lim.codec_cap_bytes() => codec_need_peak = codec_need_peak.max(w),
            _ => {
                s.stop();
                s.st.rep.decode_status = "not_checked".into();
                s.st.rep.decode_status_reason = "resource_limit".into();
                let sug = with_rows.and_then(|w| suggest_limit_mib(w, CODEC_RESERVE, 256, 1 << 20));
                let arg = sug.map(|v| format!("--limits job_commit_mib={v}")).unwrap_or_default();
                if let Some(w) = with_rows {
                    s.st.rep.resource_need = Some(ResourceNeed {
                        limit: "job_commit_mib".into(),
                        needed_bytes: Count(w),
                        allowed_bytes: Count(lim.codec_cap_bytes()),
                        current_value: lim.job_commit_mib,
                        suggested_value: sug,
                        suggested_argument: arg.clone(),
                    });
                }
                let msg = match with_rows {
                    None => "decoder memory estimate overflows 64-bit arithmetic".to_string(),
                    Some(w) => {
                        let tail = if sug.is_some() {
                            format!("rerun with {arg}")
                        } else {
                            "this exceeds the largest allowed job_commit_mib".into()
                        };
                        format!(
                            "decoder needs about {w} bytes ({} MiB: codec estimate {} + row buffers {need}), hard cap is {} bytes = job_commit_mib ({}) - 64 MiB; {tail}",
                            w.div_ceil(1 << 20),
                            scaled.unwrap_or(0) / 100,
                            lim.codec_cap_bytes(),
                            lim.job_commit_mib
                        )
                    }
                };
                issues.push(Issue::new(code::RESOURCE_LIMIT, "error", s.st.scope, msg));
            }
        }
    }

    for s in std::iter::once(&mut a).chain(b.as_mut()) {
        if s.dec.is_none() {
            continue;
        }
        if let Some(stage) = s.alloc_failed
            && let Some(f) = job_limit_failure(&s.st.rep.format, stage, lim)
        {
            s.stop();
            let st = &mut s.st;
            st.failed = true;
            st.rep.decode_status = "not_checked".into();
            st.rep.decode_status_reason = "resource_limit".into();
            st.rep.decoder_errors.extend(f.decoder_error);
            issues.push(Issue::new(code::RESOURCE_LIMIT, "error", st.scope, f.message));
            continue;
        }
        if s.open(&mut issues, lim) {
            // 這一側的列緩衝：配置失敗也算這一側（C++ 版在解碼器配置之後才配置工具的列緩衝）
            let _side = AllocSideGuard::new(&s.st);
            s.st.canon = vec![0; s.st.width as usize * s.st.layout.channels as usize];
        } else {
            s.stop();
        }
    }

    // ---- 可比性
    let mut not_cmp_reason = "";
    let mut comparable = false;
    if let Some(b) = b.as_ref() {
        let (oa, ob) = (&a.st.rep, &b.st.rep);
        comparable = a.dec.is_some() && b.dec.is_some();
        if oa.width != ob.width || oa.height != ob.height {
            not_cmp_reason = code::GEOMETRY_DIFFERS;
        } else if oa.orientation.source == "unparseable" || ob.orientation.source == "unparseable" {
            not_cmp_reason = code::ORIENTATION_UNPARSEABLE;
        } else if oa.orientation.source == "unknown" || ob.orientation.source == "unknown" {
            not_cmp_reason = code::ORIENTATION_UNKNOWN; // 沒讀到 EXIF：樣本相等也證明不了什麼
        } else if oa.orientation.value != ob.orientation.value {
            not_cmp_reason = code::ORIENTATION_DIFFERS;
        } else if !a.st.layout.model.is_empty()
            && !b.st.layout.model.is_empty()
            && color_base(a.st.layout.model) != color_base(b.st.layout.model)
        {
            not_cmp_reason = code::SAMPLE_MODEL_CHANGED;
        } else if a.st.layout.bits != 0 && b.st.layout.bits != 0 && a.st.layout.bits != b.st.layout.bits {
            not_cmp_reason = code::BIT_DEPTH_CHANGED;
        }
        if !not_cmp_reason.is_empty() {
            comparable = false;
        }
        // 交錯只影響儲存順序；樣本依影像順序比較。
        if oa.interlace >= 0 && ob.interlace >= 0 && oa.interlace != ob.interlace {
            issues.push(Issue::new(
                code::INTERLACE_DIFFERS,
                "info",
                "pair",
                format!(
                    "interlace {} vs {} (storage order only; samples compared in image order)",
                    oa.interlace, ob.interlace
                ),
            ));
        }
    }

    // ---- 解碼（並逐列同步比較）
    let stop = |cancel: &CancelToken| -> Option<Outcome> {
        if cancel.is_cancelled() {
            Some(Outcome::Cancelled)
        } else if Instant::now() >= input.deadline {
            Some(Outcome::Deadline)
        } else {
            None
        }
    };
    let t_dec = Instant::now();
    let total_rows: u64 =
        std::iter::once(&a).chain(b.as_ref()).filter(|s| s.dec.is_some()).map(|s| u64::from(s.st.height)).sum();
    let rows_done = |a: &Side<'_>, b: Option<&Side<'_>>| a.st.rows + b.map_or(0, |b| b.st.rows);
    input.progress.report(Stage::Analyze, 0, total_rows);
    let mut stats = Stats::default();
    // AVIF 的像素驗證結果（[original, output]）：(pixel_verified, basis)
    let mut avif_verdict: [Option<(Option<bool>, String)>; 2] = [None, None];
    let mut names: &[&str] = &[];
    let (mut a_alpha, mut b_alpha): (Option<usize>, Option<usize>) = (None, None);
    if comparable
        && let Some(b) = b.as_mut()
        && a.dec.is_some()
        && b.dec.is_some()
    {
        let wide = if has_alpha(a.st.layout.model) { a.st.layout.model } else { b.st.layout.model };
        names = channel_names(wide);
        let n = names.len();
        a_alpha = has_alpha(a.st.layout.model).then(|| a.st.layout.channels as usize - 1);
        b_alpha = has_alpha(b.st.layout.model).then(|| b.st.layout.channels as usize - 1);
        let alpha_max = (1u32 << a.st.layout.bits) - 1;
        stats.differing = vec![0; n];
        stats.maxabs = vec![0; n];
        stats.sumabs = vec![0; n];
        stats.sumsq = vec![0; n];
        let same_layout = a.st.layout.channels == b.st.layout.channels;
        let (ach, bch) = (a.st.layout.channels as usize, b.st.layout.channels as usize);
        let w = a.st.width;
        for y in 0..a.st.height {
            if let Some(o) = stop(input.cancel) {
                return o;
            }
            let ra = a.read_row(&mut issues);
            let rb = b.read_row(&mut issues);
            if !ra || !rb {
                break;
            }
            input.progress.report(Stage::Analyze, a.st.rows + b.st.rows, total_rows);
            if same_layout && a.st.canon == b.st.canon {
                stats.pixels += u64::from(w);
                continue;
            }
            for x in 0..w as usize {
                let pa = &a.st.canon[x * ach..(x + 1) * ach];
                let pb = &b.st.canon[x * bch..(x + 1) * bch];
                let mut pixel_diff = false;
                for c in 0..n {
                    let is_alpha = (a_alpha.is_some() || b_alpha.is_some()) && c == n - 1;
                    let va = if is_alpha { a_alpha.map_or(alpha_max, |i| u32::from(pa[i])) } else { u32::from(pa[c]) };
                    let vb = if is_alpha { b_alpha.map_or(alpha_max, |i| u32::from(pb[i])) } else { u32::from(pb[c]) };
                    if va != vb {
                        let d = va.abs_diff(vb);
                        stats.differing[c] += 1;
                        stats.maxabs[c] = stats.maxabs[c].max(d);
                        stats.sumabs[c] += u128::from(d);
                        stats.sumsq[c] += u128::from(d) * u128::from(d);
                        pixel_diff = true;
                    }
                }
                if pixel_diff {
                    stats.pixels_diff += 1;
                    if stats.first.len() < 16 {
                        stats.first.push(DiffEntry {
                            x: x as u64,
                            y: u64::from(y),
                            original: pa.iter().map(|&v| u32::from(v)).collect(),
                            output: pb.iter().map(|&v| u32::from(v)).collect(),
                        });
                    }
                }
            }
            stats.pixels += u64::from(w);
        }
    }
    // 沒有比較（或中途停下）的一側仍然解到最後，才知道它本身是否完整。
    let base = rows_done(&a, b.as_ref());
    let mut done_tail = 0u64;
    for s in std::iter::once(&mut a).chain(b.as_mut()) {
        if s.dec.is_none() {
            continue;
        }
        while !s.st.failed && s.st.rows < u64::from(s.st.height) {
            if let Some(o) = stop(input.cancel) {
                return o;
            }
            if !s.read_row(&mut issues) {
                break;
            }
            done_tail += 1;
            input.progress.report(Stage::Analyze, base + done_tail, total_rows);
        }
    }
    for s in std::iter::once(&mut a).chain(b.as_mut()) {
        s.finish(&mut issues);
    }
    let dec_ms = ms_since(t_dec);

    if let Some(b) = b.as_ref() {
        let (oa, ob) = (&a.st.rep, &b.st.rep);
        // ---- 樣本比較結果
        let complete = oa.decode_status == "complete" && ob.decode_status == "complete";
        let sc = &mut r.samples;
        if !not_cmp_reason.is_empty() {
            sc.status = "not_comparable".into();
            sc.reason = not_cmp_reason.into();
        } else if a.dec.is_none() || b.dec.is_none() || !complete {
            sc.status = if comparable { "incomplete" } else { "not_compared" }.into();
            sc.reason = "decode_incomplete".into();
        } else {
            sc.pixels_compared = Some(Count(stats.pixels));
            sc.pixels_differing = Some(Count(stats.pixels_diff));
            sc.status = if stats.pixels_diff == 0 { "equal" } else { "differs" }.into();
            sc.includes_alpha = a_alpha.is_some() || b_alpha.is_some();
            let unit = match a.st.layout.bits {
                16 => "uint16",
                8 => "uint8",
                _ => "native",
            };
            sc.unit = format!("{unit} code value; per-channel denominator = pixels_compared");
            for (c, name) in names.iter().enumerate() {
                let mut cs = ChannelStat {
                    name: (*name).into(),
                    differing: Count(stats.differing[c]),
                    max_abs_error: stats.maxabs[c],
                    mae: 0.0,
                    rmse: 0.0,
                };
                if stats.pixels > 0 {
                    cs.mae = approx(stats.sumabs[c]) / to_f64(stats.pixels);
                    cs.rmse = (approx(stats.sumsq[c]) / to_f64(stats.pixels)).sqrt();
                }
                sc.per_channel.push(cs);
            }
            sc.first_differences = std::mem::take(&mut stats.first);
            if a_alpha.is_none() && b_alpha.is_some() {
                issues.push(Issue::new(
                    code::ALPHA_CHANNEL_ADDED,
                    "info",
                    "pair",
                    "output adds an alpha channel; original compared as fully opaque",
                ));
            }
            if a_alpha.is_some() && b_alpha.is_none() {
                issues.push(Issue::new(
                    code::ALPHA_CHANNEL_REMOVED,
                    "info",
                    "pair",
                    "output drops the alpha channel; output compared as fully opaque",
                ));
            }
        }
        // PSD 合成的 alpha 已與白色混合；和直通 alpha 的語意不同。
        if (oa.sample_model.alpha == "matted_white") != (ob.sample_model.alpha == "matted_white")
            && has_alpha(a.st.layout.model)
            && has_alpha(b.st.layout.model)
        {
            let other =
                if oa.sample_model.alpha == "matted_white" { &ob.sample_model.alpha } else { &oa.sample_model.alpha };
            issues.push(Issue::new(
                code::ALPHA_SEMANTICS_DIFFER,
                "warning",
                "pair",
                format!("PSD merged colour is blended with white where alpha < max; the other file's alpha is {other}"),
            ));
        }
        // AVIF：像素驗證的無損判定，和 metadata 的判定分開記錄（欄位在最後寫回，問題照 C++ 版的順序）。
        for (i, (f, this, other)) in [(oa, "original", ob), (ob, "output", oa)].into_iter().enumerate() {
            if f.avif.as_ref().is_none_or(|av| av.lossless_metadata != "consistent") {
                continue;
            }
            let sc = &r.samples;
            avif_verdict[i] = Some(if sc.status == "equal" {
                let peer = if this == "original" { "output" } else { "original" };
                issues.push(Issue::new(
                    code::AVIF_PIXEL_VERIFIED_LOSSLESS,
                    "info",
                    this,
                    format!("pixel-verified: samples equal the {peer} ({})", other.format),
                ));
                (
                    Some(true),
                    "every sample (alpha included, also under A=0) equals the other file's native samples".to_string(),
                )
            } else if sc.status == "differs" {
                (Some(false), "samples differ from the other file".to_string())
            } else {
                let why = if sc.reason.is_empty() { &sc.status } else { &sc.reason };
                (None, format!("not compared: {why}"))
            });
        }

        // ---- 幾何
        if oa.width != 0 && ob.width != 0 {
            let same = oa.width == ob.width
                && oa.height == ob.height
                && oa.orientation.value == ob.orientation.value
                && !matches!(oa.orientation.source.as_str(), "unparseable" | "unknown")
                && !matches!(ob.orientation.source.as_str(), "unparseable" | "unknown");
            let orient_unknown =
                not_cmp_reason == code::ORIENTATION_UNPARSEABLE || not_cmp_reason == code::ORIENTATION_UNKNOWN;
            r.geometry.status = if same {
                "equal"
            } else if orient_unknown {
                "unknown"
            } else {
                "differs"
            }
            .into();
            r.geometry.basis = format!(
                "stored size {}x{} vs {}x{}; orientation {} ({}) vs {} ({})",
                oa.width,
                oa.height,
                ob.width,
                ob.height,
                oa.orientation.value,
                oa.orientation.source,
                ob.orientation.value,
                ob.orientation.source
            );
        } else {
            r.geometry.status = "unknown".into();
        }
        if [code::GEOMETRY_DIFFERS, code::ORIENTATION_DIFFERS, code::BIT_DEPTH_CHANGED, code::SAMPLE_MODEL_CHANGED]
            .contains(&not_cmp_reason)
        {
            let msg = if not_cmp_reason == code::BIT_DEPTH_CHANGED {
                format!("bit depth {} vs {}", a.st.layout.bits, b.st.layout.bits)
            } else if not_cmp_reason == code::SAMPLE_MODEL_CHANGED {
                format!("{} vs {}", a.st.layout.model, b.st.layout.model)
            } else {
                r.geometry.basis.clone()
            };
            issues.push(Issue::new(not_cmp_reason, "error", "pair", msg));
        }

        // ---- 色彩（D4、D12）與 ICC（D2）
        let (ca, cb) = (&oa.color, &ob.color);
        let mut pair_issue = |c: &str, sev: &str, m: String| issues.push(Issue::new(c, sev, "pair", m));
        let je = jxl_equivalence(oa, ob);
        if ca.kind.is_empty() || cb.kind.is_empty() {
            r.color = PairStatus::new("unknown", "colour information not available (decoder did not start)");
        } else if let Some((basis, note)) = je {
            r.color = PairStatus::new("equal", basis);
            if !note.is_empty() {
                pair_issue("rendering_intent_differs", "info", note);
            }
        } else if (ca.kind == "icc" && cb.kind == "webp_default_srgb")
            || (ca.kind == "webp_default_srgb" && cb.kind == "icc")
        {
            r.color = PairStatus::new(
                "unverified",
                "embedded ICC in one file, WebP specification default (sRGB, not declared) in the other",
            );
            let c = if ca.kind == "icc" { code::ICC_MISSING_ON_OUTPUT } else { code::ICC_MISSING_ON_ORIGINAL };
            pair_issue(c, "warning", "ICC profile not carried over to the WebP file".into());
            pair_issue(code::COLOR_UNVERIFIED, "warning", r.color.basis.clone());
        } else if ca.kind == "none" && cb.kind == "none" {
            r.color = PairStatus::new("unknown", "neither file declares colour information");
            pair_issue(code::COLOR_UNKNOWN, "warning", r.color.basis.clone());
        } else if !ca.key.is_empty() && ca.kind == cb.kind && ca.key == cb.key {
            r.color = PairStatus::new("equal", ca.description.clone());
            if ca.signals != cb.signals {
                pair_issue(
                    code::LOW_PRIORITY_COLOR_SIGNAL_DIFFERS,
                    "warning",
                    "effective colour is equal, lower-priority signals differ".into(),
                );
            }
        } else if ca.kind == "icc" && cb.kind == "icc" {
            r.color = PairStatus::new(
                "differs",
                format!(
                    "embedded ICC profiles differ (sha256 {} vs {})",
                    short_hash(&oa.icc.sha256),
                    short_hash(&ob.icc.sha256)
                ),
            );
            pair_issue(code::ICC_DIFFERS, "warning", format!("{}; appearance change is not proven", r.color.basis));
        } else if ca.kind == "icc" && cb.kind == "none" {
            r.color = PairStatus::new("differs", "ICC profile present in original, absent in output");
            pair_issue(code::ICC_MISSING_ON_OUTPUT, "warning", r.color.basis.clone());
        } else if ca.kind == "none" && cb.kind == "icc" {
            r.color = PairStatus::new("differs", "ICC profile absent in original, present in output");
            pair_issue(code::ICC_MISSING_ON_ORIGINAL, "warning", r.color.basis.clone());
        } else {
            r.color = PairStatus::new("unverified", format!("{} vs {} (no reliable equivalence)", ca.kind, cb.kind));
            pair_issue(code::COLOR_UNVERIFIED, "warning", r.color.basis.clone());
        }
        let (ia, ib) = (&oa.icc, &ob.icc);
        let (sa, sb) = (ia.status == "synthesized", ib.status == "synthesized");
        let same_hash = if ia.sha256 == ib.sha256 { "identical" } else { "differs" };
        r.icc = match (ia.status.as_str(), ib.status.as_str()) {
            ("absent", "absent") => PairStatus::new("both_absent", ""),
            ("embedded", "embedded") => PairStatus::new(
                if ia.sha256 == ib.sha256 { "equal" } else { "differs" },
                "sha256 of decompressed profile bytes",
            ),
            (_, "embedded") if sa => embedded_vs_synth(same_hash),
            ("embedded", _) if sb => embedded_vs_synth(same_hash),
            _ if sa && sb => PairStatus::new("both_synthesized", format!("sha256 {same_hash}")),
            (_, "absent") if sa => PairStatus::new("synthesized_vs_absent", ""),
            ("absent", _) if sb => PairStatus::new("synthesized_vs_absent", ""),
            ("embedded", "absent") => PairStatus::new("missing_on_output", ""),
            ("absent", "embedded") => PairStatus::new("missing_on_original", ""),
            (x, y) => PairStatus::new("unreadable", format!("{x} vs {y}")),
        };

        // WebP 政策：原檔超過 16383 時，無法以原尺寸交付成 WebP。
        if ob.format == "webp" && (oa.width > 16383 || oa.height > 16383) {
            issues.push(Issue::new(
                code::WEBP_POLICY_CANNOT_HOLD_ORIGINAL,
                "info",
                "pair",
                format!(
                    "original is {}x{}; under the 16383 WebP policy it cannot be delivered as WebP at its original size",
                    oa.width, oa.height
                ),
            ));
        }

        // ---- metadata
        let mut meta_changed = false;
        for f in METADATA_FIELDS {
            let mf = compare_item(f, oa.metadata.get(f), ob.metadata.get(f));
            meta_changed |= mf.status != "same" && mf.status != "both_absent";
            r.metadata.fields.push(mf);
        }
        if meta_changed {
            issues.push(Issue::new(
                code::METADATA_CHANGED,
                "info",
                "pair",
                "non-image metadata differs (reported separately, does not affect samples)",
            ));
        }

        // ---- 大小
        let (sa_, sb_) = (a.st.input.size(), b.st.input.size());
        r.size.original_bytes = Count(sa_);
        r.size.output_bytes = Some(Count(sb_));
        r.size.comparable = complete;
        if sa_ > 0 {
            let ratio = to_f64(sb_) / to_f64(sa_);
            r.size.output_ratio = Some(ratio);
            r.size.saving_ratio = Some(1.0 - ratio);
        }
        if sb_ > 0 {
            r.size.compression_factor = Some(to_f64(sa_) / to_f64(sb_));
        }
        if !complete {
            r.size.note = "at least one file did not decode completely; not a valid conversion size comparison".into();
        }
    } else {
        r.size.original_bytes = Count(a.st.input.size());
        r.size.comparable = false;
    }

    // ---- metrics（引擎側）
    let m = &mut r.metrics;
    m.time_ms.insert("scan_original".into(), a.st.scan_ms);
    if let Some(b) = &b {
        m.time_ms.insert("scan_output".into(), b.st.scan_ms);
    }
    m.time_ms.insert(if compare { "decode_and_compare" } else { "decode" }.into(), dec_ms);
    m.time_ms.insert("analyze_total".into(), ms_since(t_all));
    m.timing_note = if compare {
        "both files are decoded row by row in lockstep; decode_and_compare covers both decoders and the comparison together"
    } else {
        "single file decode"
    }
    .into();
    let mut mem: BTreeMap<String, u64> = BTreeMap::new();
    mem.insert("tool_row_buffers".into(), 0);
    let (mut t_written, mut t_read, mut t_ms) = (0u64, 0u64, 0f64);
    for s in std::iter::once(&a).chain(b.as_ref()) {
        *mem.get_mut("tool_row_buffers").expect("inserted") += s.st.canon.capacity() as u64 * 2;
        if let Some(d) = s.dec.as_ref().or(s.stopped.as_ref()) {
            d.memory_metrics(&mut mem);
        }
        if let Some(t) = &s.st.temp {
            t_written += t.written();
            t_read += t.bytes_read();
            t_ms += t.io_ms();
        }
    }
    for (k, v) in mem {
        m.memory_bytes.insert(k, Some(Count(v)));
    }
    m.memory_bytes.insert("row_budget_estimate".into(), Some(Count(need)));
    if codec_need_peak > 0 {
        m.memory_bytes.insert("codec_need_estimate_peak".into(), Some(Count(codec_need_peak)));
    }
    let reserved = quota.borrow().reserved();
    m.memory_bytes.insert("temp_reserved_peak".into(), Some(Count(reserved)));
    m.memory_bytes.insert("temp_written".into(), Some(Count(t_written)));
    m.memory_bytes.insert("temp_read".into(), Some(Count(t_read)));
    if reserved > 0 {
        m.time_ms.insert("temp_io".into(), t_ms);
    }
    m.io_read_bytes = Some(Count(input.original.bytes_read() + input.output.map_or(0, Input::bytes_read)));

    r.issues = issues;
    let reps = std::iter::once(&mut a.st.rep).chain(b.as_mut().map(|b| &mut b.st.rep));
    for (rep, v) in reps.zip(avif_verdict) {
        if let (Some(av), Some((pv, basis))) = (rep.avif.as_mut(), v) {
            av.pixel_verified = pv;
            av.pixel_verified_basis = basis;
        }
    }
    r.original = std::mem::take(&mut a.st.rep);
    if let Some(mut b) = b {
        r.output = Some(std::mem::take(&mut b.st.rep));
        strict_compare(&mut r, not_cmp_reason);
    } else {
        r.strict.result = "not_evaluated".into();
        r.strict.reasons.push("inspect_mode".into());
    }
    Outcome::Done(Box::new(r))
}

fn embedded_vs_synth(same_hash: &str) -> PairStatus {
    PairStatus::new(
        "embedded_vs_synthesized",
        format!("one side's ICC is synthesized by libjxl from its colour enum; sha256 {same_hash}"),
    )
}

fn strict_compare(r: &mut Report, not_cmp_reason: &str) {
    let out = r.output.as_ref().expect("compare has an output");
    let side_problem = |f: &FileReport| -> String {
        let or =
            |d: &str| if f.decode_status_reason.is_empty() { d.to_string() } else { f.decode_status_reason.clone() };
        match f.decode_status.as_str() {
            "not_checked" => or("not_checked"),
            "unsupported" => or("unsupported"),
            s if s != "complete" || f.container_status == "corrupt" => code::DECODE_FAILED.into(),
            _ => String::new(),
        }
    };
    let (pa, pb) = (side_problem(&r.original), side_problem(out));
    let mut reasons = Vec::new();
    let result;
    if !pa.is_empty() || !pb.is_empty() {
        result = "not_evaluated";
        if !pa.is_empty() {
            reasons.push(format!("original:{pa}"));
        }
        if !pb.is_empty() {
            reasons.push(format!("output:{pb}"));
        }
    } else if not_cmp_reason == code::ORIENTATION_UNPARSEABLE || not_cmp_reason == code::ORIENTATION_UNKNOWN {
        result = "indeterminate";
        reasons.push(not_cmp_reason.to_string());
    } else if !not_cmp_reason.is_empty() {
        result = "mismatch";
        reasons.push(not_cmp_reason.to_string());
    } else if r.samples.status == "differs" {
        result = "mismatch";
        reasons.push(code::SAMPLES_DIFFER.to_string());
        let d = r.samples.pixels_differing.map_or(0, |c| c.0);
        let c = r.samples.pixels_compared.map_or(0, |c| c.0);
        r.add_issue(code::SAMPLES_DIFFER, "error", "pair", format!("{d} of {c} pixels differ"));
    } else {
        let sides = [("original", &r.original), ("output", out)];
        // 結構政策：PNG 的警告是尾端資料；其他格式一律稱為容器警告。
        for (p, f) in sides {
            if f.container_status != "valid" {
                reasons
                    .push(format!("{p}:{}", if f.format == "png" { code::TRAILING_DATA } else { "container_warning" }));
            }
        }
        // Alpha 語意：只有直通 alpha（或沒有 alpha）才可能嚴格一致。
        for (p, f) in sides {
            if matches!(f.sample_model.alpha.as_str(), "associated" | "unspecified" | "matted_white") {
                reasons.push(format!("{p}:alpha_{}", f.sample_model.alpha));
            }
        }
        for c in [code::PSD_COMPOSITE_UNIFORM, code::EXTRA_CHANNEL_NOT_SUPPORTED] {
            for i in &r.issues {
                if i.code == c && (i.scope == "original" || i.scope == "output") {
                    reasons.push(format!("{}:{c}", i.scope));
                }
            }
        }
        if r.color.status != "equal" {
            const COLOR_CODES: [&str; 5] = [
                code::COLOR_UNKNOWN,
                code::COLOR_UNVERIFIED,
                code::ICC_DIFFERS,
                code::ICC_MISSING_ON_OUTPUT,
                code::ICC_MISSING_ON_ORIGINAL,
            ];
            for i in &r.issues {
                if i.scope == "pair" && COLOR_CODES.contains(&i.code.as_str()) {
                    reasons.push(i.code.clone());
                }
            }
            if reasons.is_empty() {
                reasons.push(code::COLOR_UNVERIFIED.into());
            }
        }
        result = if reasons.is_empty() { "match" } else { "indeterminate" };
    }
    r.strict.result = result.into();
    r.strict.reasons = reasons;
}

/// 檢視用的一側解碼結果（樣本交給 [`ViewSink`]，依影像順序，未做任何轉換）。
pub(crate) struct ViewSamples {
    pub layout: RowLayout,
    pub width: u32,
    pub height: u32,
    /// 成功解出的列數；小於 height 時表示檔案在中途失敗
    pub rows: u32,
    pub format: String,
    /// 第一個錯誤問題的訊息（解碼失敗或不支援時）
    pub error: Option<String>,
}

/// 檢視解碼的停止原因。
pub(crate) enum ViewStop {
    Cancelled,
    /// 樣本緩衝無法配置（bytes）
    TooLarge(u64),
    /// 寬×高超過呼叫者給的檢視上限
    OverViewLimit {
        width: u32,
        height: u32,
    },
}

/// 檢視解碼的列接收端：同一個程序裡收進記憶體，或在 worker 裡寫到 stdout。
pub(crate) trait ViewSink {
    /// 版面確定、解碼器開始之後，第一列之前呼叫一次。
    fn begin(&mut self, layout: &RowLayout, width: u32, height: u32) -> Result<(), ViewStop>;
    /// 依序每列一次。
    fn row(&mut self, canon: &[u16]) -> Result<(), ViewStop>;
}

/// 收進記憶體的接收端（先一次預留整張的大小）。
#[derive(Default)]
pub(crate) struct VecSink {
    pub samples: Vec<u16>,
}

impl ViewSink for VecSink {
    fn begin(&mut self, layout: &RowLayout, width: u32, height: u32) -> Result<(), ViewStop> {
        let total = u64::from(width).saturating_mul(u64::from(layout.channels)).saturating_mul(u64::from(height));
        let bytes = total.saturating_mul(2);
        let total = usize::try_from(total).map_err(|_| ViewStop::TooLarge(bytes))?;
        self.samples.try_reserve_exact(total).map_err(|_| ViewStop::TooLarge(bytes))
    }

    fn row(&mut self, canon: &[u16]) -> Result<(), ViewStop> {
        self.samples.extend_from_slice(canon);
        Ok(())
    }
}

/// 把一個輸入整張解碼成正規樣本，供 GUI 檢視；格式不支援或中途失敗時仍回傳已解出的部分與錯誤訊息。
pub(crate) fn decode_view_samples(
    input: &Input<'_>,
    temp: Option<&File>,
    temp_free_bytes: u64,
    lim: &Limits,
    max_pixels: u64,
    cancel: &CancelToken,
    sink: &mut dyn ViewSink,
) -> Result<ViewSamples, ViewStop> {
    let quota = Rc::new(RefCell::new(TempQuota::new(lim.temp_gib << 30, temp_free_bytes)));
    let mut issues = Vec::new();
    let mut s = new_side("original", input, temp, &quota);
    prepare_side(&mut s, &mut issues, lim);
    let mut out = ViewSamples {
        layout: RowLayout::default(),
        width: 0,
        height: 0,
        rows: 0,
        format: s.st.rep.format.clone(),
        error: None,
    };
    let first_error = |issues: &[Issue]| issues.iter().find(|i| i.severity == "error").map(|i| i.message.clone());
    if s.dec.is_none() {
        out.error = first_error(&issues);
        return Ok(out);
    }
    // 檢視上限在 open() 之前檢查：open() 會配置解碼緩衝
    if u64::from(s.st.width) * u64::from(s.st.height) > max_pixels {
        return Err(ViewStop::OverViewLimit { width: s.st.width, height: s.st.height });
    }
    if !s.open(&mut issues, lim) {
        out.error = first_error(&issues).or_else(|| Some("decoder could not start".into()));
        return Ok(out);
    }
    let row_len = s.st.width as usize * s.st.layout.channels as usize;
    sink.begin(&s.st.layout, s.st.width, s.st.height)?;
    out.layout = s.st.layout.clone();
    out.width = s.st.width;
    out.height = s.st.height;
    s.st.canon = vec![0; row_len];
    let mut rows = 0u32;
    while !s.st.failed && s.st.rows < u64::from(s.st.height) {
        if cancel.is_cancelled() {
            return Err(ViewStop::Cancelled);
        }
        if !s.read_row(&mut issues) {
            break;
        }
        sink.row(&s.st.canon)?;
        rows += 1;
    }
    s.finish(&mut issues);
    out.rows = rows;
    if out.rows < out.height || s.st.failed {
        out.error = first_error(&issues).or_else(|| Some("decoding stopped early".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_item_statuses() {
        let present = |s: &str, sum: &str| MetaItem { present: true, status: s.into(), summary: sum.into() };
        assert_eq!(compare_item("x", None, None).status, "both_absent");
        assert_eq!(compare_item("x", Some(&present("present", "a")), None).status, "removed");
        assert_eq!(compare_item("x", None, Some(&present("present", "a"))).status, "added");
        assert_eq!(compare_item("x", Some(&present("present", "a")), Some(&present("present", "a"))).status, "same");
        assert_eq!(compare_item("x", Some(&present("present", "a")), Some(&present("present", "b"))).status, "changed");
        assert_eq!(
            compare_item("x", Some(&present("parse_failed", "")), Some(&present("over_limit", ""))).status,
            "parse_failed"
        );
    }

    #[test]
    fn suggested_limits_round_up() {
        assert_eq!(suggest_limit_mib(1, 0, 16, 1 << 16), Some(16));
        assert_eq!(suggest_limit_mib(17 << 20, 0, 16, 1 << 16), Some(32));
        assert_eq!(suggest_limit_mib(u64::MAX, 1, 16, 1 << 16), None);
        assert_eq!(suggest_limit_mib(1 << 40, 0, 16, 1 << 16), None);
    }

    #[test]
    fn job_limit_failure_follows_cpp_form() {
        let lim = Limits::default();
        let png = job_limit_failure("png", AllocStage::Decoder, &lim).expect("png is side level");
        assert_eq!(png.message, "libpng: Out of memory");
        assert_eq!(png.decoder_error.as_deref(), Some("Out of memory"));
        let webp_tool = job_limit_failure("webp", AllocStage::ToolBuffers, &lim).expect("webp is side level");
        assert!(webp_tool.message.starts_with("buffers for the WebP file"), "{}", webp_tool.message);
        assert_eq!(webp_tool.decoder_error, None);
        let webp_dec = job_limit_failure("webp", AllocStage::Decoder, &lim).expect("webp is side level");
        assert_eq!(webp_dec.decoder_error.as_deref(), Some("libwebp: OUT_OF_MEMORY"));
        let jxl = job_limit_failure("jxl_container", AllocStage::Decoder, &lim).expect("jxl is side level");
        assert!(jxl.message.contains(&lim.codec_cap_bytes().to_string()), "{}", jxl.message);
        // C++ 版的 TIFF 沒有攔下配置失敗：維持執行層級
        assert_eq!(job_limit_failure("tiff", AllocStage::Decoder, &lim), None);
    }

    #[test]
    fn alloc_stage_codes_round_trip() {
        for s in [AllocStage::Decoder, AllocStage::ToolBuffers] {
            assert_eq!(AllocStage::from_code(u64::from(s.code())), Some(s));
        }
        assert_eq!(AllocStage::from_code(2), None);
    }
}
