//! iat-gui：iat-core 的 Slint 前端。core 在背景執行緒同步執行，結果以 `upgrade_in_event_loop` 送回 UI。
//! 拖放接 winit 的 HoveredFile／DroppedFile（Slint 的 DropArea 收不到檔案總管的拖放），
//! 用 50 ms 計時器等同一批檔案到齊。
#![windows_subsystem = "windows"]

mod display;
mod settings;
mod summary;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iat_core::markdown::render_markdown;
use iat_core::paths::file_name_of;
use iat_core::report::FileReport;
use iat_core::{
    CancelToken, DecodedImage, Limits, Mode, Progress, Report, ReportTargetError, RunRequest, Stage, WriteReportError,
    decode_for_view, report_json_text, run_analysis, validate_report_targets, write_new_file,
};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

mod ui {
    // Slint 產生的程式碼：lint 例外只開在這裡。
    #![expect(
        clippy::let_underscore_must_use,
        clippy::unwrap_used,
        clippy::clone_on_ref_ptr,
        clippy::todo,
        unreachable_pub,
        missing_debug_implementations,
        reason = "Slint 生成碼，不能修改"
    )]
    slint::include_modules!();
}
use ui::{AppWindow, FileInfo, IssueRow, MetaRow, ResultInfo};

/// 匯出結果的代碼（對應 app.slint 的 export-status）。
mod export_status {
    pub const SAVED: i32 = 1;
    pub const EXISTS: i32 = 2;
    pub const IS_INPUT: i32 = 3;
    pub const DIR_MISSING: i32 = 4;
    pub const FAILED: i32 = 5;
}

/// 檢視畫面的狀態（對應 app.slint 的 view-state）。
mod view_state {
    pub const LOADING: i32 = 1;
    pub const READY: i32 = 2;
    pub const ERROR: i32 = 3;
}

/// 已解碼、供游標查值的影像（只在 UI 執行緒持有）。
struct Viewed {
    key: (PathBuf, Option<PathBuf>),
    original: DecodedImage,
    output: Option<DecodedImage>,
}

#[derive(Default)]
struct AppState {
    original: Option<PathBuf>,
    output: Option<PathBuf>,
    /// 最近一次分析的請求與報告
    last: Option<(RunRequest, Report)>,
    cancel: Option<CancelToken>,
    viewed: Option<Viewed>,
    /// 目前正在背景解碼的檢視（避免重複啟動）
    view_loading: Option<(PathBuf, Option<PathBuf>)>,
}

thread_local! {
    static STATE: RefCell<AppState> = RefCell::default();
}

fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// 從背景執行緒把工作送回 UI 執行緒。事件迴圈已結束（視窗關閉）時送不到，結果也沒有人要看，直接放棄。
fn post(weak: &slint::Weak<AppWindow>, f: impl FnOnce(AppWindow) + Send + 'static) {
    if weak.upgrade_in_event_loop(f).is_err() {
        // 事件迴圈已結束
    }
}

fn path_text(p: Option<&Path>) -> (SharedString, SharedString) {
    match p {
        Some(p) => (file_name_of(p).into(), p.to_string_lossy().as_ref().into()),
        None => (SharedString::new(), SharedString::new()),
    }
}

fn refresh_files(ui: &AppWindow) {
    with_state(|s| {
        let (n, p) = path_text(s.original.as_deref());
        ui.set_original_name(n);
        ui.set_original_path(p);
        let (n, p) = path_text(s.output.as_deref());
        ui.set_output_name(n);
        ui.set_output_path(p);
    });
}

/// 進度回報：最多每 50 ms 送一次到 UI 執行緒。
struct GuiProgress {
    ui: slint::Weak<AppWindow>,
    last: Mutex<Option<Instant>>,
}

impl Progress for GuiProgress {
    fn report(&self, stage: Stage, done: u64, total: u64) {
        let mut last = self.last.lock().expect("progress lock poisoned");
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(50)) && done < total {
            return;
        }
        *last = Some(now);
        let frac = if total == 0 { 0.0 } else { (done as f64 / total as f64) as f32 };
        let st = if stage == Stage::Hash { 0 } else { 1 };
        post(&self.ui, move |ui| {
            ui.set_stage(st);
            ui.set_progress(frac);
        });
    }
}

