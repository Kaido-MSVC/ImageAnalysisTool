//! JPEG XL 前端（移植 C++ 版 `src/engine/jxl_side.cpp` 與 `whole_image.*`）：自己的 box 掃描
//! （[`crate::jxl_scan`]）+ jxl crate 0.8.0（jxl-rs）解碼（D-R18）。
//!
//! - 標頭輪：解碼器讀基本資訊與色彩，掃描各 frame 標頭但不解像素；extra channel 的位元深度與
//!   色彩聲道數由 [`crate::jxl_header`] 自己解析（jxl-rs 沒有公開）。
//! - 色彩列舉值的 ICC 用 libjxl 0.12.0 產生的查表（[`crate::jxl_icc`]），和 C++ 版逐位元組相同。
//! - 解碼輪：不轉向、不渲染 spot color、不反乘 alpha，輸出 codestream 原本位元深度的整數樣本
//!   （同 C++ 版的 JXL_BIT_DEPTH_FROM_CODESTREAM）。整張影像解出後依列寫進暫存檔，再逐列交給引擎。
//! - jxl-rs 沒有記憶體分配器的掛鉤，配置上限只靠事前的預估（C++ 公式）；jxl-rs 0.8.0 也沒有實作
//!   `coalescing = false`，所以無法回報圖層數（docs/decisions.md D-R18）。

use std::collections::BTreeMap;
use std::io::IoSliceMut;
use std::panic::{AssertUnwindSafe, catch_unwind};

use jxl::api::{
    AlphaMode, Endianness, Event, JxlBitstreamInput, JxlColorEncoding, JxlColorProfile, JxlColorType, JxlDataFormat,
    JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, JxlPixelFormat, JxlPrimaries, JxlTransferFunction, JxlWhitePoint,
    RenderingIntent,
};

use crate::engine::{Decoder, RowLayout, SideState, temp_failure};
use crate::input::Input;
use crate::jxl_header::{self, Header};
use crate::jxl_icc::{EnumKey, synthesized_icc};
use crate::jxl_scan::{JxlScan, scan_jxl};
use crate::limits::Limits;
use crate::png_scan::exif_orientation;
use crate::report::{Count, EffectiveColor, FileReport, Issue, MetaItem, Orientation, code};
use crate::temp::{TempError, TempErrorKind};
use crate::text::{sha256_hex, short_hash};
use crate::u64_to_usize;

pub const DECODER_NAME: &str = "jxl";
pub const DECODER_VERSION: &str = "0.8.0";
/// C++ 版報告 synthesized ICC 時寫的 libjxl 版本；查表的資料就是這一版產生的。
const LIBJXL_VERSION: &str = "0.12.0";
/// 解析 ImageMetadata 時最多讀的 codestream 開頭位元組數（標頭遠小於此）。
const HEADER_BYTES: u64 = 1 << 20;
/// 部分列的區間清單：與影像大小無關的固定額度（同 C++ 版 kCoverageAllowance）
const COVERAGE_ALLOWANCE: u64 = 1 << 20;

/// 依位移讀取唯讀輸入，交給 jxl-rs。
struct FileInput<'a, 'b> {
    input: &'a Input<'b>,
    pos: u64,
}

impl JxlBitstreamInput for FileInput<'_, '_> {
    fn available_bytes(&mut self) -> Result<usize, std::io::Error> {
        Ok(usize::try_from(self.input.size().saturating_sub(self.pos)).unwrap_or(usize::MAX))
    }

