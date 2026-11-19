//! 比對引擎：格式分派、預算，以及色彩／metadata／strict 判定。
//! 對照 C++ 版 `src/engine/analyze.cpp` 的 `analyze()`。
//!
//! 階段 0 還沒有任何解碼器：辨識出的格式一律回報「不支援」，其餘判定流程照 C++ 版移植，
//! 之後的階段只要在 [`prepare_side`] 接上各格式即可。

use std::fs::File;
use std::os::windows::fs::FileExt;
use std::time::Instant;

use crate::limits::Limits;
use crate::report::{Count, FileReport, MetaItem, MetadataField, PairStatus, Report, code};
use crate::sniff::sniff_format;
use crate::{Mode, ms_since};

/// 未知格式訊息中列出的支援格式。沿用 C++ 版 1.0.0 的文字（最終目標的支援範圍），
/// 階段 0 實際上還不能解碼，見 docs/decisions.md D-R05。
const SUPPORTED_FORMATS: &str = "PNG, TIFF/BigTIFF, WebP, JPEG XL, PSD/PSB, AVIF";

const METADATA_FIELDS: [&str; 9] =
    ["exif", "xmp", "text", "resolution", "time", "hdr_signals", "sbit", "ornt_private", "other_chunks"];

pub struct AnalysisInput<'a> {
    pub mode: Mode,
    pub original: &'a File,
    pub output: Option<&'a File>,
    pub limits: Limits,
}

struct Side<'a> {
    scope: &'static str,
    file: &'a File,
    size: u64,
    bytes_read: u64,
    scan_ms: f64,
    /// 解碼器已就緒（階段 0 永遠為 false）
    decodable: bool,
    /// 比較用的樣本模型與位深（解碼器填入；空字串／0 表示未知）
    layout_model: String,
    layout_bits: u32,
}

impl<'a> Side<'a> {
    fn new(scope: &'static str, file: &'a File) -> Self {
        let size = file.metadata().map_or(0, |m| m.len());
        Self {
            scope,
            file,
            size,
            bytes_read: 0,
            scan_ms: 0.0,
            decodable: false,
            layout_model: String::new(),
            layout_bits: 0,
        }
    }

    fn read_head(&mut self) -> Vec<u8> {
        let mut b = [0u8; 64];
        let mut n = 0;
        while n < b.len() {
            match self.file.seek_read(&mut b[n..], n as u64) {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
        }
        self.bytes_read += n as u64;
        b[..n].to_vec()
    }
}

fn prepare_side(s: &mut Side<'_>, fr: &mut FileReport, issues: &mut Vec<crate::report::Issue>) {
    let t0 = Instant::now();
    fr.size_bytes = Count(s.size);
    fr.format = sniff_format(&s.read_head()).to_string();
    // 階段 1 起在這裡依格式接上掃描器與解碼器。
    fr.container_status = "unverified".into();
    fr.decode_status = "unsupported".into();
    let unknown = fr.format == "unknown";
    fr.decode_status_reason = if unknown { "unknown_format" } else { "format_not_supported_in_this_build" }.into();
    fr.metadata_status = "unverified".into();
    issues.push(crate::report::Issue::new(
        if unknown { code::UNKNOWN_FORMAT } else { code::UNSUPPORTED_SAMPLE_MODEL },
        "error",
        s.scope,
        format!("format '{}' is not supported by this build ({SUPPORTED_FORMATS})", fr.format),
    ));
    s.decodable = false;
    s.scan_ms = ms_since(t0);
}

fn color_base(model: &str) -> &'static str {
    if model.starts_with("gray") { "gray" } else { "rgb" }
}

fn has_alpha(model: &str) -> bool {
    model == "gray_alpha" || model == "rgba"
}

fn short_hash(h: &str) -> &str {
    &h[..h.len().min(16)]
}

fn compare_item(name: &str, a: Option<&MetaItem>, b: Option<&MetaItem>) -> MetadataField {
    let mut f = MetadataField {
        name: name.into(),
        status: String::new(),
        original: a.map(|x| x.summary.clone()).unwrap_or_default(),
        output: b.map(|x| x.summary.clone()).unwrap_or_default(),
    };
    let pa = a.filter(|x| x.present);
    let pb = b.filter(|x| x.present);
    f.status = match (pa, pb) {
        (None, None) => "both_absent",
        _ if pa.is_some_and(|x| x.status == "parse_failed") || pb.is_some_and(|x| x.status == "parse_failed") => {
            "parse_failed"
        }
        _ if pa.is_some_and(|x| x.status == "over_limit") || pb.is_some_and(|x| x.status == "over_limit") => {
            "over_limit"
        }
        (Some(_), None) => "removed",
        (None, Some(_)) => "added",
        (Some(x), Some(y)) => {
            if x.summary == y.summary {
                "same"
            } else {
                "changed"
            }
        }
    }
    .into();
    f
}