fn file_info(f: &FileReport) -> FileInfo {
    FileInfo {
        name: f.name.as_str().into(),
        format: f.format.as_str().into(),
        size: f.size_bytes.0.to_string().into(),
        dims: if f.width > 0 { format!("{}×{}", f.width, f.height).into() } else { SharedString::new() },
        decode_status: f.decode_status.as_str().into(),
        decode_reason: f.decode_status_reason.as_str().into(),
    }
}

/// MiB 數值給 Slint 的 int（上限 1 TiB = 1048576 MiB，放得進 i32）。
fn mib_int(v: u64) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// `raised`：這次分析是「以建議值重新分析」時用的 `job_commit_mib`。
fn result_info(r: &Report, raised: Option<u64>) -> ResultInfo {
    let metadata: Vec<MetaRow> = r
        .metadata
        .fields
        .iter()
        .filter(|f| f.status != "same" && f.status != "both_absent")
        .map(|f| MetaRow { name: f.name.as_str().into(), status: f.status.as_str().into() })
        .collect();
    let suggested = summary::suggested_job_commit_mib(r);
    // 建議值超過目前可用的實體記憶體時加警告（仍允許執行）；查不到可用記憶體時不警告。
    let avail_mib = iat_core::win::available_physical_memory().map(|b| b >> 20);
    let over_memory = matches!((suggested, avail_mib), (Some(s), Some(a)) if s > a);
    let (orig_kind, orig_detail) = summary::color_source(&r.original);
    let (out_kind, out_detail) = r.output.as_ref().map(summary::color_source).unwrap_or_default();
    ResultInfo {
        pixel_identical: summary::pixel_identical(r),
        color_text: summary::color_basis_text(r).into(),
        color_original_kind: orig_kind.into(),
        color_original_detail: orig_detail.into(),
        color_output_kind: out_kind.into(),
        color_output_detail: out_detail.into(),
        suggested_mib: suggested.map_or(0, mib_int),
        suggest_over_memory: over_memory,
        available_mib: avail_mib.map_or(0, mib_int),
        raised_mib: raised.map_or(0, mib_int),
        compare: r.mode == "compare",
        exit_code: r.exit.code,
        exit_reason: r.exit.reason.as_str().into(),
        strict: r.strict.result.as_str().into(),
        strict_reasons: ModelRc::new(VecModel::from(
            r.strict.reasons.iter().map(|s| SharedString::from(s.as_str())).collect::<Vec<_>>(),
        )),
        samples_status: r.samples.status.as_str().into(),
        samples_reason: r.samples.reason.as_str().into(),
        color_status: r.color.status.as_str().into(),
        color_basis: r.color.basis.as_str().into(),
        metadata: ModelRc::new(VecModel::from(metadata)),
        original: file_info(&r.original),
        output: r.output.as_ref().map(file_info).unwrap_or_default(),
        run_id: r.run_id.as_str().into(),
    }
}

fn show_report(ui: &AppWindow, req: RunRequest, r: Report, raised: Option<u64>) {
    let rows: Vec<IssueRow> = r
        .issues
        .iter()
        .map(|i| IssueRow {
            severity: i.severity.as_str().into(),
            code: i.code.as_str().into(),
            scope: i.scope.as_str().into(),
            message: i.message.as_str().into(),
        })
        .collect();
    ui.set_result(result_info(&r, raised));
    ui.set_issues(ModelRc::new(VecModel::from(rows)));
    ui.set_export_status(0);
    ui.set_has_result(true);
    ui.set_page(2);
    with_state(|s| {
        s.cancel = None;
        s.last = Some((req, r));
    });
}

fn start(ui: &AppWindow) {
    let Some(req) = with_state(|s| {
        let original = s.original.clone()?;
        let mode = if s.output.is_some() { Mode::Compare } else { Mode::Inspect };
        Some(RunRequest {
            mode,
            original,
            output: s.output.clone(),
            temp_dir: None,
            full_paths: false,
            limits: Limits::default(),
        })
    }) else {
        return;
    };
    run_request(ui, req, None);
}

/// 「以建議值重新分析」：上一次的檔案與設定不變，只把 `job_commit_mib` 換成報告的建議值。
fn rerun_with_suggestion(ui: &AppWindow) {
    let Some((mut req, suggested)) =
        with_state(|s| s.last.as_ref().and_then(|(req, r)| Some((req.clone(), summary::suggested_job_commit_mib(r)?))))
    else {
        return;
    };
    req.limits.job_commit_mib = suggested;
    run_request(ui, req, Some(suggested));
}

