//! iat-worker.exe：分析（與 AVIF 的檢視解碼）在獨立程序裡執行，放進套了 `job_commit_mib` 的
//! Job Object（docs/decisions.md D-R02 第三次修訂；對照 C++ 版 `src/core/run.cpp` 的 `run_worker`
//! 與 `src/worker_main.cpp`）。
//!
//! 協定是 Rust 版自己的（IMB-C 只檢查 iat-worker.exe 是否存在）：
//! - 命令列：`--mode compare|inspect|view`、`--original <handle>`、`--output <handle>`、
//!   `--limit name=value`、`--test-codec-estimate-pct n`、`--temp-a`／`--temp-b <handle>`、
//!   `--temp-free <bytes>`、`--channel <handle>`、`--av1-failed original|output`、`--alloc-failed original|output`、`--view-max-pixels n`。
//!   輸入與暫存檔都以繼承的 handle 傳入，worker 從不開啟路徑。
//! - stdout：分析模式為一份 JSON 報告，或 `{"worker_error": …, "message": …}`；檢視模式見 [`view`]。
//! - 通道（另一條管線）：17 bytes 一筆，[種類][u64 LE][u64 LE]。種類 1 為進度（已解出的列數，總列數）；
//!   種類 2 為 AV1 解碼中 panic 的那一側（rav1d 的 C 介面不能 unwind，panic 會直接結束 worker，
//!   父程序據此把那一側判為解碼錯誤後重跑，見 [`run_worker`]）；種類 3 為解碼器呼叫中配置失敗的那一側
//!   （Job 的 commit 上限，見 `win::ALLOC_SIDE`），父程序重跑一次、把那一側照 C++ 版判為 `resource_limit`。

use std::fs::File;
use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::engine::{AllocStage, AnalysisInput, Outcome, RowLayout, ViewSink, ViewStop, analyze, decode_view_samples};
use crate::input::Input;
use crate::limits::Limits;
use crate::{CancelToken, Mode, Progress, Stage, u64_to_usize, win};

/// stdout 的上限（分析模式，同 C++ 版 kMaxWorkerStdout）。
const MAX_STDOUT: usize = 32 << 20;
/// stderr 的上限（同 C++ 版 kMaxWorkerStderr）。
const MAX_STDERR: usize = 64 << 10;
/// 通道一筆的長度。
const RECORD: usize = 17;
const REC_PROGRESS: u8 = 1;
const REC_AV1_PANIC: u8 = 2;
/// 取消與超過期限時結束 Job 用的代碼（同 C++ 版）。
const EXIT_CANCELLED: u32 = 0xC000_013A;
const EXIT_DEADLINE: u32 = 0x5A5A;

/// worker 的工作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerMode {
    Compare,
    Inspect,
    /// GUI 檢視用的整張解碼（只用 original）
    View,
}

impl WorkerMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::Inspect => "inspect",
            Self::View => "view",
        }
    }
}

/// 父程序交給 worker 的設定（handle 以外的部分）。
#[derive(Clone, Debug)]
pub(crate) struct WorkerSettings {
    pub mode: WorkerMode,
    pub limits: Limits,
    pub temp_free: u64,
    /// 已知在 rav1d 裡 panic 的一側（[original, output]）：worker 不再解碼，直接判為解碼錯誤
    pub av1_failed: [bool; 2],
    /// 先前在解碼器呼叫中配置失敗的一側（[original, output]）：worker 不再解碼，照 C++ 版判為 `resource_limit`
    pub alloc_failed: [Option<AllocStage>; 2],
    pub view_max_pixels: u64,
}

/// worker 的結束狀況（同 C++ 版 `WorkerRun::outcome`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerOutcome {
    Exited,
    Deadline,
    Cancelled,
    LaunchFailed(String),
}

pub(crate) struct WorkerRun<T> {
    pub outcome: WorkerOutcome,
    pub exit_code: u32,
    /// stdout 處理函式的結果（啟動失敗時為 None）
    pub stdout: Option<T>,
    pub stderr: Vec<u8>,
    pub memory_limit_signalled: bool,
    pub peak_commit: u64,
    pub io_read: u64,
    pub wall_ms: f64,
    /// AV1 解碼中 panic 的那一側（0 original、1 output）
    pub av1_panic: Option<usize>,
    /// 解碼器呼叫中配置失敗的那一側（0 original、1 output）與階段
    pub alloc_failed: Option<(usize, AllocStage)>,
}

