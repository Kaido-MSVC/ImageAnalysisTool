//! GUI 檢視用的解碼：整張影像的正規樣本，不做任何轉換（16-bit 完整保留，顯示用的 8-bit 轉換在 GUI 裡做）。

use std::fmt;
use std::path::Path;

use crate::engine::{ViewStop, decode_view_samples};
use crate::input::Input;
use crate::paths::{full_path, is_directory, trim_dir};
use crate::run::open_read_only;
use crate::temp::RunTemp;
use crate::{CancelToken, Limits, win};

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
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ViewError {}

/// 唯讀開檔並整張解碼。交錯 PNG 需要暫存檔，放在 `temp_dir`（None 為系統暫存目錄）。
/// 寬×高超過 `max_pixels` 時在配置樣本緩衝前就以 [`ViewError::TooLarge`] 停止。
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
    let run_id = crate::run::new_run_id().map_err(|st| ViewError::Temp(format!("NTSTATUS 0x{:08X}", st as u32)))?;
    let mut temp = RunTemp::create(&dir, &run_id).map_err(|e| ViewError::Temp(e.to_string()))?;
    let res = decode_view_samples(&input, temp.files[0].as_ref(), free, limits, max_pixels, cancel);
    // 關閉即刪除；檢視用的暫存檔留下時只能交給下次執行的舊檔清理。
    temp.close();
    let v = res.map_err(|s| match s {
        ViewStop::Cancelled => ViewError::Cancelled,
        ViewStop::TooLarge(b) => ViewError::TooLarge(b),
    })?;
    if v.layout.channels == 0 {
        return Err(if v.format == "png" {
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
        samples: v.samples,
        error: v.error,
    })
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
    fn pixel_cap_stops_before_allocating() {
        let e = decode_for_view(&fixture("png_gray16_trns.png"), None, &Limits::default(), 1, &CancelToken::new())
            .unwrap_err();
        assert!(matches!(e, ViewError::TooLarge(_)), "{e:?}");
    }

    #[test]
    fn unknown_format_is_unsupported() {
        let e = decode_for_view(&fixture("manifest.json"), None, &Limits::default(), u64::MAX, &CancelToken::new())
            .unwrap_err();
        assert_eq!(e, ViewError::Unsupported("unknown".into()));
    }
}
