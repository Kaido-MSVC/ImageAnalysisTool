//! 顯示用的轉換：只在這裡把原始樣本轉成 8-bit RGBA；差異熱圖與游標下的樣本值也在這裡算。
//! 判定一律用原始值，這些結果只供畫面顯示。

use std::path::PathBuf;

use iat_core::DecodedImage;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// 把 0..2^bits-1 的值四捨五入換算成 0..255。
#[expect(clippy::cast_possible_truncation, reason = "v ≤ max，結果 ≤ 255")]
fn to8(v: u16, bits: u32) -> u8 {
    let max = (1u32 << bits) - 1;
    ((u32::from(v) * 255 + max / 2) / max) as u8
}

fn has_alpha(model: &str) -> bool {
    model == "gray_alpha" || model == "rgba"
}

fn is_gray(model: &str) -> bool {
    model.starts_with("gray")
}

/// 顯示用的 RGBA8；沒有解出的列是透明的。
pub fn to_rgba8(img: &DecodedImage) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(img.width, img.height);
    let ch = img.channels as usize;
    let (gray, alpha) = (is_gray(img.model), has_alpha(img.model));
    let decoded = img.width as usize * img.rows as usize;
    for (px, s) in buf.make_mut_slice().iter_mut().take(decoded).zip(img.samples.chunks_exact(ch)) {
        let c = |i: usize| to8(s[i], img.bits);
        let (r, g, b) = if gray { (c(0), c(0), c(0)) } else { (c(0), c(1), c(2)) };
        let a = if alpha { c(ch - 1) } else { 255 };
        *px = Rgba8Pixel { r, g, b, a };
    }
    buf
}

/// 差異熱圖不能產生的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffUnavailable {
    /// 寬高不同
    Dimensions,
    /// 灰階對彩色或位元數不同：沒有一對一的樣本可比
    SampleModel,
}

/// 每個像素各聲道差的最大值（缺少 alpha 的一側視為不透明，同引擎的比較規則）。
fn max_diff(a: &[u16], b: &[u16], a_img: &DecodedImage, b_img: &DecodedImage) -> u32 {
    let color = if is_gray(a_img.model) { 1 } else { 3 };
    let opaque = (1u32 << a_img.bits) - 1;
    let mut d = 0;
    for c in 0..color {
        d = d.max(u32::from(a[c]).abs_diff(u32::from(b[c])));
    }
    if has_alpha(a_img.model) || has_alpha(b_img.model) {
        let aa = if has_alpha(a_img.model) { u32::from(a[a.len() - 1]) } else { opaque };
        let ba = if has_alpha(b_img.model) { u32::from(b[b.len() - 1]) } else { opaque };
        d = d.max(aa.abs_diff(ba));
    }
    d
}

/// 熱圖色階：0 為黑；非 0 依對數比例由暗紅、紅、黃到白，1 個單位的差也看得見。
fn heat(d: u32, max: u32) -> Rgba8Pixel {
    if d == 0 {
        return Rgba8Pixel { r: 0, g: 0, b: 0, a: 255 };
    }
    let t = ((1.0 + f64::from(d)).ln() / (1.0 + f64::from(max)).ln()).clamp(0.0, 1.0);
    let t = 0.2 + 0.8 * t;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已 clamp 到 0..=255")]
    let ramp = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgba8Pixel { r: ramp(t * 3.0), g: ramp(t * 3.0 - 1.0), b: ramp(t * 3.0 - 2.0), a: 255 }
}

/// 差異熱圖；只有兩側都解出的列有顏色，其餘透明。
pub fn heatmap(a: &DecodedImage, b: &DecodedImage) -> Result<SharedPixelBuffer<Rgba8Pixel>, DiffUnavailable> {
    if a.width != b.width || a.height != b.height {
        return Err(DiffUnavailable::Dimensions);
    }
    if is_gray(a.model) != is_gray(b.model) || a.bits != b.bits {
        return Err(DiffUnavailable::SampleModel);
    }
    let max = (1u32 << a.bits) - 1;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(a.width, a.height);
    let n = a.width as usize * a.rows.min(b.rows) as usize;
    let pa = a.samples.chunks_exact(a.channels as usize);
    let pb = b.samples.chunks_exact(b.channels as usize);
    for ((px, sa), sb) in buf.make_mut_slice().iter_mut().take(n).zip(pa).zip(pb) {
        *px = heat(max_diff(sa, sb, a, b), max);
    }
    Ok(buf)
}

/// 金字塔縮小時的合併方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    /// 2×2 平均（一般影像）
    Average,
    /// 2×2 取最亮（差異熱圖：單一個不同的像素縮小後仍看得見）
    Max,
}

