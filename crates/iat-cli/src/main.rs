//! iat.exe：iat-core 的 CLI 前端。解析參數、執行一次分析、寫出／印出報告並決定退出碼。
//! 參數、訊息與退出碼對照 C++ 版 `src/iat_main.cpp`。

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use iat_core::markdown::render_markdown;
use iat_core::paths::{full_path, is_directory};
use iat_core::{CancelToken, Limits, Mode, NoProgress, RunRequest, report_json_text, run_analysis};

const USAGE: &str = "usage:
  iat compare <original> <output> [--report-md <path>] [--report-json <path>] [--stdout md|json]
              [--full-paths] [--temp-dir <dir>] [--limits name=value ...]
  iat inspect <file> [--report-md <path>] [--report-json <path>] [--stdout md|json] [--full-paths]
              [--temp-dir <dir>] [--limits name=value ...]
  iat --version
temp: decoded samples may be written to delete-on-close files in --temp-dir (default: system temp)
limits: job_commit_mib (1024), buffer_mib (64), metadata_item_mib (8), metadata_file_mib (32),
        temp_gib (8), deadline_s (600)
exit codes: 0 match/ok, 1 mismatch, 2 indeterminate/unsupported, 3 decode/structure failure,
            4 resource limit/deadline/cancel (not checked), 5 argument/I-O/worker/environment error";

#[derive(Clone, Copy, PartialEq, Eq)]
enum StdoutFormat {
    Md,
    Json,
}

struct Args {
    req: RunRequest,
    report_md: Option<PathBuf>,
    report_json: Option<PathBuf>,
    stdout: Option<StdoutFormat>,
}

fn lossy(s: &OsString) -> String {
    s.to_string_lossy().into_owned()
}

fn parse_args(argv: &[OsString]) -> Result<Args, String> {
    let Some(cmd) = argv.get(1) else { return Err("missing command".into()) };
    let mode = match lossy(cmd).as_str() {
        "compare" => Mode::Compare,
        "inspect" => Mode::Inspect,
        other => return Err(format!("unknown command: {other}")),
    };
    let mut limits = Limits::default();
    let (mut report_md, mut report_json, mut stdout, mut temp_dir) = (None, None, None, None);
    let mut full_paths = false;
    let mut pos: Vec<PathBuf> = Vec::new();
    let mut i = 2;
    while i < argv.len() {
        let s = lossy(&argv[i]);
        let next = |i: &mut usize| -> Option<&OsString> {
            if *i + 1 >= argv.len() {
                return None;
            }
            *i += 1;
            Some(&argv[*i])
        };
        match s.as_str() {
            "--report-md" => report_md = Some(PathBuf::from(next(&mut i).ok_or("--report-md needs a path")?)),
            "--report-json" => report_json = Some(PathBuf::from(next(&mut i).ok_or("--report-json needs a path")?)),
            "--stdout" => {
                stdout = Some(match next(&mut i).map(lossy).as_deref() {
                    Some("md") => StdoutFormat::Md,
                    Some("json") => StdoutFormat::Json,
                    _ => return Err("--stdout needs md or json".into()),
                });
            }
            "--full-paths" => full_paths = true,
            "--temp-dir" => temp_dir = Some(PathBuf::from(next(&mut i).ok_or("--temp-dir needs a directory")?)),
            "--test-codec-estimate-pct" => {
                // 僅供測試（不在說明文字裡）：壓低整圖型解碼器的預估值，報告一定會揭露。
                const BAD: &str = "--test-codec-estimate-pct needs 1..100";
                let t = next(&mut i).map(lossy).unwrap_or_default();
                if t.is_empty() || t.len() > 3 || !t.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(BAD.into());
                }
                let pct: u64 = t.parse().map_err(|_| BAD)?;
                if !(1..=100).contains(&pct) {
                    return Err(BAD.into());
                }
                limits.test_codec_estimate_pct = pct;
            }
            "--limits" => {
                let mut any = false;
                while i + 1 < argv.len() {
                    let a = lossy(&argv[i + 1]);
                    if !a.contains('=') || a.starts_with("--") {
                        break;
                    }
                    i += 1;
                    limits.set(&a)?;
                    any = true;
                }
                if !any {
                    return Err("--limits needs name=value".into());
                }
            }
            _ if s.starts_with("--") => return Err(format!("unknown option: {s}")),
            _ => pos.push(PathBuf::from(&argv[i])),
        }
        i += 1;
    }
    let want = if mode == Mode::Compare { 2 } else { 1 };
    if pos.len() != want {
        return Err(format!("{} needs {want} input path(s)", mode.as_str()));
    }
    let output = (want == 2).then(|| pos[1].clone());
    if report_md.is_none() && report_json.is_none() && stdout.is_none() {
        return Err("no report output requested (use --report-md, --report-json or --stdout)".into());
    }
    if let Some(d) = &temp_dir
        && !is_directory(d)
    {
        return Err(format!("--temp-dir is not an existing directory: {}", d.to_string_lossy()));
    }
    Ok(Args {
        req: RunRequest { mode, original: pos[0].clone(), output, temp_dir, full_paths, limits },
        report_md,
        report_json,
        stdout,
    })
}

fn err(s: &str) {
    eprintln!("{s}");
}

fn write_stdout(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

fn run() -> i32 {
    let argv: Vec<OsString> = std::env::args_os().collect();
    if argv.len() == 2 && argv[1] == "--version" {
        write_stdout(&format!("iat {}\n", iat_core::VERSION));
        return 0;
    }
    let a = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            err(&format!("error: {e}"));
            err(USAGE);
            return 5;
        }
    };
    if let Err(e) = iat_core::validate_report_targets(&a.req, a.report_md.as_deref(), a.report_json.as_deref()) {
        err(&format!("error: {e}"));
        return 5;
    }
    let cancel = CancelToken::new();
    cancel.install_console_handler();

    let r = run_analysis(&a.req, &cancel, &NoProgress);

    let json_text = report_json_text(&r);
    let md_text = render_markdown(&r);
    let (mut written, mut failed) = (Vec::new(), Vec::new());
    let mut emit = |path: &PathBuf, data: &str| match iat_core::write_new_file(path, data.as_bytes(), &r.run_id) {
        Ok(()) => written.push(full_path(path).to_string_lossy().into_owned()),
        Err(e) => failed.push(format!("{} ({e})", path.to_string_lossy())),
    };
    if let Some(p) = &a.report_json {
        emit(p, &json_text);
    }
    if let Some(p) = &a.report_md {
        emit(p, &md_text);
    }
    match a.stdout {
        Some(StdoutFormat::Json) => write_stdout(&json_text),
        Some(StdoutFormat::Md) => write_stdout(&md_text),
        None => {}
    }

    err(&format!("iat: strict={} exit={} ({}) run_id={}", r.strict.result, r.exit.code, r.exit.reason, r.run_id));
    for p in &written {
        err(&format!("iat: report written: {p}"));
    }
    // 任一份報告寫入失敗時程序退出碼為 5，優先於報告內的 exit.code。
    if !failed.is_empty() {
        for p in &failed {
            err(&format!("iat: report NOT written: {p}"));
        }
        return 5;
    }
    r.exit.code
}

fn main() {
    std::process::exit(run());
}
