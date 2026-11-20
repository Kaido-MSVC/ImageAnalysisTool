//! 一次分析的外層流程：唯讀開檔、雜湊、暫存目錄、呼叫引擎、組成最終報告。
//! 對照 C++ 版 `src/core/run.cpp` 與 `src/core/report_io.cpp`。
//!
//! 與 C++ 版的主要差別：引擎在同一程序內執行，沒有 iat-worker.exe 與 Job Object
//! （見 docs/decisions.md D-R02）；取消與期限在引擎的列與列之間檢查。

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use std::time::Duration;

use crate::engine::{AnalysisInput, Outcome, analyze};
use crate::exit::decide_exit;
use crate::input::{Input, read_full_at};
use crate::limits::Limits;
use crate::paths::{file_name_of, full_path, is_directory, parent_dir_of, same_path, trim_dir};
use crate::report::{Count, FileReport, Report, TestSettings, code};
use crate::sniff::ext_format;
use crate::temp::{self, RunTemp};
use crate::{CancelToken, Mode, Progress, Stage, ms_since, win};

const FILE_SHARE_READ: u32 = 0x1;
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const ERROR_DIRECTORY: u32 = 267;
const ERROR_FILE_EXISTS: u32 = 80;
const ERROR_ALREADY_EXISTS: u32 = 183;

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub mode: Mode,
    pub original: PathBuf,
    /// compare 才有
    pub output: Option<PathBuf>,
    /// None：系統暫存目錄；否則是已存在的目錄
    pub temp_dir: Option<PathBuf>,
    /// 報告記錄完整路徑
    pub full_paths: bool,
    pub limits: Limits,
}

/// 唯讀開檔，只允許別人讀（期間沒有人能寫入或取代）。目錄視為錯誤。
fn open_read_only(p: &Path) -> Result<File, u32> {
    let f = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(p)
        .map_err(|e| win::os_code(&e))?;
    if f.metadata().is_ok_and(|m| m.is_dir()) {
        return Err(ERROR_DIRECTORY);
    }
    Ok(f)
}

/// 整檔雜湊的結果：取消與讀取失敗要分開處理（取消停在這裡，讀取失敗是輸入錯誤）。
#[derive(Debug)]
enum HashOutcome {
    Done(String),
    Cancelled,
    Failed(io::Error),
}

