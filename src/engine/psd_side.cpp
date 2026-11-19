// PSD / PSB front end: own structure scan (psd_scan.cpp) + own row decoder of the
// merged ("composite") image data. Raw and RLE (PackBits) are decoded row by row:
// the channels are stored as planes, so each used channel is read through its own
// small sequential stream, and no whole-image buffer or temporary file is needed.
// Samples are native integers (8/16-bit big-endian, 1-bit bitmap), never converted;
// colour modes outside Gray / RGB / Indexed / Bitmap and 32-bit float are rejected.
#include "../common/checked.hpp"
#include "png_scan.hpp"   // exif_orientation
#include "psd_scan.hpp"
#include "side.hpp"

#include <algorithm>
#include <cstring>
#include <new>

namespace iat {

namespace {

const char* mode_name(uint16_t m) {
    switch (m) {
        case 0: return "bitmap";
        case 1: return "grayscale";
        case 2: return "indexed";
        case 3: return "rgb";
        case 4: return "cmyk";
        case 7: return "multichannel";
        case 8: return "duotone";
        case 9: return "lab";
        default: return "unknown";
    }
}

const char* compression_name(uint16_t c) {
    switch (c) {
        case 0: return "raw";
        case 1: return "rle";
        case 2: return "zip";
        case 3: return "zip_prediction";
        default: return "unknown";
    }
}

constexpr uint64_t kStreamBuffer = 1ull << 20;

// Sequential reader over one channel plane.
class Stream {
public:
    void init(const ReadOnlyFile* f, uint64_t pos, uint64_t end, size_t cap) {
        f_ = f;
        pos_ = pos;
        end_ = end;
        cap_ = cap;
        buf_.clear();
        at_ = 0;
    }
    // Copies n bytes into dst; false on I/O error or when the plane ends early.
    bool take(uint8_t* dst, size_t n) {
        while (n) {
            if (at_ == buf_.size()) {
                if (pos_ >= end_) return false;
                const size_t want = static_cast<size_t>(std::min<uint64_t>(end_ - pos_, cap_));
                buf_.resize(want);
                auto got = f_->read_at(pos_, buf_.data(), want);
                if (!got || *got != want) return false;
                pos_ += want;
                at_ = 0;
            }
            const size_t k = std::min(n, buf_.size() - at_);
            memcpy(dst, buf_.data() + at_, k);
            at_ += k;
            dst += k;
            n -= k;
        }
        return true;
    }
    size_t capacity() const { return buf_.capacity(); }

private:
    const ReadOnlyFile* f_ = nullptr;
    uint64_t pos_ = 0, end_ = 0;
    size_t cap_ = 0, at_ = 0;
    std::vector<uint8_t> buf_;
};

class PsdDecoder final : public Decoder {
public:
    PsdDecoder(const PsdScan& sc, uint32_t used, uint32_t out_ch) : sc_(sc), used_(used), out_ch_(out_ch) {}

    // Optimal PackBits size of one row (literal runs of 128 + header byte): only sizes the
    // read buffer; longer valid rows are decoded in pieces (D33).
    uint64_t max_packed() const { return sc_.row_bytes + (sc_.row_bytes + 127) / 128; }

    std::optional<uint64_t> buffer_estimate() const override {
        const auto canon = checked_product({sc_.width, out_ch_, 2});
        const auto rows = checked_mul(sc_.row_bytes, used_);
        const auto packed = checked_mul(max_packed(), sc_.compression == 1 ? 1 : 0);
        const uint64_t stream = std::min<uint64_t>(kStreamBuffer, std::max<uint64_t>(sc_.row_bytes, 4096));
        const auto streams = checked_mul(stream, used_);
        const auto table = sc_.compression == 1 ? checked_product({sc_.height, used_, 4}) : std::optional<uint64_t>(0);
        if (!canon || !rows || !packed || !streams || !table) return std::nullopt;
        return checked_sum({*canon, *rows, *packed, *streams, *table, 64u << 10});
    }

