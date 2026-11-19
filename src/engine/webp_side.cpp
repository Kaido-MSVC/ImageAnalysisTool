// WebP: own RIFF scan (webp_scan.cpp) + libwebp decode of still VP8L only.
// Decoding always produces straight RGBA (MODE_RGBA), so the RGB under A=0 is
// kept; the VP8L alpha_is_used bit is only a hint (stage 3 measurements), so the
// decoded alpha decides whether the image is compared as RGB or RGBA.
#include "../common/checked.hpp"
#include "png_scan.hpp"   // exif_orientation
#include "side.hpp"
#include "webp_scan.hpp"
#include "whole_image.hpp"

#include <webp/decode.h>

#include <cstring>
#include <new>

namespace iat {

namespace {

constexpr uint32_t kWebpSpecMax = 16384;    // VP8L: 14-bit field + 1
constexpr uint32_t kWebpPolicyMax = 16383;  // IMB-C analysis compatibility policy (plan section 2)

std::string status_text(VP8StatusCode st) {
    switch (st) {
        case VP8_STATUS_OK: return "OK";
        case VP8_STATUS_OUT_OF_MEMORY: return "OUT_OF_MEMORY";
        case VP8_STATUS_INVALID_PARAM: return "INVALID_PARAM";
        case VP8_STATUS_BITSTREAM_ERROR: return "BITSTREAM_ERROR";
        case VP8_STATUS_UNSUPPORTED_FEATURE: return "UNSUPPORTED_FEATURE";
        case VP8_STATUS_SUSPENDED: return "SUSPENDED";
        case VP8_STATUS_USER_ABORT: return "USER_ABORT";
        case VP8_STATUS_NOT_ENOUGH_DATA: return "NOT_ENOUGH_DATA";
    }
    return "status " + std::to_string(static_cast<int>(st));
}

// EXIF chunks are raw TIFF data; some writers prepend "Exif\0\0".
std::vector<uint8_t> exif_tiff(const std::vector<uint8_t>& e, bool& had_prefix) {
    had_prefix = e.size() >= 6 && memcmp(e.data(), "Exif\0\0", 6) == 0;
    return had_prefix ? std::vector<uint8_t>(e.begin() + 6, e.end()) : e;
}

class WebpDecoder final : public WholeImageDecoder {
public:
    WebpDecoder(const WebpScan& sc, uint64_t file_size) : alpha_hint_(sc.vp8l_alpha_hint), file_size_(file_size) {}
    void configure(uint32_t w, uint32_t h) {
        set_geometry(w, h, 4, 8);
        // Stage 3 measurement: 8.03-8.07 B/px (RGBA output + VP8L internal ARGB), plus
        // the whole file held in memory for WebPDecode and a fixed allowance. libwebp has no
        // allocator hook, so only the Job limit backs this up: the sum is scaled by 1.15
        // (8192x8192: 524.1 MiB unscaled was within 1.4 % of the 516.9 MiB Job peak).
        const auto px = checked_mul(w, h);
        const auto b8 = px ? checked_mul(*px, 8) : std::nullopt;
        const auto base = b8 ? checked_sum({*b8, *px / 8, file_size_, 4ull << 20}) : std::nullopt;
        const auto scaled = base ? checked_mul(*base, 115) : std::nullopt;
        codec_estimate_ = scaled ? std::optional<uint64_t>(*scaled / 100) : std::nullopt;
    }

protected:
    bool decode_all(Side& s, Report& r, const Limits&) override {
        const uint32_t w = s.width, h = s.height;
        std::vector<uint8_t> data, rgba;
        try {
            data.resize(static_cast<size_t>(file_size_));
            rgba.resize(static_cast<size_t>(static_cast<uint64_t>(w) * h * 4));
        } catch (const std::bad_alloc&) {
            fail_resource(s, r, "buffers for the WebP file and its RGBA output could not be allocated");
            return false;
        }
        for (uint64_t off = 0; off < file_size_;) {
            const size_t take = static_cast<size_t>(std::min<uint64_t>(file_size_ - off, 64ull << 20));
            auto got = s.file->read_at(off, data.data() + off, take);
            if (!got || *got != take) {
                fail_decode(s, r, "io_error", "reading the WebP file failed", false);
                return false;
            }
            off += take;
        }
        WebPDecoderConfig cfg;
        if (!WebPInitDecoderConfig(&cfg)) { fail_decode(s, r, "decoder_init", "WebPInitDecoderConfig failed", false); return false; }
        cfg.options.use_threads = 0;
        cfg.output.colorspace = MODE_RGBA;  // straight (non-premultiplied) alpha
        cfg.output.is_external_memory = 1;
        cfg.output.u.RGBA.rgba = rgba.data();
        cfg.output.u.RGBA.stride = static_cast<int>(w * 4);
        cfg.output.u.RGBA.size = rgba.size();
        const VP8StatusCode st = WebPDecode(data.data(), data.size(), &cfg);
        WebPFreeDecBuffer(&cfg.output);
        std::vector<uint8_t>().swap(data);  // the encoded bytes are no longer needed
        if (st != VP8_STATUS_OK) {
            s.rep->decoder_errors.push_back("libwebp: " + status_text(st));
            if (st == VP8_STATUS_OUT_OF_MEMORY) fail_resource(s, r, "libwebp ran out of memory (OUT_OF_MEMORY)");
            else fail_decode(s, r, st == VP8_STATUS_NOT_ENOUGH_DATA ? "truncated" : "decode_error",
                             "libwebp: " + status_text(st), false);
            return false;
        }
        // Alpha decides the model; the header hint is only checked against it.
        uint64_t not_opaque = 0;
        for (size_t i = 3; i < rgba.size(); i += 4) not_opaque += rgba[i] != 255;
        if (!alpha_hint_ && not_opaque) {
            s.rep->container_status = "warning";
            add_issue(r, code::alpha_hint_mismatch, "warning", s.scope,
                      "VP8L alpha_is_used is clear but " + std::to_string(not_opaque) + " pixels have alpha < 255; compared as RGBA");
        }
        if (!alpha_hint_ && !not_opaque) {
            set_output_channels(s, 3, "rgb");
            s.rep->sample_model.model = "rgb";
            s.rep->sample_model.channel_bits.assign(3, 8);
            s.rep->sample_model.alpha = "none";
        }
        std::vector<uint16_t> row(static_cast<size_t>(w) * 4);
        for (uint32_t y = 0; y < h; ++y) {
            const uint8_t* p = rgba.data() + static_cast<size_t>(y) * w * 4;
            for (size_t i = 0; i < row.size(); ++i) row[i] = p[i];
            put(y, 0, w, row.data());
        }
        return true;
    }

private:
    bool alpha_hint_;
    uint64_t file_size_;
};

void fill_metadata(FileReport& fr, const WebpScan& sc) {
    auto item = [&](const std::string& name, bool present, const std::string& status, const std::string& summary) {
        fr.metadata[name] = MetaItem{present, present ? status : "absent", summary};
    };
    if (sc.exif_status == "over_limit") item("exif", true, "over_limit", "exceeds metadata budget");
    else if (sc.exif_status == "present") {
        bool prefix = false;
        const int o = exif_orientation(exif_tiff(sc.exif, prefix));
        item("exif", true, o < 0 ? "parse_failed" : "present",
             "bytes=" + std::to_string(sc.exif.size()) + " sha256=" + short_hash(sha256_hex(sc.exif.data(), sc.exif.size())) +
                 (prefix ? " exif_prefix" : "") + (o > 0 ? " orientation=" + std::to_string(o) : ""));
    } else item("exif", false, "", "");
    if (sc.xmp_bytes) item("xmp", true, "present", "bytes=" + std::to_string(*sc.xmp_bytes) + " sha256=" + short_hash(sc.xmp_sha256));
    else item("xmp", false, "", "");
    std::string others;
    for (const auto& c : sc.unknown_chunks) others += (others.empty() ? "" : ",") + c;
    if (!others.empty()) item("other_chunks", true, "present", others);
    else item("other_chunks", false, "", "");
}

}  // namespace

std::string libwebp_version() {
    const int v = WebPGetDecoderVersion();
    return std::to_string((v >> 16) & 0xFF) + "." + std::to_string((v >> 8) & 0xFF) + "." + std::to_string(v & 0xFF);
}

std::unique_ptr<Decoder> prepare_webp(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    const auto t0 = Clock::now();
    WebpScan sc = scan_webp(*s.file, s.size, lim);
    s.scan_ms = ms_since(t0);
    for (Issue i : sc.issues) { i.scope = s.scope; r.issues.push_back(i); }
    fr.container_status = sc.container_status;
    fr.metadata_status = sc.metadata_status;
    fr.chunks = sc.chunks;
    fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
    if (sc.trailing_bytes) fr.coverage.trailing_bytes = Count{*sc.trailing_bytes};
    fill_metadata(fr, sc);
    if (sc.icc_status == "embedded") {
        fr.icc.status = "embedded";
        fr.icc.bytes = Count{sc.icc.size()};
        fr.icc.sha256 = sha256_hex(sc.icc.data(), sc.icc.size());
        fr.color = {"icc", "icc:" + fr.icc.sha256, "embedded ICC " + std::to_string(sc.icc.size()) + " bytes (ICCP)",
                    {"ICCP(" + short_hash(fr.icc.sha256) + ")"}};
    } else if (sc.icc_status == "over_budget") {
        fr.icc.status = "over_budget";
        fr.color = {"icc_unreadable", "", "ICCP exceeds the metadata budget", {"ICCP(over budget)"}};
    } else {
        // D12: the WebP container's default is recorded as a specification default, not as a declaration.
        fr.color = {"webp_default_srgb", "webp_default_srgb",
                    "no ICCP: WebP container default (sRGB assumed by the specification, not declared in the file)", {}};
    }
    if (sc.container_status != "corrupt" && sc.animated) {
        // Frames live in ANMF chunks, so there is no top-level image size: report the VP8X canvas.
        fr.width = s.width = sc.canvas_width;
        fr.height = s.height = sc.canvas_height;
        fr.orientation = {"default", 1};
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "animation";
        add_issue(r, code::animation_not_supported, "error", s.scope, "animated WebP (ANIM/ANMF) is not supported");
        return nullptr;
    }
    if (sc.container_status == "corrupt" || sc.width == 0) {
        fr.decode_status = "failed";
        fr.decode_status_reason = "structure_error";
        return nullptr;
    }
    fr.width = s.width = sc.width;
    fr.height = s.height = sc.height;
    fr.coverage.rows_total = sc.height;
    fr.dimension_policy = DimensionPolicy{kWebpSpecMax, kWebpPolicyMax, sc.width <= kWebpSpecMax && sc.height <= kWebpSpecMax,
                                          sc.width <= kWebpPolicyMax && sc.height <= kWebpPolicyMax};
    // Orientation: WebP has no orientation field; Exif orientation is used as for PNG eXIf.
    if (sc.exif_status == "over_limit") {
        // Not read, so not known: never assume "no rotation" (D21).
        fr.orientation = {"unknown", 0};
        add_issue(r, code::orientation_unknown, "warning", s.scope, "EXIF exceeds the metadata budget; orientation not read");
    } else if (sc.exif_status == "present") {
        bool prefix = false;
        const int o = exif_orientation(exif_tiff(sc.exif, prefix));
        if (o < 0) { fr.orientation = {"unparseable", 0}; add_issue(r, code::orientation_unparseable, "warning", s.scope, "EXIF orientation could not be parsed"); }
        else if (o == 0) fr.orientation = {"default", 1};
        else fr.orientation = {"exif", static_cast<uint32_t>(o)};
    } else {
        fr.orientation = {"default", 1};
    }

    if (sc.image_chunk != "VP8L") {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "lossy";
        add_issue(r, code::lossy_not_supported, "error", s.scope,
                  std::string("lossy WebP (VP8") + (sc.has_alph ? " + ALPH" : "") + ") is outside the strict scope (still VP8L only)");
        return nullptr;
    }
    if (!fr.dimension_policy->within_policy) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "dimension_policy";
        add_issue(r, code::unsupported_dimension, "error", s.scope,
                  std::to_string(sc.width) + "x" + std::to_string(sc.height) + " exceeds the tool policy of " +
                      std::to_string(kWebpPolicyMax) + " per side (the VP8L specification allows up to " +
                      std::to_string(kWebpSpecMax) + "); not a file error");
        return nullptr;
    }
    s.layout = {"rgba", 4, 8};
    SampleModel& sm = fr.sample_model;
    sm.model = "rgba";
    sm.container_bits = 8;
    sm.channel_bits.assign(4, 8);
    sm.compare_type = "uint8";
    sm.alpha = "straight";
    fr.decode_path = "libwebp-vp8l-rgba";
    auto dec = std::make_unique<WebpDecoder>(sc, s.size);
    dec->configure(sc.width, sc.height);
    return dec;
}

}  // namespace iat
