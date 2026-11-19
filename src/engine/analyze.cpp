// Comparison engine: format dispatch, budgets, lockstep row comparison and
// the colour / metadata / strict verdicts. Format specifics live behind
// the Decoder interface (side.hpp).
#include "analyze.hpp"

#include "../common/checked.hpp"
#include "png_shim.h"
#include "side.hpp"

#include <psapi.h>
#include <zlib.h>

#include <algorithm>
#include <cmath>
#include <cstring>

namespace iat {

namespace {

// ------------------------------------------------------------ format sniffing
std::string sniff_format(const ReadOnlyFile& f) {
    uint8_t b[64] = {};
    const size_t n = f.read_at(0, b, sizeof b).value_or(0);
    if (n >= 6 && !memcmp(b, "8BPS\0\x01", 6)) return "psd";
    if (n >= 6 && !memcmp(b, "8BPS\0\x02", 6)) return "psb";
    if (n >= 6 && !memcmp(b, "8BPS", 4)) return "psd";   // unknown version: the scanner reports it
    if (n >= 16 && !memcmp(b + 4, "ftyp", 4)) {
        // ISOBMFF: AVIF when the major or a compatible brand (within the bytes read) is avif / avis.
        const uint32_t box = static_cast<uint32_t>(b[0]) << 24 | static_cast<uint32_t>(b[1]) << 16 | static_cast<uint32_t>(b[2]) << 8 | b[3];
        const size_t end = std::min<size_t>(n, box >= 16 ? box : 16);
        for (size_t i = 8; i + 4 <= end; i += 4)
            if (i != 12 && (!memcmp(b + i, "avif", 4) || !memcmp(b + i, "avis", 4))) return "avif";
    }
    if (n >= 8 && !memcmp(b, "\x89PNG\r\n\x1a\n", 8)) return "png";
    if (n >= 4 && (!memcmp(b, "II*\0", 4) || !memcmp(b, "MM\0*", 4))) return "tiff";
    if (n >= 4 && (!memcmp(b, "II+\0", 4) || !memcmp(b, "MM\0+", 4))) return "bigtiff";
    if (n >= 2 && b[0] == 0xFF && b[1] == 0x0A) return "jxl_codestream";
    if (n >= 12 && !memcmp(b, "\0\0\0\x0CJXL \r\n\x87\n", 12)) return "jxl_container";
    if (n >= 12 && !memcmp(b, "RIFF", 4) && !memcmp(b + 8, "WEBP", 4)) return "webp";
    if (n >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF) return "jpeg";
    return "unknown";
}

std::vector<std::string> channel_names(const std::string& model) {
    if (model == "gray") return {"Gray"};
    if (model == "gray_alpha") return {"Gray", "A"};
    if (model == "rgb") return {"R", "G", "B"};
    return {"R", "G", "B", "A"};
}

MetadataField compare_item(const std::string& name, const MetaItem* a, const MetaItem* b) {
    MetadataField f{name, "", a ? a->summary : "", b ? b->summary : ""};
    const bool pa = a && a->present, pb = b && b->present;
    if (!pa && !pb) f.status = "both_absent";
    else if ((pa && a->status == "parse_failed") || (pb && b->status == "parse_failed")) f.status = "parse_failed";
    else if ((pa && a->status == "over_limit") || (pb && b->status == "over_limit")) f.status = "over_limit";
    else if (pa && !pb) f.status = "removed";
    else if (!pa && pb) f.status = "added";
    else f.status = a->summary == b->summary ? "same" : "changed";
    return f;
}

// ------------------------------------------------------------ JXL colour equivalence (D12)
// JXL enum values equal CICP codes for the cases accepted here: primaries sRGB=1,
// BT.2100=9; transfer BT.709=1, linear=8, sRGB=13, PQ=16, HLG=18; white point D65=1.
std::map<std::string, std::string> enum_fields(const std::string& key) {
    std::map<std::string, std::string> m;
    const size_t start = key.find(':');
    std::string rest = start == std::string::npos ? "" : key.substr(start + 1);
    size_t p = 0;
    while (p < rest.size()) {
        size_t e = rest.find(';', p);
        if (e == std::string::npos) e = rest.size();
        const std::string kv = rest.substr(p, e - p);
        const size_t eq = kv.find('=');
        if (eq != std::string::npos) m[kv.substr(0, eq)] = kv.substr(eq + 1);
        p = e + 1;
    }
    return m;
}

int signal_int(const std::vector<std::string>& signals, const std::string& marker) {
    for (const auto& s : signals) {
        const size_t p = s.find(marker);
        if (p != std::string::npos) return std::atoi(s.c_str() + p + marker.size());
    }
    return -1;
}

}  // namespace

std::optional<uint64_t> suggest_limit_mib(uint64_t needed_bytes, uint64_t reserve_bytes, uint64_t step_mib, uint64_t max_mib) {
    const auto total = checked_add(needed_bytes, reserve_bytes);
    if (!total || step_mib == 0) return std::nullopt;
    const uint64_t mib = (*total >> 20) + ((*total & ((1ull << 20) - 1)) ? 1 : 0);
    const uint64_t rounded = (mib + step_mib - 1) / step_mib * step_mib;
    if (rounded < mib || rounded > max_mib) return std::nullopt;
    return rounded;
}

JxlEquivalence jxl_equivalence(const FileReport& a, const FileReport& b) {
    const FileReport* j = a.color.kind == "jxl_enum" ? &a : (b.color.kind == "jxl_enum" ? &b : nullptr);
    if (!j) return {};
    const FileReport* o = j == &a ? &b : &a;
    if (o->color.kind == "jxl_enum") return {};   // two JXL enums: plain key equality applies
    // Synthesized ICC byte-identical to the other side's embedded ICC (user decision, D12).
    if (o->color.kind == "icc" && o->icc.status == "embedded" && j->icc.status == "synthesized" && !j->icc.sha256.empty() &&
        j->icc.sha256 == o->icc.sha256)
        return {true, "JXL colour enum whose ICC (" + j->icc.profile_name + ") is byte-identical to the other file's embedded ICC (sha256 " +
                          short_hash(o->icc.sha256) + ")", ""};
    auto f = enum_fields(j->color.key);
    const int space = std::atoi(f["space"].c_str()), white = std::atoi(f["white"].c_str());
    const int prim = std::atoi(f["primaries"].c_str()), tf = std::atoi(f["transfer"].c_str());
    if ((space != 0 && space != 1) || white != 1) return {};
    if (o->color.kind == "srgb" && (space == 1 || prim == 1) && tf == 13) {
        const int ji = signal_int(j->color.signals, "intent="), pi = signal_int(o->color.signals, "sRGB(intent ");
        std::string note;
        if (ji >= 0 && pi >= 0 && ji != pi)
            note = "rendering intent differs: JXL enum " + std::to_string(ji) + ", PNG sRGB " + std::to_string(pi) +
                   " (recorded only; does not affect colour equality)";
        return {true, "JXL sRGB colour enum and PNG sRGB chunk", note};
    }
    if (o->color.kind == "cicp" && space == 0 && (prim == 1 || prim == 9) &&
        (tf == 1 || tf == 8 || tf == 13 || tf == 16 || tf == 18)) {
        const std::string want = "cicp:" + std::to_string(prim) + "/" + std::to_string(tf) + "/0/1";
        if (o->color.key == want) return {true, "JXL colour enum equals PNG cICP " + want.substr(5), ""};
    }
    return {};
}

namespace {

// ------------------------------------------------------------ side setup
void prepare_side(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    fr.size_bytes = Count{s.size};
    fr.format = sniff_format(*s.file);
    if (fr.format == "png") s.dec = prepare_png(s, r, lim);
    else if (fr.format == "tiff" || fr.format == "bigtiff") s.dec = prepare_tiff(s, r, lim);
    else if (fr.format == "webp") s.dec = prepare_webp(s, r, lim);
    else if (fr.format == "jxl_codestream" || fr.format == "jxl_container") s.dec = prepare_jxl(s, r, lim);
    else if (fr.format == "psd" || fr.format == "psb") s.dec = prepare_psd(s, r, lim);
    else if (fr.format == "avif") s.dec = prepare_avif(s, r, lim);
    else {
        fr.container_status = "unverified";
        fr.decode_status = "unsupported";
        fr.decode_status_reason = fr.format == "unknown" ? "unknown_format" : "format_not_supported_in_this_build";
        fr.metadata_status = "unverified";
        add_issue(r, fr.format == "unknown" ? code::unknown_format : code::unsupported_sample_model, "error", s.scope,
                  "format '" + fr.format + "' is not supported by this build (PNG, TIFF/BigTIFF, WebP, JPEG XL, PSD/PSB, AVIF)");
    }
    s.decodable = s.dec != nullptr;
}

bool open_side(Side& s, Report& r, const Limits& lim) {
    if (!s.dec->open(s, r, lim)) return false;
    s.canon.resize(static_cast<size_t>(s.width) * s.layout.channels);
    return true;
}

// ------------------------------------------------------------ statistics
struct Stats {
    std::vector<uint64_t> differing;
    std::vector<uint32_t> maxabs;
    std::vector<U128> sumabs, sumsq;
    uint64_t pixels = 0, pixels_diff = 0;
    std::vector<DiffEntry> first;
};

}  // namespace

Report analyze(const AnalysisInput& in) {
    const auto t_all = Clock::now();
    Report r;
    r.mode = in.mode;
    r.codecs["libpng"] = pngs_version();
    r.codecs["zlib"] = zlibVersion();
    r.codecs["libtiff"] = libtiff_version();
    r.codecs["libwebp"] = libwebp_version();
    r.codecs["libjxl"] = libjxl_version();
    r.codecs["libavif"] = libavif_version();
    const bool compare = in.mode == "compare";
    r.original.name = "original";
    if (compare) r.output = FileReport{};

    Side a, b;
    a.scope = "original";
    a.file = in.original;
    a.size = in.original->size().value_or(0);
    a.rep = &r.original;
    if (compare) {
        b.scope = "output";
        b.file = in.output;
        b.size = in.output->size().value_or(0);
        b.rep = &*r.output;
    }
    // Temporary storage: one parent-created file per side, one shared quota.
    TempQuota quota(in.limits.temp_gib << 30, in.temp_free_bytes);
    TempFile ta(in.temp[0], &quota), tb(in.temp[1], &quota);
    a.temp = &ta;
    b.temp = &tb;
    prepare_side(a, r, in.limits);
    if (compare) prepare_side(b, r, in.limits);
    std::vector<Side*> sides{&a};
    if (compare) sides.push_back(&b);

    // ---- budget: rows only (never the whole image), checked 64-bit arithmetic
    uint64_t need = 0;
    bool overflow = false;
    for (Side* s : sides) {
        if (!s->decodable) continue;
        auto est = s->dec->buffer_estimate();
        if (!est) { overflow = true; break; }
        s->budget = *est;
        auto sum = checked_add(need, *est);
        if (!sum) { overflow = true; break; }
        need = *sum;
    }
    if (overflow || need > in.limits.buffer_bytes()) {
        // Row buffers are one budget for both sides: every stopped side carries the same need.
        const auto sug = overflow ? std::nullopt : suggest_limit_mib(need, 0, 16, 1u << 16);
        const std::string arg = sug ? "--limits buffer_mib=" + std::to_string(*sug) : "";
        for (Side* s : sides) {
            if (!s->decodable) continue;
            s->decodable = false;
            s->rep->decode_status = "not_checked";
            s->rep->decode_status_reason = "resource_limit";
            if (!overflow) s->rep->resource_need = ResourceNeed{"buffer_mib", Count{need}, Count{in.limits.buffer_bytes()}, in.limits.buffer_mib, sug, arg};
        }
        add_issue(r, code::resource_limit, "error", "run",
                  overflow ? std::string("row buffer size overflows 64-bit arithmetic")
                           : "row buffers need " + std::to_string(need) + " bytes (" + std::to_string((need + (1u << 20) - 1) >> 20) +
                                 " MiB), budget is " + std::to_string(in.limits.buffer_bytes()) + " bytes (buffer_mib=" +
                                 std::to_string(in.limits.buffer_mib) + "); " +
                                 (sug ? "rerun with " + arg : std::string("this exceeds the largest allowed buffer_mib")));
    }

    // ---- whole-image decoders: codec memory estimate against the hard cap (D10).
    // Sides decode one after the other, so each side is checked on its own,
    // together with all row buffers (which stay allocated throughout).
    const uint64_t pct = in.limits.test_codec_estimate_pct;
    uint64_t codec_need_peak = 0;
    for (Side* s : sides) {
        if (!s->decodable) continue;
        const auto est = s->dec->codec_estimate();
        if (est && *est == 0) continue;
        const auto scaled = est ? checked_mul(*est, pct) : std::nullopt;
        const auto with_rows = scaled ? checked_add(*scaled / 100, need) : std::nullopt;
        if (!with_rows || *with_rows > in.limits.codec_cap_bytes()) {
            s->decodable = false;
            s->rep->decode_status = "not_checked";
            s->rep->decode_status_reason = "resource_limit";
            // The hard cap is job_commit_mib - 64 MiB (D10): suggest the smallest job_commit_mib
            // (multiple of 256) whose cap holds the estimate.
            const auto sug = with_rows ? suggest_limit_mib(*with_rows, Limits::kCodecReserve, 256, 1u << 20) : std::nullopt;
            const std::string arg = sug ? "--limits job_commit_mib=" + std::to_string(*sug) : "";
            if (with_rows)
                s->rep->resource_need = ResourceNeed{"job_commit_mib", Count{*with_rows}, Count{in.limits.codec_cap_bytes()},
                                                     in.limits.job_commit_mib, sug, arg};
            add_issue(r, code::resource_limit, "error", s->scope,
                      !with_rows ? std::string("decoder memory estimate overflows 64-bit arithmetic")
                                 : "decoder needs about " + std::to_string(*with_rows) + " bytes (" +
                                       std::to_string((*with_rows + (1u << 20) - 1) >> 20) + " MiB: codec estimate " +
                                       std::to_string(*scaled / 100) + " + row buffers " + std::to_string(need) +
                                       "), hard cap is " + std::to_string(in.limits.codec_cap_bytes()) + " bytes = job_commit_mib (" +
                                       std::to_string(in.limits.job_commit_mib) + ") - 64 MiB; " +
                                       (sug ? "rerun with " + arg : std::string("this exceeds the largest allowed job_commit_mib")));
            continue;
        }
        codec_need_peak = std::max(codec_need_peak, *with_rows);
    }

    for (Side* s : sides)
        if (s->decodable && !open_side(*s, r, in.limits)) s->decodable = false;

    // ---- comparability
    bool comparable = compare && a.decodable && b.decodable;
    std::string not_cmp_reason;
    if (compare) {
        if (a.rep->width != b.rep->width || a.rep->height != b.rep->height) {
            not_cmp_reason = code::geometry_differs;
        } else if (a.rep->orientation.source == "unparseable" || b.rep->orientation.source == "unparseable") {
            not_cmp_reason = code::orientation_unparseable;
        } else if (a.rep->orientation.source == "unknown" || b.rep->orientation.source == "unknown") {
            not_cmp_reason = code::orientation_unknown;   // EXIF not read: equal stored values prove nothing
        } else if (a.rep->orientation.value != b.rep->orientation.value) {
            not_cmp_reason = code::orientation_differs;
        } else if (!a.layout.model.empty() && !b.layout.model.empty() &&
                   color_base(a.layout.model) != color_base(b.layout.model)) {
            not_cmp_reason = code::sample_model_changed;
        } else if (a.layout.bits != 0 && b.layout.bits != 0 && a.layout.bits != b.layout.bits) {
            not_cmp_reason = code::bit_depth_changed;
        }
        if (!not_cmp_reason.empty()) comparable = false;
        // Interlacing is storage order only; samples are compared in image order.
        if (a.rep->interlace >= 0 && b.rep->interlace >= 0 && a.rep->interlace != b.rep->interlace)
            add_issue(r, code::interlace_differs, "info", "pair",
                      "interlace " + std::to_string(a.rep->interlace) + " vs " + std::to_string(b.rep->interlace) +
                          " (storage order only; samples compared in image order)");
    }

    // ---- decode (+ compare in lockstep)
    const auto t_dec = Clock::now();
    Stats st;
    std::vector<std::string> names;
    uint32_t cmp_ch = 0;
    int a_alpha = -1, b_alpha = -1;  // canonical index of alpha, -1 when absent
    uint32_t alpha_max = 0;
    if (comparable) {
        const std::string& wide_model = has_alpha(a.layout.model) ? a.layout.model : b.layout.model;
        names = channel_names(wide_model);
        cmp_ch = static_cast<uint32_t>(names.size());
        a_alpha = has_alpha(a.layout.model) ? static_cast<int>(a.layout.channels) - 1 : -1;
        b_alpha = has_alpha(b.layout.model) ? static_cast<int>(b.layout.channels) - 1 : -1;
        alpha_max = (1u << a.layout.bits) - 1u;
        st.differing.assign(cmp_ch, 0);
        st.maxabs.assign(cmp_ch, 0);
        st.sumabs.assign(cmp_ch, U128{});
        st.sumsq.assign(cmp_ch, U128{});
    }
    const bool same_layout = comparable && a.layout.channels == b.layout.channels;
    if (comparable) {
        const uint32_t w = a.width;
        for (uint32_t y = 0; y < a.height; ++y) {
            const bool ra = a.dec->read_row(a, r);
            const bool rb = b.dec->read_row(b, r);
            if (!ra || !rb) break;
            if (same_layout && memcmp(a.canon.data(), b.canon.data(), a.canon.size() * 2) == 0) {
                st.pixels += w;
                continue;
            }
            for (uint32_t x = 0; x < w; ++x) {
                const uint16_t* pa = a.canon.data() + static_cast<size_t>(x) * a.layout.channels;
                const uint16_t* pb = b.canon.data() + static_cast<size_t>(x) * b.layout.channels;
                bool pixel_diff = false;
                for (uint32_t c = 0; c < cmp_ch; ++c) {
                    const bool is_alpha = (a_alpha >= 0 || b_alpha >= 0) && c == cmp_ch - 1;
                    const uint32_t va = is_alpha ? (a_alpha >= 0 ? pa[a_alpha] : alpha_max) : pa[c];
                    const uint32_t vb = is_alpha ? (b_alpha >= 0 ? pb[b_alpha] : alpha_max) : pb[c];
                    if (va != vb) {
                        const uint32_t d = va > vb ? va - vb : vb - va;
                        ++st.differing[c];
                        st.maxabs[c] = std::max(st.maxabs[c], d);
                        st.sumabs[c].add(d);
                        st.sumsq[c].add(static_cast<uint64_t>(d) * d);
                        pixel_diff = true;
                    }
                }
                if (pixel_diff) {
                    ++st.pixels_diff;
                    if (st.first.size() < 16) {
                        DiffEntry e{x, y, {}, {}};
                        for (uint32_t c = 0; c < a.layout.channels; ++c) e.original.push_back(pa[c]);
                        for (uint32_t c = 0; c < b.layout.channels; ++c) e.output.push_back(pb[c]);
                        st.first.push_back(e);
                    }
                }
            }
            st.pixels += w;
        }
    }
    // Sides that were not compared (or stopped) are still decoded to the end
    // so their own completeness is known.
    for (Side* s : sides)
        if (s->decodable)
            while (!s->failed && s->rows < s->height && s->dec->read_row(*s, r)) {}
    for (Side* s : sides)
        if (s->decodable) s->dec->finish(*s, r);
    const double dec_ms = ms_since(t_dec);

    // ---- sample comparison result
    SampleComparison& sc = r.samples;
    if (compare) {
        const bool complete = a.rep->decode_status == "complete" && b.rep->decode_status == "complete";
        if (!not_cmp_reason.empty()) {
            sc.status = "not_comparable";
            sc.reason = not_cmp_reason;
        } else if (!a.decodable || !b.decodable || !complete) {
            sc.status = comparable ? "incomplete" : "not_compared";
            sc.reason = "decode_incomplete";
        } else {
            sc.pixels_compared = Count{st.pixels};
            sc.pixels_differing = Count{st.pixels_diff};
            sc.status = st.pixels_diff == 0 ? "equal" : "differs";
            sc.includes_alpha = a_alpha >= 0 || b_alpha >= 0;
            sc.unit = std::string(a.layout.bits == 16 ? "uint16" : (a.layout.bits == 8 ? "uint8" : "native")) +
                      " code value; per-channel denominator = pixels_compared";
            for (uint32_t c = 0; c < cmp_ch; ++c) {
                ChannelStat cs{names[c], Count{st.differing[c]}, st.maxabs[c], 0, 0};
                if (st.pixels) {
                    cs.mae = st.sumabs[c].approx() / static_cast<double>(st.pixels);
                    cs.rmse = std::sqrt(st.sumsq[c].approx() / static_cast<double>(st.pixels));
                }
                sc.per_channel.push_back(cs);
            }
            sc.first_differences = st.first;
            if (a_alpha < 0 && b_alpha >= 0)
                add_issue(r, code::alpha_channel_added, "info", "pair", "output adds an alpha channel; original compared as fully opaque");
            if (a_alpha >= 0 && b_alpha < 0)
                add_issue(r, code::alpha_channel_removed, "info", "pair", "output drops the alpha channel; output compared as fully opaque");
        }
        // PSD merged alpha is matted with white; a straight-alpha counterpart has different semantics.
        if ((a.rep->sample_model.alpha == "matted_white") != (b.rep->sample_model.alpha == "matted_white") &&
            has_alpha(a.layout.model) && has_alpha(b.layout.model))
            add_issue(r, code::alpha_semantics_differ, "warning", "pair",
                      "PSD merged colour is blended with white where alpha < max; the other file's alpha is " +
                          (a.rep->sample_model.alpha == "matted_white" ? b.rep->sample_model.alpha : a.rep->sample_model.alpha));
        // AVIF: the pixel-verified lossless verdict, kept separate from the metadata verdict.
        for (FileReport* f : {a.rep, b.rep}) {
            if (!f->avif || f->avif->lossless_metadata != "consistent") continue;
            const FileReport* other = f == a.rep ? b.rep : a.rep;
            if (sc.status == "equal") {
                f->avif->pixel_verified = true;
                f->avif->pixel_verified_basis = "every sample (alpha included, also under A=0) equals the other file's native samples";
                add_issue(r, code::avif_pixel_verified_lossless, "info", f == a.rep ? "original" : "output",
                          "pixel-verified: samples equal the " + std::string(f == a.rep ? "output" : "original") + " (" + other->format + ")");
            } else if (sc.status == "differs") {
                f->avif->pixel_verified = false;
                f->avif->pixel_verified_basis = "samples differ from the other file";
            } else {
                f->avif->pixel_verified_basis = "not compared: " + (sc.reason.empty() ? sc.status : sc.reason);
            }
        }
        // geometry
        if (a.rep->width && b.rep->width) {
            const bool same = a.rep->width == b.rep->width && a.rep->height == b.rep->height &&
                              a.rep->orientation.value == b.rep->orientation.value &&
                              a.rep->orientation.source != "unparseable" && b.rep->orientation.source != "unparseable" &&
                              a.rep->orientation.source != "unknown" && b.rep->orientation.source != "unknown";
            const bool orient_unknown = not_cmp_reason == code::orientation_unparseable || not_cmp_reason == code::orientation_unknown;
            r.geometry.status = same ? "equal" : (orient_unknown ? "unknown" : "differs");
            r.geometry.basis = "stored size " + std::to_string(a.rep->width) + "x" + std::to_string(a.rep->height) + " vs " +
                               std::to_string(b.rep->width) + "x" + std::to_string(b.rep->height) + "; orientation " +
                               std::to_string(a.rep->orientation.value) + " (" + a.rep->orientation.source + ") vs " +
                               std::to_string(b.rep->orientation.value) + " (" + b.rep->orientation.source + ")";
        } else {
            r.geometry.status = "unknown";
        }
        if (not_cmp_reason == code::geometry_differs || not_cmp_reason == code::orientation_differs ||
            not_cmp_reason == code::bit_depth_changed || not_cmp_reason == code::sample_model_changed)
            add_issue(r, not_cmp_reason, "error", "pair",
                      not_cmp_reason == code::bit_depth_changed
                          ? "bit depth " + std::to_string(a.layout.bits) + " vs " + std::to_string(b.layout.bits)
                          : (not_cmp_reason == code::sample_model_changed ? a.layout.model + " vs " + b.layout.model
                                                                          : r.geometry.basis));

        // colour (D4, D12) and ICC (D2)
        const EffectiveColor& ca = a.rep->color;
        const EffectiveColor& cb = b.rep->color;
        const JxlEquivalence je = jxl_equivalence(*a.rep, *b.rep);
        if (ca.kind.empty() || cb.kind.empty()) {
            r.color = {"unknown", "colour information not available (decoder did not start)"};
        } else if (je.equal) {
            r.color = {"equal", je.basis};
            if (!je.note.empty()) add_issue(r, "rendering_intent_differs", "info", "pair", je.note);
        } else if ((ca.kind == "icc" && cb.kind == "webp_default_srgb") || (ca.kind == "webp_default_srgb" && cb.kind == "icc")) {
            r.color = {"unverified", "embedded ICC in one file, WebP specification default (sRGB, not declared) in the other"};
            add_issue(r, ca.kind == "icc" ? code::icc_missing_on_output : code::icc_missing_on_original, "warning", "pair",
                      "ICC profile not carried over to the WebP file");
            add_issue(r, code::color_unverified, "warning", "pair", r.color.basis);
        } else if (ca.kind == "none" && cb.kind == "none") {
            r.color = {"unknown", "neither file declares colour information"};
            add_issue(r, code::color_unknown, "warning", "pair", r.color.basis);
        } else if (!ca.key.empty() && ca.kind == cb.kind && ca.key == cb.key) {
            r.color = {"equal", ca.description};
            if (ca.signals != cb.signals)
                add_issue(r, code::low_priority_color_signal_differs, "warning", "pair",
                          "effective colour is equal, lower-priority signals differ");
        } else if (ca.kind == "icc" && cb.kind == "icc") {
            r.color = {"differs", "embedded ICC profiles differ (sha256 " + short_hash(a.rep->icc.sha256) + " vs " + short_hash(b.rep->icc.sha256) + ")"};
            add_issue(r, code::icc_differs, "warning", "pair", r.color.basis + "; appearance change is not proven");
        } else if (ca.kind == "icc" && cb.kind == "none") {
            r.color = {"differs", "ICC profile present in original, absent in output"};
            add_issue(r, code::icc_missing_on_output, "warning", "pair", r.color.basis);
        } else if (ca.kind == "none" && cb.kind == "icc") {
            r.color = {"differs", "ICC profile absent in original, present in output"};
            add_issue(r, code::icc_missing_on_original, "warning", "pair", r.color.basis);
        } else {
            r.color = {"unverified", ca.kind + " vs " + cb.kind + " (no reliable equivalence)"};
            add_issue(r, code::color_unverified, "warning", "pair", r.color.basis);
        }
        const IccInfo& ia = a.rep->icc;
        const IccInfo& ib = b.rep->icc;
        const bool sa = ia.status == "synthesized", sb = ib.status == "synthesized";
        if (ia.status == "absent" && ib.status == "absent") r.icc = {"both_absent", ""};
        else if (ia.status == "embedded" && ib.status == "embedded")
            r.icc = {ia.sha256 == ib.sha256 ? "equal" : "differs", "sha256 of decompressed profile bytes"};
        else if ((sa && ib.status == "embedded") || (ia.status == "embedded" && sb))
            r.icc = {"embedded_vs_synthesized", std::string("one side's ICC is synthesized by libjxl from its colour enum; sha256 ") +
                                                    (ia.sha256 == ib.sha256 ? "identical" : "differs")};
        else if (sa && sb) r.icc = {"both_synthesized", std::string("sha256 ") + (ia.sha256 == ib.sha256 ? "identical" : "differs")};
        else if ((sa && ib.status == "absent") || (ia.status == "absent" && sb)) r.icc = {"synthesized_vs_absent", ""};
        else if (ia.status == "embedded" && ib.status == "absent") r.icc = {"missing_on_output", ""};
        else if (ia.status == "absent" && ib.status == "embedded") r.icc = {"missing_on_original", ""};
        else r.icc = {"unreadable", ia.status + " vs " + ib.status};

        // WebP policy (plan section 2): an original larger than 16383 cannot be delivered as WebP at full size.
        if (b.rep->format == "webp" && (a.rep->width > 16383 || a.rep->height > 16383))
            add_issue(r, code::webp_policy_cannot_hold_original, "info", "pair",
                      "original is " + std::to_string(a.rep->width) + "x" + std::to_string(a.rep->height) +
                          "; under the 16383 WebP policy it cannot be delivered as WebP at its original size");

        // metadata
        static const char* fields[] = {"exif", "xmp", "text", "resolution", "time", "hdr_signals", "sbit", "ornt_private", "other_chunks"};
        bool meta_changed = false;
        for (const char* f : fields) {
            auto ia_ = a.rep->metadata.find(f);
            auto ib_ = b.rep->metadata.find(f);
            MetadataField mf = compare_item(f, ia_ == a.rep->metadata.end() ? nullptr : &ia_->second,
                                            ib_ == b.rep->metadata.end() ? nullptr : &ib_->second);
            meta_changed = meta_changed || (mf.status != "same" && mf.status != "both_absent");
            r.metadata.push_back(mf);
        }
        if (meta_changed) add_issue(r, code::metadata_changed, "info", "pair", "non-image metadata differs (reported separately, does not affect samples)");

        // size
        r.size.original_bytes = Count{a.size};
        r.size.output_bytes = Count{b.size};
        r.size.comparable = complete;
        if (a.size > 0) {
            r.size.output_ratio = static_cast<double>(b.size) / static_cast<double>(a.size);
            r.size.saving_ratio = 1.0 - *r.size.output_ratio;
        }
        if (b.size > 0) r.size.compression_factor = static_cast<double>(a.size) / static_cast<double>(b.size);
        if (!complete) r.size.note = "at least one file did not decode completely; not a valid conversion size comparison";
    } else {
        r.size.original_bytes = Count{a.size};
        r.size.comparable = false;
    }

    // ---- strict
    Strict& s = r.strict;
    if (!compare) {
        s.result = "not_evaluated";
        s.reasons.push_back("inspect_mode");
    } else {
        auto side_problem = [&](const FileReport& f) -> std::string {
            if (f.decode_status == "not_checked") return f.decode_status_reason.empty() ? "not_checked" : f.decode_status_reason;
            if (f.decode_status == "unsupported") return f.decode_status_reason.empty() ? "unsupported" : f.decode_status_reason;
            if (f.decode_status != "complete" || f.container_status == "corrupt") return code::decode_failed;
            return {};
        };
        const std::string pa = side_problem(*a.rep), pb = side_problem(*b.rep);
        if (!pa.empty() || !pb.empty()) {
            s.result = "not_evaluated";
            if (!pa.empty()) s.reasons.push_back("original:" + pa);
            if (!pb.empty()) s.reasons.push_back("output:" + pb);
        } else if (not_cmp_reason == code::orientation_unparseable || not_cmp_reason == code::orientation_unknown) {
            s.result = "indeterminate";
            s.reasons.push_back(not_cmp_reason);
        } else if (!not_cmp_reason.empty()) {
            s.result = "mismatch";
            s.reasons.push_back(not_cmp_reason);
        } else if (sc.status == "differs") {
            s.result = "mismatch";
            s.reasons.push_back(code::samples_differ);
            add_issue(r, code::samples_differ, "error", "pair",
                      std::to_string(sc.pixels_differing ? sc.pixels_differing->v : 0) + " of " +
                          std::to_string(sc.pixels_compared ? sc.pixels_compared->v : 0) + " pixels differ");
        } else {
            // Structure policy: PNG warnings are trailing data; other formats name the warning generically.
            for (const FileReport* f : {a.rep, b.rep})
                if (f->container_status != "valid")
                    s.reasons.push_back(std::string(f == a.rep ? "original:" : "output:") +
                                        (f->format == "png" ? code::trailing_data : "container_warning"));
            // Alpha semantics: only straight alpha (or none) can give a strict match (plan 3.1.4).
            for (const FileReport* f : {a.rep, b.rep})
                if (f->sample_model.alpha == "associated" || f->sample_model.alpha == "unspecified" ||
                    f->sample_model.alpha == "matted_white")
                    s.reasons.push_back(std::string(f == a.rep ? "original:" : "output:") + "alpha_" + f->sample_model.alpha);
            // A uniform PSD merged image may not be a real composite (user decision, D28).
            for (const Issue& i : r.issues)
                if (i.code == code::psd_composite_uniform && (i.scope == "original" || i.scope == "output"))
                    s.reasons.push_back(i.scope + ":" + code::psd_composite_uniform);
            // Unhandled extra channels never give a strict match (plan 3.1.4).
            for (const Issue& i : r.issues)
                if (i.code == code::extra_channel_not_supported && (i.scope == "original" || i.scope == "output"))
                    s.reasons.push_back(i.scope + ":" + code::extra_channel_not_supported);
            if (r.color.status != "equal") {
                for (const Issue& i : r.issues)
                    if (i.scope == "pair" && (i.code == code::color_unknown || i.code == code::color_unverified ||
                                              i.code == code::icc_differs || i.code == code::icc_missing_on_output ||
                                              i.code == code::icc_missing_on_original))
                        s.reasons.push_back(i.code);
                if (s.reasons.empty()) s.reasons.push_back(code::color_unverified);
            }
            s.result = s.reasons.empty() ? "match" : "indeterminate";
        }
    }

    // ---- metrics (worker side)
    r.metrics.time_ms["scan_original"] = a.scan_ms;
    if (compare) r.metrics.time_ms["scan_output"] = b.scan_ms;
    r.metrics.time_ms[compare ? "decode_and_compare" : "decode"] = dec_ms;
    r.metrics.time_ms["worker_total"] = ms_since(t_all);
    r.metrics.timing_note = compare ? "both files are decoded row by row in lockstep; decode_and_compare covers both decoders and the comparison together"
                                    : "single file decode";
    std::map<std::string, uint64_t> mem{{"tool_row_buffers", 0}, {"libpng_alloc_peak", 0}};
    for (Side* s2 : sides) {
        mem["tool_row_buffers"] += s2->canon.capacity() * 2;
        if (s2->dec) s2->dec->memory_metrics(mem);
    }
    for (const auto& [k, v] : mem) r.metrics.memory_bytes[k] = Count{v};
    r.metrics.memory_bytes["row_budget_estimate"] = Count{need};
    if (codec_need_peak) r.metrics.memory_bytes["codec_need_estimate_peak"] = Count{codec_need_peak};
    // Temporary storage: logical bytes reserved (peak, both sides) and actual I/O.
    r.metrics.memory_bytes["temp_reserved_peak"] = Count{quota.reserved()};
    r.metrics.memory_bytes["temp_written"] = Count{ta.bytes_written() + tb.bytes_written()};
    r.metrics.memory_bytes["temp_read"] = Count{ta.bytes_read() + tb.bytes_read()};
    if (quota.reserved() > 0) r.metrics.time_ms["temp_io"] = ta.io_ms() + tb.io_ms();
    PROCESS_MEMORY_COUNTERS_EX pmc{};
    if (GetProcessMemoryInfo(GetCurrentProcess(), reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&pmc), sizeof pmc)) {
        r.metrics.memory_bytes["worker_peak_working_set"] = Count{pmc.PeakWorkingSetSize};
        r.metrics.memory_bytes["worker_peak_commit"] = Count{pmc.PeakPagefileUsage};
    }
    uint64_t io = 0;
    for (Side* s2 : sides) io += s2->file->bytes_read();
    r.metrics.io_read_bytes = Count{io};
    return r;
}

}  // namespace iat