    bool open(Side& s, Report& r, const Limits&) override {
        FileReport& fr = *s.rep;
        try {
            raw_.assign(used_, std::vector<uint8_t>(static_cast<size_t>(sc_.row_bytes)));
            streams_.resize(used_);
            if (sc_.compression == 1) {
                packed_.resize(static_cast<size_t>(max_packed()));
                counts_.resize(static_cast<size_t>(static_cast<uint64_t>(sc_.height) * used_));
            }
        } catch (const std::bad_alloc&) {
            return fail(s, r, "not_checked", "resource_limit", code::resource_limit, "PSD row buffers could not be allocated");
        }
        const size_t cap = static_cast<size_t>(std::min<uint64_t>(kStreamBuffer, std::max<uint64_t>(sc_.row_bytes, 4096)));
        const uint64_t data_end = sc_.data_offset + sc_.data_bytes;
        if (sc_.compression == 0) {
            const uint64_t plane = static_cast<uint64_t>(sc_.height) * sc_.row_bytes;
            for (uint32_t c = 0; c < used_; ++c) streams_[c].init(s.file, sc_.data_offset + c * plane, sc_.data_offset + (c + 1) * plane, cap);
            return true;
        }
        // RLE: row byte counts of the used channels (channel-major), and each plane's start.
        const bool wide = sc_.version == 2;
        const size_t entry = wide ? 4 : 2;
        std::vector<uint8_t> tb(static_cast<size_t>(std::min<uint64_t>(static_cast<uint64_t>(sc_.height) * entry, 1u << 20)));
        uint64_t start = sc_.data_offset;
        for (uint32_t c = 0; c < used_; ++c) {
            uint64_t off = sc_.rle_table_offset + static_cast<uint64_t>(c) * sc_.height * entry, plane = 0;
            for (uint32_t y = 0; y < sc_.height;) {
                const uint32_t n = static_cast<uint32_t>(std::min<uint64_t>(sc_.height - y, tb.size() / entry));
                auto got = s.file->read_at(off, tb.data(), static_cast<size_t>(n) * entry);
                if (!got || *got != static_cast<size_t>(n) * entry)
                    return fail(s, r, "failed", "io_error", code::decode_failed, "reading the RLE row byte-count table failed");
                for (uint32_t i = 0; i < n; ++i) {
                    const uint8_t* p = tb.data() + static_cast<size_t>(i) * entry;
                    const uint32_t v = wide ? (static_cast<uint32_t>(p[0]) << 24 | p[1] << 16 | p[2] << 8 | p[3]) : static_cast<uint32_t>(p[0] << 8 | p[1]);
                    counts_[static_cast<size_t>(c) * sc_.height + y + i] = v;
                    plane += v;
                }
                off += static_cast<uint64_t>(n) * entry;
                y += n;
            }
            streams_[c].init(s.file, start, std::min(start + plane, data_end), cap);
            start += plane;
        }
        (void)fr;
        return true;
    }

    bool read_row(Side& s, Report& r) override {
        if (s.failed) return false;
        const uint32_t y = static_cast<uint32_t>(s.rows);
        for (uint32_t c = 0; c < used_; ++c) {
            if (sc_.compression == 0) {
                if (!streams_[c].take(raw_[c].data(), raw_[c].size()))
                    return fail(s, r, s.rows ? "partial" : "failed", "io_error", code::decode_failed,
                                "channel " + std::to_string(c) + " row " + std::to_string(y) + " could not be read");
                continue;
            }
            // The row's byte count from the table is consumed exactly, in pieces of at most
            // packed_.size(); valid but non-optimal PackBits (0x80 no-ops, split runs) can be
            // longer than the optimal maximum (D33), so the count itself is not bounded here.
            uint64_t left = counts_[static_cast<size_t>(c) * sc_.height + y];
            PackBitsRow row(raw_[c].data(), raw_[c].size());
            std::string why;
            bool ok = true;
            while (left && ok) {
                const size_t k = static_cast<size_t>(std::min<uint64_t>(left, packed_.size()));
                if (!streams_[c].take(packed_.data(), k))
                    return fail(s, r, s.rows ? "partial" : "failed", "io_error", code::decode_failed,
                                "channel " + std::to_string(c) + " row " + std::to_string(y) + " could not be read");
                ok = row.feed(packed_.data(), k, why);
                left -= k;
            }
            if (!ok || !row.finish(why))
                return fail(s, r, s.rows ? "partial" : "failed", "rle_row_invalid", code::structure_error,
                            "channel " + std::to_string(c) + " row " + std::to_string(y) + ": " + why);
        }
        expand(s);
        if (uniform_) check_uniform(s);
        ++s.rows;
        return true;
    }

