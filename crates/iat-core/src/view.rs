//! GUI 檢視用的解碼：整張影像的正規樣本，不做任何轉換（16-bit 完整保留，顯示用的 8-bit 轉換在 GUI 裡做）。
//!
//! AVIF 在 iat-worker.exe 裡解碼（rav1d 的 unsafe 不進 GUI 程序，D-R02 第三次修訂）；其他格式都是
//! 純 safe 的 Rust 解碼器，留在 GUI 程序裡，樣本不必再經過管線複製一次（評估見 D-R02）。

use std::fmt;
use std::path::Path;

use crate::engine::{VecSink, ViewSamples, ViewStop, decode_view_samples};
use crate::input::Input;
use crate::paths::{full_path, is_directory, trim_dir};
use crate::run::open_read_only;
use crate::sniff::sniff_format;
use crate::temp::RunTemp;
use crate::worker::{WorkerFiles, WorkerMode, WorkerSettings, describe_failure, read_view_stdout, run_worker};
use crate::{CancelToken, Limits, NoProgress, win};

/// 解碼後的樣本。`samples` 依列排列，每個像素 `channels` 個值，值域 0..2^bits-1。
#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// gray | gray_alpha | rgb | rgba
    pub model: &'static str,
    pub channels: u32,
    /// 每個聲道的位元數（1、2、4、8、16）
    pub bits: u32,
    /// 成功解出的列數；小於 `height` 時其餘列沒有資料
    pub rows: u32,
    pub samples: Vec<u16>,
    /// 解碼中途失敗的原因（`rows < height` 時一定有）
    pub error: Option<String>,
}

impl DecodedImage {
    /// (x, y) 的樣本；超出範圍或該列沒有解出時為 None。
    pub fn pixel(&self, x: u32, y: u32) -> Option<&[u16]> {
        if x >= self.width || y >= self.rows {
            return None;
        }
        let ch = self.channels as usize;
        let i = (y as usize * self.width as usize + x as usize) * ch;
        self.samples.get(i..i + ch)
    }
}

