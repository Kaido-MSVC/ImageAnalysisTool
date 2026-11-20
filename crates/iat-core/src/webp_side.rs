//! WebP 前端（移植 C++ 版 `src/engine/webp_side.cpp` 與 `whole_image.*`）：自己的 RIFF 掃描 +
//! image-webp 只解 still VP8L。
//!
//! 解碼一律得到直通（非預乘）RGBA，所以 alpha 為 0 底下的 RGB 會保留；VP8L 的 alpha_is_used
//! 只是提示，比較時以 RGB 或 RGBA 由實際解出的 alpha 決定。整張影像解碼後依列寫進這一側的
//! delete-on-close 暫存檔，再由 read_row() 依序交給引擎，兩側的整張影像不會同時留在記憶體裡。
//! 解碼器的輸入只含 VP8L chunk 的原始位元組，見 [`vp8l_decoder_input`] 與 docs/decisions.md D-R15。

use std::collections::BTreeMap;
use std::io::Cursor;

use crate::engine::{Decoder, RowLayout, SideState, temp_failure};
use crate::input::Input;
use crate::limits::Limits;
use crate::png_scan::exif_orientation;
use crate::report::{Count, DimensionPolicy, EffectiveColor, FileReport, Issue, MetaItem, Orientation, code};
use crate::temp::{TempError, TempErrorKind};
use crate::text::{sha256_hex, short_hash};
use crate::webp_scan::{WebpScan, scan_webp, vp8l_decoder_input};

pub const DECODER_NAME: &str = "image-webp";
pub const DECODER_VERSION: &str = "0.2.4";

/// VP8L：14 位元欄位 + 1
const WEBP_SPEC_MAX: u32 = 16384;
/// IMB-C 分析相容政策（規劃書第 2 節）
const WEBP_POLICY_MAX: u32 = 16383;
/// 部分列的區間清單：與影像大小無關的固定額度（同 C++ 版 kCoverageAllowance）
const COVERAGE_ALLOWANCE: u64 = 1 << 20;

/// EXIF chunk 是原始的 TIFF 資料；有些程式會在前面加 "Exif\0\0"。
fn exif_tiff(e: &[u8]) -> (&[u8], bool) {
    match e.strip_prefix(b"Exif\0\0") {
        Some(rest) => (rest, true),
        None => (e, false),
    }
}

fn error_text(e: &image_webp::DecodingError) -> String {
    format!("{e:?}")
}

struct WebpDecoder<'a> {
    input: &'a Input<'a>,
    payload: (u64, u64),
    alpha_hint: bool,
    width: u32,
    height: u32,
    /// 暫存檔裡每個像素的聲道數（一律 4）與交給引擎的聲道數（3 或 4）
    out_channels: u32,
    codec_estimate: Option<u64>,
    row: Vec<u8>,
    decoded: bool,
}