/// iat-worker.exe 的位置：和目前的執行檔同一個資料夾（C++ 版相同）。
pub(crate) fn worker_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    Some(me.with_file_name("iat-worker.exe"))
}

fn handle_arg(f: &File) -> u64 {
    use std::os::windows::io::AsRawHandle;
    f.as_raw_handle() as usize as u64
}

/// 交給 worker 繼承的檔案：唯讀輸入與兩個暫存檔。
#[derive(Clone, Copy)]
pub(crate) struct WorkerFiles<'a> {
    pub original: &'a File,
    pub output: Option<&'a File>,
    pub temp: [&'a File; 2],
}

/// 啟動 worker、等它結束（取消與期限由這裡結束 Job），收集輸出。
/// `read_stdout` 在另一個執行緒讀 worker 的 stdout。
pub(crate) fn run_worker<T: Send>(
    settings: &WorkerSettings,
    files: WorkerFiles<'_>,
    cancel: &CancelToken,
    deadline: Option<Duration>,
    progress: &dyn Progress,
    read_stdout: impl FnOnce(&mut File) -> T + Send,
) -> WorkerRun<T> {
    let WorkerFiles { original, output, temp } = files;
    let mut run = WorkerRun {
        outcome: WorkerOutcome::Exited,
        exit_code: 0,
        stdout: None,
        stderr: Vec::new(),
        memory_limit_signalled: false,
        peak_commit: 0,
        io_read: 0,
        wall_ms: 0.0,
        av1_panic: None,
        alloc_failed: None,
    };
    let fail = |mut run: WorkerRun<T>, msg: String| {
        run.outcome = WorkerOutcome::LaunchFailed(msg);
        run
    };
    let Some(exe) = worker_exe().filter(|p| win::path_exists(p)) else {
        return fail(run, "iat-worker.exe not found next to iat.exe".into());
    };
    // 先建 Job：沒有上限絕不執行解碼器
    let job = match win::WorkerJob::new(settings.limits.job_commit_bytes()) {
        Ok(j) => j,
        Err(e) => return fail(run, format!("Job Object could not be configured: {}", win::error_text(e.code))),
    };
    let l = &settings.limits;
    let mut args = format!("--mode {} --original {}", settings.mode.as_str(), handle_arg(original));
    if let Some(o) = output {
        args += &format!(" --output {}", handle_arg(o));
    }
    for (k, v) in [
        ("job_commit_mib", l.job_commit_mib),
        ("buffer_mib", l.buffer_mib),
        ("metadata_item_mib", l.metadata_item_mib),
        ("metadata_file_mib", l.metadata_file_mib),
        ("temp_gib", l.temp_gib),
    ] {
        args += &format!(" --limit {k}={v}");
    }
    if l.test_codec_estimate_pct != 100 {
        args += &format!(" --test-codec-estimate-pct {}", l.test_codec_estimate_pct);
    }
    args += &format!(
        " --temp-a {} --temp-b {} --temp-free {}",
        handle_arg(temp[0]),
        handle_arg(temp[1]),
        settings.temp_free
    );
    for (i, side) in ["original", "output"].iter().enumerate() {
        if settings.av1_failed[i] {
            args += &format!(" --av1-failed {side}");
        }
        if let Some(stage) = settings.alloc_failed[i] {
            args += &format!(" --alloc-failed {side}:{}", stage.code());
        }
    }
    if settings.mode == WorkerMode::View {
        args += &format!(" --view-max-pixels {}", settings.view_max_pixels);
    }
    let mut inherit = vec![original, temp[0], temp[1]];
    inherit.extend(output);
    let (mut channel, chan_w) = match win::pipe() {
        Ok(p) => p,
        Err(e) => return fail(run, format!("{}: {}", e.call, win::error_text(e.code))),
    };
    {
        use std::os::windows::io::AsRawHandle;
        args += &format!(" --channel {}", chan_w.as_raw_handle() as usize);
    }
    let t0 = Instant::now();
    let spawned = win::spawn_worker(job, &exe, &args, &inherit, chan_w);
    let mut w = match spawned {
        Ok(w) => w,
        Err(e) => return fail(run, format!("{}: {}", e.call, win::error_text(e.code))),
    };
    let deadline_at = deadline.and_then(|d| t0.checked_add(d));
    let (stdout, stderr, channel) = (&mut w.stdout, &mut w.stderr, &mut channel);
    let (out, err, failed) = std::thread::scope(|sc| {
        let t_out = sc.spawn(move || read_stdout(stdout));
        let t_err = sc.spawn(move || read_capped(stderr, MAX_STDERR).0);
        let t_chan = sc.spawn(move || read_channel(channel, progress));
        loop {
            let exited = w.child.wait(50);
            run.memory_limit_signalled |= w.job.drain_notifications(0);
            if exited {
                break;
            }
            if run.outcome == WorkerOutcome::Exited {
                if cancel.is_cancelled() {
                    run.outcome = WorkerOutcome::Cancelled;
                    w.job.terminate(EXIT_CANCELLED);
                } else if deadline_at.is_some_and(|d| Instant::now() > d) {
                    run.outcome = WorkerOutcome::Deadline;
                    w.job.terminate(EXIT_DEADLINE);
                }
            }
        }
        let out = t_out.join().ok();
        let err = t_err.join().unwrap_or_default();
        let failed = t_chan.join().unwrap_or_default();
        (out, err, failed)
    });
    run.wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
    run.memory_limit_signalled |= w.job.drain_notifications(20);
    run.exit_code = w.child.exit_code();
    (run.peak_commit, run.io_read) = w.job.accounting();
    run.stdout = out;
    run.stderr = err;
    run.av1_panic = failed.av1_panic;
    run.alloc_failed = failed.alloc_failed;
    run
}

