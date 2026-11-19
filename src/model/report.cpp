#include "report.hpp"

#include <stdexcept>

namespace iat {

void to_json(nlohmann::json& j, const Count& c) { j = std::to_string(c.v); }

void from_json(const nlohmann::json& j, Count& c) {
    if (j.is_string()) {
        const std::string& s = j.get_ref<const std::string&>();
        if (s.empty() || s.size() > 20 || s.find_first_not_of("0123456789") != std::string::npos)
            throw std::invalid_argument("bad count string");
        c.v = std::stoull(s);
    } else if (j.is_number_unsigned()) {
        c.v = j.get<uint64_t>();
    } else {
        throw std::invalid_argument("count must be a decimal string");
    }
}

NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(Issue, code, severity, scope, message)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(Coverage, rows_decoded, rows_total, bytes_scanned, trailing_bytes)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(SampleModel, model, container_bits, channel_bits, compare_type, alpha,
                                                expansion)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(Orientation, source, value)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(EffectiveColor, kind, key, description, signals)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(IccInfo, status, bytes, sha256, profile_name, decoder_accepted)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(MetaItem, present, status, summary)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(DimensionPolicy, spec_max, policy_max, within_spec, within_policy)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(ResourceNeed, limit, needed_bytes, allowed_bytes, current_value,
                                                suggested_value, suggested_argument)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(PsdInfo, version, color_mode, depth, channels, compression, composite,
                                                composite_uniform, layer_count_field, layer_count, layers_known, alpha)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(AvifInfo, yuv_format, depth, color_primaries, transfer_characteristics,
                                                matrix_coefficients, full_range, cicp_source, alpha, alpha_premultiplied,
                                                image_count, lossless_metadata, lossless_metadata_reasons, pixel_verified,
                                                pixel_verified_basis)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(FileReport, name, path, size_bytes, sha256, format, extension_mismatch,
                                                container_status, decode_status, decode_status_reason,
                                                metadata_status, coverage, width, height, interlace, sample_model,
                                                decode_path, orientation, color, icc, metadata, chunks,
                                                dimension_policy, psd, avif, resource_need, decoder_warnings,
                                                decoder_errors)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(ChannelStat, name, differing, max_abs_error, mae, rmse)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(DiffEntry, x, y, original, output)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(SampleComparison, status, reason, pixels_compared, pixels_differing,
                                                per_channel, first_differences, includes_alpha, unit)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(PairStatus, status, basis)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(MetadataField, name, status, original, output)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(SizeInfo, original_bytes, output_bytes, output_ratio, saving_ratio,
                                                compression_factor, comparable, note)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(Metrics, time_ms, memory_bytes, io_read_bytes, timing_note)
NLOHMANN_DEFINE_TYPE_NON_INTRUSIVE_WITH_DEFAULT(Strict, result, reasons)

void to_json(nlohmann::json& j, const Report& r) {
    j = nlohmann::json::object();
    j["schema_version"] = r.schema_version;
    j["run_id"] = r.run_id;
    j["tool"] = r.tool;
    j["codecs"] = r.codecs;
    j["settings"] = r.settings;
    j["mode"] = r.mode;
    j["original"] = r.original;
    j["output"] = r.output;
    j["geometry"] = r.geometry;
    j["samples"] = r.samples;
    j["color"] = r.color;
    j["icc"] = r.icc;
    j["metadata"] = {{"fields", r.metadata}};
    j["size"] = r.size;
    j["metrics"] = r.metrics;
    j["strict"] = r.strict;
    j["issues"] = r.issues;
    j["exit"] = {{"code", r.exit_code}, {"reason", r.exit_reason}};
}

void from_json(const nlohmann::json& j, Report& r) {
    r.schema_version = j.value("schema_version", r.schema_version);
    r.run_id = j.value("run_id", std::string());
    r.tool = j.value("tool", std::map<std::string, std::string>());
    r.codecs = j.value("codecs", std::map<std::string, std::string>());
    r.settings = j.value("settings", nlohmann::json::object());
    r.mode = j.value("mode", std::string());
    if (j.contains("original")) r.original = j.at("original").get<FileReport>();
    if (j.contains("output") && !j.at("output").is_null()) r.output = j.at("output").get<FileReport>();
    r.geometry = j.value("geometry", PairStatus{});
    r.samples = j.value("samples", SampleComparison{});
    r.color = j.value("color", PairStatus{});
    r.icc = j.value("icc", PairStatus{});
    if (j.contains("metadata") && j.at("metadata").contains("fields"))
        r.metadata = j.at("metadata").at("fields").get<std::vector<MetadataField>>();
    r.size = j.value("size", SizeInfo{});
    r.metrics = j.value("metrics", Metrics{});
    r.strict = j.value("strict", Strict{});
    r.issues = j.value("issues", std::vector<Issue>{});
    if (j.contains("exit")) {
        r.exit_code = j.at("exit").value("code", 5);
        r.exit_reason = j.at("exit").value("reason", std::string());
    }
}

}  // namespace iat