impl<'a> WebpDecoder<'a> {
    fn row_bytes(&self) -> u64 {
        u64::from(self.width) * 4
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

    /// 解出整張 RGBA；失敗時已記錄原因。
    fn decode_all(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>) -> Option<Vec<u8>> {
        let (off, len) = self.payload;
        let mut payload = Vec::new();
        if usize::try_from(len).ok().and_then(|n| payload.try_reserve_exact(n).ok()).is_none() {
            self.fail_resource(s, issues, "buffers for the WebP file and its RGBA output could not be allocated");
            return None;
        }
        payload.resize(len as usize, 0);
        if !matches!(self.input.read_at(off, &mut payload), Ok(n) if n as u64 == len) {
            self.fail_decode(s, issues, "io_error", "reading the WebP file failed".into());
            return None;
        }
        let rgba_len = self.row_bytes() * u64::from(self.height);
        let mut rgba = Vec::new();
        if usize::try_from(rgba_len).ok().and_then(|n| rgba.try_reserve_exact(n).ok()).is_none() {
            self.fail_resource(s, issues, "buffers for the WebP file and its RGBA output could not be allocated");
            return None;
        }
        rgba.resize(rgba_len as usize, 0);
        let res = image_webp::WebPDecoder::new(Cursor::new(vp8l_decoder_input(payload))).and_then(|mut d| {
            if d.dimensions() != (self.width, self.height) || !d.has_alpha() {
                return Err(image_webp::DecodingError::InconsistentImageSizes);
            }
            d.read_image(&mut rgba)
        });
        if let Err(e) = res {
            let t = error_text(&e);
            s.rep.decoder_errors.push(format!("image-webp: {t}"));
            if matches!(e, image_webp::DecodingError::MemoryLimitExceeded) {
                self.fail_resource(s, issues, &format!("image-webp ran out of memory ({t})"));
            } else {
                let truncated = matches!(&e, image_webp::DecodingError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof);
                self.fail_decode(
                    s,
                    issues,
                    if truncated { "truncated" } else { "decode_error" },
                    format!("image-webp: {t}"),
                );
            }
            return None;
        }
        Some(rgba)
    }
}

impl<'a> Decoder<'a> for WebpDecoder<'a> {
    fn buffer_estimate(&self) -> Option<u64> {
        // 正規列 + 暫存列（讀回）+ 轉換緩衝（最多一列）+ 每列一個狀態位元組 + 部分列額度（同 C++ 版）
        let canon = u64::from(self.width).checked_mul(4)?.checked_mul(2)?;
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
        let Some(rgba) = self.decode_all(s, issues) else { return false };

        // alpha 決定比較的樣本模型；標頭的提示只拿來核對。
        let not_opaque = rgba.as_chunks::<4>().0.iter().filter(|p| p[3] != 255).count();
        if !self.alpha_hint && not_opaque > 0 {
            s.rep.container_status = "warning".into();
            issues.push(Issue::new(
                code::ALPHA_HINT_MISMATCH,
                "warning",
                s.scope,
                format!("VP8L alpha_is_used is clear but {not_opaque} pixels have alpha < 255; compared as RGBA"),
            ));
        }
        if !self.alpha_hint && not_opaque == 0 {
            self.out_channels = 3;
            s.layout = RowLayout::new("rgb", 3, 8);
            s.rep.sample_model.model = "rgb".into();
            s.rep.sample_model.channel_bits = vec![8; 3];
            s.rep.sample_model.alpha = "none".into();
        }
        let temp = s.temp.as_mut().expect("reserve() succeeded on this temp file");
        let rb = self.row_bytes() as usize;
        let mut written = Ok(());
        for row in rgba.chunks_exact(rb) {
            written = temp.append(row);
            if written.is_err() {
                break;
            }
        }
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
        let oc = self.out_channels as usize;
        for (dst, src) in s.canon.chunks_exact_mut(oc).zip(self.row.as_chunks::<4>().0) {
            for c in 0..oc {
                dst[c] = u16::from(src[c]);
            }
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

/// C++ 版第 3 階段量測的 libwebp 用量：8.03–8.07 B/px（RGBA 輸出 + VP8L 內部 ARGB），加上整個檔案
/// 與固定額度，再乘 1.15。image-webp 的實際用量不同（D-R15），預估值照 C++ 版，讓上限判定一致。
fn codec_estimate(w: u32, h: u32, file_size: u64) -> Option<u64> {
    let px = u64::from(w).checked_mul(u64::from(h))?;
    let base = px.checked_mul(8)?.checked_add(px / 8)?.checked_add(file_size)?.checked_add(4 << 20)?;
    Some(base.checked_mul(115)? / 100)
}

fn fill_metadata(fr: &mut FileReport, sc: &WebpScan) {
    let mut item = |name: &str, present: bool, status: &str, summary: String| {
        let status = if present { status } else { "absent" };
        fr.metadata.insert(name.into(), MetaItem { present, status: status.into(), summary });
    };
    if sc.exif_status == "over_limit" {
        item("exif", true, "over_limit", "exceeds metadata budget".into());
    } else if sc.exif_status == "present" {
        let (tiff, prefix) = exif_tiff(&sc.exif);
        let o = exif_orientation(tiff);
        let summary = format!(
            "bytes={} sha256={}{}{}",
            sc.exif.len(),
            short_hash(&sha256_hex(&sc.exif)),
            if prefix { " exif_prefix" } else { "" },
            if o > 0 { format!(" orientation={o}") } else { String::new() }
        );
        item("exif", true, if o < 0 { "parse_failed" } else { "present" }, summary);
    } else {
        item("exif", false, "", String::new());
    }
    match sc.xmp_bytes {
        Some(n) => item("xmp", true, "present", format!("bytes={n} sha256={}", short_hash(&sc.xmp_sha256))),
        None => item("xmp", false, "", String::new()),
    }
    let others = sc.unknown_chunks.join(",");
    item("other_chunks", !others.is_empty(), "present", others);
}

pub fn prepare_webp<'a>(
    s: &mut SideState<'a>,
    issues: &mut Vec<Issue>,
    lim: &Limits,
) -> Option<Box<dyn Decoder<'a> + 'a>> {
    let t0 = std::time::Instant::now();
    let sc = scan_webp(s.input, lim);
    s.scan_ms = crate::ms_since(t0);
    let scope = s.scope;
    for mut i in sc.issues.iter().cloned() {
        i.scope = scope.into();
        issues.push(i);
    }
    let fr = &mut s.rep;
    fr.container_status = sc.container_status.into();
    fr.metadata_status = sc.metadata_status.into();
    fr.chunks = sc.chunks.clone();
    fr.coverage.bytes_scanned = Count(sc.bytes_scanned);
    fr.coverage.trailing_bytes = sc.trailing_bytes.map(Count);
    fill_metadata(fr, &sc);
    if sc.icc_status == "embedded" {
        let sha = sha256_hex(&sc.icc);
        fr.icc.status = "embedded".into();
        fr.icc.bytes = Some(Count(sc.icc.len() as u64));
        fr.icc.sha256 = sha.clone();
        fr.color = EffectiveColor {
            kind: "icc".into(),
            key: format!("icc:{sha}"),
            description: format!("embedded ICC {} bytes (ICCP)", sc.icc.len()),
            signals: vec![format!("ICCP({})", short_hash(&sha))],
        };
    } else if sc.icc_status == "over_budget" {
        fr.icc.status = "over_budget".into();
        fr.color = EffectiveColor {
            kind: "icc_unreadable".into(),
            key: String::new(),
            description: "ICCP exceeds the metadata budget".into(),
            signals: vec!["ICCP(over budget)".into()],
        };
    } else {
        // D12：WebP 容器的預設值記為規格預設，不是檔案的宣告。
        fr.color = EffectiveColor {
            kind: "webp_default_srgb".into(),
            key: "webp_default_srgb".into(),
            description:
                "no ICCP: WebP container default (sRGB assumed by the specification, not declared in the file)".into(),
            signals: Vec::new(),
        };
    }
    if sc.container_status != "corrupt" && sc.animated {
        // 影格在 ANMF 裡，沒有最上層的影像尺寸：回報 VP8X 畫布。
        fr.width = sc.canvas_width;
        fr.height = sc.canvas_height;
        s.width = sc.canvas_width;
        s.height = sc.canvas_height;
        let fr = &mut s.rep;
        fr.orientation = Orientation { source: "default".into(), value: 1 };
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "animation".into();
        issues.push(Issue::new(
            code::ANIMATION_NOT_SUPPORTED,
            "error",
            scope,
            "animated WebP (ANIM/ANMF) is not supported",
        ));
        return None;
    }
    if sc.container_status == "corrupt" || sc.width == 0 {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "structure_error".into();
        return None;
    }
    fr.width = sc.width;
    fr.height = sc.height;
    s.width = sc.width;
    s.height = sc.height;
    let fr = &mut s.rep;
    fr.coverage.rows_total = u64::from(sc.height);
    let (w, h) = (sc.width, sc.height);
    let policy = DimensionPolicy {
        spec_max: WEBP_SPEC_MAX,
        policy_max: WEBP_POLICY_MAX,
        within_spec: w <= WEBP_SPEC_MAX && h <= WEBP_SPEC_MAX,
        within_policy: w <= WEBP_POLICY_MAX && h <= WEBP_POLICY_MAX,
    };
    let within_policy = policy.within_policy;
    fr.dimension_policy = Some(policy);
    // 方向：WebP 沒有方向欄位；有 Exif 時和 PNG 的 eXIf 一樣使用。
    if sc.exif_status == "over_limit" {
        // 沒讀到就是不知道：不假設「不旋轉」（D21）。
        fr.orientation = Orientation { source: "unknown".into(), value: 0 };
        issues.push(Issue::new(
            code::ORIENTATION_UNKNOWN,
            "warning",
            scope,
            "EXIF exceeds the metadata budget; orientation not read",
        ));
    } else if sc.exif_status == "present" {
        let o = exif_orientation(exif_tiff(&sc.exif).0);
        if o < 0 {
            fr.orientation = Orientation { source: "unparseable".into(), value: 0 };
            issues.push(Issue::new(
                code::ORIENTATION_UNPARSEABLE,
                "warning",
                scope,
                "EXIF orientation could not be parsed",
            ));
        } else if o == 0 {
            fr.orientation = Orientation { source: "default".into(), value: 1 };
        } else {
            fr.orientation = Orientation { source: "exif".into(), value: o as u32 };
        }
    } else {
        fr.orientation = Orientation { source: "default".into(), value: 1 };
    }

    if sc.image_chunk != "VP8L" {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "lossy".into();
        issues.push(Issue::new(
            code::LOSSY_NOT_SUPPORTED,
            "error",
            scope,
            format!(
                "lossy WebP (VP8{}) is outside the strict scope (still VP8L only)",
                if sc.has_alph { " + ALPH" } else { "" }
            ),
        ));
        return None;
    }
    if !within_policy {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "dimension_policy".into();
        issues.push(Issue::new(
            code::UNSUPPORTED_DIMENSION,
            "error",
            scope,
            format!(
                "{w}x{h} exceeds the tool policy of {WEBP_POLICY_MAX} per side (the VP8L specification allows up to {WEBP_SPEC_MAX}); not a file error"
            ),
        ));
        return None;
    }
    s.layout = RowLayout::new("rgba", 4, 8);
    let fr = &mut s.rep;
    let sm = &mut fr.sample_model;
    sm.model = "rgba".into();
    sm.container_bits = 8;
    sm.channel_bits = vec![8; 4];
    sm.compare_type = "uint8".into();
    sm.alpha = "straight".into();
    fr.decode_path = "image-webp-vp8l-rgba".into();
    let payload = sc.vp8l_payload.expect("a VP8L image chunk was found");
    Some(Box::new(WebpDecoder {
        input: s.input,
        payload,
        alpha_hint: sc.vp8l_alpha_hint,
        width: w,
        height: h,
        out_channels: 4,
        codec_estimate: codec_estimate(w, h, s.input.size()),
        row: Vec::new(),
        decoded: false,
    }))
}