/// 讀到 EOF；超過 `cap` 的部分丟掉並標記。
pub(crate) fn read_capped(f: &mut File, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut overflow = false;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match f.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() + n <= cap {
                    out.extend_from_slice(&buf[..n]);
                } else {
                    overflow = true;
                }
            }
        }
    }
    (out, overflow)
}

/// 分析模式的 stdout（上限 32 MiB）。
pub(crate) fn read_report_stdout(f: &mut File) -> (Vec<u8>, bool) {
    read_capped(f, MAX_STDOUT)
}

/// 通道回報的出事的一側（0 original、1 output）。
#[derive(Debug, Default)]
struct SideFailures {
    av1_panic: Option<usize>,
    alloc_failed: Option<(usize, AllocStage)>,
}

/// 讀通道直到 worker 關閉它：轉發進度，回傳出事的一側。
fn read_channel(f: &mut File, progress: &dyn Progress) -> SideFailures {
    let mut failed = SideFailures::default();
    let mut rec = [0u8; RECORD];
    while f.read_exact(&mut rec).is_ok() {
        let a = u64::from_le_bytes(rec[1..9].try_into().expect("8 bytes"));
        let b = u64::from_le_bytes(rec[9..17].try_into().expect("8 bytes"));
        match rec[0] {
            REC_PROGRESS => progress.report(Stage::Analyze, a, b),
            REC_AV1_PANIC if a < 2 => failed.av1_panic = Some(u64_to_usize(a)),
            win::REC_ALLOC_FAILED if a < 2 => {
                failed.alloc_failed = AllocStage::from_code(b).map(|stage| (u64_to_usize(a), stage));
            }
            _ => {}
        }
    }
    failed
}

// ---------------------------------------------------------------- worker 端

/// 正在 rav1d 裡解碼的一側（0：沒有；1：original；2：output）。panic hook 依此寫通道。
static AV1_DECODING: AtomicU8 = AtomicU8::new(0);
static CHANNEL: Mutex<Option<&'static File>> = Mutex::new(None);

/// AV1 解碼期間標記這一側；離開時（含 unwind）清除。
pub(crate) struct Av1Guard;

impl Av1Guard {
    pub(crate) fn new(scope: &str) -> Self {
        AV1_DECODING.store(if scope == "output" { 2 } else { 1 }, Ordering::SeqCst);
        Self
    }
}

impl Drop for Av1Guard {
    fn drop(&mut self) {
        AV1_DECODING.store(0, Ordering::SeqCst);
    }
}