/// 最小層的長邊不再小於這個值。
const PYRAMID_MIN_SIDE: u32 = 256;

/// 2×2 合併成一個像素；奇數邊的最後一欄／列只和自己合併。
pub fn half(src: &SharedPixelBuffer<Rgba8Pixel>, mode: Reduce) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (src.width(), src.height());
    let (nw, nh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(nw, nh);
    let sp = src.as_slice();
    let at = |x: u32, y: u32| sp[(y.min(h - 1) * w + x.min(w - 1)) as usize];
    for (i, px) in (0u32..).zip(out.make_mut_slice().iter_mut()) {
        let (x, y) = (i % nw * 2, i / nw * 2);
        let q = [at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1)];
        *px = match mode {
            Reduce::Average => {
                #[expect(clippy::cast_possible_truncation, reason = "四個 u8 的平均 ≤ 255")]
                let avg = |f: fn(&Rgba8Pixel) -> u8| ((q.iter().map(|p| u32::from(f(p))).sum::<u32>() + 2) / 4) as u8;
                Rgba8Pixel { r: avg(|p| p.r), g: avg(|p| p.g), b: avg(|p| p.b), a: avg(|p| p.a) }
            }
            Reduce::Max => {
                *q.iter().max_by_key(|p| u32::from(p.r) + u32::from(p.g) + u32::from(p.b)).expect("4 pixels")
            }
        };
    }
    out
}

/// 顯示用的縮圖金字塔：第 0 層是原尺寸，之後每層長寬減半，直到長邊不超過 [`PYRAMID_MIN_SIDE`]。
pub fn pyramid(full: SharedPixelBuffer<Rgba8Pixel>, mode: Reduce) -> Vec<SharedPixelBuffer<Rgba8Pixel>> {
    let mut levels = vec![full];
    loop {
        let last = levels.last().expect("level 0 exists");
        if last.width().max(last.height()) <= PYRAMID_MIN_SIDE {
            break;
        }
        let next = half(last, mode);
        levels.push(next);
    }
    levels
}

fn channel_letters(model: &str) -> &'static [&'static str] {
    match model {
        "gray" => &["Gray"],
        "gray_alpha" => &["Gray", "A"],
        "rgb" => &["R", "G", "B"],
        _ => &["R", "G", "B", "A"],
    }
}

fn samples_text(img: &DecodedImage, x: u32, y: u32) -> String {
    match img.pixel(x, y) {
        Some(s) => {
            channel_letters(img.model).iter().zip(s).map(|(n, v)| format!("{n} {v}")).collect::<Vec<_>>().join(" ")
        }
        None => "—".into(),
    }
}

/// 游標下的原始樣本值（兩側各自的座標相同）；超出影像時為空字串。
pub fn probe_text(a: &DecodedImage, b: Option<&DecodedImage>, x: i32, y: i32) -> String {
    let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else { return String::new() };
    let inside = |i: &DecodedImage| x < i.width && y < i.height;
    if !inside(a) && !b.is_some_and(inside) {
        return String::new();
    }
    let mut s = format!("x {x}  y {y}  │  A: {}", samples_text(a, x, y));
    if let Some(b) = b {
        s.push_str(&format!("  │  B: {}", samples_text(b, x, y)));
    }
    s
}

/// 影像資訊列，例如 `37×29 rgba 16-bit`。
pub fn image_info(img: &DecodedImage) -> String {
    let partial = if img.rows < img.height { format!(" ({}/{} rows)", img.rows, img.height) } else { String::new() };
    format!("{}×{} {} {}-bit{partial}", img.width, img.height, img.model, img.bits)
}

/// 拖放規則：一次兩個以上依拖入順序填入原檔與輸出（多的忽略）；一個時先填原檔，原檔已有則填輸出。
pub fn assign_drops(
    original: Option<PathBuf>,
    output: Option<PathBuf>,
    dropped: Vec<PathBuf>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut it = dropped.into_iter();
    match (it.next(), it.next()) {
        (Some(a), Some(b)) => (Some(a), Some(b)),
        (Some(a), None) if original.is_none() => (Some(a), output),
        (Some(a), None) => (original, Some(a)),
        (None, _) => (original, output),
    }
}

/// 介面語言：0 English、1 繁體中文、2 日本語。依 Windows 顯示語言的偏好順序挑第一個支援的；
/// 中文一律用繁中（沒有簡中翻譯）。
pub fn language_from_locales<I: IntoIterator<Item = String>>(locales: I) -> usize {
    for l in locales {
        let l = l.to_ascii_lowercase();
        if l.starts_with("zh") {
            return 1;
        }
        if l.starts_with("ja") {
            return 2;
        }
        if l.starts_with("en") {
            return 0;
        }
    }
    0
}