fn sha256_file(input: &Input<'_>, cancel: &CancelToken, progress: &dyn Progress) -> HashOutcome {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        if cancel.is_cancelled() {
            return HashOutcome::Cancelled;
        }
        let n = match read_full_at(input.file(), done, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return HashOutcome::Failed(e),
        };
        h.update(&buf[..n]);
        done += n as u64;
        progress.report(Stage::Hash, done, input.size());
    }
    HashOutcome::Done(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// UUID 第 4 版，8-4-4-4-12 小寫十六進位；系統亂數失敗時回傳 NTSTATUS。
fn new_run_id() -> Result<String, i32> {
    let mut b = win::random_16()?;
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

/// run_id 的前 8 個字元（報告暫存檔名用）；不足 8 個位元組或不在字元邊界時用整個 run_id。
fn run_id_prefix(run_id: &str) -> &str {
    run_id.get(..8).unwrap_or(run_id)
}

fn set_not_checked(f: &mut FileReport, reason: &str) {
    f.decode_status = "not_checked".into();
    f.decode_status_reason = reason.into();
    if f.container_status.is_empty() {
        f.container_status = "unverified".into();
    }
}

/// 暫存目錄的準備結果。`error` 為 None 時 `files` 一定有值；有錯誤時其餘欄位照樣寫進報告。
#[derive(Default)]
struct TempInfo {
    dir: Option<PathBuf>,
    user_specified: bool,
    free_bytes: u64,
    stale_removed: u32,
    files: Option<RunTemp>,
    error: Option<String>,
}

/// 暫存目錄、清掉舊的暫存檔、剩餘空間，再建立這次的兩個暫存檔（同 C++ 版 `create_temp`，D-R04）。
fn prepare_temp(req: &RunRequest, run_id: &str) -> TempInfo {
    let user_specified = req.temp_dir.is_some();
    let dir = match &req.temp_dir {
        Some(d) => trim_dir(full_path(d)),
        None => trim_dir(std::env::temp_dir()),
    };
    let mut t = TempInfo { user_specified, ..TempInfo::default() };
    if dir.as_os_str().is_empty() || !is_directory(&dir) {
        t.error = Some("temporary directory is not available".into());
        return t;
    }
    t.dir = Some(dir.clone());
    t.stale_removed = temp::remove_stale(&dir);
    match win::disk_free_bytes(&dir) {
        Ok(n) => t.free_bytes = n,
        Err(e) => {
            t.error = Some(format!("cannot query free space of the temporary directory: {}", win::error_text(e)));
            return t;
        }
    }
    match RunTemp::create(&dir, run_id) {
        Ok(f) => t.files = Some(f),
        Err(e) => t.error = Some(e.to_string()),
    }
    t
}

/// 一個輸入的身分欄位：檔名、大小、雜湊。開檔、取大小、讀檔頭或雜湊讀取失敗都記為
/// input_open_failed（同 C++ 版開檔失敗的對應：退出碼 5），此時不回傳 Input。
/// 雜湊時被取消則回傳 Input、sha256 留空，由呼叫者判定取消。
fn describe_input<'f>(
    r: &mut Report,
    req: &RunRequest,
    which: &str,
    path: &Path,
    h: &'f Result<File, u32>,
    cancel: &CancelToken,
    progress: &dyn Progress,
) -> (FileReport, Option<Input<'f>>) {
    let mut f = FileReport { name: file_name_of(path), ..FileReport::default() };
    if req.full_paths {
        f.path = full_path(path).to_string_lossy().into_owned();
    }
    let failure = match h {
        Err(e) => format!("cannot open input: {}", win::error_text(*e)),
        Ok(file) => match Input::new(file) {
            Err(e) => format!("cannot read input: {}", win::error_text(win::os_code(&e))),
            Ok(input) => {
                f.size_bytes = Count(input.size());
                match sha256_file(&input, cancel, progress) {
                    HashOutcome::Done(hex) => {
                        f.sha256 = hex;
                        return (f, Some(input));
                    }
                    HashOutcome::Cancelled => return (f, Some(input)),
                    HashOutcome::Failed(e) => {
                        format!("cannot read input while hashing: {}", win::error_text(win::os_code(&e)))
                    }
                }
            }
        },
    };
    set_not_checked(&mut f, "input_open_failed");
    r.add_issue(code::INPUT_OPEN_FAILED, "error", which, failure);
    (f, None)
}

/// 執行一次分析。輸入、環境的問題（包括取消）一律寫進回傳的報告，
/// 報告的 exit 欄位已經判定好；這裡不寫報告檔。
pub fn run_analysis(req: &RunRequest, cancel: &CancelToken, progress: &dyn Progress) -> Report {
    let t_all = Instant::now();

    // 系統亂數失敗時 run_id 留空並記為環境錯誤，絕不用固定值代替。
    let (run_id, run_id_error) = match new_run_id() {
        Ok(id) => (id, None),
        Err(st) => (
            String::new(),
            Some(format!("cannot generate a run id: BCryptGenRandom failed with NTSTATUS 0x{:08X}", st as u32)),
        ),
    };
    let mut r = Report { run_id, mode: req.mode.as_str().into(), ..Report::default() };
    r.tool.insert("name".into(), "iat".into());
    r.tool.insert("version".into(), crate::VERSION.into());
    r.tool.insert("compiler".into(), crate::COMPILER.into());
    r.settings.limits = req.limits.settings();
    r.settings.codec_threads = 1;
    r.settings.full_paths = req.full_paths;
    let pct = req.limits.test_codec_estimate_pct;
    if pct != 100 {
        r.settings.test = Some(TestSettings { codec_estimate_pct: pct });
        r.add_issue(
            code::TEST_OPTION_USED,
            "warning",
            "run",
            format!(
                "test-only --test-codec-estimate-pct {pct} scaled decoder memory estimates down; results show limit handling, not normal operation"
            ),
        );
    }

    // ---- 唯讀開檔
    let t_open = Instant::now();
    let fo = open_read_only(&req.original);
    let fp = req.output.as_deref().map(open_read_only);
    let (rep_o, in_o) = describe_input(&mut r, req, "original", &req.original, &fo, cancel, progress);
    r.original = rep_o;
    let mut open_failed = in_o.is_none();
    let mut in_p = None;
    if let (Some(p), Some(h)) = (req.output.as_deref(), fp.as_ref()) {
        let (out, input) = describe_input(&mut r, req, "output", p, h, cancel, progress);
        open_failed |= input.is_none();
        in_p = input;
        r.output = Some(out);
        if let (Ok(a), Ok(b)) = (&fo, h)
            && let (Some(ia), Some(ib)) = (win::file_identity(a), win::file_identity(b))
            && ia == ib
        {
            r.add_issue(code::SAME_FILE, "info", "pair", "original and output are the same file");
        }
    }
    r.metrics.time_ms.insert("open_and_hash".into(), ms_since(t_open));

    // 雜湊時被取消：到此為止。
    let cancelled_early = !open_failed && cancel.is_cancelled();
    if cancelled_early {
        set_not_checked(&mut r.original, "cancelled");
        if let Some(o) = r.output.as_mut() {
            set_not_checked(o, "cancelled");
        }
        r.add_issue(
            code::CANCELLED,
            "error",
            "run",
            "cancelled by user (Ctrl+C / Ctrl+Break) while hashing the inputs",
        );
    }

    // ---- 暫存目錄與暫存檔（C++ 版只在要啟動 worker 時才準備，失敗時位置記為 system_temp）
    // 沒有 run_id 就不能建立暫存檔：當作環境錯誤，與暫存目錄失敗一樣停在這裡。
    let mut temp = if open_failed || cancelled_early {
        TempInfo::default()
    } else if let Some(e) = run_id_error {
        TempInfo { error: Some(e), ..TempInfo::default() }
    } else {
        prepare_temp(req, &r.run_id)
    };
    r.settings.temp.location = if temp.user_specified { "user_specified" } else { "system_temp" }.into();
    r.settings.temp.free_bytes_at_start = Count(temp.free_bytes);
    r.settings.temp.delete_on_close = true;
    if req.full_paths {
        r.settings.temp.dir = temp.dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    }
    if temp.stale_removed > 0 {
        r.add_issue(
            "temp_stale_removed",
            "info",
            "run",
            format!("{} leftover temporary file(s) of an earlier run removed", temp.stale_removed),
        );
    }
    if let Some(e) = &temp.error {
        set_not_checked(&mut r.original, "environment_error");
        if let Some(o) = r.output.as_mut() {
            set_not_checked(o, "environment_error");
        }
        r.add_issue(code::ENVIRONMENT_ERROR, "error", "run", e.clone());
    }

    if !open_failed
        && !cancelled_early
        && temp.error.is_none()
        && let Some(orig) = in_o.as_ref()
        && let Some(mut files) = temp.files.take()
    {
        progress.report(Stage::Analyze, 0, 1);
        let t_engine = Instant::now();
        let input = AnalysisInput {
            mode: req.mode,
            original: orig,
            output: in_p.as_ref(),
            limits: req.limits,
            temp: [files.files[0].as_ref(), files.files[1].as_ref()],
            temp_free_bytes: temp.free_bytes,
            cancel,
            deadline: t_engine + Duration::from_secs(req.limits.deadline_s),
        };
        let outcome = analyze(&input);
        r.metrics.time_ms.insert("engine_wall".into(), ms_since(t_engine));
        // 引擎已結束：關閉 handle 就是最後一次關閉，暫存檔此時必須消失。
        for left in files.close() {
            r.add_issue(
                code::TEMP_CLEANUP_FAILED,
                "warning",
                "run",
                format!("temporary file still exists after close: {}", file_name_of(&left)),
            );
        }
        let wr = match outcome {
            Outcome::Done(wr) => *wr,
            Outcome::Cancelled | Outcome::Deadline => {
                let cancelled = matches!(outcome, Outcome::Cancelled);
                let reason = if cancelled { "cancelled" } else { "deadline" };
                set_not_checked(&mut r.original, reason);
                if let Some(o) = r.output.as_mut() {
                    set_not_checked(o, reason);
                }
                if cancelled {
                    r.add_issue(code::CANCELLED, "error", "run", "cancelled by user (Ctrl+C / Ctrl+Break)");
                } else {
                    let s = req.limits.deadline_s;
                    r.add_issue(
                        code::DEADLINE_EXCEEDED,
                        "error",
                        "run",
                        format!("analysis exceeded deadline of {s} s (raise with --limits deadline_s=)"),
                    );
                }
                return finish_report(req, r, t_all);
            }
        };
        // 身分欄位（檔名、大小、雜湊）與執行層級資料保留外層的值。
        let merge = |dst: &mut FileReport, mut src: FileReport| {
            src.name = std::mem::take(&mut dst.name);
            src.path = std::mem::take(&mut dst.path);
            src.size_bytes = dst.size_bytes;
            src.sha256 = std::mem::take(&mut dst.sha256);
            *dst = src;
        };
        merge(&mut r.original, wr.original);
        if let (Some(dst), Some(src)) = (r.output.as_mut(), wr.output) {
            merge(dst, src);
        }
        r.codecs = wr.codecs;
        r.geometry = wr.geometry;
        r.samples = wr.samples;
        r.color = wr.color;
        r.icc = wr.icc;
        r.metadata = wr.metadata;
        r.size = wr.size;
        r.strict = wr.strict;
        r.issues.extend(wr.issues);
        r.metrics.time_ms.extend(wr.metrics.time_ms);
        r.metrics.memory_bytes.extend(wr.metrics.memory_bytes);
        r.metrics.io_read_bytes = wr.metrics.io_read_bytes;
        r.metrics.timing_note = wr.metrics.timing_note;
        progress.report(Stage::Analyze, 1, 1);
    }
    finish_report(req, r, t_all)
}

/// 副檔名檢查、大小補值與退出碼判定。
fn finish_report(req: &RunRequest, mut r: Report, t_all: Instant) -> Report {
    let compare = req.mode == Mode::Compare;
    // ---- 副檔名檢查（格式由內容判定，從不看副檔名）
    let mut ext_issues = Vec::new();
    let sides =
        [("original", Some(&req.original), Some(&mut r.original)), ("output", req.output.as_ref(), r.output.as_mut())];
    for (scope, path, f) in sides {
        let (Some(path), Some(f)) = (path, f) else { continue };
        if f.format.is_empty() || f.format == "unknown" {
            continue;
        }
        let ef = ext_format(&file_name_of(path));
        let ok =
            ef == f.format || (ef == "tiff" && f.format == "bigtiff") || (ef == "jxl" && f.format.starts_with("jxl"));
        if !ok {
            f.extension_mismatch = true;
            ext_issues.push((scope, format!("content is {} but the extension does not match", f.format)));
        }
    }
    for (scope, msg) in ext_issues {
        r.add_issue(code::EXTENSION_MISMATCH, "warning", scope, msg);
    }
    if r.size.original_bytes.0 == 0 {
        r.size.original_bytes = r.original.size_bytes;
    }
    if compare && r.size.output_bytes.is_none() {
        r.size.output_bytes = r.output.as_ref().map(|o| o.size_bytes);
    }

    let (code, reason) = decide_exit(&r);
    r.exit.code = code;
    r.exit.reason = reason;
    r.metrics.time_ms.insert("total".into(), ms_since(t_all));
    r
}

/// 報告目標不合格的原因；Display 的文字與 C++ 版 `validate_report_targets` 相同。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportTargetError {
    /// 兩份報告指到同一個檔案
    SamePath,
    /// 報告路徑就是某個輸入檔
    IsInput(PathBuf),
    /// 報告已存在（絕不覆寫）
    AlreadyExists(PathBuf),
    /// 報告所在的目錄不存在
    DirectoryMissing(PathBuf),
}