fn write_record(kind: u8, a: u64, b: u64) {
    let mut rec = [0u8; RECORD];
    rec[0] = kind;
    rec[1..9].copy_from_slice(&a.to_le_bytes());
    rec[9..17].copy_from_slice(&b.to_le_bytes());
    // panic hook 也會呼叫：鎖不住時（理論上不會）就放棄這筆，不能卡住
    if let Ok(g) = CHANNEL.try_lock()
        && let Some(mut f) = *g
    {
        // 父程序提早關閉通道時進度就不再送，不影響分析
        let _ignored = f.write_all(&rec);
    }
}

/// 透過通道回報進度，最多每 50 ms 一次（最後一列一定送）。
struct ChannelProgress {
    last: Mutex<Option<Instant>>,
}

impl Progress for ChannelProgress {
    fn report(&self, stage: Stage, done: u64, total: u64) {
        if stage != Stage::Analyze {
            return;
        }
        let mut last = self.last.lock().expect("progress lock poisoned");
        let due = last.is_none_or(|t| t.elapsed() >= Duration::from_millis(50)) || done >= total;
        if due {
            *last = Some(Instant::now());
            write_record(REC_PROGRESS, done, total);
        }
    }
}

/// panic 不印到 stderr（會變成多餘的 worker_stderr）。解碼器的 panic 由引擎用 catch_unwind 判為
/// 解碼錯誤；rav1d 的 C 介面不能 unwind，那時改由通道告訴父程序是哪一側。
fn install_panic_hook() {
    panic::set_hook(Box::new(|_| {
        let side = AV1_DECODING.load(Ordering::SeqCst);
        if side != 0 {
            write_record(REC_AV1_PANIC, u64::from(side - 1), 0);
        }
    }));
}

struct Args {
    mode: Option<WorkerMode>,
    original: Option<u64>,
    output: Option<u64>,
    limits: Limits,
    temp: [Option<u64>; 2],
    temp_free: u64,
    channel: Option<u64>,
    av1_failed: [bool; 2],
    alloc_failed: [Option<AllocStage>; 2],
    view_max_pixels: u64,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        mode: None,
        original: None,
        output: None,
        limits: Limits::default(),
        temp: [None, None],
        temp_free: 0,
        channel: None,
        av1_failed: [false; 2],
        alloc_failed: [None; 2],
        view_max_pixels: u64::MAX,
    };
    let num = |v: &str| v.parse::<u64>().map_err(|_| format!("bad number: {v}"));
    let mut it = argv.iter();
    while let Some(k) = it.next() {
        let v = it.next().ok_or_else(|| format!("missing value for {k}"))?;
        match k.as_str() {
            "--mode" => {
                a.mode = Some(match v.as_str() {
                    "compare" => WorkerMode::Compare,
                    "inspect" => WorkerMode::Inspect,
                    "view" => WorkerMode::View,
                    _ => return Err(format!("bad mode: {v}")),
                });
            }
            "--original" => a.original = Some(num(v)?),
            "--output" => a.output = Some(num(v)?),
            "--temp-a" => a.temp[0] = Some(num(v)?),
            "--temp-b" => a.temp[1] = Some(num(v)?),
            "--temp-free" => a.temp_free = num(v)?,
            "--channel" => a.channel = Some(num(v)?),
            "--view-max-pixels" => a.view_max_pixels = num(v)?,
            "--limit" => a.limits.set(v).map_err(|e| e.to_string())?,
            "--test-codec-estimate-pct" => {
                let p = num(v)?;
                if !(1..=100).contains(&p) {
                    return Err("test-codec-estimate-pct out of range".into());
                }
                a.limits.test_codec_estimate_pct = p;
            }
            "--av1-failed" => a.av1_failed[side_index(v)?] = true,
            "--alloc-failed" => {
                let (side, stage) = v.split_once(':').ok_or_else(|| format!("bad side and stage: {v}"))?;
                let stage = AllocStage::from_code(num(stage)?).ok_or_else(|| format!("bad stage: {v}"))?;
                a.alloc_failed[side_index(side)?] = Some(stage);
            }
            _ => return Err(format!("unknown argument: {k}")),
        }
    }
    Ok(a)
}

fn side_index(v: &str) -> Result<usize, String> {
    match v {
        "original" => Ok(0),
        "output" => Ok(1),
        _ => Err(format!("bad side: {v}")),
    }
}