    fn read(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize, std::io::Error> {
        let mut total = 0;
        for b in bufs {
            if b.is_empty() {
                continue;
            }
            let n = self.input.read_at(self.pos, b)?;
            self.pos += n as u64;
            total += n;
            if n < b.len() {
                break;
            }
        }
        Ok(total)
    }
}

fn options(scan_only: bool) -> JxlDecoderOptions {
    let mut o = JxlDecoderOptions::default();
    o.adjust_orientation = false; // 以儲存的方向比較（規劃書 3.1）
    o.render_spot_colors = false;
    o.alpha_mode = AlphaMode::AsIs; // 不反乘
    o.scan_frames_only = scan_only;
    o
}

/// 解碼器一步：jxl-rs 的事件、錯誤，或 panic（視為解碼錯誤，不讓整個程式結束）。
fn step(
    d: &mut JxlDecoder,
    input: &mut FileInput<'_, '_>,
    buffers: Option<&mut [JxlOutputBuffer<'_>]>,
) -> Result<Event, String> {
    match catch_unwind(AssertUnwindSafe(|| d.process(input, buffers, None))) {
        Ok(Ok(e)) => Ok(e),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("decoder panicked".into()),
    }
}

/// 標頭輪的結果。
struct HeaderInfo {
    /// ok | truncated | decode_error | io_error
    status: &'static str,
    error: String,
    header: Option<Header>,
    profile: Option<JxlColorProfile>,
    intensity_target: f32,
    all_channels: u32,
}

/// codestream 開頭（容器時把 jxlc／jxlp 各段接起來），供 ImageMetadata 解析。
fn codestream_head(input: &Input<'_>, sc: &JxlScan) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for &(off, len) in &sc.codestream_ranges {
        let take = len.min(HEADER_BYTES - out.len() as u64);
        let start = out.len();
        out.resize(start + usize::try_from(take).ok()?, 0);
        if !matches!(input.read_at(off, &mut out[start..]), Ok(n) if n as u64 == take) {
            return None;
        }
        if out.len() as u64 >= HEADER_BYTES {
            break;
        }
    }
    Some(out)
}

fn read_header(input: &Input<'_>, sc: &JxlScan) -> HeaderInfo {
    let mut h = HeaderInfo {
        status: "decode_error",
        error: String::new(),
        header: None,
        profile: None,
        intensity_target: 0.0,
        all_channels: 0,
    };
    let mut d = JxlDecoder::new(options(true));
    let mut fi = FileInput { input, pos: 0 };
    loop {
        match step(&mut d, &mut fi, None) {
            Err(e) => {
                h.error = e;
                return h;
            }
            Ok(Event::NeedMoreInput { .. }) => {
                // 最後一個 frame 的標頭已經讀到：libjxl 的標頭輪到這裡就成功了
                if d.basic_info().is_some() && d.scanned_frames().last().is_some_and(|f| f.is_last) {
                    break;
                }
                h.status = "truncated";
                return h;
            }
            Ok(Event::Complete) => break,
            Ok(Event::FrameHeader) if d.scanned_frames().last().is_some_and(|f| f.is_last) => break,
            Ok(_) => {}
        }
    }
    let (Some(bi), Some(profile)) = (d.basic_info(), d.embedded_color_profile()) else {
        h.error = "no basic info".into();
        return h;
    };
    h.intensity_target = bi.tone_mapping.intensity_target;
    h.profile = Some(profile.clone());
    let Some(head) = codestream_head(input, sc) else {
        h.status = "io_error";
        return h;
    };
    match jxl_header::parse(&head) {
        Ok(p) => {
            h.all_channels =
                u32::try_from(p.extra.len()).ok().and_then(|n| p.color_channels().checked_add(n)).unwrap_or(u32::MAX);
            h.header = Some(p);
            h.status = "ok";
        }
        Err(_) => h.error = "image header could not be parsed".into(),
    }
    h
}

/// libjxl JxlColorEncoding 的數值。
fn white_code(w: &JxlWhitePoint) -> u8 {
    match w {
        JxlWhitePoint::D65 => 1,
        JxlWhitePoint::Chromaticity { .. } => 2,
        JxlWhitePoint::E => 10,
        JxlWhitePoint::DCI => 11,
    }
}
fn primaries_code(p: &JxlPrimaries) -> u8 {
    match p {
        JxlPrimaries::SRGB => 1,
        JxlPrimaries::Chromaticities { .. } => 2,
        JxlPrimaries::BT2100 => 9,
        JxlPrimaries::P3 => 11,
    }
}
fn transfer_code(t: &JxlTransferFunction) -> u16 {
    match t {
        JxlTransferFunction::BT709 => 1,
        JxlTransferFunction::Linear => 8,
        JxlTransferFunction::SRGB => 13,
        JxlTransferFunction::PQ => 16,
        JxlTransferFunction::DCI => 17,
        JxlTransferFunction::HLG => 18,
        JxlTransferFunction::Gamma(_) => 65535,
    }
}
fn intent_code(r: &RenderingIntent) -> u8 {
    match r {
        RenderingIntent::Perceptual => 0,
        RenderingIntent::Relative => 1,
        RenderingIntent::Saturation => 2,
        RenderingIntent::Absolute => 3,
    }
}
fn tf_name(code: u16) -> &'static str {
    match code {
        1 => "BT.709",
        8 => "linear",
        13 => "sRGB",
        16 => "PQ",
        17 => "DCI",
        18 => "HLG",
        65535 => "gamma",
        _ => "unknown",
    }
}