/// 在背景執行緒分析；`raised` 是以建議值重新分析時的上限（結果畫面註明）。
fn run_request(ui: &AppWindow, req: RunRequest, raised: Option<u64>) {
    let cancel = CancelToken::new();
    with_state(|s| s.cancel = Some(cancel.clone()));
    ui.set_stage(0);
    ui.set_progress(0.0);
    ui.set_cancelling(false);
    ui.set_page(1);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let progress = GuiProgress { ui: weak.clone(), last: Mutex::new(None) };
        let report = run_analysis(&req, &cancel, &progress);
        post(&weak, move |ui| show_report(&ui, req, report, raised));
    });
}

/// 匯出一份報告；文字與 CLI 寫出的完全相同（同一個 report_json_text／render_markdown）。
fn export(ui: &AppWindow, markdown: bool) {
    let Some((req, report)) = with_state(|s| s.last.clone()) else { return };
    let (ext, filter) = if markdown { ("md", "Markdown") } else { ("json", "JSON") };
    let stem = req.original.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "report".into());
    let mut dialog = rfd::FileDialog::new().add_filter(filter, &[ext]).set_file_name(format!("{stem}.iat.{ext}"));
    if let Some(dir) = req.original.parent() {
        dialog = dialog.set_directory(dir);
    }
    let Some(path) = dialog.save_file() else { return };
    let (code, detail) = match save_report(&req, &report, &path, markdown) {
        Ok(()) => (export_status::SAVED, path.to_string_lossy().into_owned()),
        Err(e) => e,
    };
    ui.set_export_status(code);
    ui.set_export_detail(detail.into());
}

fn save_report(req: &RunRequest, r: &Report, path: &Path, markdown: bool) -> Result<(), (i32, String)> {
    let shown = path.to_string_lossy().into_owned();
    let (md, json) = if markdown { (Some(path), None) } else { (None, Some(path)) };
    validate_report_targets(req, md, json).map_err(|e| match e {
        ReportTargetError::AlreadyExists(_) => (export_status::EXISTS, shown.clone()),
        ReportTargetError::IsInput(_) => (export_status::IS_INPUT, shown.clone()),
        ReportTargetError::DirectoryMissing(d) => (export_status::DIR_MISSING, d.to_string_lossy().into_owned()),
        e @ ReportTargetError::SamePath => (export_status::FAILED, e.to_string()),
    })?;
    let text = if markdown { render_markdown(r) } else { report_json_text(r) };
    write_new_file(path, text.as_bytes(), &r.run_id).map_err(|e| match e {
        WriteReportError::TargetExists { .. } => (export_status::EXISTS, shown.clone()),
        e => (export_status::FAILED, e.to_string()),
    })
}

/// 解碼結果送回 UI 執行緒時的內容（像素緩衝可跨執行緒，Image 不行）。
struct ViewLoaded {
    original: DecodedImage,
    output: Option<DecodedImage>,
    /// 顯示用金字塔（第 0 層原尺寸）
    original_px: Vec<SharedPixelBuffer<Rgba8Pixel>>,
    output_px: Option<Vec<SharedPixelBuffer<Rgba8Pixel>>>,
    diff: Result<Vec<SharedPixelBuffer<Rgba8Pixel>>, Option<display::DiffUnavailable>>,
}

/// 顯示用影像的上限（16K×16K）。超過時樣本、RGBA 緩衝與金字塔合計需要好幾 GiB，解碼前就停止。
const MAX_VIEW_PIXELS: u64 = 1 << 28;

fn load_view(req: &RunRequest) -> Result<ViewLoaded, iat_core::ViewError> {
    let cancel = CancelToken::new();
    let original = decode_for_view(&req.original, req.temp_dir.as_deref(), &req.limits, MAX_VIEW_PIXELS, &cancel)?;
    let output = match &req.output {
        Some(p) => Some(decode_for_view(p, req.temp_dir.as_deref(), &req.limits, MAX_VIEW_PIXELS, &cancel)?),
        None => None,
    };
    let diff = match &output {
        Some(o) => display::heatmap(&original, o).map(|h| display::pyramid(h, display::Reduce::Max)).map_err(Some),
        None => Err(None),
    };
    let levels = |i: &DecodedImage| display::pyramid(display::to_rgba8(i), display::Reduce::Average);
    Ok(ViewLoaded { original_px: levels(&original), output_px: output.as_ref().map(levels), original, output, diff })
}