/// 無法產生可檢視影像的原因；Display 為英文說明（GUI 依種類另外提示）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewError {
    /// 開檔或讀取失敗（Win32 錯誤文字）
    Open(String),
    /// 這個版本不支援的格式（內容判定的格式名稱）
    Unsupported(String),
    /// 一列都沒有解出
    Decode(String),
    /// 暫存檔無法建立（交錯影像需要）
    Temp(String),
    /// 樣本緩衝太大，無法配置（bytes）
    TooLarge(u64),
    /// 寬×高超過檢視上限（`max_pixels`）；分析與報告不受影響
    OverViewLimit {
        width: u32,
        height: u32,
    },
    Cancelled,
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(m) => write!(f, "cannot open input: {m}"),
            Self::Unsupported(fmt_name) => write!(f, "format '{fmt_name}' cannot be viewed by this build"),
            Self::Decode(m) => write!(f, "decoding failed: {m}"),
            Self::Temp(m) => write!(f, "temporary storage: {m}"),
            Self::TooLarge(b) => write!(f, "decoded samples need {b} bytes, which cannot be allocated"),
            Self::OverViewLimit { width, height } => write!(f, "{width}×{height} is larger than the viewing limit"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ViewError {}

/// 唯讀開檔並整張解碼。交錯 PNG 需要暫存檔，放在 `temp_dir`（None 為系統暫存目錄）。
/// 寬×高超過 `max_pixels` 時在配置樣本緩衝前就以 [`ViewError::OverViewLimit`] 停止。
///
/// # Errors
///
/// 見 [`ViewError`]。中途失敗但已解出部分列時回傳 `Ok`，`error` 記錄原因。
pub fn decode_for_view(
    path: &Path,
    temp_dir: Option<&Path>,
    limits: &Limits,
    max_pixels: u64,
    cancel: &CancelToken,
) -> Result<DecodedImage, ViewError> {
    let file = open_read_only(path).map_err(|e| ViewError::Open(win::error_text(e)))?;
    let input = Input::new(&file).map_err(|e| ViewError::Open(win::error_text(win::os_code(&e))))?;
    let dir = trim_dir(temp_dir.map_or_else(std::env::temp_dir, full_path));
    if !is_directory(&dir) {
        return Err(ViewError::Temp("temporary directory is not available".into()));
    }
    let free = win::disk_free_bytes(&dir).map_err(|e| ViewError::Temp(win::error_text(e)))?;
    let run_id =
        crate::run::new_run_id().map_err(|st| ViewError::Temp(format!("NTSTATUS 0x{:08X}", st.cast_unsigned())))?;
    let mut temp = RunTemp::create(&dir, &run_id).map_err(|e| ViewError::Temp(e.to_string()))?;
    let res = if sniff_format(input.head()) == "avif" {
        view_in_worker(&file, &temp, free, limits, max_pixels, cancel)
    } else {
        let mut sink = VecSink::default();
        decode_view_samples(&input, temp.files[0].as_ref(), free, limits, max_pixels, cancel, &mut sink)
            .map_err(stop_error)
            .map(|v| (v, sink.samples))
    };
    // 關閉即刪除；檢視用的暫存檔留下時只能交給下次執行的舊檔清理。
    temp.close();
    let (v, samples) = res?;
    if v.layout.channels == 0 {
        return Err(if crate::engine::has_frontend(&v.format) {
            ViewError::Decode(v.error.unwrap_or_default())
        } else {
            ViewError::Unsupported(v.format)
        });
    }
    if v.rows == 0 {
        return Err(ViewError::Decode(v.error.unwrap_or_else(|| "no rows decoded".into())));
    }
    Ok(DecodedImage {
        width: v.width,
        height: v.height,
        model: v.layout.model,
        channels: v.layout.channels,
        bits: v.layout.bits,
        rows: v.rows,
        samples,
        error: v.error,
    })
}

fn stop_error(s: ViewStop) -> ViewError {
    match s {
        ViewStop::Cancelled => ViewError::Cancelled,
        ViewStop::TooLarge(b) => ViewError::TooLarge(b),
        ViewStop::OverViewLimit { width, height } => ViewError::OverViewLimit { width, height },
    }
}

/// 在 worker 裡解碼（套用 `limits.job_commit_mib` 的 Job 上限），樣本經 stdout 傳回。
fn view_in_worker(
    file: &std::fs::File,
    temp: &RunTemp,
    free: u64,
    limits: &Limits,
    max_pixels: u64,
    cancel: &CancelToken,
) -> Result<(ViewSamples, Vec<u16>), ViewError> {
    let [Some(ta), Some(tb)] = &temp.files else {
        return Err(ViewError::Temp("temporary files are not available".into()));
    };
    let settings = WorkerSettings {
        mode: WorkerMode::View,
        limits: *limits,
        temp_free: free,
        av1_failed: [false; 2],
        alloc_failed: [None; 2],
        view_max_pixels: max_pixels,
    };
    let w = run_worker(
        &settings,
        WorkerFiles { original: file, output: None, temp: [ta, tb] },
        cancel,
        None,
        &NoProgress,
        read_view_stdout,
    );
    if cancel.is_cancelled() {
        return Err(ViewError::Cancelled);
    }
    if let Some(m) = describe_failure(&w) {
        // rav1d 的 panic 也在這裡：檢視只回報失敗，不重跑
        return Err(ViewError::Decode(m));
    }
    let Some(o) = w.stdout else { return Err(ViewError::Decode("no output from the worker".into())) };
    if let Some(e) = o.error {
        return Err(ViewError::Decode(e));
    }
    let h = o.header;
    match h.stop.as_str() {
        "too_large" => return Err(ViewError::TooLarge(h.bytes)),
        "over_view_limit" => return Err(ViewError::OverViewLimit { width: h.width, height: h.height }),
        _ => {}
    }
    let t = o.trailer.unwrap_or_default();
    let layout = crate::engine::RowLayout::new(crate::engine::model_name(&h.model), h.channels, h.bits);
    let v = ViewSamples { layout, width: h.width, height: h.height, rows: t.rows, format: h.format, error: t.error };
    Ok((v, o.samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated").join(name)
    }

    #[test]
    fn decodes_png_samples_without_conversion() {
        let img =
            decode_for_view(&fixture("png_gray16_trns.png"), None, &Limits::default(), u64::MAX, &CancelToken::new())
                .expect("fixture decodes");
        assert_eq!(img.bits, 16);
        assert_eq!(img.model, "gray_alpha");
        assert_eq!(img.rows, img.height);
        assert_eq!(img.samples.len(), (img.width * img.height * img.channels) as usize);
        assert!(img.pixel(img.width, 0).is_none());
    }

    #[test]
    fn interlaced_png_uses_temp_and_matches_size() {
        let img = decode_for_view(
            &fixture("s2_rgba16_37x29_adam7.png"),
            None,
            &Limits::default(),
            u64::MAX,
            &CancelToken::new(),
        )
        .expect("fixture decodes");
        assert_eq!(img.rows, img.height);
        assert!(img.error.is_none(), "{:?}", img.error);
    }

    #[test]
    fn tiff_strips_and_tiles_match_the_png() {
        let view = |n: &str, lim: &Limits| decode_for_view(&fixture(n), None, lim, u64::MAX, &CancelToken::new());
        let png = view("s2_rgb16_icc1.png", &Limits::default()).expect("png decodes");
        for n in
            ["s2_tif_rgb16_be_strip_lzw.tif", "s2_tif_rgb16_le_tile_deflate.tif", "s2_btf_rgb16_be_tile_deflate.tif"]
        {
            let img = view(n, &Limits::default()).expect("tiff decodes");
            assert_eq!((img.model, img.bits, img.rows), ("rgb", 16, img.height), "{n}");
            assert_eq!(img.samples, png.samples, "{n}");
        }
        // 一排 tile 超過預算的一半：逐 tile 寫進暫存再依列重組，樣本與 strip 版相同
        let mut small = Limits::default();
        small.set("buffer_mib=8").expect("valid limit");
        let tile = view("s2_tif_rgb16_4096x300_tile256.tif", &small).expect("tile via temp");
        let strip = view("s2_tif_rgb16_4096x300_strip16_be.tif", &small).expect("strips");
        assert_eq!(tile.samples, strip.samples);
    }

    #[test]
    fn jxl_matches_the_png_and_keeps_native_depth() {
        let view = |n: &str| decode_for_view(&fixture(n), None, &Limits::default(), u64::MAX, &CancelToken::new());
        let png = view("s3_rgba8_srgb.png").expect("png decodes");
        let jxl = view("s3_jxl_rgba8_srgb.jxl").expect("jxl decodes");
        assert_eq!((jxl.model, jxl.bits, jxl.rows), ("rgba", 8, jxl.height));
        assert_eq!(jxl.samples, png.samples);
        let twelve = view("s3_jxl_rgb12_600x40.jxl").expect("12-bit jxl decodes");
        assert_eq!(twelve.bits, 12);
        assert!(twelve.samples.iter().all(|&v| v < 4096));
    }

    #[test]
    fn pixel_cap_stops_before_allocating() {
        let e = decode_for_view(&fixture("png_gray16_trns.png"), None, &Limits::default(), 1, &CancelToken::new())
            .unwrap_err();
        assert!(matches!(e, ViewError::OverViewLimit { .. }), "{e:?}");
    }

    #[test]
    fn webp_keeps_alpha_and_rgb_under_zero_alpha() {
        let view = |n: &str| decode_for_view(&fixture(n), None, &Limits::default(), u64::MAX, &CancelToken::new());
        let img = view("s3_webp_rgba8_icc.webp").expect("fixture decodes");
        assert_eq!((img.model, img.bits, img.rows), ("rgba", 8, img.height));
        let hidden = img.samples.as_chunks::<4>().0.iter().filter(|p| p[3] == 0 && p[..3] != [0, 0, 0]).count();
        assert!(hidden > 0, "RGB under alpha 0 is kept");
        // VP8L 提示關閉但有 alpha：仍以 RGBA 交出
        let img = view("s3_webp_hint_off_alpha40.webp").expect("fixture decodes");
        assert_eq!(img.model, "rgba");
        assert!(img.samples.as_chunks::<4>().0.iter().all(|p| p[3] == 64));
        // 提示關閉且完全不透明：RGB
        assert_eq!(view("s3_webp_rgb8_icc.webp").expect("fixture decodes").model, "rgb");
    }

    #[test]
    fn unknown_format_is_unsupported() {
        let e = decode_for_view(&fixture("manifest.json"), None, &Limits::default(), u64::MAX, &CancelToken::new())
            .unwrap_err();
        assert_eq!(e, ViewError::Unsupported("unknown".into()));
    }
}