/// C++ 的 `std::to_string(double)`（"%f"）。
fn cpp_double(v: f32) -> String {
    format!("{:.6}", f64::from(v))
}

/// 列舉值的報告形式：(key, 描述, 訊號, 查表鍵, 轉換函數代碼)。
struct EnumColor {
    key: String,
    intent: u8,
    transfer: u16,
    lookup: Option<EnumKey>,
}

fn enum_color(c: &JxlColorEncoding) -> EnumColor {
    let (space, white, prim, tf, ri) = match c {
        JxlColorEncoding::RgbColorSpace { white_point, primaries, transfer_function, rendering_intent } => {
            (0u8, Some(white_point), Some(primaries), Some(transfer_function), rendering_intent)
        }
        JxlColorEncoding::GrayscaleColorSpace { white_point, transfer_function, rendering_intent } => {
            (1, Some(white_point), None, Some(transfer_function), rendering_intent)
        }
        JxlColorEncoding::XYB { rendering_intent } => (2, None, None, None, rendering_intent),
    };
    let w = white.map_or(1, white_code);
    let p = prim.map_or(0, primaries_code);
    let t = tf.map_or(65535, transfer_code);
    let mut key = format!("jxl_enum:space={space};white={w};primaries={p};transfer={t}");
    if let Some(JxlTransferFunction::Gamma(g)) = tf {
        key += &format!(";gamma={}", cpp_double(*g));
    }
    if let Some(JxlWhitePoint::Chromaticity { wx, wy }) = white {
        key += &format!(";wxy={},{}", cpp_double(*wx), cpp_double(*wy));
    }
    if let Some(JxlPrimaries::Chromaticities { rx, ry, gx, gy, bx, by }) = prim {
        key += &format!(
            ";rxy={},{};gxy={},{};bxy={},{}",
            cpp_double(*rx),
            cpp_double(*ry),
            cpp_double(*gx),
            cpp_double(*gy),
            cpp_double(*bx),
            cpp_double(*by)
        );
    }
    let intent = intent_code(ri);
    let custom = w == 2 || p == 2 || t == 65535;
    let lookup = u8::try_from(t).ok().filter(|_| !custom).map(|t| EnumKey {
        color_space: space,
        white_point: w,
        primaries: p,
        transfer_function: t,
        rendering_intent: intent,
    });
    EnumColor { key, intent, transfer: t, lookup }
}

struct JxlImageDecoder<'a> {
    input: &'a Input<'a>,
    width: u32,
    height: u32,
    channels: u32,
    bits: u32,
    bare: bool,
    codec_estimate: Option<u64>,
    row: Vec<u8>,
    decoded: bool,
}

