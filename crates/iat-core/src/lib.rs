//! iat-core：一次分析與它的報告，不做主控台輸出、不結束程序。
//! CLI 與 GUI 都直接呼叫這個函式庫。
//!
//! 對照組是 C++ 版 iat 1.0.0；判定與 JSON 契約見 `docs/plan.md` 與 C++ 版
//! `docs/cli-json-contract.md`，和 C++ 版不同的地方記在 `docs/decisions.md`。
//!
//! unsafe 只允許出現在 `win` 模組（D-R03）；AV1 的 unsafe 包裝放在獨立的 iat-av1 crate。
#![deny(unsafe_code)]

pub mod engine;
pub mod exit;
pub mod inflate;
pub mod input;
pub mod jxl_header;
pub mod jxl_icc;
pub mod jxl_scan;
pub mod jxl_side;
pub mod limits;
pub mod markdown;
pub mod paths;
pub mod png_scan;
pub mod png_side;
pub mod psd_scan;
pub mod psd_side;
pub mod report;
pub mod run;
pub mod sniff;
pub mod temp;
pub mod text;
pub mod tiff_codec;
pub mod tiff_scan;
pub mod tiff_side;
pub mod view;
pub mod webp_scan;
pub mod webp_side;
pub mod win;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use limits::LimitError;
pub use limits::Limits;
pub use report::{Report, report_json_text};
pub use run::{
    CleanupFailure, ReportTargetError, RunRequest, WriteReportError, run_analysis, validate_report_targets,
    write_new_file,
};
pub use view::{DecodedImage, ViewError, decode_for_view};

/// 工具版本（workspace 的 version，單一來源）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 建置時的 rustc 版本，寫進報告的 `tool.compiler`。
pub const COMPILER: &str = env!("IAT_RUSTC_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Compare,
    Inspect,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Compare => "compare",
            Mode::Inspect => "inspect",
        }
    }
}

/// 取消旗標；可跨執行緒共用（CLI 的 Ctrl+C 處理、GUI 的取消按鈕）。
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// 讓 Ctrl+C／Ctrl+Break／關閉主控台設定這個旗標（整個程序只安裝一次）。
    pub fn install_console_handler(&self) {
        win::install_ctrl_handler(Arc::clone(&self.0));
    }
}

/// 進度階段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// 開檔並計算 SHA-256（done／total 為 bytes）
    Hash,
    /// 掃描、解碼與比較
    Analyze,
}

pub trait Progress: Send + Sync {
    fn report(&self, stage: Stage, done: u64, total: u64);
}

/// 不需要進度回報時使用。
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _: Stage, _: u64, _: u64) {}
}

pub(crate) fn ms_since(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}