impl fmt::Display for ReportTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SamePath => f.write_str("report paths refer to the same file"),
            Self::IsInput(p) => write!(f, "report path is an input file: {}", p.to_string_lossy()),
            Self::AlreadyExists(p) => write!(f, "report already exists (never overwritten): {}", p.to_string_lossy()),
            Self::DirectoryMissing(p) => write!(f, "report directory does not exist: {}", p.to_string_lossy()),
        }
    }
}

impl std::error::Error for ReportTargetError {}

/// 報告目標必須是既有目錄下的新檔，且不能和任何輸入或另一份報告相同。
///
/// # Errors
///
/// 第一個不合格的目標，見 [`ReportTargetError`]。
pub fn validate_report_targets(
    req: &RunRequest,
    report_md: Option<&Path>,
    report_json: Option<&Path>,
) -> Result<(), ReportTargetError> {
    let reports: Vec<&Path> = [report_md, report_json].into_iter().flatten().collect();
    if reports.len() == 2 && same_path(reports[0], reports[1]) {
        return Err(ReportTargetError::SamePath);
    }
    for rp in reports {
        if same_path(rp, &req.original) || req.output.as_deref().is_some_and(|o| same_path(rp, o)) {
            return Err(ReportTargetError::IsInput(rp.to_path_buf()));
        }
        if win::path_exists(rp) {
            return Err(ReportTargetError::AlreadyExists(rp.to_path_buf()));
        }
        let parent = parent_dir_of(rp);
        if !is_directory(&parent) {
            return Err(ReportTargetError::DirectoryMissing(parent));
        }
    }
    Ok(())
}