impl<'a> JxlImageDecoder<'a> {
    fn row_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.channels) * 2
    }

    fn fail_decode(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>, reason: &str, msg: String) {
        s.failed = true;
        s.rep.decode_status = "failed".into();
        s.rep.decode_status_reason = reason.into();
        issues.push(Issue::new(code::DECODE_FAILED, "error", s.scope, msg));
    }

    fn fail_resource(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>, msg: &str) {
        s.failed = true;
        s.rep.decode_status = "not_checked".into();
        s.rep.decode_status_reason = "resource_limit".into();
        issues.push(Issue::new(code::RESOURCE_LIMIT, "error", s.scope, msg));
    }

    /// 解出整張影像（主機位元組順序的 u16 樣本）；失敗時已記錄原因。
    /// 回傳 (資料, 資料在緩衝裡的起點)：緩衝的起點要對齊 2 bytes。
    fn decode_all(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>) -> Option<(Vec<u8>, usize)> {
        let total = self.row_bytes().checked_mul(u64::from(self.height));
        let mut buf = Vec::new();
        let Some(n) = total.and_then(|t| usize::try_from(t).ok()) else {
            self.fail_resource(s, issues, "decoded image size overflows the address space");
            return None;
        };
        if buf.try_reserve_exact(n + 1).is_err() {
            self.fail_resource(s, issues, "the buffer for the decoded image could not be allocated");
            return None;
        }
        buf.resize(n + 1, 0);
        let start = buf.as_ptr().align_offset(2).min(1);
        let mut d = JxlDecoder::new(options(false));
        let mut fi = FileInput { input: self.input, pos: 0 };
        let color_type = match (self.channels, self.bits) {
            (1, _) => JxlColorType::Grayscale,
            (2, _) => JxlColorType::GrayscaleAlpha,
            (3, _) => JxlColorType::Rgb,
            _ => JxlColorType::Rgba,
        };
        let mut fail = String::new();
        let mut complete = false;
        let mut format_set = false;
        loop {
            let ev = if format_set {
                let rows = &mut buf[start..start + n];
                let mut ob = [JxlOutputBuffer::new(rows, self.height as usize, u64_to_usize(self.row_bytes()))];
                step(&mut d, &mut fi, Some(&mut ob))
            } else {
                step(&mut d, &mut fi, None)
            };
            match ev {
                Err(e) => {
                    fail = format!("decode_error ({e})");
                    break;
                }
                Ok(Event::NeedMoreInput { .. }) => {
                    fail = "truncated".into();
                    break;
                }
                Ok(Event::BasicInfo) => {
                    let extra = d.basic_info().map_or(0, |b| b.extra_channels.len());
                    #[expect(clippy::cast_possible_truncation, reason = "JXL 位元深度 ≤ 64（1 + 6 bits 編碼）")]
                    let pf = JxlPixelFormat {
                        color_type,
                        color_data_format: Some(JxlDataFormat::U16 {
                            endianness: Endianness::native(),
                            bit_depth: self.bits as u8,
                        }),
                        extra_channel_format: vec![None; extra],
                    };
                    if let Err(e) = d.set_pixel_format(pf) {
                        fail = format!("callback_rejected ({e})");
                        break;
                    }
                    format_set = true;
                }
                Ok(Event::FrameComplete { .. }) => complete = true,
                Ok(Event::Complete) => break,
                Ok(Event::FrameHeader) => {}
            }
        }
        if !fail.is_empty() {
            if fail == "truncated" {
                issues.push(Issue::new(
                    code::TRUNCATED,
                    "error",
                    s.scope,
                    "codestream ends before the image is complete",
                ));
            }
            let reason = fail.split(' ').next().unwrap_or("decode_error").to_string();
            self.fail_decode(s, issues, &reason, format!("jxl-rs: {fail}"));
            return None;
        }
        if !complete {
            self.fail_decode(s, issues, "no_full_image", "jxl-rs finished without a full image".into());
            return None;
        }
        // 裸 codestream 在解碼器停下的地方結束，之後的位元組是尾端資料（容器的每個位元組都已由掃描交代）。
        if self.bare {
            let consumed = d.file_length().unwrap_or(fi.pos);
            let unconsumed = self.input.size().saturating_sub(consumed);
            s.rep.coverage.trailing_bytes = Some(Count(unconsumed));
            if unconsumed > 0 {
                s.rep.container_status = "warning".into();
                issues.push(Issue::new(
                    code::TRAILING_DATA,
                    "warning",
                    s.scope,
                    format!("{unconsumed} bytes after the end of the codestream"),
                ));
            }
        }
        Some((buf, start))
    }
}

