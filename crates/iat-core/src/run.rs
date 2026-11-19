//! 一次分析的外層流程：唯讀開檔、雜湊、暫存目錄、呼叫引擎、組成最終報告。
//! 對照 C++ 版 `src/core/run.cpp` 與 `src/core/report_io.cpp`。
//!
//! 與 C++ 版的主要差別：引擎在同一程序內執行，沒有 iat-worker.exe 與 Job Object
//! （見 docs/decisions.md D-R02）。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::engine::{AnalysisInput, analyze};
use crate::exit::decide_exit;
use crate::limits::Limits;
use crate::paths::{file_name_of, full_path, is_directory, parent_dir_of, same_path, trim_dir};
use crate::report::{Count, FileReport, Report, TestSettings, code};
use crate::sniff::ext_format;
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

/// 被取消或讀取失敗時回傳 None（報告的 sha256 留空）。
fn sha256_file(mut f: &File, size: u64, cancel: &CancelToken, progress: &dyn Progress) -> Option<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        progress.report(Stage::Hash, done, size);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// UUID 第 4 版，8-4-4-4-12 小寫十六進位。
fn new_run_id() -> String {
    let mut b = win::random_16();
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn set_not_checked(f: &mut FileReport, reason: &str) {
    f.decode_status = "not_checked".into();
    f.decode_status_reason = reason.into();
    if f.container_status.is_empty() {
        f.container_status = "unverified".into();
    }
}

struct TempInfo {
    dir: Option<PathBuf>,
    user_specified: bool,
    free_bytes: u64,
}

/// 暫存目錄是否可用與剩餘空間。階段 0 沒有解碼器需要暫存，所以不建立暫存檔，
/// 也不清理舊的暫存檔（見 docs/decisions.md D-R04）。
fn prepare_temp(req: &RunRequest) -> Result<TempInfo, (TempInfo, String)> {
    let user_specified = req.temp_dir.is_some();
    let dir = match &req.temp_dir {
        Some(d) => trim_dir(full_path(d)),
        None => trim_dir(std::env::temp_dir()),
    };
    let mut t = TempInfo { dir: Some(dir.clone()), user_specified, free_bytes: 0 };
    if dir.as_os_str().is_empty() || !is_directory(&dir) {
        t.dir = None;
        return Err((t, "temporary directory is not available".into()));
    }
    match win::disk_free_bytes(&dir) {
        Ok(n) => t.free_bytes = n,
        Err(e) => {
            return Err((t, format!("cannot query free space of the temporary directory: {}", win::error_text(e))));
        }
    }
    Ok(t)
}

/// 執行一次分析。輸入、環境的問題（包括取消）一律寫進回傳的報告，
/// 報告的 exit 欄位已經判定好；這裡不寫報告檔。
pub fn run_analysis(req: &RunRequest, cancel: &CancelToken, progress: &dyn Progress) -> Report {
    let t_all = Instant::now();
    let compare = req.mode == Mode::Compare;

    let mut r = Report { run_id: new_run_id(), mode: req.mode.as_str().into(), ..Report::default() };
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
    let mut open_failed = false;
    let mut describe = |r: &mut Report, which: &str, path: &Path, h: &Result<File, u32>| {
        let mut f = FileReport { name: file_name_of(path), ..FileReport::default() };
        if req.full_paths {
            f.path = full_path(path).to_string_lossy().into_owned();
        }
        match h {
            Err(e) => {
                open_failed = true;
                set_not_checked(&mut f, "input_open_failed");
                r.add_issue(
                    code::INPUT_OPEN_FAILED,
                    "error",
                    which,
                    format!("cannot open input: {}", win::error_text(*e)),
                );
            }
            Ok(file) => {
                let size = file.metadata().map_or(0, |m| m.len());
                f.size_bytes = Count(size);
                f.sha256 = sha256_file(file, size, cancel, progress).unwrap_or_default();
            }
        }
        f
    };
    r.original = describe(&mut r, "original", &req.original, &fo);
    if let (Some(p), Some(h)) = (req.output.as_deref(), fp.as_ref()) {
        let out = describe(&mut r, "output", p, h);
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

    // ---- 暫存目錄（C++ 版只在要啟動 worker 時才準備，失敗時位置記為 system_temp）
    let (temp, temp_err) = if open_failed || cancelled_early {
        (TempInfo { dir: None, user_specified: false, free_bytes: 0 }, None)
    } else {
        match prepare_temp(req) {
            Ok(t) => (t, None),
            Err((t, e)) => (t, Some(e)),
        }
    };
    r.settings.temp.location = if temp.user_specified { "user_specified" } else { "system_temp" }.into();
    r.settings.temp.free_bytes_at_start = Count(temp.free_bytes);
    r.settings.temp.delete_on_close = true;
    if req.full_paths {
        r.settings.temp.dir = temp.dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    }
    if let Some(e) = &temp_err {
        set_not_checked(&mut r.original, "environment_error");
        if let Some(o) = r.output.as_mut() {
            set_not_checked(o, "environment_error");
        }
        r.add_issue(code::ENVIRONMENT_ERROR, "error", "run", e.clone());
    }

    if !open_failed && !cancelled_early && temp_err.is_none() {
        let (Ok(fo), Some(Ok(_)) | None) = (&fo, &fp) else { unreachable!("open failures are handled above") };
        progress.report(Stage::Analyze, 0, 1);
        let input = AnalysisInput {
            mode: req.mode,
            original: fo,
            output: fp.as_ref().and_then(|h| h.as_ref().ok()),
            limits: req.limits,
        };
        let t_engine = Instant::now();
        let wr = analyze(&input);
        r.metrics.time_ms.insert("engine_wall".into(), ms_since(t_engine));
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

/// 報告目標必須是既有目錄下的新檔，且不能和任何輸入或另一份報告相同。
/// 錯誤訊息與 C++ 版 `validate_report_targets` 相同。
pub fn validate_report_targets(
    req: &RunRequest,
    report_md: Option<&Path>,
    report_json: Option<&Path>,
) -> Result<(), String> {
    let reports: Vec<&Path> = [report_md, report_json].into_iter().flatten().collect();
    if reports.len() == 2 && same_path(reports[0], reports[1]) {
        return Err("report paths refer to the same file".into());
    }
    for rp in reports {
        let shown = rp.to_string_lossy();
        if same_path(rp, &req.original) || req.output.as_deref().is_some_and(|o| same_path(rp, o)) {
            return Err(format!("report path is an input file: {shown}"));
        }
        if win::path_exists(rp) {
            return Err(format!("report already exists (never overwritten): {shown}"));
        }
        let parent = parent_dir_of(rp);
        if !is_directory(&parent) {
            return Err(format!("report directory does not exist: {}", parent.to_string_lossy()));
        }
    }
    Ok(())
}

/// 先寫入同目錄的暫存檔並 flush，再以不取代既有檔的方式改名：既有目標絕不覆寫。
pub fn write_new_file(target: &Path, data: &[u8], run_id: &str) -> Result<(), String> {
    let full = full_path(target);
    let mut tmp_name = full.clone().into_os_string();
    tmp_name.push(format!(".iat-tmp-{}", &run_id[..run_id.len().min(8)]));
    let tmp = PathBuf::from(tmp_name);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(&tmp)
        .map_err(|e| format!("cannot create temporary file: {}", win::error_text(win::os_code(&e))))?;
    let written = f.write_all(data).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write failed: {}", win::error_text(win::os_code(&e))));
    }
    if let Err(e) = win::move_no_replace(&tmp, &full) {
        let _ = std::fs::remove_file(&tmp);
        let what = if e == ERROR_ALREADY_EXISTS || e == ERROR_FILE_EXISTS {
            "target appeared meanwhile, not overwritten: "
        } else {
            "rename failed: "
        };
        return Err(format!("{what}{}", win::error_text(e)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_is_uuid_v4() {
        let id = new_run_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(id, new_run_id());
    }

    #[test]
    fn write_new_file_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("iat-core-test-{}", new_run_id()));
        std::fs::create_dir(&dir).unwrap();
        let target = dir.join("報告 一.json");
        write_new_file(&target, b"first", "12345678-x").unwrap();
        let e = write_new_file(&target, b"second", "abcdefgh-x").unwrap_err();
        assert!(e.starts_with("target appeared meanwhile, not overwritten: "), "{e}");
        assert_eq!(std::fs::read(&target).unwrap(), b"first");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