/// 一次分析，產生 C++ 版 worker 那一半的報告（身分欄位與執行層級資料由 [`crate::run`] 補上）。
pub fn analyze(input: &AnalysisInput<'_>) -> Report {
    let t_all = Instant::now();
    let compare = input.mode == Mode::Compare;
    let mut r = Report { mode: input.mode.as_str().into(), ..Report::default() };
    r.original.name = "original".into();
    if compare {
        r.output = Some(FileReport::default());
    }

    let mut a = Side::new("original", input.original);
    let mut b = input.output.map(|f| Side::new("output", f));
    prepare_side(&mut a, &mut r.original, &mut r.issues);
    if let (Some(b), Some(out)) = (b.as_mut(), r.output.as_mut()) {
        prepare_side(b, out, &mut r.issues);
    }

    // 預算檢查與整圖型解碼器的預估：階段 0 沒有可解碼的一側，need 恆為 0。
    let need: u64 = 0;

    let t_dec = Instant::now();
    // 解碼與逐列比較：階段 1 加入。
    let dec_ms = ms_since(t_dec);

    if let (Some(b), Some(out)) = (b.as_ref(), r.output.as_ref()) {
        let orig = &r.original;
        let mut issues = Vec::new();
        // ---- comparability
        let mut not_cmp_reason = "";
        if orig.width != out.width || orig.height != out.height {
            not_cmp_reason = code::GEOMETRY_DIFFERS;
        } else if orig.orientation.source == "unparseable" || out.orientation.source == "unparseable" {
            not_cmp_reason = code::ORIENTATION_UNPARSEABLE;
        } else if orig.orientation.source == "unknown" || out.orientation.source == "unknown" {
            not_cmp_reason = code::ORIENTATION_UNKNOWN;
        } else if orig.orientation.value != out.orientation.value {
            not_cmp_reason = code::ORIENTATION_DIFFERS;
        } else if !a.layout_model.is_empty()
            && !b.layout_model.is_empty()
            && color_base(&a.layout_model) != color_base(&b.layout_model)
        {
            not_cmp_reason = code::SAMPLE_MODEL_CHANGED;
        } else if a.layout_bits != 0 && b.layout_bits != 0 && a.layout_bits != b.layout_bits {
            not_cmp_reason = code::BIT_DEPTH_CHANGED;
        }
        let comparable = a.decodable && b.decodable && not_cmp_reason.is_empty();
        if orig.interlace >= 0 && out.interlace >= 0 && orig.interlace != out.interlace {
            issues.push(crate::report::Issue::new(
                code::INTERLACE_DIFFERS,
                "info",
                "pair",
                format!(
                    "interlace {} vs {} (storage order only; samples compared in image order)",
                    orig.interlace, out.interlace
                ),
            ));
        }

        // ---- sample comparison result
        let complete = orig.decode_status == "complete" && out.decode_status == "complete";
        if !not_cmp_reason.is_empty() {
            r.samples.status = "not_comparable".into();
            r.samples.reason = not_cmp_reason.into();
        } else if !a.decodable || !b.decodable || !complete {
            r.samples.status = if comparable { "incomplete" } else { "not_compared" }.into();
            r.samples.reason = "decode_incomplete".into();
        }
        // 雙方完整解碼後的統計（equal／differs）：階段 1 加入。

        if (orig.sample_model.alpha == "matted_white") != (out.sample_model.alpha == "matted_white")
            && has_alpha(&a.layout_model)
            && has_alpha(&b.layout_model)
        {
            let other = if orig.sample_model.alpha == "matted_white" {
                &out.sample_model.alpha
            } else {
                &orig.sample_model.alpha
            };
            issues.push(crate::report::Issue::new(
                code::ALPHA_SEMANTICS_DIFFER,
                "warning",
                "pair",
                format!("PSD merged colour is blended with white where alpha < max; the other file's alpha is {other}"),
            ));
        }
        // AVIF 的像素驗證判定：階段 6 加入。

        // ---- geometry
        if orig.width != 0 && out.width != 0 {
            let same = orig.width == out.width
                && orig.height == out.height
                && orig.orientation.value == out.orientation.value
                && !matches!(orig.orientation.source.as_str(), "unparseable" | "unknown")
                && !matches!(out.orientation.source.as_str(), "unparseable" | "unknown");
            let orient_unknown =
                not_cmp_reason == code::ORIENTATION_UNPARSEABLE || not_cmp_reason == code::ORIENTATION_UNKNOWN;
            r.geometry.status = if same {
                "equal"
            } else if orient_unknown {
                "unknown"
            } else {
                "differs"
            }
            .into();
            r.geometry.basis = format!(
                "stored size {}x{} vs {}x{}; orientation {} ({}) vs {} ({})",
                orig.width,
                orig.height,
                out.width,
                out.height,
                orig.orientation.value,
                orig.orientation.source,
                out.orientation.value,
                out.orientation.source
            );
        } else {
            r.geometry.status = "unknown".into();
        }
        if [code::GEOMETRY_DIFFERS, code::ORIENTATION_DIFFERS, code::BIT_DEPTH_CHANGED, code::SAMPLE_MODEL_CHANGED]
            .contains(&not_cmp_reason)
        {
            let msg = if not_cmp_reason == code::BIT_DEPTH_CHANGED {
                format!("bit depth {} vs {}", a.layout_bits, b.layout_bits)
            } else if not_cmp_reason == code::SAMPLE_MODEL_CHANGED {
                format!("{} vs {}", a.layout_model, b.layout_model)
            } else {
                r.geometry.basis.clone()
            };
            issues.push(crate::report::Issue::new(not_cmp_reason, "error", "pair", msg));
        }

        // ---- colour (D4, D12) and ICC (D2)
        let (ca, cb) = (&orig.color, &out.color);
        let mut pair_issue = |c: &str, sev: &str, m: String| issues.push(crate::report::Issue::new(c, sev, "pair", m));
        // JXL 色彩 enum 的等價規則（jxl_equivalence）：階段 5 加入。
        if ca.kind.is_empty() || cb.kind.is_empty() {
            r.color = PairStatus::new("unknown", "colour information not available (decoder did not start)");
        } else if (ca.kind == "icc" && cb.kind == "webp_default_srgb")
            || (ca.kind == "webp_default_srgb" && cb.kind == "icc")
        {
            r.color = PairStatus::new(
                "unverified",
                "embedded ICC in one file, WebP specification default (sRGB, not declared) in the other",
            );
            let c = if ca.kind == "icc" { code::ICC_MISSING_ON_OUTPUT } else { code::ICC_MISSING_ON_ORIGINAL };
            pair_issue(c, "warning", "ICC profile not carried over to the WebP file".into());
            pair_issue(code::COLOR_UNVERIFIED, "warning", r.color.basis.clone());
        } else if ca.kind == "none" && cb.kind == "none" {
            r.color = PairStatus::new("unknown", "neither file declares colour information");
            pair_issue(code::COLOR_UNKNOWN, "warning", r.color.basis.clone());
        } else if !ca.key.is_empty() && ca.kind == cb.kind && ca.key == cb.key {
            r.color = PairStatus::new("equal", ca.description.clone());
            if ca.signals != cb.signals {
                pair_issue(
                    code::LOW_PRIORITY_COLOR_SIGNAL_DIFFERS,
                    "warning",
                    "effective colour is equal, lower-priority signals differ".into(),
                );
            }
        } else if ca.kind == "icc" && cb.kind == "icc" {
            r.color = PairStatus::new(
                "differs",
                format!(
                    "embedded ICC profiles differ (sha256 {} vs {})",
                    short_hash(&orig.icc.sha256),
                    short_hash(&out.icc.sha256)
                ),
            );
            pair_issue(code::ICC_DIFFERS, "warning", format!("{}; appearance change is not proven", r.color.basis));
        } else if ca.kind == "icc" && cb.kind == "none" {
            r.color = PairStatus::new("differs", "ICC profile present in original, absent in output");
            pair_issue(code::ICC_MISSING_ON_OUTPUT, "warning", r.color.basis.clone());
        } else if ca.kind == "none" && cb.kind == "icc" {
            r.color = PairStatus::new("differs", "ICC profile absent in original, present in output");
            pair_issue(code::ICC_MISSING_ON_ORIGINAL, "warning", r.color.basis.clone());
        } else {
            r.color = PairStatus::new("unverified", format!("{} vs {} (no reliable equivalence)", ca.kind, cb.kind));
            pair_issue(code::COLOR_UNVERIFIED, "warning", r.color.basis.clone());
        }
        let (ia, ib) = (&orig.icc, &out.icc);
        let (sa, sb) = (ia.status == "synthesized", ib.status == "synthesized");
        let same_hash = if ia.sha256 == ib.sha256 { "identical" } else { "differs" };
        r.icc = match (ia.status.as_str(), ib.status.as_str()) {
            ("absent", "absent") => PairStatus::new("both_absent", ""),
            ("embedded", "embedded") => PairStatus::new(
                if ia.sha256 == ib.sha256 { "equal" } else { "differs" },
                "sha256 of decompressed profile bytes",
            ),
            (_, "embedded") if sa => embedded_vs_synth(same_hash),
            ("embedded", _) if sb => embedded_vs_synth(same_hash),
            _ if sa && sb => PairStatus::new("both_synthesized", format!("sha256 {same_hash}")),
            (_, "absent") if sa => PairStatus::new("synthesized_vs_absent", ""),
            ("absent", _) if sb => PairStatus::new("synthesized_vs_absent", ""),
            ("embedded", "absent") => PairStatus::new("missing_on_output", ""),
            ("absent", "embedded") => PairStatus::new("missing_on_original", ""),
            (x, y) => PairStatus::new("unreadable", format!("{x} vs {y}")),
        };

        // WebP 政策：原檔超過 16383 時，無法以原尺寸交付成 WebP。
        if out.format == "webp" && (orig.width > 16383 || orig.height > 16383) {
            issues.push(crate::report::Issue::new(
                code::WEBP_POLICY_CANNOT_HOLD_ORIGINAL,
                "info",
                "pair",
                format!(
                    "original is {}x{}; under the 16383 WebP policy it cannot be delivered as WebP at its original size",
                    orig.width, orig.height
                ),
            ));
        }

        // ---- metadata
        let mut meta_changed = false;
        for f in METADATA_FIELDS {
            let mf = compare_item(f, orig.metadata.get(f), out.metadata.get(f));
            meta_changed |= mf.status != "same" && mf.status != "both_absent";
            r.metadata.fields.push(mf);
        }
        if meta_changed {
            issues.push(crate::report::Issue::new(
                code::METADATA_CHANGED,
                "info",
                "pair",
                "non-image metadata differs (reported separately, does not affect samples)",
            ));
        }

        // ---- size
        r.size.original_bytes = Count(a.size);
        r.size.output_bytes = Some(Count(b.size));
        r.size.comparable = complete;
        if a.size > 0 {
            let ratio = b.size as f64 / a.size as f64;
            r.size.output_ratio = Some(ratio);
            r.size.saving_ratio = Some(1.0 - ratio);
        }
        if b.size > 0 {
            r.size.compression_factor = Some(a.size as f64 / b.size as f64);
        }
        if !complete {
            r.size.note = "at least one file did not decode completely; not a valid conversion size comparison".into();
        }
        r.issues.extend(issues);
        strict_compare(&mut r, not_cmp_reason);
    } else {
        r.size.original_bytes = Count(a.size);
        r.size.comparable = false;
        r.strict.result = "not_evaluated".into();
        r.strict.reasons.push("inspect_mode".into());
    }

    // ---- metrics（引擎側）
    let m = &mut r.metrics;
    m.time_ms.insert("scan_original".into(), a.scan_ms);
    if let Some(b) = &b {
        m.time_ms.insert("scan_output".into(), b.scan_ms);
    }
    m.time_ms.insert(if compare { "decode_and_compare" } else { "decode" }.into(), dec_ms);
    m.time_ms.insert("analyze_total".into(), ms_since(t_all));
    m.timing_note = if compare {
        "both files are decoded row by row in lockstep; decode_and_compare covers both decoders and the comparison together"
    } else {
        "single file decode"
    }
    .into();
    for k in ["tool_row_buffers", "temp_reserved_peak", "temp_written", "temp_read"] {
        m.memory_bytes.insert(k.into(), Some(Count(0)));
    }
    m.memory_bytes.insert("row_budget_estimate".into(), Some(Count(need)));
    m.io_read_bytes = Some(Count(a.bytes_read + b.as_ref().map_or(0, |b| b.bytes_read)));
    r
}