impl<'a> Decoder<'a> for JxlImageDecoder<'a> {
    fn buffer_estimate(&self) -> Option<u64> {
        // 正規列 + 暫存列（讀回）+ 轉換緩衝 + 每列一個狀態位元組 + 部分列額度（同 C++ 版 WholeImageDecoder）
        let canon = u64::from(self.width).checked_mul(u64::from(self.channels))?.checked_mul(2)?;
        let temp_row = self.row_bytes();
        canon
            .checked_add(temp_row)?
            .checked_add(temp_row)?
            .checked_add(u64::from(self.height))?
            .checked_add(COVERAGE_ALLOWANCE)
    }

    fn codec_estimate(&self) -> Option<u64> {
        self.codec_estimate
    }

    fn open(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, _lim: &Limits) -> bool {
        let Some(total) = self.row_bytes().checked_mul(u64::from(self.height)) else {
            self.fail_resource(s, issues, "decoded image size overflows 64-bit arithmetic");
            return false;
        };
        let res = match s.temp.as_mut() {
            Some(t) => t.reserve(&mut s.quota.borrow_mut(), total),
            None => Err(TempError::new(TempErrorKind::Unavailable, "no temporary file for this side")),
        };
        if let Err(e) = res {
            temp_failure(s, issues, &e);
            return false;
        }
        s.rep.decode_path += "+temp";
        let Some((buf, start)) = self.decode_all(s, issues) else { return false };
        let temp = s.temp.as_mut().expect("reserve() succeeded on this temp file");
        let rb = u64_to_usize(self.row_bytes());
        let rows = &buf[start..start + rb * self.height as usize];
        let mut written = Ok(());
        for row in rows.chunks_exact(rb) {
            written = temp.append(row);
            if written.is_err() {
                break;
            }
        }
        drop(buf);
        if let Err(e) = written.and_then(|()| temp.flush()) {
            temp_failure(s, issues, &e);
            return false;
        }
        self.row = vec![0; rb];
        self.decoded = true;
        true
    }

    fn read_row(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool {
        if s.failed || !self.decoded {
            return false;
        }
        let off = s.rows * self.row_bytes();
        let temp = s.temp.as_mut().expect("open() wrote the temp file");
        if let Err(e) = temp.read_at(off, &mut self.row) {
            temp_failure(s, issues, &e);
            return false;
        }
        for (dst, src) in s.canon.iter_mut().zip(self.row.as_chunks::<2>().0) {
            *dst = u16::from_ne_bytes(*src);
        }
        s.rows += 1;
        true
    }

    fn finish(&mut self, s: &mut SideState<'a>, _issues: &mut Vec<Issue>) {
        s.rep.coverage.rows_decoded = s.rows;
        if !s.failed && self.decoded && s.rows == u64::from(self.height) {
            s.rep.decode_status = "complete".into();
            s.rep.decode_status_reason.clear();
        }
    }

    fn memory_metrics(&self, m: &mut BTreeMap<String, u64>) {
        *m.entry("tool_row_buffers".into()).or_default() += self.row.capacity() as u64;
        *m.entry("coverage_state".into()).or_default() += u64::from(self.height);
        if let Some(c) = self.codec_estimate {
            *m.entry("codec_estimate".into()).or_default() += c;
        }
    }
}

/// C++ 版第 3 階段量測的 libjxl 用量：約 (4 × 所有聲道 + 0.5) B/px + 6 MiB；以 1 B/px 與 8 MiB 估。
/// jxl-rs 的實際用量不同（D-R18），預估值照 C++ 版，讓上限判定一致。
fn codec_estimate(w: u32, h: u32, all_channels: u32) -> Option<u64> {
    let px = u64::from(w).checked_mul(u64::from(h))?;
    px.checked_mul(4 * u64::from(all_channels) + 1)?.checked_add(8 << 20)
}