    void finish(Side& s, Report& r) override {
        FileReport& fr = *s.rep;
        fr.coverage.rows_decoded = s.rows;
        if (palette_oob_) {
            fr.decode_status = "failed";
            fr.decode_status_reason = "palette_index_out_of_range";
            add_issue(r, code::decode_failed, "error", s.scope, "palette index outside the declared number of colours");
            return;
        }
        if (s.failed || s.rows != sc_.height) return;
        fr.decode_status = "complete";
        fr.decode_status_reason.clear();
        if (uniform_ && fr.psd) {
            fr.psd->composite_uniform = true;
            add_issue(r, "psd_composite_uniform", "warning", s.scope,
                      "every pixel of the merged image has the same value; the composite may not be a real merge of the layers");
        }
    }

    void memory_metrics(std::map<std::string, uint64_t>& m) const override {
        uint64_t b = packed_.capacity() + counts_.capacity() * 4;
        for (const auto& v : raw_) b += v.capacity();
        for (const auto& st : streams_) b += st.capacity();
        m["tool_row_buffers"] += b;
    }

private:
    bool fail(Side& s, Report& r, const char* status, const char* reason, const char* issue_code, const std::string& msg) {
        s.failed = true;
        s.rep->decode_status = status;
        s.rep->decode_status_reason = reason;
        add_issue(r, issue_code, "error", s.scope, msg);
        return false;
    }

    uint32_t sample(uint32_t c, uint32_t x) const {
        const uint8_t* p = raw_[c].data();
        if (sc_.depth == 16) return static_cast<uint32_t>(p[2 * x] << 8 | p[2 * x + 1]);
        if (sc_.depth == 8) return p[x];
        return (p[x >> 3] >> (7 - (x & 7))) & 1u;   // bitmap
    }

    void expand(Side& s) {
        uint16_t* o = s.canon.data();
        for (uint32_t x = 0; x < sc_.width; ++x) {
            uint16_t* px = o + static_cast<size_t>(x) * out_ch_;
            if (sc_.mode == 2) {
                const uint32_t idx = sample(0, x);
                if (sc_.palette_count && idx >= *sc_.palette_count) palette_oob_ = true;
                px[0] = sc_.palette[idx];
                px[1] = sc_.palette[256 + idx];
                px[2] = sc_.palette[512 + idx];
                if (out_ch_ == 4) px[3] = static_cast<uint16_t>(sc_.transparent_index && idx == *sc_.transparent_index ? 0 : 255);
            } else if (sc_.mode == 0) {
                px[0] = static_cast<uint16_t>(sample(0, x) ? 0 : 1);   // bitmap: 1 = black -> gray code 0
            } else {
                for (uint32_t c = 0; c < out_ch_; ++c) px[c] = static_cast<uint16_t>(sample(c, x));
            }
        }
    }

    void check_uniform(const Side& s) {
        const uint16_t* p = s.canon.data();
        if (s.rows == 0) first_.assign(p, p + out_ch_);
        for (uint32_t x = 0; x < sc_.width && uniform_; ++x)
            if (memcmp(p + static_cast<size_t>(x) * out_ch_, first_.data(), out_ch_ * 2)) uniform_ = false;
    }