fn worker_error(kind: &str, message: &str) -> Vec<u8> {
    serde_json::json!({ "worker_error": kind, "message": message }).to_string().into_bytes()
}

/// 取得命令列上的 handle（[`win::take_inherited_files`] 檢查重複與有效性，整個程序只取一次）。
fn take_files(a: &Args) -> Result<(File, Option<File>, [File; 2], File), String> {
    let need = |v: Option<u64>, what: &str| v.ok_or_else(|| format!("missing {what} handle"));
    let compare = a.mode == Some(WorkerMode::Compare);
    let mut values = vec![
        need(a.channel, "channel")?,
        need(a.original, "original")?,
        need(a.temp[0], "temp-a")?,
        need(a.temp[1], "temp-b")?,
    ];
    if compare {
        values.push(need(a.output, "output")?);
    }
    let mut files = win::take_inherited_files(&values)?.into_iter();
    let mut next = || files.next().ok_or_else(|| "inherited handle count mismatch".to_string());
    let (channel, original, temp_a, temp_b) = (next()?, next()?, next()?, next()?);
    let output = if compare { Some(next()?) } else { None };
    Ok((original, output, [temp_a, temp_b], channel))
}

/// iat-worker.exe 的進入點：回傳程序的結束代碼。
pub fn worker_main(argv: &[String]) -> i32 {
    win::worker_process_setup();
    install_panic_hook();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let a = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ignored = out.write_all(&worker_error("bad_argument", &e));
            return 2;
        }
    };
    let (original, output, temp, channel) = match take_files(&a) {
        Ok(f) => f,
        Err(e) => {
            let _ignored = out.write_all(&worker_error("bad_argument", &e));
            return 2;
        }
    };
    // 通道存活到程序結束：配置器存下的 handle 值不會被其他物件重新使用
    let channel: &'static File = Box::leak(Box::new(channel));
    win::set_alloc_failure_channel(channel);
    *CHANNEL.lock().expect("channel lock poisoned") = Some(channel);
    let result = panic::catch_unwind(AssertUnwindSafe(|| run_mode(&a, &original, output.as_ref(), &temp, &mut out)));
    let code = match result {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            let _ignored = out.write_all(&worker_error("exception", &e));
            4
        }
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            let _ignored = out.write_all(&worker_error("exception", &format!("panic: {msg}")));
            4
        }
    };
    let _ignored = out.flush();
    code
}

fn run_mode(
    a: &Args,
    original: &File,
    output: Option<&File>,
    temp: &[File; 2],
    out: &mut impl Write,
) -> Result<i32, String> {
    let read_err = |e: io::Error| format!("cannot read input: {}", win::error_text(win::os_code(&e)));
    let in_o = Input::new(original).map_err(read_err)?;
    let in_p = output.map(Input::new).transpose().map_err(read_err)?;
    let mode = a.mode.ok_or("missing mode")?;
    if mode == WorkerMode::View {
        return view(&in_o, &temp[0], a, out);
    }
    let cancel = CancelToken::new();
    let progress = ChannelProgress { last: Mutex::new(None) };
    // 期限由父程序結束 Job 來執行；引擎這裡不需要自己的期限。
    let far = Instant::now() + Duration::from_secs(10 * 365 * 24 * 3600);
    let input = AnalysisInput {
        mode: if mode == WorkerMode::Compare { Mode::Compare } else { Mode::Inspect },
        original: &in_o,
        output: in_p.as_ref(),
        limits: a.limits,
        temp: [Some(&temp[0]), Some(&temp[1])],
        temp_free_bytes: a.temp_free,
        cancel: &cancel,
        progress: &progress,
        deadline: far,
        av1_failed: a.av1_failed,
        alloc_failed: a.alloc_failed,
    };
    let report = match analyze(&input) {
        Outcome::Done(r) => r,
        Outcome::Cancelled | Outcome::Deadline => return Err("engine stopped without a parent request".into()),
    };
    let text = serde_json::to_string(&report).map_err(|e| e.to_string())?;
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(0)
}

// ---------------------------------------------------------------- 檢視模式