fn fill_metadata(fr: &mut FileReport, sc: &JxlScan, transfer: Option<u16>, intensity_target: f32) {
    let mut item = |name: &str, present: bool, status: &str, summary: String| {
        let status = if present { status } else { "absent" };
        fr.metadata.insert(name.into(), MetaItem { present, status: status.into(), summary });
    };
    match sc.exif_status {
        "over_limit" => item("exif", true, "over_limit", "exceeds metadata budget".into()),
        "compressed" => item("exif", true, "present", "brotli-compressed (brob), not decoded".into()),
        "present" => {
            let o = if sc.exif.is_empty() { -1 } else { exif_orientation(&sc.exif) };
            let summary = format!(
                "bytes={} sha256={}{}",
                sc.exif.len(),
                short_hash(&sha256_hex(&sc.exif)),
                if o > 0 {
                    format!(" orientation={o} (metadata only; the codestream orientation applies)")
                } else {
                    String::new()
                }
            );
            item("exif", true, if o < 0 { "parse_failed" } else { "present" }, summary);
        }
        _ => item("exif", false, "", String::new()),
    }
    match sc.xmp_bytes {
        Some(n) => item("xmp", true, "present", format!("bytes={n} sha256={}", short_hash(&sc.xmp_sha256))),
        None => item("xmp", false, "", String::new()),
    }
    let mut hdr = String::new();
    if let Some(t @ (16 | 18)) = transfer {
        hdr = format!("transfer={}", tf_name(t));
    }
    if intensity_target > 0.0 && intensity_target != 255.0 {
        if !hdr.is_empty() {
            hdr.push(' ');
        }
        #[expect(clippy::cast_possible_truncation, reason = "對照 C++ static_cast 截斷")]
        let target = intensity_target as i32;
        hdr += &format!("intensity_target={target}");
    }
    item("hdr_signals", !hdr.is_empty(), "present", hdr);
    let mut others = sc.other_boxes.join(",");
    if sc.jbrd {
        if !others.is_empty() {
            others.push(',');
        }
        others += "jbrd(JPEG reconstruction data)";
    }
    item("other_chunks", !others.is_empty(), "present", others);
}

