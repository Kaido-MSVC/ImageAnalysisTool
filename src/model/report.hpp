// Report model shared by worker (producer) and parent (finalizer/renderer).
// The JSON produced from this model is the contract in docs/cli-json-contract.md.
#pragma once

#include <nlohmann/json.hpp>

#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <vector>

namespace iat {

// 64-bit counts/sizes are serialized as decimal strings (JSON safe-integer range).
struct Count {
    uint64_t v = 0;
};
void to_json(nlohmann::json& j, const Count& c);
void from_json(const nlohmann::json& j, Count& c);

struct Issue {
    std::string code;
    std::string severity;  // error | warning | info
    std::string scope;     // original | output | pair | run
    std::string message;
};

struct Coverage {
    uint64_t rows_decoded = 0;
    uint64_t rows_total = 0;
    Count bytes_scanned;
    std::optional<Count> trailing_bytes;  // null when the end marker was not reached
};

struct SampleModel {
    std::string model;                    // gray | gray_alpha | rgb | rgba | "" (unknown)
    uint32_t container_bits = 0;          // IHDR bit depth
    std::vector<uint32_t> channel_bits;   // canonical bits per channel
    std::string compare_type;             // uint8 | uint16 | native_codes
    std::string alpha;                    // none | straight | associated | unspecified | matted_white (PSD merged)
    std::string expansion;                // explicit lossless expansion applied, or ""
};

struct Orientation {
    std::string source;   // exif | default | unparseable | unknown (EXIF not read: over budget)
    uint32_t value = 1;   // 1..8
};

struct EffectiveColor {
    std::string kind;          // cicp | icc | srgb | chrm_gama | partial | icc_unreadable | none
    std::string key;           // canonical comparison key (values or ICC sha256)
    std::string description;   // human-readable
    std::vector<std::string> signals;  // all explicit colour signals present in the file
};

struct IccInfo {
    std::string status = "absent";  // embedded | absent | unreadable | over_budget
    std::optional<Count> bytes;
    std::string sha256;
    std::string profile_name;        // as written in iCCP (escaped on render)
    bool decoder_accepted = false;   // libpng kept the profile (benign-error check)
};

struct MetaItem {
    bool present = false;
    std::string status = "absent";   // present | absent | parse_failed | over_limit
    std::string summary;             // hash / size / small structured values only
};

// Format size limits recorded as two separate fields (plan section 2, WebP 16383 rule):
// what the bitstream specification can describe and what this tool's policy accepts.
struct DimensionPolicy {
    uint32_t spec_max = 0;      // e.g. 16384 for VP8L (14-bit field + 1)
    uint32_t policy_max = 0;    // e.g. 16383 (IMB-C analysis compatibility policy)
    bool within_spec = true;
    bool within_policy = true;
};

// A limit that stopped this file before decoding, with the value that would let it run
// (0.5.0). Set only when the tool's own estimate was the reason (row buffers or a
// whole-image decoder against the hard cap), so callers such as IMB-C can retry with
// `suggested_argument`. suggested_value is null when the need exceeds the limit's maximum.
struct ResourceNeed {
    std::string limit;                 // job_commit_mib | buffer_mib
    Count needed_bytes;                // the estimate that was checked
    Count allowed_bytes;               // what the current limit allows for it
    uint64_t current_value = 0;        // current setting (MiB)
    std::optional<uint64_t> suggested_value;   // smallest value (rounded up) that fits the estimate
    std::string suggested_argument;    // e.g. "--limits job_commit_mib=2048", empty when out of range
};

// PSD / PSB merged-image facts (0.5.0).
struct PsdInfo {
    uint32_t version = 0;            // 1 = PSD, 2 = PSB
    std::string color_mode;          // bitmap | grayscale | indexed | rgb | cmyk | multichannel | duotone | lab
    uint32_t depth = 0;
    uint32_t channels = 0;
    std::string compression;         // raw | rle | zip | zip_prediction
    std::string composite;           // saved | not_saved | flattened | flag_absent
    bool composite_uniform = false;  // every merged pixel has the same value
    int32_t layer_count_field = 0;   // signed header field (< 0: merged transparency in the first extra channel)
    uint32_t layer_count = 0;
    bool layers_known = true;
    std::string alpha;               // none | matted_white | straight (indexed transparency)
};

// AVIF facts and the two separate lossless verdicts (0.5.0).
struct AvifInfo {
    std::string yuv_format;          // 444 | 422 | 420 | 400
    uint32_t depth = 0;
    uint32_t color_primaries = 2, transfer_characteristics = 2, matrix_coefficients = 2;
    bool full_range = false;
    std::string cicp_source;         // colr_nclx | av1_sequence_header_or_default
    bool alpha = false;
    bool alpha_premultiplied = false;
    uint32_t image_count = 0;
    std::string lossless_metadata;   // consistent | inconsistent
    std::vector<std::string> lossless_metadata_reasons;
    std::optional<bool> pixel_verified;   // compare only; null otherwise
    std::string pixel_verified_basis;
};

struct FileReport {
    std::string name;
    std::string path;                // empty unless --full-paths
    Count size_bytes;
    std::string sha256;
    std::string format;              // png | unknown | ...
    bool extension_mismatch = false;
    std::string container_status = "unverified";  // valid | warning | corrupt | unverified | over_limit
    std::string decode_status = "not_checked";    // complete | partial | failed | unsupported | not_checked
    std::string decode_status_reason;             // resource_limit | deadline | cancelled | ...
    std::string metadata_status = "unverified";   // valid | warning | parse_failed | over_limit | unverified
    Coverage coverage;
    uint32_t width = 0;
    uint32_t height = 0;
    int interlace = -1;                            // IHDR field; -1 unknown
    SampleModel sample_model;
    std::string decode_path;
    Orientation orientation;
    EffectiveColor color;
    IccInfo icc;
    std::map<std::string, MetaItem> metadata;
    std::vector<std::string> chunks;               // chunk types in file order (deduplicated runs)
    std::optional<DimensionPolicy> dimension_policy;  // formats with a size policy (WebP)
    std::optional<PsdInfo> psd;
    std::optional<AvifInfo> avif;
    std::optional<ResourceNeed> resource_need;
    std::vector<std::string> decoder_warnings;
    std::vector<std::string> decoder_errors;
};

struct ChannelStat {
    std::string name;
    Count differing;
    uint32_t max_abs_error = 0;
    double mae = 0;
    double rmse = 0;
};

struct DiffEntry {
    uint64_t x = 0;
    uint64_t y = 0;
    std::vector<uint32_t> original;
    std::vector<uint32_t> output;
};

struct SampleComparison {
    std::string status = "not_compared";  // equal | differs | not_comparable | incomplete | not_compared
    std::string reason;
    std::optional<Count> pixels_compared;
    std::optional<Count> pixels_differing;
    std::vector<ChannelStat> per_channel;
    std::vector<DiffEntry> first_differences;
    bool includes_alpha = false;
    std::string unit;
};

struct PairStatus {
    std::string status;   // see docs
    std::string basis;    // explanation / comparison basis
};

struct MetadataField {
    std::string name;
    std::string status;   // same | changed | added | removed | both_absent | parse_failed | over_limit
    std::string original;
    std::string output;
};

struct SizeInfo {
    Count original_bytes;
    std::optional<Count> output_bytes;
    std::optional<double> output_ratio;
    std::optional<double> saving_ratio;
    std::optional<double> compression_factor;
    bool comparable = false;
    std::string note;
};

struct Metrics {
    std::map<std::string, double> time_ms;
    std::map<std::string, std::optional<Count>> memory_bytes;
    std::optional<Count> io_read_bytes;
    std::string timing_note;
};

struct Strict {
    std::string result = "not_evaluated";  // match | mismatch | indeterminate | not_evaluated
    std::vector<std::string> reasons;
};

struct Report {
    std::string schema_version = "1.1";
    std::string run_id;
    std::map<std::string, std::string> tool;
    std::map<std::string, std::string> codecs;
    nlohmann::json settings = nlohmann::json::object();
    std::string mode;                      // compare | inspect
    FileReport original;
    std::optional<FileReport> output;
    PairStatus geometry;
    SampleComparison samples;
    PairStatus color;
    PairStatus icc;
    std::vector<MetadataField> metadata;
    SizeInfo size;
    Metrics metrics;
    Strict strict;
    std::vector<Issue> issues;
    int exit_code = 5;
    std::string exit_reason;
};

void to_json(nlohmann::json& j, const Report& r);
void from_json(const nlohmann::json& j, Report& r);

// Stable issue codes (docs/cli-json-contract.md).
namespace code {
inline constexpr const char* input_open_failed = "input_open_failed";
inline constexpr const char* extension_mismatch = "extension_mismatch";
inline constexpr const char* unknown_format = "unknown_format";
inline constexpr const char* truncated = "truncated";
inline constexpr const char* crc_error = "crc_error";
inline constexpr const char* trailing_data = "trailing_data";
inline constexpr const char* missing_end_marker = "missing_end_marker";
inline constexpr const char* structure_error = "structure_error";
inline constexpr const char* decode_failed = "decode_failed";
inline constexpr const char* animation_not_supported = "animation_not_supported";
inline constexpr const char* interlace_not_supported = "interlace_not_supported";
inline constexpr const char* unsupported_sample_model = "unsupported_sample_model";
inline constexpr const char* geometry_differs = "geometry_differs";
inline constexpr const char* orientation_differs = "orientation_differs";
inline constexpr const char* orientation_unparseable = "orientation_unparseable";
inline constexpr const char* orientation_unknown = "orientation_unknown";   // EXIF over the metadata budget (D21)
inline constexpr const char* bit_depth_changed = "bit_depth_changed";
inline constexpr const char* sample_model_changed = "sample_model_changed";
inline constexpr const char* samples_differ = "samples_differ";
inline constexpr const char* alpha_channel_added = "alpha_channel_added";
inline constexpr const char* alpha_channel_removed = "alpha_channel_removed";
inline constexpr const char* icc_missing_on_output = "icc_missing_on_output";
inline constexpr const char* icc_missing_on_original = "icc_missing_on_original";
inline constexpr const char* icc_differs = "icc_differs";
inline constexpr const char* icc_rejected_by_decoder = "icc_rejected_by_decoder";
inline constexpr const char* color_unknown = "color_unknown";
inline constexpr const char* color_unverified = "color_unverified";
inline constexpr const char* low_priority_color_signal_differs = "low_priority_color_signal_differs";
inline constexpr const char* metadata_parse_failed = "metadata_parse_failed";
inline constexpr const char* metadata_over_limit = "metadata_over_limit";
inline constexpr const char* metadata_changed = "metadata_changed";
inline constexpr const char* resource_limit = "resource_limit";
inline constexpr const char* cancelled = "cancelled";
inline constexpr const char* deadline_exceeded = "deadline_exceeded";
inline constexpr const char* worker_crashed = "worker_crashed";
inline constexpr const char* worker_protocol_error = "worker_protocol_error";
inline constexpr const char* environment_error = "environment_error";
inline constexpr const char* report_write_failed = "report_write_failed";
inline constexpr const char* same_file = "same_file";
inline constexpr const char* decoder_warning = "decoder_warning";
inline constexpr const char* temp_cleanup_failed = "temp_cleanup_failed";
inline constexpr const char* interlace_differs = "interlace_differs";
inline constexpr const char* compression_not_supported = "compression_not_supported";
inline constexpr const char* multi_image_not_supported = "multi_image_not_supported";
// stage 3
inline constexpr const char* lossy_not_supported = "lossy_not_supported";            // WebP VP8
inline constexpr const char* xyb_not_native = "xyb_not_native";                      // JXL uses_original_profile=false
inline constexpr const char* unsupported_dimension = "unsupported_dimension";        // tool policy (WebP > 16383)
inline constexpr const char* webp_policy_cannot_hold_original = "webp_policy_cannot_hold_original";
inline constexpr const char* alpha_hint_mismatch = "alpha_hint_mismatch";            // WebP header hint vs data / VP8X flag
inline constexpr const char* extra_channel_not_supported = "extra_channel_not_supported";  // JXL non-alpha extra channel
inline constexpr const char* layered_image = "layered_image";                        // JXL still image composed of layers
inline constexpr const char* test_option_used = "test_option_used";
// 0.5.0
inline constexpr const char* psd_composite_not_saved = "psd_composite_not_saved";
inline constexpr const char* psd_composite_flag_absent = "psd_composite_flag_absent";
inline constexpr const char* psd_composite_uniform = "psd_composite_uniform";
inline constexpr const char* avif_not_lossless_by_metadata = "avif_not_lossless_by_metadata";
inline constexpr const char* avif_lossless_metadata_only = "avif_lossless_metadata_only";
inline constexpr const char* avif_pixel_verified_lossless = "avif_pixel_verified_lossless";
inline constexpr const char* avif_clap_not_supported = "avif_clap_not_supported";
inline constexpr const char* alpha_semantics_differ = "alpha_semantics_differ";
}  // namespace code

}  // namespace iat
