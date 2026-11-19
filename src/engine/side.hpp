// One input file inside the worker: format-neutral state shared by the
// comparison engine, plus the decoder interface each format implements.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"
#include "analyze.hpp"
#include "temp_store.hpp"

#include <chrono>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace iat {

using Clock = std::chrono::steady_clock;
inline double ms_since(Clock::time_point t0) {
    return std::chrono::duration<double, std::milli>(Clock::now() - t0).count();
}
inline std::string short_hash(const std::string& h) { return h.size() > 16 ? h.substr(0, 16) : h; }
inline void add_issue(Report& r, const std::string& code, const std::string& sev, const std::string& scope,
                      const std::string& msg) {
    r.issues.push_back(Issue{code, sev, scope, msg});
}
inline bool has_alpha(const std::string& model) { return model == "gray_alpha" || model == "rgba"; }
inline std::string color_base(const std::string& model) { return model.rfind("gray", 0) == 0 ? "gray" : "rgb"; }

struct Side;

// Decoder for one side. The engine calls, in order: open() once, read_row()
// once per image row (top to bottom) until it fails or all rows are read,
// then finish() once. Each successful read_row() leaves exactly
// width * layout.channels canonical samples in Side::canon.
class Decoder {
public:
    virtual ~Decoder() = default;
    // Tool-managed buffer bytes needed while decoding (checked by the engine
    // against buffer_mib before open), or nullopt on arithmetic overflow.
    virtual std::optional<uint64_t> buffer_estimate() const = 0;
    // Codec-internal memory that grows with the image (whole-image decoders such as
    // libjxl and libwebp), checked by the engine against Limits::codec_cap_bytes()
    // before open. Row decoders need none. nullopt on arithmetic overflow.
    virtual std::optional<uint64_t> codec_estimate() const { return uint64_t{0}; }
    virtual bool open(Side& s, Report& r, const Limits& lim) = 0;
    virtual bool read_row(Side& s, Report& r) = 0;
    virtual void finish(Side& s, Report& r) = 0;
    // Decoder-specific metrics (bytes), summed across sides by key.
    virtual void memory_metrics(std::map<std::string, uint64_t>& m) const = 0;
};

struct Side {
    std::string scope;        // original | output
    const ReadOnlyFile* file = nullptr;
    uint64_t size = 0;
    FileReport* rep = nullptr;
    bool decodable = false;   // format supported and decode may start
    RowLayout layout;
    uint32_t width = 0, height = 0;
    std::vector<uint16_t> canon;  // canonical row
    bool failed = false;
    uint64_t rows = 0;
    uint64_t budget = 0;
    double scan_ms = 0;
    TempFile* temp = nullptr;  // this side's temporary file (may be unavailable)
    std::unique_ptr<Decoder> dec;
};

// Temporary storage failures: exhausted quota or disk space is a resource
// limit (not checked, exit 4); anything else is an environment problem.
inline void temp_failure(Side& s, Report& r, const TempError& e) {
    s.failed = true;
    s.rep->decode_status = "not_checked";
    if (e.kind == "quota" || e.kind == "disk") {
        s.rep->decode_status_reason = "resource_limit";
        add_issue(r, code::resource_limit, "error", s.scope, "temporary storage: " + e.message);
    } else {
        s.rep->decode_status_reason = "temp_" + e.kind;
        add_issue(r, code::environment_error, "error", s.scope, "temporary storage: " + e.message);
    }
}

// Format front ends: scan the container, fill the FileReport, and return a
// decoder when decoding may start (nullptr otherwise, with the reason recorded).
std::unique_ptr<Decoder> prepare_png(Side& s, Report& r, const Limits& lim);
std::unique_ptr<Decoder> prepare_tiff(Side& s, Report& r, const Limits& lim);  // TIFF and BigTIFF
std::unique_ptr<Decoder> prepare_webp(Side& s, Report& r, const Limits& lim);  // still VP8L only
std::unique_ptr<Decoder> prepare_jxl(Side& s, Report& r, const Limits& lim);   // codestream and container
std::unique_ptr<Decoder> prepare_psd(Side& s, Report& r, const Limits& lim);   // PSD and PSB merged image
std::unique_ptr<Decoder> prepare_avif(Side& s, Report& r, const Limits& lim);  // still AVIF, lossless by metadata only
std::string libtiff_version();
std::string libwebp_version();
std::string libjxl_version();
std::string libavif_version();

}  // namespace iat