fn enter_viewer(ui: &AppWindow) {
    let Some(req) = with_state(|s| s.last.as_ref().map(|(r, _)| r.clone())) else { return };
    let key = (req.original.clone(), req.output.clone());
    let busy = with_state(|s| s.viewed.as_ref().is_some_and(|v| v.key == key) || s.view_loading.as_ref() == Some(&key));
    if busy {
        return;
    }
    with_state(|s| {
        s.viewed = None;
        s.view_loading = Some(key.clone());
    });
    ui.set_view_state(view_state::LOADING);
    ui.set_probe_text(SharedString::new());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = load_view(&req);
        post(&weak, move |ui| {
            // 期間又跑了另一次分析：丟掉過時的結果。
            if with_state(|s| s.view_loading.as_ref() != Some(&key)) {
                return;
            }
            with_state(|s| s.view_loading = None);
            match res {
                Err(iat_core::ViewError::OverViewLimit { width, height }) => {
                    ui.set_view_state(view_state::ERROR);
                    ui.set_view_error_kind(1);
                    ui.set_view_error(format!("{width}×{height}").into());
                }
                Err(e) => {
                    ui.set_view_state(view_state::ERROR);
                    ui.set_view_error_kind(0);
                    ui.set_view_error(e.to_string().into());
                }
                Ok(v) => show_view(&ui, key, v),
            }
        });
    });
}

fn images(levels: Vec<SharedPixelBuffer<Rgba8Pixel>>) -> ModelRc<Image> {
    ModelRc::new(VecModel::from(levels.into_iter().map(Image::from_rgba8).collect::<Vec<_>>()))
}

fn show_view(ui: &AppWindow, key: (PathBuf, Option<PathBuf>), v: ViewLoaded) {
    ui.set_img_original(images(v.original_px));
    ui.set_view_has_output(v.output_px.is_some());
    ui.set_img_output(images(v.output_px.unwrap_or_default()));
    let diff_state = match &v.diff {
        Ok(_) => 0,
        Err(Some(display::DiffUnavailable::Dimensions)) => 1,
        Err(Some(display::DiffUnavailable::SampleModel)) => 3,
        Err(None) => 2,
    };
    ui.set_view_diff_state(diff_state);
    ui.set_img_diff(images(v.diff.unwrap_or_default()));
    let mut info = format!("A: {}", display::image_info(&v.original));
    if let Some(o) = &v.output {
        info.push_str(&format!("  ·  B: {}", display::image_info(o)));
    }
    ui.set_view_info(info.into());
    let mode = ui.get_view_mode();
    if (mode == 1 && v.output.is_none()) || (mode == 2 && diff_state != 0) {
        ui.set_view_mode(0);
    }
    ui.set_view_state(view_state::READY);
    ui.set_view_fit_token(ui.get_view_fit_token().wrapping_add(1));
    with_state(|s| s.viewed = Some(Viewed { key, original: v.original, output: v.output }));
}

fn pick_file(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter(
            "PNG, TIFF, WebP, JPEG XL, PSD, PSB, AVIF",
            &["png", "tif", "tiff", "webp", "jxl", "psd", "psb", "avif"],
        )
        .add_filter("*", &["*"])
        .pick_file()
}

fn select_language(ui: &AppWindow, language: usize) {
    ui.set_language(language as i32);
    // 找不到的語系 Slint 會維持目前的語言；三個都是內建的，失敗只可能是建置設定錯誤。
    if let Err(e) = slint::select_bundled_translation(display::translation_name(language)) {
        eprintln!("iat-gui: cannot select translation: {e:?}");
    }
}

/// 介面上選的語言存進使用者設定；寫不進去時只少了「下次記住」，不影響操作。
fn remember_language(language: usize) {
    let Some(path) = settings::settings_path() else { return };
    let s = settings::Settings { language: Some(display::language_code(language).into()) };
    if let Err(e) = settings::save(&path, &s) {
        eprintln!("iat-gui: cannot save {}: {e}", path.display());
    }
}