pub fn prepare_jxl<'a>(
    s: &mut SideState<'a>,
    issues: &mut Vec<Issue>,
    lim: &Limits,
) -> Option<Box<dyn Decoder<'a> + 'a>> {
    let t0 = std::time::Instant::now();
    let sc = scan_jxl(s.input, lim);
    let scope = s.scope;
    for mut i in sc.issues.iter().cloned() {
        i.scope = scope.into();
        issues.push(i);
    }
    let fr = &mut s.rep;
    fr.container_status = sc.container_status.into();
    fr.metadata_status = sc.metadata_status.into();
    fr.chunks = sc.boxes.clone();
    fr.coverage.bytes_scanned = Count(sc.bytes_scanned);
    if sc.container_status == "corrupt" {
        s.scan_ms = crate::ms_since(t0);
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "structure_error".into();
        return None;
    }
    let h = read_header(s.input, &sc);
    s.scan_ms = crate::ms_since(t0);
    let enc = match &h.profile {
        Some(JxlColorProfile::Simple(c)) => Some(enum_color(c)),
        _ => None,
    };
    fill_metadata(&mut s.rep, &sc, enc.as_ref().map(|e| e.transfer), h.intensity_target);
    let fr = &mut s.rep;
    let (Some(head), Some(profile), "ok") = (h.header.as_ref(), h.profile.as_ref(), h.status) else {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = h.status.into();
        if h.status == "truncated" {
            issues.push(Issue::new(code::TRUNCATED, "error", scope, "codestream ends before all frames are present"));
        }
        let detail = if h.error.is_empty() { h.status.to_string() } else { format!("{} ({})", h.status, h.error) };
        issues.push(Issue::new(code::DECODE_FAILED, "error", scope, format!("jxl-rs header pass: {detail}")));
        return None;
    };
    fr.width = head.width;
    fr.height = head.height;
    s.width = head.width;
    s.height = head.height;
    fr.coverage.rows_total = u64::from(head.height);
    fr.orientation = Orientation { source: "jxl_codestream".into(), value: head.orientation };

    // ---- 色彩：列舉值（ICC 由 libjxl 0.12.0 產生，查表）或附帶的 ICC（規劃書 3.2、D12）
    match (profile, &enc) {
        (_, Some(e)) => {
            fr.color = EffectiveColor {
                kind: "jxl_enum".into(),
                key: e.key.clone(),
                description: format!(
                    "JXL colour enum (transfer {}, rendering intent {})",
                    tf_name(e.transfer),
                    e.intent
                ),
                signals: vec![format!("enum({};intent={})", &e.key[9..], e.intent)],
            };
            match e.lookup.and_then(synthesized_icc) {
                Some(icc) if icc.len() as u64 > lim.metadata_item_bytes() => {}
                Some(icc) => {
                    fr.icc.status = "synthesized".into(); // libjxl 從列舉值產生，不是內嵌資料
                    fr.icc.bytes = Some(Count(icc.len() as u64));
                    fr.icc.sha256 = sha256_hex(icc);
                    fr.icc.profile_name = format!("synthesized by libjxl {LIBJXL_VERSION} from the colour enum");
                }
                None => {
                    // 自訂白點、原色或 gamma：libjxl 會產生 ICC，這裡查不到（D-R18 的已知差異）
                    fr.icc.status = "unreadable".into();
                    fr.icc.profile_name = "custom colour enum: the ICC libjxl would synthesize is not tabulated".into();
                }
            }
        }
        (JxlColorProfile::Icc(icc), None) if icc.len() as u64 <= lim.metadata_item_bytes() => {
            let sha = sha256_hex(icc);
            fr.icc.status = "embedded".into();
            fr.icc.bytes = Some(Count(icc.len() as u64));
            fr.icc.sha256 = sha.clone();
            fr.color = EffectiveColor {
                kind: "icc".into(),
                key: format!("icc:{sha}"),
                description: format!("embedded ICC {} bytes", icc.len()),
                signals: vec![format!("ICC({})", short_hash(&sha))],
            };
        }
        (JxlColorProfile::Icc(_), None) => {
            fr.icc.status = "over_budget".into();
            fr.color = EffectiveColor {
                kind: "icc_unreadable".into(),
                key: String::new(),
                description: "ICC profile not available within the metadata budget".into(),
                signals: Vec::new(),
            };
        }
        (JxlColorProfile::Simple(_), None) => unreachable!("enum colour is always converted"),
    }

    // ---- 支援範圍（同 C++ 版）
    if head.xyb_encoded {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "xyb".into();
        issues.push(Issue::new(
            code::XYB_NOT_NATIVE,
            "error",
            scope,
            "XYB (uses_original_profile=false): the decoder cannot return native samples, so samples are not comparable",
        ));
        return None;
    }
    if head.have_animation {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "animation".into();
        issues.push(Issue::new(code::ANIMATION_NOT_SUPPORTED, "error", scope, "animated JPEG XL is not supported"));
        return None;
    }
    let alpha = head.alpha();
    let color_ch = head.color_channels();
    let why = if head.depth.exponent_bits != 0 {
        Some("floating-point samples".to_string())
    } else if head.depth.bits < 1 || head.depth.bits > 16 {
        Some(format!("{}-bit samples", head.depth.bits))
    } else if color_ch != 1 && color_ch != 3 {
        Some(format!("{color_ch} colour channels"))
    } else if alpha.is_some_and(|a| a.depth.bits != head.depth.bits || a.depth.exponent_bits != 0) {
        Some("alpha depth differs from colour depth".to_string())
    } else {
        None
    };
    if let Some(why) = why {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "sample_model".into();
        issues.push(Issue::new(
            code::UNSUPPORTED_SAMPLE_MODEL,
            "error",
            scope,
            format!("JPEG XL {why} is not supported"),
        ));
        return None;
    }
    let non_alpha = head.extra.len() - usize::from(alpha.is_some());
    if non_alpha > 0 {
        issues.push(Issue::new(
            code::EXTRA_CHANNEL_NOT_SUPPORTED,
            "warning",
            scope,
            format!("{non_alpha} extra channel(s) besides alpha are not compared; no strict match"),
        ));
    }
    let bits = head.depth.bits;
    let ch = color_ch + u32::from(alpha.is_some());
    let model = match (color_ch, alpha.is_some()) {
        (3, true) => "rgba",
        (3, false) => "rgb",
        (_, true) => "gray_alpha",
        (_, false) => "gray",
    };
    s.layout = RowLayout::new(model, ch, bits);
    let sm = &mut fr.sample_model;
    sm.model = model.into();
    sm.container_bits = bits;
    sm.channel_bits = vec![bits; ch as usize];
    sm.compare_type = if bits > 8 { "uint16" } else { "uint8" }.into();
    sm.alpha = match alpha {
        None => "none",
        Some(a) if a.alpha_associated => "associated",
        Some(_) => "straight",
    }
    .into();
    fr.decode_path = "jxl-rs".into();
    Some(Box::new(JxlImageDecoder {
        input: s.input,
        width: head.width,
        height: head.height,
        channels: ch,
        bits,
        bare: !sc.container,
        codec_estimate: codec_estimate(head.width, head.height, h.all_channels),
        row: Vec::new(),
        decoded: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jxl::api::{ExtraChannel, JxlBitDepth};

    fn orientation_value(o: jxl::api::Orientation) -> u32 {
        use jxl::api::Orientation as O;
        match o {
            O::Identity => 1,
            O::FlipHorizontal => 2,
            O::Rotate180 => 3,
            O::FlipVertical => 4,
            O::Transpose => 5,
            O::Rotate90Cw => 6,
            O::AntiTranspose => 7,
            O::Rotate90Ccw => 8,
        }
    }

    /// jxl-rs 的基本資訊：(寬, 高, 位元深度, extra channel 數, 方向, alpha 數)。
    fn decoder_basic_info(input: &Input<'_>) -> Option<(u32, u32, u32, usize, u32, usize)> {
        let mut d = JxlDecoder::new(options(true));
        let mut fi = FileInput { input, pos: 0 };
        loop {
            match step(&mut d, &mut fi, None).ok()? {
                Event::BasicInfo => break,
                Event::NeedMoreInput { .. } | Event::Complete => return None,
                _ => {}
            }
        }
        let bi = d.basic_info()?;
        let bits = match bi.bit_depth {
            JxlBitDepth::Int { bits_per_sample } | JxlBitDepth::Float { bits_per_sample, .. } => bits_per_sample,
        };
        let alpha = bi.extra_channels.iter().filter(|e| matches!(e.ec_type, ExtraChannel::Alpha)).count();
        Some((
            u32::try_from(bi.size.0).ok()?,
            u32::try_from(bi.size.1).ok()?,
            bits,
            bi.extra_channels.len(),
            orientation_value(bi.orientation),
            alpha,
        ))
    }

    #[test]
    fn own_header_parser_agrees_with_jxl_rs_on_every_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
        let mut checked = 0;
        for e in std::fs::read_dir(&dir).expect("fixtures") {
            let p = e.expect("entry").path();
            if p.extension().is_none_or(|x| x != "jxl") {
                continue;
            }
            let f = std::fs::File::open(&p).expect("open");
            let input = Input::new(&f).expect("input");
            let sc = scan_jxl(&input, &Limits::default());
            if sc.container_status == "corrupt" {
                continue;
            }
            let Some(want) = decoder_basic_info(&input) else { continue };
            let head = codestream_head(&input, &sc).expect("codestream");
            let h = jxl_header::parse(&head).unwrap_or_else(|_| panic!("{}", p.display()));
            let alpha = h.extra.iter().filter(|e| e.kind == jxl_header::EC_ALPHA).count();
            let got = (h.width, h.height, h.depth.bits, h.extra.len(), h.orientation, alpha);
            assert_eq!(got, want, "{}", p.display());
            checked += 1;
        }
        assert!(checked >= 30, "only {checked} fixtures checked");
    }
}