fn embedded_vs_synth(same_hash: &str) -> PairStatus {
    PairStatus::new(
        "embedded_vs_synthesized",
        format!("one side's ICC is synthesized by libjxl from its colour enum; sha256 {same_hash}"),
    )
}

fn strict_compare(r: &mut Report, not_cmp_reason: &str) {
    let out = r.output.as_ref().expect("compare has an output");
    let side_problem = |f: &FileReport| -> String {
        let or =
            |d: &str| if f.decode_status_reason.is_empty() { d.to_string() } else { f.decode_status_reason.clone() };
        match f.decode_status.as_str() {
            "not_checked" => or("not_checked"),
            "unsupported" => or("unsupported"),
            s if s != "complete" || f.container_status == "corrupt" => code::DECODE_FAILED.into(),
            _ => String::new(),
        }
    };
    let (pa, pb) = (side_problem(&r.original), side_problem(out));
    let mut reasons = Vec::new();
    let result;
    if !pa.is_empty() || !pb.is_empty() {
        result = "not_evaluated";
        if !pa.is_empty() {
            reasons.push(format!("original:{pa}"));
        }
        if !pb.is_empty() {
            reasons.push(format!("output:{pb}"));
        }
    } else if not_cmp_reason == code::ORIENTATION_UNPARSEABLE || not_cmp_reason == code::ORIENTATION_UNKNOWN {
        result = "indeterminate";
        reasons.push(not_cmp_reason.to_string());
    } else if !not_cmp_reason.is_empty() {
        result = "mismatch";
        reasons.push(not_cmp_reason.to_string());
    } else if r.samples.status == "differs" {
        result = "mismatch";
        reasons.push(code::SAMPLES_DIFFER.to_string());
        let d = r.samples.pixels_differing.map_or(0, |c| c.0);
        let c = r.samples.pixels_compared.map_or(0, |c| c.0);
        r.add_issue(code::SAMPLES_DIFFER, "error", "pair", format!("{d} of {c} pixels differ"));
    } else {
        let sides = [("original", &r.original), ("output", out)];
        for (p, f) in sides {
            if f.container_status != "valid" {
                reasons
                    .push(format!("{p}:{}", if f.format == "png" { code::TRAILING_DATA } else { "container_warning" }));
            }
        }
        for (p, f) in sides {
            if matches!(f.sample_model.alpha.as_str(), "associated" | "unspecified" | "matted_white") {
                reasons.push(format!("{p}:alpha_{}", f.sample_model.alpha));
            }
        }
        for c in [code::PSD_COMPOSITE_UNIFORM, code::EXTRA_CHANNEL_NOT_SUPPORTED] {
            for i in &r.issues {
                if i.code == c && (i.scope == "original" || i.scope == "output") {
                    reasons.push(format!("{}:{c}", i.scope));
                }
            }
        }
        if r.color.status != "equal" {
            const COLOR_CODES: [&str; 5] = [
                code::COLOR_UNKNOWN,
                code::COLOR_UNVERIFIED,
                code::ICC_DIFFERS,
                code::ICC_MISSING_ON_OUTPUT,
                code::ICC_MISSING_ON_ORIGINAL,
            ];
            for i in &r.issues {
                if i.scope == "pair" && COLOR_CODES.contains(&i.code.as_str()) {
                    reasons.push(i.code.clone());
                }
            }
            if reasons.is_empty() {
                reasons.push(code::COLOR_UNVERIFIED.into());
            }
        }
        result = if reasons.is_empty() { "match" } else { "indeterminate" };
    }
    r.strict.result = result.into();
    r.strict.reasons = reasons;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_item_statuses() {
        let present = |s: &str, sum: &str| MetaItem { present: true, status: s.into(), summary: sum.into() };
        assert_eq!(compare_item("x", None, None).status, "both_absent");
        assert_eq!(compare_item("x", Some(&present("present", "a")), None).status, "removed");
        assert_eq!(compare_item("x", None, Some(&present("present", "a"))).status, "added");
        assert_eq!(compare_item("x", Some(&present("present", "a")), Some(&present("present", "a"))).status, "same");
        assert_eq!(compare_item("x", Some(&present("present", "a")), Some(&present("present", "b"))).status, "changed");
        assert_eq!(
            compare_item("x", Some(&present("parse_failed", "")), Some(&present("over_limit", ""))).status,
            "parse_failed"
        );
    }
}