/// 檢視模式的 stdout：開頭一行 JSON（[`ViewHeader`]），接著每列一個 1 加上該列的 u16 LE 樣本，
/// 最後一個 0 與一行 JSON（[`ViewTrailer`]）。解碼前就停止時只有一行 [`ViewHeader`]（`stop` 有值）。
pub(crate) mod view_protocol {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    #[serde(default)]
    pub struct ViewHeader {
        pub format: String,
        pub model: String,
        pub channels: u32,
        pub bits: u32,
        pub width: u32,
        pub height: u32,
        /// 停止原因：over_view_limit | too_large（`bytes`）；空字串表示接著有列資料
        pub stop: String,
        pub bytes: u64,
    }

    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    #[serde(default)]
    pub struct ViewTrailer {
        pub rows: u32,
        pub error: Option<String>,
    }
}

use view_protocol::{ViewHeader, ViewTrailer};

/// 把列直接寫到 stdout 的接收端。
struct StdoutSink<'a, W: Write> {
    out: &'a mut W,
    format: String,
    started: bool,
    buf: Vec<u8>,
}

impl<W: Write> ViewSink for StdoutSink<'_, W> {
    fn begin(&mut self, layout: &RowLayout, width: u32, height: u32) -> Result<(), ViewStop> {
        let h = ViewHeader {
            format: self.format.clone(),
            model: layout.model.into(),
            channels: layout.channels,
            bits: layout.bits,
            width,
            height,
            ..ViewHeader::default()
        };
        let mut line = serde_json::to_vec(&h).map_err(|_| ViewStop::Cancelled)?;
        line.push(b'\n');
        self.out.write_all(&line).map_err(|_| ViewStop::Cancelled)?;
        self.started = true;
        Ok(())
    }

    fn row(&mut self, canon: &[u16]) -> Result<(), ViewStop> {
        self.buf.clear();
        self.buf.push(1);
        for v in canon {
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        // 父程序關閉管線（取消）時停止
        self.out.write_all(&self.buf).map_err(|_| ViewStop::Cancelled)
    }
}

fn view(input: &Input<'_>, temp: &File, a: &Args, out: &mut impl Write) -> Result<i32, String> {
    let cancel = CancelToken::new();
    let mut sink = StdoutSink {
        out,
        format: crate::sniff::sniff_format(input.head()).to_string(),
        started: false,
        buf: Vec::new(),
    };
    let res = decode_view_samples(input, Some(temp), a.temp_free, &a.limits, a.view_max_pixels, &cancel, &mut sink);
    let started = sink.started;
    let format = std::mem::take(&mut sink.format);
    let out = sink.out;
    let write = |out: &mut dyn Write, v: &[u8]| out.write_all(v).map_err(|e| e.to_string());
    match res {
        Err(ViewStop::Cancelled) => Ok(0),
        Err(stop) => {
            let (kind, bytes, width, height) = match stop {
                ViewStop::TooLarge(b) => ("too_large", b, 0, 0),
                ViewStop::OverViewLimit { width, height } => ("over_view_limit", 0, width, height),
                ViewStop::Cancelled => unreachable!("handled above"),
            };
            let h = ViewHeader { format, stop: kind.into(), bytes, width, height, ..ViewHeader::default() };
            let mut line = serde_json::to_vec(&h).map_err(|e| e.to_string())?;
            line.push(b'\n');
            write(out, &line)?;
            Ok(0)
        }
        Ok(v) => {
            if !started {
                // 解碼前就停下（不支援或開檔失敗）：版面為空的標頭
                let h = ViewHeader { format, ..ViewHeader::default() };
                let mut line = serde_json::to_vec(&h).map_err(|e| e.to_string())?;
                line.push(b'\n');
                write(out, &line)?;
            }
            let t = ViewTrailer { rows: v.rows, error: v.error };
            let mut line = vec![0u8];
            line.extend(serde_json::to_vec(&t).map_err(|e| e.to_string())?);
            line.push(b'\n');
            write(out, &line)?;
            Ok(0)
        }
    }
}

/// 父程序：解析檢視模式的 stdout。
pub(crate) struct ViewOutput {
    pub header: ViewHeader,
    pub samples: Vec<u16>,
    pub trailer: Option<ViewTrailer>,
    /// 解析失敗的原因（協定錯誤）
    pub error: Option<String>,
}

fn read_line(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if r.read(&mut b)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if b[0] == b'\n' {
            return Ok(line);
        }
        if line.len() > (1 << 20) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        line.push(b[0]);
    }
}