fn install_drop_handler(ui: &AppWindow) {
    use slint::winit_030::winit::event::WindowEvent;
    use slint::winit_030::{EventResult, WinitWindowAccessor};
    let weak = ui.as_weak();
    let pending: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let timer = Rc::new(slint::Timer::default());
    ui.window().on_winit_window_event(move |_w, ev| {
        let Some(ui) = weak.upgrade() else { return EventResult::Propagate };
        match ev {
            WindowEvent::HoveredFile(_) => ui.set_os_drag(true),
            WindowEvent::HoveredFileCancelled => ui.set_os_drag(false),
            WindowEvent::DroppedFile(p) => {
                ui.set_os_drag(false);
                pending.borrow_mut().push(p.clone());
                let pending = pending.clone();
                let weak = weak.clone();
                timer.start(slint::TimerMode::SingleShot, Duration::from_millis(50), move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let dropped: Vec<PathBuf> = pending.borrow_mut().drain(..).collect();
                    // 執行中不換檔；其餘畫面一律回到選檔畫面顯示結果。
                    if ui.get_page() == 1 {
                        return;
                    }
                    with_state(|s| {
                        let (o, p) = display::assign_drops(s.original.take(), s.output.take(), dropped);
                        s.original = o;
                        s.output = p;
                    });
                    refresh_files(&ui);
                    ui.set_page(0);
                });
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_version(iat_core::VERSION.into());
    // 介面語言的優先順序：IAT_GUI_LANG（en、zh_TW、ja）> 上次在介面上選的 > Windows 顯示語言。
    let saved = settings::settings_path().map(|p| settings::load(&p)).and_then(|s| s.language);
    let locales: Vec<String> =
        std::env::var("IAT_GUI_LANG").into_iter().chain(saved).chain(sys_locale::get_locales()).collect();
    select_language(&ui, display::language_from_locales(locales));
    install_drop_handler(&ui);
    // `iat-gui <原檔> [<輸出檔>]`：預先填入（檔案總管的「開啟檔案」也會這樣傳入）。
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    with_state(|s| {
        let (o, p) = display::assign_drops(None, None, args);
        s.original = o;
        s.output = p;
    });
    refresh_files(&ui);

    let weak = ui.as_weak();
    ui.on_language_selected(move |i| {
        if let Some(ui) = weak.upgrade() {
            let language = usize::try_from(i).unwrap_or(0);
            select_language(&ui, language);
            remember_language(language);
        }
    });
    let weak = ui.as_weak();
    ui.on_rerun_suggested(move || {
        if let Some(ui) = weak.upgrade() {
            rerun_with_suggestion(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_open_original(move || {
        if let (Some(ui), Some(p)) = (weak.upgrade(), pick_file("Original")) {
            with_state(|s| s.original = Some(p));
            refresh_files(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_open_output(move || {
        if let (Some(ui), Some(p)) = (weak.upgrade(), pick_file("Output")) {
            with_state(|s| s.output = Some(p));
            refresh_files(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_clear_original(move || {
        with_state(|s| s.original = None);
        if let Some(ui) = weak.upgrade() {
            refresh_files(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_clear_output(move || {
        with_state(|s| s.output = None);
        if let Some(ui) = weak.upgrade() {
            refresh_files(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_swap(move || {
        with_state(|s| std::mem::swap(&mut s.original, &mut s.output));
        if let Some(ui) = weak.upgrade() {
            refresh_files(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_start(move || {
        if let Some(ui) = weak.upgrade() {
            start(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_cancel(move || {
        if let Some(c) = with_state(|s| s.cancel.clone()) {
            c.cancel();
        }
        if let Some(ui) = weak.upgrade() {
            ui.set_cancelling(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_export_json(move || {
        if let Some(ui) = weak.upgrade() {
            export(&ui, false);
        }
    });
    let weak = ui.as_weak();
    ui.on_export_md(move || {
        if let Some(ui) = weak.upgrade() {
            export(&ui, true);
        }
    });
    let weak = ui.as_weak();
    ui.on_enter_viewer(move || {
        if let Some(ui) = weak.upgrade() {
            enter_viewer(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_probe(move |x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let text = with_state(|s| {
            s.viewed.as_ref().map(|v| display::probe_text(&v.original, v.output.as_ref(), x, y)).unwrap_or_default()
        });
        ui.set_probe_text(text.into());
    });

    ui.run()
}
