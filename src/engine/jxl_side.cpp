// JPEG XL: own box scan (jxl_scan.cpp) + libjxl 0.12.0.
//
// prepare: a header pre-pass with coalescing off reads basic info, extra channels,
//          the colour description and every layer header without decoding pixels
//          (stage 3 measurements: no pixel callbacks, ~3 KB allocated).
// decode:  JxlDecoderSetImageOutCallback with UINT16 + JXL_BIT_DEPTH_FROM_CODESTREAM
//          (D1); pieces arrive per 256-wide group, out of raster order, and are
//          written by coordinate (D11). Both passes allocate through a
//          JxlMemoryManager that refuses beyond Limits::codec_cap_bytes() (D10).
#include "../common/checked.hpp"
#include "jxl_scan.hpp"
#include "png_scan.hpp"   // exif_orientation
#include "side.hpp"
#include "whole_image.hpp"

#include <jxl/decode.h>

#include <algorithm>
#include <cstring>
#include <new>

namespace iat {

namespace {

// ------------------------------------------------------------ capped allocator
struct CappedAlloc {
    uint64_t cap = 0, cur = 0, peak = 0, refusals = 0;
    JxlMemoryManager mm() { return JxlMemoryManager{this, &alloc, &release}; }
    static void* alloc(void* op, size_t n) {
        auto* t = static_cast<CappedAlloc*>(op);
        if (n > t->cap || t->cur > t->cap - n) { ++t->refusals; return nullptr; }
        auto* p = static_cast<uint64_t*>(malloc(n + 16));
        if (!p) { ++t->refusals; return nullptr; }
        p[0] = n;
        t->cur += n;
        t->peak = std::max(t->peak, t->cur);
        return reinterpret_cast<char*>(p) + 16;
    }
    static void release(void* op, void* a) {
        if (!a) return;
        auto* p = reinterpret_cast<uint64_t*>(static_cast<char*>(a) - 16);
        static_cast<CappedAlloc*>(op)->cur -= p[0];
        free(p);
    }
};

// ------------------------------------------------------------ chunked input
// Feeds the read-only file in 1 MiB chunks and keeps the bytes libjxl did not consume.
// Truncation = JXL_DEC_NEED_MORE_INPUT after the whole file was given. This relies only
// on documented decode.h semantics: NEED_MORE_INPUT means more bytes are needed, and
// without JXL_DEC_BOX the decoder finds the end of the frames on its own, so
// JxlDecoderCloseInput is optional and is not called. (With CloseInput, 0.12.0 turns
// that state into a plain JXL_DEC_ERROR, which is indistinguishable from bad data; S12.)
// PRECONDITION: neither JXL_DEC_BOX nor JXL_DEC_JPEG_RECONSTRUCTION is subscribed. With
// either, a complete file may legitimately end in NEED_MORE_INPUT until the input is
// closed, so subscribing to them requires re-reviewing this truncation rule (S12).
class Feeder {
public:
    Feeder(const ReadOnlyFile& f, uint64_t size) : f_(f), size_(size) {}
    // Returns false on a read error.
    bool refill(JxlDecoder* dec) {
        const size_t remaining = buf_.empty() ? 0 : JxlDecoderReleaseInput(dec);
        if (remaining) memmove(buf_.data(), buf_.data() + (len_ - remaining), remaining);
        len_ = remaining;
        const size_t take = static_cast<size_t>(std::min<uint64_t>(size_ - next_, kChunk));
        if (buf_.size() < len_ + take) buf_.resize(len_ + take);
        if (take) {
            auto got = f_.read_at(next_, buf_.data() + len_, take);
            if (!got || *got != take) return false;
            next_ += take;
            len_ += take;
        }
        JxlDecoderSetInput(dec, buf_.data(), len_);
        return true;
    }
    // Called on NEED_MORE_INPUT: refills, or reports truncation once the file is exhausted.
    enum class More { fed, truncated, io_error };
    More more(JxlDecoder* dec) {
        if (next_ < size_) return refill(dec) ? More::fed : More::io_error;
        return More::truncated;
    }
    uint64_t unconsumed_after_success(JxlDecoder* dec) { return JxlDecoderReleaseInput(dec) + (size_ - next_); }

private:
    static constexpr uint64_t kChunk = 1ull << 20;
    const ReadOnlyFile& f_;
    uint64_t size_, next_ = 0;
    size_t len_ = 0;
    std::vector<uint8_t> buf_;
};

const char* tf_name(JxlTransferFunction t) {
    switch (t) {
        case JXL_TRANSFER_FUNCTION_709: return "BT.709";
        case JXL_TRANSFER_FUNCTION_UNKNOWN: return "unknown";
        case JXL_TRANSFER_FUNCTION_LINEAR: return "linear";
        case JXL_TRANSFER_FUNCTION_SRGB: return "sRGB";
        case JXL_TRANSFER_FUNCTION_PQ: return "PQ";
        case JXL_TRANSFER_FUNCTION_DCI: return "DCI";
        case JXL_TRANSFER_FUNCTION_HLG: return "HLG";
        case JXL_TRANSFER_FUNCTION_GAMMA: return "gamma";
    }
    return "?";
}

// ------------------------------------------------------------ header pre-pass
struct Header {
    std::string status = "error";      // ok | truncated | decode_error | io_error | resource_limit
    JxlBasicInfo bi{};
    std::vector<JxlExtraChannelInfo> extra;
    bool encoded = false;
    JxlColorEncoding enc{};
    std::vector<uint8_t> icc;          // ORIGINAL target (embedded, or synthesized when encoded)
    std::string icc_status = "absent"; // ok | absent | over_budget
    std::vector<JxlFrameHeader> layers;
    CappedAlloc mem;
};

Header read_header(const ReadOnlyFile& f, uint64_t size, const Limits& lim) {
    Header h;
    h.mem.cap = lim.codec_cap_bytes();
    JxlMemoryManager mm = h.mem.mm();
    JxlDecoder* dec = JxlDecoderCreate(&mm);
    if (!dec) { h.status = "resource_limit"; return h; }
    JxlDecoderSubscribeEvents(dec, JXL_DEC_BASIC_INFO | JXL_DEC_COLOR_ENCODING | JXL_DEC_FRAME);
    JxlDecoderSetKeepOrientation(dec, JXL_TRUE);
    JxlDecoderSetCoalescing(dec, JXL_FALSE);   // report individual layers
    Feeder in(f, size);
    if (!in.refill(dec)) { h.status = "io_error"; JxlDecoderDestroy(dec); return h; }
    for (;;) {
        const JxlDecoderStatus st = JxlDecoderProcessInput(dec);
        if (st == JXL_DEC_ERROR) { h.status = h.mem.refusals ? "resource_limit" : "decode_error"; break; }
        if (st == JXL_DEC_NEED_MORE_INPUT) {
            const auto m = in.more(dec);
            if (m == Feeder::More::truncated) { h.status = "truncated"; break; }
            if (m == Feeder::More::io_error) { h.status = "io_error"; break; }
            continue;
        }
        if (st == JXL_DEC_BASIC_INFO) {
            JxlDecoderGetBasicInfo(dec, &h.bi);
            for (uint32_t i = 0; i < h.bi.num_extra_channels && i < 256; ++i) {
                JxlExtraChannelInfo ei{};
                JxlDecoderGetExtraChannelInfo(dec, i, &ei);
                h.extra.push_back(ei);
            }
        } else if (st == JXL_DEC_COLOR_ENCODING) {
            h.encoded = JxlDecoderGetColorAsEncodedProfile(dec, JXL_COLOR_PROFILE_TARGET_ORIGINAL, &h.enc) == JXL_DEC_SUCCESS;
            size_t n = 0;
            if (JxlDecoderGetICCProfileSize(dec, JXL_COLOR_PROFILE_TARGET_ORIGINAL, &n) == JXL_DEC_SUCCESS && n) {
                if (n > lim.metadata_item_bytes()) h.icc_status = "over_budget";
                else {
                    h.icc.resize(n);
                    if (JxlDecoderGetColorAsICCProfile(dec, JXL_COLOR_PROFILE_TARGET_ORIGINAL, h.icc.data(), n) == JXL_DEC_SUCCESS)
                        h.icc_status = "ok";
                    else h.icc.clear();
                }
            }
        } else if (st == JXL_DEC_FRAME) {
            JxlFrameHeader fh{};
            JxlDecoderGetFrameHeader(dec, &fh);
            if (h.layers.size() < 4096) h.layers.push_back(fh);
        } else if (st == JXL_DEC_SUCCESS) {
            h.status = "ok";
            break;
        }
    }
    JxlDecoderDestroy(dec);
    return h;
}

// ------------------------------------------------------------ decoder
class JxlImageDecoder final : public WholeImageDecoder {
public:
    JxlImageDecoder(uint32_t out_channels, bool bare) : out_channels_(out_channels), bare_(bare) {}
    void configure(uint32_t w, uint32_t h, uint32_t out_ch, uint32_t bits, uint32_t all_channels) {
        set_geometry(w, h, out_ch, bits);
        // Stage 3 measurement: libjxl allocates ~(4 x channels + 0.5) B/px + ~6 MiB, all
        // channels including non-alpha extra channels. Estimate with 0.5 B/px and 2 MiB to spare.
        const auto px = checked_mul(w, h);
        const auto per = px ? checked_mul(*px, 4ull * all_channels + 1) : std::nullopt;
        codec_estimate_ = per ? checked_add(*per, 8ull << 20) : std::nullopt;
    }

protected:
    bool decode_all(Side& s, Report& r, const Limits& lim) override {
        mem_.cap = lim.codec_cap_bytes();
        JxlMemoryManager mm = mem_.mm();
        JxlDecoder* dec = JxlDecoderCreate(&mm);
        if (!dec) { fail_resource(s, r, "libjxl decoder could not be created within the hard cap"); return false; }
        JxlDecoderSubscribeEvents(dec, JXL_DEC_BASIC_INFO | JXL_DEC_FULL_IMAGE);
        JxlDecoderSetKeepOrientation(dec, JXL_TRUE);   // compare in stored orientation (plan 3.1)
        JxlDecoderSetRenderSpotcolors(dec, JXL_FALSE); // S7
        JxlDecoderSetUnpremultiplyAlpha(dec, JXL_FALSE);
        Feeder in(*s.file, s.size);
        bool ok = in.refill(dec), full = false;
        std::string fail;
        while (ok) {
            const JxlDecoderStatus st = JxlDecoderProcessInput(dec);
            if (st == JXL_DEC_ERROR) { fail = mem_.refusals ? "cap" : "decode_error"; break; }
            if (st == JXL_DEC_NEED_MORE_INPUT) {
                const auto m = in.more(dec);
                if (m == Feeder::More::truncated) { fail = "truncated"; break; }
                if (m == Feeder::More::io_error) { fail = "io_error"; ok = false; }
                continue;
            }
            if (st == JXL_DEC_NEED_IMAGE_OUT_BUFFER) {
                JxlPixelFormat pf{out_channels_, JXL_TYPE_UINT16, JXL_NATIVE_ENDIAN, 0};
                JxlBitDepth bd{JXL_BIT_DEPTH_FROM_CODESTREAM, 0, 0};
                if (JxlDecoderSetImageOutCallback(dec, &pf, &JxlImageDecoder::on_pixels, this) != JXL_DEC_SUCCESS ||
                    JxlDecoderSetImageOutBitDepth(dec, &bd) != JXL_DEC_SUCCESS) {
                    fail = "callback_rejected";
                    break;
                }
            } else if (st == JXL_DEC_FULL_IMAGE) {
                full = true;
            } else if (st == JXL_DEC_SUCCESS) {
                // Only the full decode consumes every frame; the header pass skips frame data
                // without consuming it, so trailing bytes are measured here.
                unconsumed_ = in.unconsumed_after_success(dec);
                break;
            }
        }
        JxlDecoderDestroy(dec);
        if (fail == "cap") {
            fail_resource(s, r, "libjxl allocation refused at the hard cap of " + std::to_string(mem_.cap) +
                                    " bytes (job_commit_mib - 64 MiB; raise with --limits job_commit_mib=)");
            return false;
        }
        if (!fail.empty()) {
            if (fail == "truncated") add_issue(r, code::truncated, "error", s.scope, "codestream ends before the image is complete");
            fail_decode(s, r, fail, "libjxl: " + fail, false);
            return false;
        }
        if (!full) { fail_decode(s, r, "no_full_image", "libjxl finished without a full image event", false); return false; }
        // A bare codestream ends where the decoder stops; anything after it is trailing data.
        // (In a container the box scan already accounts for every byte.)
        if (bare_) {
            s.rep->coverage.trailing_bytes = Count{unconsumed_};
            if (unconsumed_) {
                s.rep->container_status = "warning";
                add_issue(r, code::trailing_data, "warning", s.scope, std::to_string(unconsumed_) + " bytes after the end of the codestream");
            }
        }
        return true;
    }
    void codec_metrics(std::map<std::string, uint64_t>& m) const override {
        m["libjxl_alloc_peak"] = std::max(m["libjxl_alloc_peak"], mem_.peak);
        if (mem_.refusals) m["libjxl_alloc_refusals"] += mem_.refusals;
    }

private:
    static void on_pixels(void* op, size_t x, size_t y, size_t n, const void* px) {
        auto* self = static_cast<JxlImageDecoder*>(op);
        self->put(static_cast<uint32_t>(y), static_cast<uint32_t>(x), static_cast<uint32_t>(n), static_cast<const uint16_t*>(px));
    }
    uint32_t out_channels_;
    bool bare_;
    uint64_t unconsumed_ = 0;
    CappedAlloc mem_;
};

void fill_metadata(FileReport& fr, const JxlScan& sc, const Header& h) {
    auto item = [&](const std::string& name, bool present, const std::string& status, const std::string& summary) {
        fr.metadata[name] = MetaItem{present, present ? status : "absent", summary};
    };
    if (sc.exif_status == "over_limit") item("exif", true, "over_limit", "exceeds metadata budget");
    else if (sc.exif_status == "compressed") item("exif", true, "present", "brotli-compressed (brob), not decoded");
    else if (sc.exif_status == "present") {
        const int o = sc.exif.empty() ? -1 : exif_orientation(sc.exif);
        item("exif", true, o < 0 ? "parse_failed" : "present",
             "bytes=" + std::to_string(sc.exif.size()) + " sha256=" + short_hash(sha256_hex(sc.exif.data(), sc.exif.size())) +
                 (o > 0 ? " orientation=" + std::to_string(o) + " (metadata only; the codestream orientation applies)" : ""));
    } else item("exif", false, "", "");
    if (sc.xmp_bytes) item("xmp", true, "present", "bytes=" + std::to_string(*sc.xmp_bytes) + " sha256=" + short_hash(sc.xmp_sha256));
    else item("xmp", false, "", "");
    std::string hdr;
    if (h.encoded && (h.enc.transfer_function == JXL_TRANSFER_FUNCTION_PQ || h.enc.transfer_function == JXL_TRANSFER_FUNCTION_HLG))
        hdr = std::string("transfer=") + tf_name(h.enc.transfer_function);
    if (h.bi.intensity_target > 0 && h.bi.intensity_target != 255.0f)
        hdr += (hdr.empty() ? "" : " ") + std::string("intensity_target=") + std::to_string(static_cast<int>(h.bi.intensity_target));
    if (!hdr.empty()) item("hdr_signals", true, "present", hdr);
    else item("hdr_signals", false, "", "");
    std::string others;
    for (const auto& b : sc.other_boxes) others += (others.empty() ? "" : ",") + b;
    if (sc.jbrd) others += std::string(others.empty() ? "" : ",") + "jbrd(JPEG reconstruction data)";
    if (!others.empty()) item("other_chunks", true, "present", others);
    else item("other_chunks", false, "", "");
}

std::string layer_text(const JxlFrameHeader& f) {
    const auto& l = f.layer_info;
    return (l.have_crop ? "crop " + std::to_string(l.crop_x0) + "," + std::to_string(l.crop_y0) + " " : std::string("full ")) +
           std::to_string(l.xsize) + "x" + std::to_string(l.ysize) + " blend=" + std::to_string(static_cast<int>(l.blend_info.blendmode));
}

}  // namespace

std::string libjxl_version() {
    const uint32_t v = JxlDecoderVersion();
    return std::to_string(v / 1000000) + "." + std::to_string((v / 1000) % 1000) + "." + std::to_string(v % 1000);
}

std::unique_ptr<Decoder> prepare_jxl(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    const auto t0 = Clock::now();
    JxlScan sc = scan_jxl(*s.file, s.size, lim);
    for (Issue i : sc.issues) { i.scope = s.scope; r.issues.push_back(i); }
    fr.container_status = sc.container_status;
    fr.metadata_status = sc.metadata_status;
    fr.chunks = sc.boxes;
    if (sc.container_status == "corrupt") {
        s.scan_ms = ms_since(t0);
        fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
        fr.decode_status = "failed";
        fr.decode_status_reason = "structure_error";
        return nullptr;
    }
    Header h = read_header(*s.file, s.size, lim);
    s.scan_ms = ms_since(t0);
    fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
    fill_metadata(fr, sc, h);
    if (h.status != "ok") {
        if (h.status == "resource_limit") {
            fr.decode_status = "not_checked";
            fr.decode_status_reason = "resource_limit";
            add_issue(r, code::resource_limit, "error", s.scope, "libjxl header pass refused an allocation at the hard cap");
        } else {
            fr.decode_status = "failed";
            fr.decode_status_reason = h.status;
            if (h.status == "truncated") add_issue(r, code::truncated, "error", s.scope, "codestream ends before all frames are present");
            add_issue(r, code::decode_failed, "error", s.scope, "libjxl header pass: " + h.status);
        }
        return nullptr;
    }
    const JxlBasicInfo& bi = h.bi;
    fr.width = s.width = bi.xsize;
    fr.height = s.height = bi.ysize;
    fr.coverage.rows_total = bi.ysize;
    fr.orientation = {"jxl_codestream", static_cast<uint32_t>(bi.orientation)};

    // ---- colour: enum (ICC synthesized by the API) vs attached ICC (plan 3.2, D12)
    if (h.encoded) {
        const JxlColorEncoding& c = h.enc;
        std::string key = "jxl_enum:space=" + std::to_string(c.color_space) + ";white=" + std::to_string(c.white_point) +
                          ";primaries=" + std::to_string(c.primaries) + ";transfer=" + std::to_string(c.transfer_function);
        if (c.transfer_function == JXL_TRANSFER_FUNCTION_GAMMA) key += ";gamma=" + std::to_string(c.gamma);
        if (c.white_point == JXL_WHITE_POINT_CUSTOM)
            key += ";wxy=" + std::to_string(c.white_point_xy[0]) + "," + std::to_string(c.white_point_xy[1]);
        if (c.primaries == JXL_PRIMARIES_CUSTOM)
            key += ";rxy=" + std::to_string(c.primaries_red_xy[0]) + "," + std::to_string(c.primaries_red_xy[1]) +
                   ";gxy=" + std::to_string(c.primaries_green_xy[0]) + "," + std::to_string(c.primaries_green_xy[1]) +
                   ";bxy=" + std::to_string(c.primaries_blue_xy[0]) + "," + std::to_string(c.primaries_blue_xy[1]);
        fr.color = {"jxl_enum", key,
                    std::string("JXL colour enum (transfer ") + tf_name(c.transfer_function) + ", rendering intent " +
                        std::to_string(c.rendering_intent) + ")",
                    {"enum(" + key.substr(9) + ";intent=" + std::to_string(c.rendering_intent) + ")"}};
        if (h.icc_status == "ok") {
            fr.icc.status = "synthesized";   // generated by libjxl from the enum, not embedded data
            fr.icc.bytes = Count{h.icc.size()};
            fr.icc.sha256 = sha256_hex(h.icc.data(), h.icc.size());
            fr.icc.profile_name = "synthesized by libjxl " + libjxl_version() + " from the colour enum";
        }
    } else if (h.icc_status == "ok") {
        fr.icc.status = "embedded";
        fr.icc.bytes = Count{h.icc.size()};
        fr.icc.sha256 = sha256_hex(h.icc.data(), h.icc.size());
        fr.color = {"icc", "icc:" + fr.icc.sha256, "embedded ICC " + std::to_string(h.icc.size()) + " bytes",
                    {"ICC(" + short_hash(fr.icc.sha256) + ")"}};
    } else {
        fr.icc.status = h.icc_status == "over_budget" ? "over_budget" : "unreadable";
        fr.color = {"icc_unreadable", "", "ICC profile not available within the metadata budget", {}};
    }

    // ---- support matrix
    if (!bi.uses_original_profile) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "xyb";
        add_issue(r, code::xyb_not_native, "error", s.scope,
                  "XYB (uses_original_profile=false): the decoder cannot return native samples, so samples are not comparable");
        return nullptr;
    }
    if (bi.have_animation) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "animation";
        add_issue(r, code::animation_not_supported, "error", s.scope, "animated JPEG XL is not supported");
        return nullptr;
    }
    const bool alpha = bi.alpha_bits > 0;
    std::string why;
    if (bi.exponent_bits_per_sample != 0) why = "floating-point samples";
    else if (bi.bits_per_sample < 1 || bi.bits_per_sample > 16) why = std::to_string(bi.bits_per_sample) + "-bit samples";
    else if (bi.num_color_channels != 1 && bi.num_color_channels != 3) why = std::to_string(bi.num_color_channels) + " colour channels";
    else if (alpha && (bi.alpha_bits != bi.bits_per_sample || bi.alpha_exponent_bits != 0)) why = "alpha depth differs from colour depth";
    if (!why.empty()) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "sample_model";
        add_issue(r, code::unsupported_sample_model, "error", s.scope, "JPEG XL " + why + " is not supported");
        return nullptr;
    }
    uint32_t non_alpha = 0;
    bool alpha_seen = false;
    for (const auto& e : h.extra) {
        if (e.type == JXL_CHANNEL_ALPHA && !alpha_seen) { alpha_seen = true; continue; }
        ++non_alpha;
    }
    if (non_alpha)
        add_issue(r, code::extra_channel_not_supported, "warning", s.scope,
                  std::to_string(non_alpha) + " extra channel(s) besides alpha are not compared; no strict match");
    std::string path = "libjxl-float-pipeline";
    if (h.layers.size() > 1) {
        std::string t;
        for (size_t i = 0; i < h.layers.size() && i < 8; ++i) t += (i ? "; " : "") + layer_text(h.layers[i]);
        add_issue(r, code::layered_image, "info", s.scope,
                  "still image composed of " + std::to_string(h.layers.size()) + " layers, compared as composited by libjxl: " + t);
        path += "+coalesced-" + std::to_string(h.layers.size()) + "-layers";
    }
    const uint32_t ch = bi.num_color_channels + (alpha ? 1u : 0u);
    const std::string model = bi.num_color_channels == 3 ? (alpha ? "rgba" : "rgb") : (alpha ? "gray_alpha" : "gray");
    s.layout = {model, ch, bi.bits_per_sample};
    SampleModel& sm = fr.sample_model;
    sm.model = model;
    sm.container_bits = bi.bits_per_sample;
    sm.channel_bits.assign(ch, bi.bits_per_sample);
    sm.compare_type = bi.bits_per_sample > 8 ? "uint16" : "uint8";
    sm.alpha = !alpha ? "none" : (bi.alpha_premultiplied ? "associated" : "straight");
    fr.decode_path = path;
    auto dec = std::make_unique<JxlImageDecoder>(ch, !sc.container);
    dec->configure(bi.xsize, bi.ysize, ch, bi.bits_per_sample, bi.num_color_channels + bi.num_extra_channels);
    return dec;
}

}  // namespace iat
