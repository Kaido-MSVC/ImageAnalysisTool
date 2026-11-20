//! iat-gui：iat-core 的 Slint 前端。core 在背景執行緒同步執行，結果以 `upgrade_in_event_loop` 送回 UI。
//! 拖放接 winit 的 HoveredFile／DroppedFile（Slint 的 DropArea 收不到檔案總管的拖放），
//! 用 50 ms 計時器等同一批檔案到齊。
#![windows_subsystem = "windows"]

mod display;

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
    #![allow(clippy::let_underscore_must_use)]
    slint::include_modules!();
}
use ui::{AppWindow, IssueRow, ResultInfo};

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

fn file_detail(f: &FileReport) -> String {
    let mut parts = vec![f.format.clone(), format!("{} bytes", f.size_bytes.0)];
    if f.width > 0 {
        parts.push(format!("{}×{}", f.width, f.height));
    }
    if !f.decode_status.is_empty() {
        parts.push(format!("decode {}", f.decode_status));
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

fn status_text(status: &str, detail: &str) -> String {
    if detail.is_empty() { status.to_string() } else { format!("{status} ({detail})") }
}

fn result_info(r: &Report) -> ResultInfo {
    let changed: Vec<String> = r
        .metadata
        .fields
        .iter()
        .filter(|f| f.status != "same" && f.status != "both_absent")
        .map(|f| format!("{} {}", f.name, f.status))
        .collect();
    let metadata = if changed.is_empty() { "same".to_string() } else { changed.join(", ") };
    let out = r.output.as_ref();
    ResultInfo {
        compare: r.mode == "compare",
        exit_code: r.exit.code,
        exit_reason: r.exit.reason.as_str().into(),
        strict: r.strict.result.as_str().into(),
        strict_reasons: r.strict.reasons.join(", ").into(),
        samples: status_text(&r.samples.status, &r.samples.reason).into(),
        color: status_text(&r.color.status, &r.color.basis).into(),
        metadata: metadata.into(),
        original_name: r.original.name.as_str().into(),
        original_detail: file_detail(&r.original).into(),
        output_name: out.map(|o| o.name.clone()).unwrap_or_default().into(),
        output_detail: out.map(file_detail).unwrap_or_default().into(),
        run_id: r.run_id.as_str().into(),
    }
}

fn show_report(ui: &AppWindow, req: RunRequest, r: Report) {
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
    ui.set_result(result_info(&r));
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
        post(&weak, move |ui| show_report(&ui, req, report));
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
                Err(e) => {
                    ui.set_view_state(view_state::ERROR);
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
    rfd::FileDialog::new().set_title(title).add_filter("PNG", &["png"]).add_filter("*", &["*"]).pick_file()
}

fn select_language(ui: &AppWindow, language: usize) {
    ui.set_language(language as i32);
    // 找不到的語系 Slint 會維持目前的語言；三個都是內建的，失敗只可能是建置設定錯誤。
    if let Err(e) = slint::select_bundled_translation(display::translation_name(language)) {
        eprintln!("iat-gui: cannot select translation: {e:?}");
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
    // IAT_GUI_LANG（en、zh_TW、ja）優先於 Windows 顯示語言。
    let locales: Vec<String> = std::env::var("IAT_GUI_LANG").into_iter().chain(sys_locale::get_locales()).collect();
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
            select_language(&ui, usize::try_from(i).unwrap_or(0));
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