/// 樣本模型的聲道數（gray 1、gray_alpha 2、rgb 3、rgba 4）；不認得的模型為 None。
fn model_channels(model: &str) -> Option<u32> {
    match model {
        "gray" => Some(1),
        "gray_alpha" => Some(2),
        "rgb" => Some(3),
        "rgba" => Some(4),
        _ => None,
    }
}

/// worker 送來的檢視標頭（不可信）不能用時的原因：聲道數與模型不符、位元數不在 1..=16、
/// 寬高為 0，或像素數超過 `max_pixels`。通過後 `width × channels` 與總樣本數都不會溢位。
fn view_header_problem(h: &ViewHeader, max_pixels: u64) -> Option<&'static str> {
    if model_channels(&h.model) != Some(h.channels) {
        return Some("channel count does not match the sample model");
    }
    if !(1..=16).contains(&h.bits) {
        return Some("bit depth is outside 1..=16");
    }
    if h.width == 0 || h.height == 0 {
        return Some("width or height is zero");
    }
    if u64::from(h.width) * u64::from(h.height) > max_pixels {
        return Some("pixel count exceeds the viewing limit");
    }
    None
}

/// 剩下的輸出讀掉丟棄：worker 才不會因管線寫滿而卡住。
fn drain(r: &mut impl Read) {
    let _ignored = io::copy(r, &mut io::sink());
}

/// 父程序：解析檢視模式的 stdout。輸出來自 worker，一律視為不可信：標頭先驗證，
/// 樣本緩衝的大小只由驗證過的標頭決定（見 [`view_header_problem`]）。
pub(crate) fn read_view_stdout(f: &mut impl Read, max_pixels: u64) -> ViewOutput {
    let mut r = io::BufReader::with_capacity(1 << 20, f);
    let mut o = ViewOutput { header: ViewHeader::default(), samples: Vec::new(), trailer: None, error: None };
    let header: Result<ViewHeader, String> = read_line(&mut r)
        .map_err(|e| e.to_string())
        .and_then(|l| serde_json::from_slice(&l).map_err(|e| e.to_string()));
    match header {
        Ok(h) => o.header = h,
        Err(e) => {
            o.error = Some(format!("view header: {e}"));
            drain(&mut r);
            return o;
        }
    }
    if !o.header.stop.is_empty() {
        drain(&mut r);
        return o;
    }
    if let Some(why) = view_header_problem(&o.header, max_pixels) {
        o.error = Some(format!("view header is invalid: {why}"));
        drain(&mut r);
        return o;
    }
    let row_len = o.header.width as usize * o.header.channels as usize;
    let total = row_len.saturating_mul(o.header.height as usize);
    if o.samples.try_reserve_exact(total).is_err() {
        o.header.stop = "too_large".into();
        o.header.bytes = (total as u64).saturating_mul(2);
        drain(&mut r);
        return o;
    }
    let mut row = vec![0u8; row_len * 2];
    loop {
        let mut tag = [0u8; 1];
        if r.read_exact(&mut tag).is_err() {
            o.error = Some("view output ended early".into());
            return o;
        }
        if tag[0] == 0 {
            break;
        }
        if tag[0] != 1 || o.samples.len() + row_len > total || r.read_exact(&mut row).is_err() {
            o.error = Some("view output is malformed".into());
            drain(&mut r);
            return o;
        }
        o.samples.extend(row.as_chunks::<2>().0.iter().map(|&b| u16::from_le_bytes(b)));
    }
    match read_line(&mut r)
        .map_err(|e| e.to_string())
        .and_then(|l| serde_json::from_slice(&l).map_err(|e| e.to_string()))
    {
        Ok(t) => o.trailer = Some(t),
        Err(e) => o.error = Some(format!("view trailer: {e}")),
    }
    // 結尾之後不該再有資料；有的話也讀掉，worker 才能結束
    drain(&mut r);
    o
}

/// worker 結束的情況寫成一句英文（GUI 的錯誤訊息用；分析的報告另有 C++ 版的文字）。
pub(crate) fn describe_failure<T>(w: &WorkerRun<T>) -> Option<String> {
    match &w.outcome {
        WorkerOutcome::LaunchFailed(m) => Some(m.clone()),
        WorkerOutcome::Cancelled => None,
        WorkerOutcome::Deadline => Some("worker exceeded the deadline".into()),
        WorkerOutcome::Exited if w.exit_code == win::ALLOC_FAILED_EXIT => Some(format!(
            "worker allocation failed under the Job commit limit{}",
            if w.memory_limit_signalled { " (Job memory-limit notification received)" } else { "" }
        )),
        WorkerOutcome::Exited if w.exit_code != 0 => Some(format!("worker exit 0x{:08X}", w.exit_code)),
        WorkerOutcome::Exited => None,
    }
}