/// 報告暫存檔沒能刪掉（留在磁碟上，使用者要知道）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupFailure {
    pub tmp: PathBuf,
    /// Win32 錯誤碼
    pub code: u32,
}

/// 寫報告檔失敗的原因（Win32 錯誤碼）；Display 的文字與 C++ 版 `write_new_file` 相同，
/// 暫存檔刪除失敗時再附加一段（D-R13）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteReportError {
    /// 建立同目錄的暫存檔失敗
    CreateTemp { code: u32 },
    /// 寫入或 flush 失敗
    Write { code: u32, cleanup: Option<CleanupFailure> },
    /// 改名時目標已經出現（不覆寫）
    TargetExists { code: u32, cleanup: Option<CleanupFailure> },
    /// 其他改名失敗
    Rename { code: u32, cleanup: Option<CleanupFailure> },
}

impl fmt::Display for WriteReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, code, cleanup) = match self {
            Self::CreateTemp { code } => ("cannot create temporary file: ", *code, None),
            Self::Write { code, cleanup } => ("write failed: ", *code, cleanup.as_ref()),
            Self::TargetExists { code, cleanup } => {
                ("target appeared meanwhile, not overwritten: ", *code, cleanup.as_ref())
            }
            Self::Rename { code, cleanup } => ("rename failed: ", *code, cleanup.as_ref()),
        };
        write!(f, "{what}{}", win::error_text(code))?;
        if let Some(c) = cleanup {
            write!(f, "; temporary file not removed: {} ({})", c.tmp.to_string_lossy(), win::error_text(c.code))?;
        }
        Ok(())
    }
}