/// 設定檔裡保存的語系代碼；`language_from_locales` 讀得回同一個語言。
pub fn language_code(language: usize) -> &'static str {
    match language {
        1 => "zh_TW",
        2 => "ja",
        _ => "en",
    }
}

/// `select_bundled_translation` 用的語系目錄名稱。
pub fn translation_name(language: usize) -> &'static str {
    match language {
        1 => "zh_TW",
        2 => "ja",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(model: &'static str, channels: u32, bits: u32, w: u32, h: u32, samples: Vec<u16>) -> DecodedImage {
        DecodedImage { width: w, height: h, model, channels, bits, rows: h, samples, error: None }
    }

    #[test]
    fn display_scaling_rounds_any_bit_depth() {
        assert_eq!(to8(65535, 16), 255);
        assert_eq!(to8(0, 16), 0);
        assert_eq!(to8(0x8080, 16), 0x80);
        assert_eq!(to8(1, 1), 255);
        assert_eq!(to8(2, 2), 170);
        assert_eq!(to8(200, 8), 200);
    }

    #[test]
    fn rgba8_expands_gray_and_leaves_missing_rows_transparent() {
        let mut i = img("gray", 1, 16, 2, 2, vec![0, 65535]);
        i.rows = 1;
        let buf = to_rgba8(&i);
        let p = buf.as_slice();
        assert_eq!(p[1], Rgba8Pixel { r: 255, g: 255, b: 255, a: 255 });
        assert_eq!(p[2].a, 0, "row 1 was not decoded");
    }

    #[test]
    fn heatmap_marks_one_unit_and_rejects_mismatches() {
        let a = img("rgb", 3, 16, 2, 1, vec![10, 10, 10, 5, 5, 5]);
        let b = img("rgba", 4, 16, 2, 1, vec![10, 10, 10, 65535, 5, 6, 5, 65535]);
        let buf = heatmap(&a, &b).unwrap();
        assert_eq!(buf.as_slice()[0], Rgba8Pixel { r: 0, g: 0, b: 0, a: 255 }, "missing alpha is opaque");
        assert!(buf.as_slice()[1].r > 100, "a 1-unit difference is visible");
        let c = img("rgb", 3, 16, 1, 1, vec![0, 0, 0]);
        assert_eq!(heatmap(&a, &c).unwrap_err(), DiffUnavailable::Dimensions);
        let g = img("gray", 1, 16, 2, 1, vec![0, 0]);
        assert_eq!(heatmap(&a, &g).unwrap_err(), DiffUnavailable::SampleModel);
    }

    #[test]
    fn pyramid_halves_down_to_min_side_and_keeps_isolated_differences() {
        let mut full = SharedPixelBuffer::<Rgba8Pixel>::new(1025, 3);
        full.make_mut_slice()[1024] = Rgba8Pixel { r: 255, g: 0, b: 0, a: 255 };
        let lv = pyramid(full.clone(), Reduce::Max);
        let sizes: Vec<(u32, u32)> = lv.iter().map(|b| (b.width(), b.height())).collect();
        assert_eq!(sizes, vec![(1025, 3), (513, 2), (257, 1), (129, 1)]);
        assert_eq!(lv[3].as_slice()[128].r, 255, "max pooling keeps the lone pixel at the odd edge");
        let avg = half(&full, Reduce::Average);
        assert_eq!(avg.as_slice()[512].r, 128, "edge column averages with itself and the black row below");
    }

    #[test]
    fn probe_shows_raw_values() {
        let a = img("rgb", 3, 16, 1, 1, vec![1023, 7, 65535]);
        assert_eq!(probe_text(&a, None, 0, 0), "x 0  y 0  │  A: R 1023 G 7 B 65535");
        assert_eq!(probe_text(&a, None, 1, 0), "");
        assert_eq!(probe_text(&a, None, -1, 0), "");
    }

    #[test]
    fn drop_rules() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(assign_drops(None, None, vec![p("a"), p("b"), p("c")]), (Some(p("a")), Some(p("b"))));
        assert_eq!(assign_drops(None, Some(p("o")), vec![p("a")]), (Some(p("a")), Some(p("o"))));
        assert_eq!(assign_drops(Some(p("x")), None, vec![p("a")]), (Some(p("x")), Some(p("a"))));
    }

    #[test]
    fn language_choice() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(language_from_locales(v(&["zh-TW"])), 1);
        assert_eq!(language_from_locales(v(&["fr-FR", "ja-JP", "en-US"])), 2);
        assert_eq!(language_from_locales(v(&["de-DE"])), 0);
        assert_eq!(translation_name(1), "zh_TW");
        for l in 0..3 {
            assert_eq!(language_from_locales(v(&[language_code(l)])), l, "saved code reads back");
        }
    }
}