/// 呼叫端用：iat-worker.exe 是否在執行檔旁邊。
pub fn worker_available() -> bool {
    worker_exe().is_some_and(|p| win::path_exists(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 1 << 28;

    /// 標頭一行，後面接 `rest`。
    fn stream(header: &str, rest: &[u8]) -> Vec<u8> {
        let mut v = header.as_bytes().to_vec();
        v.push(b'\n');
        v.extend_from_slice(rest);
        v
    }

    fn parse(bytes: &[u8]) -> ViewOutput {
        read_view_stdout(&mut io::Cursor::new(bytes), LIMIT)
    }

    fn assert_invalid(header: &str, why: &str) {
        // 標頭之後還有大量資料：必須全部讀掉，而且不配置樣本緩衝
        let bytes = stream(header, &[1u8; 4096]);
        let mut cur = io::Cursor::new(&bytes[..]);
        let o = read_view_stdout(&mut cur, LIMIT);
        assert_eq!(cur.position(), bytes.len() as u64, "{why}: the rest of the output is drained");
        let e = o.error.unwrap_or_else(|| panic!("{why}: accepted {header}"));
        assert!(e.starts_with("view header is invalid"), "{why}: {e}");
        assert_eq!(o.samples.capacity(), 0, "{why}: no sample buffer");
    }

    #[test]
    fn valid_stream_round_trips() {
        let mut rest = vec![1u8];
        rest.extend([1, 0, 2, 0]); // 一列 2 個 gray 樣本：1、2
        rest.push(0);
        rest.extend(br#"{"rows":1}"#);
        rest.push(b'\n');
        let o = parse(&stream(r#"{"format":"png","model":"gray","channels":1,"bits":8,"width":2,"height":1}"#, &rest));
        assert_eq!(o.error, None);
        assert_eq!(o.samples, [1, 2]);
        assert_eq!(o.trailer.map(|t| t.rows), Some(1));
    }

    #[test]
    fn malformed_headers_are_rejected_without_allocating() {
        assert_invalid(r#"{"model":"gray","channels":1,"bits":0,"width":2,"height":2}"#, "bits 0");
        assert_invalid(r#"{"model":"gray","channels":1,"bits":32,"width":2,"height":2}"#, "bits 32");
        assert_invalid(r#"{"model":"gray","channels":1,"bits":17,"width":2,"height":2}"#, "bits 17");
        assert_invalid(r#"{"model":"rgba","channels":1,"bits":8,"width":2,"height":2}"#, "channels vs model");
        assert_invalid(r#"{"model":"rgb","channels":4,"bits":8,"width":2,"height":2}"#, "channels vs model");
        assert_invalid(r#"{"model":"cmyk","channels":4,"bits":8,"width":2,"height":2}"#, "unknown model");
        assert_invalid(r#"{"model":"rgba","channels":4,"bits":8,"width":4294967295,"height":0}"#, "height 0");
        assert_invalid(r#"{"model":"rgba","channels":4,"bits":8,"width":0,"height":5}"#, "width 0");
        assert_invalid(
            r#"{"model":"rgba","channels":4,"bits":16,"width":4294967295,"height":4294967295}"#,
            "over the pixel limit",
        );
    }

    #[test]
    fn rows_beyond_the_height_are_malformed() {
        // 標頭說 1 列，送來 2 列
        let rest = [1u8, 7, 0, 1, 8, 0];
        let bytes = stream(r#"{"model":"gray","channels":1,"bits":8,"width":1,"height":1}"#, &rest);
        let mut cur = io::Cursor::new(&bytes[..]);
        let o = read_view_stdout(&mut cur, LIMIT);
        assert_eq!(o.error.as_deref(), Some("view output is malformed"));
        assert_eq!(o.samples, [7]);
        assert_eq!(cur.position(), bytes.len() as u64, "the rest of the output is drained");
    }
}