impl std::error::Error for WriteReportError {}

/// 先寫入同目錄的暫存檔並 flush，再以不取代既有檔的方式改名：既有目標絕不覆寫。
///
/// # Errors
///
/// 見 [`WriteReportError`]；失敗時目標檔不會被建立或修改。
pub fn write_new_file(target: &Path, data: &[u8], run_id: &str) -> Result<(), WriteReportError> {
    let full = full_path(target);
    let mut tmp_name = full.clone().into_os_string();
    tmp_name.push(format!(".iat-tmp-{}", run_id_prefix(run_id)));
    let tmp = PathBuf::from(tmp_name);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(&tmp)
        .map_err(|e| WriteReportError::CreateTemp { code: win::os_code(&e) })?;
    let written = f.write_all(data).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let cleanup = cleanup_failure(&tmp, std::fs::remove_file(&tmp));
        return Err(WriteReportError::Write { code: win::os_code(&e), cleanup });
    }
    if let Err(code) = win::move_no_replace(&tmp, &full) {
        let cleanup = cleanup_failure(&tmp, std::fs::remove_file(&tmp));
        return Err(if code == ERROR_ALREADY_EXISTS || code == ERROR_FILE_EXISTS {
            WriteReportError::TargetExists { code, cleanup }
        } else {
            WriteReportError::Rename { code, cleanup }
        });
    }
    Ok(())
}