    PsdScan sc_;
    uint32_t used_, out_ch_;
    std::vector<std::vector<uint8_t>> raw_;   // one decoded row per used channel
    std::vector<Stream> streams_;
    std::vector<uint8_t> packed_;
    std::vector<uint32_t> counts_;            // RLE row byte counts, used channels only
    std::vector<uint16_t> first_;
    bool uniform_ = true, palette_oob_ = false;
};

void fill_metadata(FileReport& fr, const PsdScan& sc) {
    auto item = [&](const std::string& name, bool present, const std::string& status, const std::string& summary) {
        fr.metadata[name] = MetaItem{present, present ? status : "absent", summary};
    };
    if (sc.exif_status == "over_limit") item("exif", true, "over_limit", "exceeds metadata budget");
    else if (sc.exif_status == "present") {
        const int o = exif_orientation(sc.exif);
        item("exif", true, o < 0 ? "parse_failed" : "present",
             "bytes=" + std::to_string(sc.exif.size()) + " sha256=" + short_hash(sha256_hex(sc.exif.data(), sc.exif.size())) +
                 (o > 0 ? " orientation=" + std::to_string(o) : ""));
    } else item("exif", false, "", "");
    if (sc.xmp_bytes) item("xmp", true, "present", "bytes=" + std::to_string(*sc.xmp_bytes) + " sha256=" + short_hash(sc.xmp_sha256));
    else item("xmp", false, "", "");
    if (sc.iptc_bytes) item("text", true, "present", "IPTC bytes=" + std::to_string(*sc.iptc_bytes) + " sha256=" + short_hash(sc.iptc_sha256));
    else item("text", false, "", "");
    item("resolution", sc.resolution.has_value(), "present", sc.resolution.value_or(""));
    item("other_chunks", sc.resource_count > 0, "present", "image_resources=" + std::to_string(sc.resource_count));
}

}  // namespace

std::unique_ptr<Decoder> prepare_psd(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    const auto t0 = Clock::now();
    PsdScan sc = scan_psd(*s.file, s.size, lim);
    s.scan_ms = ms_since(t0);
    for (Issue i : sc.issues) { i.scope = s.scope; r.issues.push_back(i); }
    fr.container_status = sc.container_status;
    fr.metadata_status = sc.metadata_status;
    fr.chunks = sc.sections;
    fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
    if (sc.trailing_bytes) fr.coverage.trailing_bytes = Count{*sc.trailing_bytes};
    fill_metadata(fr, sc);
    if (!sc.header_ok) {
        fr.decode_status = "failed";
        fr.decode_status_reason = "structure_error";
        return nullptr;
    }
    fr.width = s.width = sc.width;
    fr.height = s.height = sc.height;
    fr.coverage.rows_total = sc.height;

    PsdInfo pi;
    pi.version = sc.version;
    pi.color_mode = mode_name(sc.mode);
    pi.depth = sc.depth;
    pi.channels = sc.channels;
    pi.compression = compression_name(sc.compression);
    pi.layer_count_field = sc.layer_count_field;
    pi.layer_count = sc.layer_count;
    pi.layers_known = sc.layers_known;
    if (sc.version_info) pi.composite = sc.has_real_merged ? "saved" : "not_saved";
    else if (sc.layers_known && sc.layer_count == 0) pi.composite = "flattened";
    else pi.composite = "flag_absent";
    fr.psd = pi;

    // Colour: embedded ICC only. PSD has no specified default colour space (D7 as for TIFF).
    std::vector<std::string> signals;
    if (sc.icc_untagged) signals.push_back("ICC untagged flag (0x0411)");
    if (sc.icc_status == "embedded") {
        fr.icc.status = "embedded";
        fr.icc.bytes = Count{sc.icc.size()};
        fr.icc.sha256 = sha256_hex(sc.icc.data(), sc.icc.size());
        signals.push_back("ICC(" + short_hash(fr.icc.sha256) + ")");
        fr.color = {"icc", "icc:" + fr.icc.sha256, "embedded ICC " + std::to_string(sc.icc.size()) + " bytes (resource 0x040F)", signals};
    } else if (sc.icc_status == "over_budget") {
        fr.icc.status = "over_budget";
        fr.color = {"icc_unreadable", "", "ICC resource exceeds the metadata budget", signals};
    } else {
        fr.color = {"none", "", "no ICC profile (PSD has no specified default colour space)", signals};
    }
    // Orientation: Photoshop stores none in the header; EXIF (0x0422) is used when present.
    if (sc.exif_status == "over_limit") {
        fr.orientation = {"unknown", 0};
        add_issue(r, code::orientation_unknown, "warning", s.scope, "EXIF exceeds the metadata budget; orientation not read");
    } else if (sc.exif_status == "present") {
        const int o = exif_orientation(sc.exif);
        if (o < 0) { fr.orientation = {"unparseable", 0}; add_issue(r, code::orientation_unparseable, "warning", s.scope, "EXIF orientation could not be parsed"); }
        else if (o == 0) fr.orientation = {"default", 1};
        else fr.orientation = {"exif", static_cast<uint32_t>(o)};
    } else {
        fr.orientation = {"default", 1};
    }

    if (sc.container_status == "corrupt") {
        fr.decode_status = "failed";
        fr.decode_status_reason = "structure_error";
        return nullptr;
    }
    if (pi.composite == "not_saved") {
        fr.decode_status = "not_checked";
        fr.decode_status_reason = "composite_not_saved";
        add_issue(r, "psd_composite_not_saved", "error", s.scope,
                  "version info (0x0421) says the file has no real merged image (saved without Maximize Compatibility); "
                  "the composite data is not compared");
        return nullptr;
    }
    if (pi.composite == "flag_absent")
        add_issue(r, "psd_composite_flag_absent", "warning", s.scope,
                  "the file has layers but no version info resource (0x0421), so it cannot tell whether the merged image is "
                  "a real composite; the stored merged image is compared as is");

    // Sample model.
    uint32_t base = 0;
    std::string model;
    switch (sc.mode) {
        case 0: base = 1; model = "gray"; break;
        case 1: base = 1; model = "gray"; break;
        case 2: base = 1; model = "rgb"; break;
        case 3: base = 3; model = "rgb"; break;
        default: break;
    }
    if (base == 0 || sc.depth == 32) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "unsupported_sample_model";
        add_issue(r, code::unsupported_sample_model, "error", s.scope,
                  std::string("PSD colour mode ") + mode_name(sc.mode) + ", " + std::to_string(sc.depth) +
                      "-bit is outside the strict scope (Bitmap, Grayscale, Indexed, RGB at 8/16-bit)");
        return nullptr;
    }
    if (sc.compression == 2 || sc.compression == 3) {
        fr.decode_status = "not_checked";
        fr.decode_status_reason = "compression_not_supported";
        add_issue(r, code::compression_not_supported, "error", s.scope,
                  std::string("merged image data compression ") + compression_name(sc.compression) + " is not supported (raw and RLE only)");
        return nullptr;
    }
    const bool merged_alpha = sc.layer_count_field < 0 && sc.mode != 2 && sc.mode != 0 && sc.channels > base;
    if (sc.layer_count_field < 0 && !merged_alpha && sc.mode != 2 && sc.mode != 0)
        add_issue(r, "psd_container_warning", "warning", s.scope, "negative layer count announces merged transparency, but there is no extra channel");
    const uint32_t used = base + (merged_alpha ? 1 : 0);
    const uint32_t ignored = sc.channels - used;
    if (ignored)
        add_issue(r, code::extra_channel_not_supported, "warning", s.scope,
                  std::to_string(ignored) + " extra channel(s) (saved selections / spot colours) are not part of the image and are not compared");

