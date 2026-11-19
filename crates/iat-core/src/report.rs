//! 報告模型：欄位對應 C++ 版 `src/model/report.hpp` 與 `docs/cli-json-contract.md`（schema 1.1）。
//!
//! 表示狀態的欄位維持字串，因為 JSON 契約本身就是字串列舉，而且問題代碼是開放集合
//! （例如 `worker_stderr`、`temp_stale_removed`）。穩定代碼集中在 [`code`]。

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// 64 位元計數；JSON 一律寫成十進位字串（超出 2^53 的安全整數範圍也不失真）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Count(pub u64);

impl Serialize for Count {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Count;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal string")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Count, E> {
                if s.is_empty() || s.len() > 20 || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(E::custom("bad count string"));
                }
                s.parse().map(Count).map_err(|_| E::custom("bad count string"))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Count, E> {
                Ok(Count(v))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Issue {
    pub code: String,
    /// error | warning | info
    pub severity: String,
    /// original | output | pair | run
    pub scope: String,
    pub message: String,
}

impl Issue {
    pub fn new(code: &str, severity: &str, scope: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), severity: severity.into(), scope: scope.into(), message: message.into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Coverage {
    pub rows_decoded: u64,
    pub rows_total: u64,
    pub bytes_scanned: Count,
    /// 未到達結尾標記時為 null
    pub trailing_bytes: Option<Count>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SampleModel {
    pub model: String,
    pub container_bits: u32,
    pub channel_bits: Vec<u32>,
    pub compare_type: String,
    pub alpha: String,
    pub expansion: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Orientation {
    pub source: String,
    pub value: u32,
}

impl Default for Orientation {
    fn default() -> Self {
        Self { source: String::new(), value: 1 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectiveColor {
    pub kind: String,
    pub key: String,
    pub description: String,
    pub signals: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IccInfo {
    pub status: String,
    pub bytes: Option<Count>,
    pub sha256: String,
    pub profile_name: String,
    pub decoder_accepted: bool,
}

impl Default for IccInfo {
    fn default() -> Self {
        Self {
            status: "absent".into(),
            bytes: None,
            sha256: String::new(),
            profile_name: String::new(),
            decoder_accepted: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetaItem {
    pub present: bool,
    pub status: String,
    pub summary: String,
}

impl Default for MetaItem {
    fn default() -> Self {
        Self { present: false, status: "absent".into(), summary: String::new() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DimensionPolicy {
    pub spec_max: u32,
    pub policy_max: u32,
    pub within_spec: bool,
    pub within_policy: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceNeed {
    pub limit: String,
    pub needed_bytes: Count,
    pub allowed_bytes: Count,
    pub current_value: u64,
    pub suggested_value: Option<u64>,
    pub suggested_argument: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PsdInfo {
    pub version: u32,
    pub color_mode: String,
    pub depth: u32,
    pub channels: u32,
    pub compression: String,
    pub composite: String,
    pub composite_uniform: bool,
    pub layer_count_field: i32,
    pub layer_count: u32,
    pub layers_known: bool,
    pub alpha: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AvifInfo {
    pub yuv_format: String,
    pub depth: u32,
    pub color_primaries: u32,
    pub transfer_characteristics: u32,
    pub matrix_coefficients: u32,
    pub full_range: bool,
    pub cicp_source: String,
    pub alpha: bool,
    pub alpha_premultiplied: bool,
    pub image_count: u32,
    pub lossless_metadata: String,
    pub lossless_metadata_reasons: Vec<String>,
    pub pixel_verified: Option<bool>,
    pub pixel_verified_basis: String,
}

impl Default for AvifInfo {
    fn default() -> Self {
        Self {
            yuv_format: String::new(),
            depth: 0,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            full_range: false,
            cicp_source: String::new(),
            alpha: false,
            alpha_premultiplied: false,
            image_count: 0,
            lossless_metadata: String::new(),
            lossless_metadata_reasons: Vec::new(),
            pixel_verified: None,
            pixel_verified_basis: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileReport {
    pub name: String,
    /// 只有 `--full-paths` 時才有值
    pub path: String,
    pub size_bytes: Count,
    pub sha256: String,
    pub format: String,
    pub extension_mismatch: bool,
    pub container_status: String,
    pub decode_status: String,
    pub decode_status_reason: String,
    pub metadata_status: String,
    pub coverage: Coverage,
    pub width: u32,
    pub height: u32,
    /// IHDR 欄位；-1 表示未知
    pub interlace: i32,
    pub sample_model: SampleModel,
    pub decode_path: String,
    pub orientation: Orientation,
    pub color: EffectiveColor,
    pub icc: IccInfo,
    pub metadata: BTreeMap<String, MetaItem>,
    pub chunks: Vec<String>,
    pub dimension_policy: Option<DimensionPolicy>,
    pub psd: Option<PsdInfo>,
    pub avif: Option<AvifInfo>,
    pub resource_need: Option<ResourceNeed>,
    pub decoder_warnings: Vec<String>,
    pub decoder_errors: Vec<String>,
}

impl Default for FileReport {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            size_bytes: Count(0),
            sha256: String::new(),
            format: String::new(),
            extension_mismatch: false,
            container_status: "unverified".into(),
            decode_status: "not_checked".into(),
            decode_status_reason: String::new(),
            metadata_status: "unverified".into(),
            coverage: Coverage::default(),
            width: 0,
            height: 0,
            interlace: -1,
            sample_model: SampleModel::default(),
            decode_path: String::new(),
            orientation: Orientation::default(),
            color: EffectiveColor::default(),
            icc: IccInfo::default(),
            metadata: BTreeMap::new(),
            chunks: Vec::new(),
            dimension_policy: None,
            psd: None,
            avif: None,
            resource_need: None,
            decoder_warnings: Vec::new(),
            decoder_errors: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelStat {
    pub name: String,
    pub differing: Count,
    pub max_abs_error: u32,
    pub mae: f64,
    pub rmse: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffEntry {
    pub x: u64,
    pub y: u64,
    pub original: Vec<u32>,
    pub output: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SampleComparison {
    pub status: String,
    pub reason: String,
    pub pixels_compared: Option<Count>,
    pub pixels_differing: Option<Count>,
    pub per_channel: Vec<ChannelStat>,
    pub first_differences: Vec<DiffEntry>,
    pub includes_alpha: bool,
    pub unit: String,
}

impl Default for SampleComparison {
    fn default() -> Self {
        Self {
            status: "not_compared".into(),
            reason: String::new(),
            pixels_compared: None,
            pixels_differing: None,
            per_channel: Vec::new(),
            first_differences: Vec::new(),
            includes_alpha: false,
            unit: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PairStatus {
    pub status: String,
    pub basis: String,
}

impl PairStatus {
    pub fn new(status: &str, basis: impl Into<String>) -> Self {
        Self { status: status.into(), basis: basis.into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataField {
    pub name: String,
    /// same | changed | added | removed | both_absent | parse_failed | over_limit
    pub status: String,
    pub original: String,
    pub output: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataReport {
    pub fields: Vec<MetadataField>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SizeInfo {
    pub original_bytes: Count,
    pub output_bytes: Option<Count>,
    pub output_ratio: Option<f64>,
    pub saving_ratio: Option<f64>,
    pub compression_factor: Option<f64>,
    pub comparable: bool,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    pub time_ms: BTreeMap<String, f64>,
    pub memory_bytes: BTreeMap<String, Option<Count>>,
    pub io_read_bytes: Option<Count>,
    pub timing_note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Strict {
    /// match | mismatch | indeterminate | not_evaluated
    pub result: String,
    pub reasons: Vec<String>,
}

impl Default for Strict {
    fn default() -> Self {
        Self { result: "not_evaluated".into(), reasons: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Exit {
    pub code: i32,
    pub reason: String,
}

impl Default for Exit {
    fn default() -> Self {
        Self { code: 5, reason: String::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempSettings {
    pub location: String,
    pub free_bytes_at_start: Count,
    pub delete_on_close: bool,
    /// 只在 `--full-paths` 時出現
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

impl Default for TempSettings {
    fn default() -> Self {
        Self { location: "system_temp".into(), free_bytes_at_start: Count(0), delete_on_close: true, dir: None }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitSettings {
    pub job_commit_mib: u64,
    pub buffer_mib: u64,
    pub metadata_item_mib: u64,
    pub metadata_file_mib: u64,
    pub temp_gib: u64,
    pub deadline_s: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestSettings {
    pub codec_estimate_pct: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub limits: LimitSettings,
    pub codec_threads: u32,
    pub full_paths: bool,
    pub temp: TempSettings,
    /// 只在使用 `--test-codec-estimate-pct` 時出現
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<TestSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            limits: LimitSettings::default(),
            codec_threads: 1,
            full_paths: false,
            temp: TempSettings::default(),
            test: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub schema_version: String,
    pub run_id: String,
    pub tool: BTreeMap<String, String>,
    pub codecs: BTreeMap<String, String>,
    pub settings: Settings,
    /// compare | inspect
    pub mode: String,
    pub original: FileReport,
    /// 只有 compare 才有；inspect 時為 null
    pub output: Option<FileReport>,
    pub geometry: PairStatus,
    pub samples: SampleComparison,
    pub color: PairStatus,
    pub icc: PairStatus,
    pub metadata: MetadataReport,
    pub size: SizeInfo,
    pub metrics: Metrics,
    pub strict: Strict,
    pub issues: Vec<Issue>,
    pub exit: Exit,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            schema_version: "1.1".into(),
            run_id: String::new(),
            tool: BTreeMap::new(),
            codecs: BTreeMap::new(),
            settings: Settings::default(),
            mode: String::new(),
            original: FileReport::default(),
            output: None,
            geometry: PairStatus::default(),
            samples: SampleComparison::default(),
            color: PairStatus::default(),
            icc: PairStatus::default(),
            metadata: MetadataReport::default(),
            size: SizeInfo::default(),
            metrics: Metrics::default(),
            strict: Strict::default(),
            issues: Vec::new(),
            exit: Exit::default(),
        }
    }
}

impl Report {
    pub fn add_issue(&mut self, code: &str, severity: &str, scope: &str, message: impl Into<String>) {
        self.issues.push(Issue::new(code, severity, scope, message));
    }

    pub fn has_issue(&self, codes: &[&str]) -> bool {
        self.issues.iter().any(|i| codes.contains(&i.code.as_str()))
    }

    /// 原檔，compare 時再加輸出檔。
    pub fn files(&self) -> impl Iterator<Item = &FileReport> {
        std::iter::once(&self.original).chain(self.output.as_ref())
    }
}

/// CLI 寫出與印出的 JSON 文字：鍵依字母排序、縮排一格、非 ASCII 不跳脫、結尾換行，
/// 與 C++ 版 `nlohmann::json::dump(1)` 相同的版面。
pub fn report_json_text(r: &Report) -> String {
    // serde_json::Value 的物件是 BTreeMap（未啟用 preserve_order），所以鍵會排序。
    let value = serde_json::to_value(r).expect("Report always serializes");
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    value.serialize(&mut ser).expect("writing to Vec cannot fail");
    let mut s = String::from_utf8(out).expect("serde_json writes UTF-8");
    s.push('\n');
    s
}

/// 穩定問題代碼（docs/cli-json-contract.md）。
pub mod code {
    pub const INPUT_OPEN_FAILED: &str = "input_open_failed";
    pub const EXTENSION_MISMATCH: &str = "extension_mismatch";
    pub const UNKNOWN_FORMAT: &str = "unknown_format";
    pub const TRUNCATED: &str = "truncated";
    pub const CRC_ERROR: &str = "crc_error";
    pub const TRAILING_DATA: &str = "trailing_data";
    pub const MISSING_END_MARKER: &str = "missing_end_marker";
    pub const STRUCTURE_ERROR: &str = "structure_error";
    pub const DECODE_FAILED: &str = "decode_failed";
    pub const ANIMATION_NOT_SUPPORTED: &str = "animation_not_supported";
    pub const INTERLACE_NOT_SUPPORTED: &str = "interlace_not_supported";
    pub const UNSUPPORTED_SAMPLE_MODEL: &str = "unsupported_sample_model";
    pub const GEOMETRY_DIFFERS: &str = "geometry_differs";
    pub const ORIENTATION_DIFFERS: &str = "orientation_differs";
    pub const ORIENTATION_UNPARSEABLE: &str = "orientation_unparseable";
    pub const ORIENTATION_UNKNOWN: &str = "orientation_unknown";
    pub const BIT_DEPTH_CHANGED: &str = "bit_depth_changed";
    pub const SAMPLE_MODEL_CHANGED: &str = "sample_model_changed";
    pub const SAMPLES_DIFFER: &str = "samples_differ";
    pub const ALPHA_CHANNEL_ADDED: &str = "alpha_channel_added";
    pub const ALPHA_CHANNEL_REMOVED: &str = "alpha_channel_removed";
    pub const ICC_MISSING_ON_OUTPUT: &str = "icc_missing_on_output";
    pub const ICC_MISSING_ON_ORIGINAL: &str = "icc_missing_on_original";
    pub const ICC_DIFFERS: &str = "icc_differs";
    pub const ICC_REJECTED_BY_DECODER: &str = "icc_rejected_by_decoder";
    pub const COLOR_UNKNOWN: &str = "color_unknown";
    pub const COLOR_UNVERIFIED: &str = "color_unverified";
    pub const LOW_PRIORITY_COLOR_SIGNAL_DIFFERS: &str = "low_priority_color_signal_differs";
    pub const METADATA_PARSE_FAILED: &str = "metadata_parse_failed";
    pub const METADATA_OVER_LIMIT: &str = "metadata_over_limit";
    pub const METADATA_CHANGED: &str = "metadata_changed";
    pub const RESOURCE_LIMIT: &str = "resource_limit";
    pub const CANCELLED: &str = "cancelled";
    pub const DEADLINE_EXCEEDED: &str = "deadline_exceeded";
    pub const WORKER_CRASHED: &str = "worker_crashed";
    pub const WORKER_PROTOCOL_ERROR: &str = "worker_protocol_error";
    pub const ENVIRONMENT_ERROR: &str = "environment_error";
    pub const REPORT_WRITE_FAILED: &str = "report_write_failed";
    pub const SAME_FILE: &str = "same_file";
    pub const DECODER_WARNING: &str = "decoder_warning";
    pub const TEMP_CLEANUP_FAILED: &str = "temp_cleanup_failed";
    pub const INTERLACE_DIFFERS: &str = "interlace_differs";
    pub const COMPRESSION_NOT_SUPPORTED: &str = "compression_not_supported";
    pub const MULTI_IMAGE_NOT_SUPPORTED: &str = "multi_image_not_supported";
    pub const LOSSY_NOT_SUPPORTED: &str = "lossy_not_supported";
    pub const XYB_NOT_NATIVE: &str = "xyb_not_native";
    pub const UNSUPPORTED_DIMENSION: &str = "unsupported_dimension";
    pub const WEBP_POLICY_CANNOT_HOLD_ORIGINAL: &str = "webp_policy_cannot_hold_original";
    pub const ALPHA_HINT_MISMATCH: &str = "alpha_hint_mismatch";
    pub const EXTRA_CHANNEL_NOT_SUPPORTED: &str = "extra_channel_not_supported";
    pub const LAYERED_IMAGE: &str = "layered_image";
    pub const TEST_OPTION_USED: &str = "test_option_used";
    pub const PSD_COMPOSITE_NOT_SAVED: &str = "psd_composite_not_saved";
    pub const PSD_COMPOSITE_FLAG_ABSENT: &str = "psd_composite_flag_absent";
    pub const PSD_COMPOSITE_UNIFORM: &str = "psd_composite_uniform";
    pub const AVIF_NOT_LOSSLESS_BY_METADATA: &str = "avif_not_lossless_by_metadata";
    pub const AVIF_LOSSLESS_METADATA_ONLY: &str = "avif_lossless_metadata_only";
    pub const AVIF_PIXEL_VERIFIED_LOSSLESS: &str = "avif_pixel_verified_lossless";
    pub const AVIF_CLAP_NOT_SUPPORTED: &str = "avif_clap_not_supported";
    pub const ALPHA_SEMANTICS_DIFFER: &str = "alpha_semantics_differ";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_is_a_decimal_string() {
        assert_eq!(serde_json::to_string(&Count(u64::MAX)).unwrap(), "\"18446744073709551615\"");
        let c: Count = serde_json::from_str("\"42\"").unwrap();
        assert_eq!(c, Count(42));
        assert!(serde_json::from_str::<Count>("\"-1\"").is_err());
        assert!(serde_json::from_str::<Count>("\"\"").is_err());
    }

    #[test]
    fn json_layout_is_sorted_one_space_indent() {
        let r = Report { mode: "inspect".into(), ..Report::default() };
        let t = report_json_text(&r);
        assert!(t.starts_with("{\n \"codecs\": {},\n \"color\": {\n  \"basis\": \"\",\n"), "{t}");
        assert!(t.ends_with("}\n"));
        assert!(t.contains("\n \"output\": null,\n"));
        assert!(!t.contains("\"dir\""));
        assert!(!t.contains("\"test\""));
    }

    #[test]
    fn report_round_trips() {
        let mut r = Report { output: Some(FileReport::default()), ..Report::default() };
        r.add_issue(code::SAME_FILE, "info", "pair", "x");
        let back: Report = serde_json::from_str(&report_json_text(&r)).unwrap();
        assert_eq!(back, r);
    }
}