fn cleanup_failure(tmp: &Path, removed: io::Result<()>) -> Option<CleanupFailure> {
    removed.err().map(|e| CleanupFailure { tmp: tmp.to_path_buf(), code: win::os_code(&e) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_is_uuid_v4() {
        let id = new_run_id().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(id, new_run_id().unwrap());
    }

    #[test]
    fn run_id_prefix_never_splits_a_character() {
        assert_eq!(run_id_prefix("12345678-x"), "12345678");
        assert_eq!(run_id_prefix("abc"), "abc");
        assert_eq!(run_id_prefix(""), "");
        // 第 8 個位元組落在多位元組字元中間：用整個字串，不 panic
        assert_eq!(run_id_prefix("1234567\u{e9}9"), "1234567\u{e9}9");
    }

    #[test]
    fn hash_reads_whole_file_after_head_and_honours_cancel() {
        let dir = std::env::temp_dir().join(format!("iat-core-test-{}", new_run_id().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        let p = dir.join("abc.bin");
        std::fs::write(&p, b"abc").unwrap();
        let f = File::open(&p).unwrap();
        // Input::new 先讀了檔頭；雜湊仍要從位移 0 開始。
        let input = Input::new(&f).unwrap();
        let cancel = CancelToken::new();
        match sha256_file(&input, &cancel, &crate::NoProgress) {
            HashOutcome::Done(h) => {
                assert_eq!(h, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
            }
            other => panic!("expected Done, got {other:?}"),
        }
        cancel.cancel();
        assert!(matches!(sha256_file(&input, &cancel, &crate::NoProgress), HashOutcome::Cancelled));
        drop(f);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleanup_failure_is_appended() {
        let tmp = Path::new("r.json.iat-tmp-1");
        assert_eq!(cleanup_failure(tmp, Ok(())), None);
        let cleanup = cleanup_failure(tmp, Err(io::Error::from_raw_os_error(5)));
        let m = WriteReportError::Write { code: 112, cleanup }.to_string();
        assert!(m.starts_with("write failed: 112: "), "{m}");
        assert!(m.ends_with("; temporary file not removed: r.json.iat-tmp-1 (5: Access is denied.)"), "{m}");
    }

    #[test]
    fn write_new_file_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("iat-core-test-{}", new_run_id().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        let target = dir.join("報告 一.json");
        write_new_file(&target, b"first", "12345678-x").unwrap();
        let e = write_new_file(&target, b"second", "abcdefgh-x").unwrap_err();
        assert!(matches!(e, WriteReportError::TargetExists { cleanup: None, .. }), "{e:?}");
        assert!(e.to_string().starts_with("target appeared meanwhile, not overwritten: "), "{e}");
        assert_eq!(std::fs::read(&target).unwrap(), b"first");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