    SampleModel& sm = fr.sample_model;
    sm.container_bits = sc.depth;
    if (sc.mode == 2) {
        const bool trans = sc.transparent_index.has_value();
        s.layout = {trans ? "rgba" : "rgb", trans ? 4u : 3u, 8u};
        sm.expansion = std::string("palette") + (trans ? "+transparent index" : "") + " -> " + s.layout.model + "8";
        sm.alpha = trans ? "straight" : "none";
    } else if (sc.mode == 0) {
        s.layout = {"gray", 1, 1};
        sm.expansion = "bitmap (1 = black) inverted to gray 1-bit (0 = black)";
        sm.alpha = "none";
    } else {
        s.layout = {merged_alpha ? (base == 1 ? "gray_alpha" : "rgba") : model, used, sc.depth};
        // Photoshop blends semi-transparent merged pixels with white; the samples are kept as stored.
        sm.alpha = merged_alpha ? "matted_white" : "none";
    }
    sm.model = s.layout.model;
    sm.channel_bits.assign(s.layout.channels, s.layout.bits);
    sm.compare_type = s.layout.bits == 16 ? "uint16" : (s.layout.bits == 8 ? "uint8" : "native_codes");
    fr.psd->alpha = sm.alpha;
    fr.decode_path = std::string("psd-composite-") + compression_name(sc.compression) + "-rows";
    return std::make_unique<PsdDecoder>(sc, used, s.layout.channels);
}

}  // namespace iat
