// PNG front end: independent container scan + libpng row decoding with no
// transforms, expanded to canonical samples by explicit rules.
#include "../common/checked.hpp"
#include "png_scan.hpp"
#include "png_shim.h"
#include "side.hpp"

#include <algorithm>
#include <cstring>

namespace iat {

namespace {

RowLayout layout_for(int depth, int ctype, bool has_trns) {
    RowLayout l;
    switch (ctype) {
        case 0: l = {has_trns ? "gray_alpha" : "gray", has_trns ? 2u : 1u, static_cast<uint32_t>(depth)}; break;
        case 2: l = {has_trns ? "rgba" : "rgb", has_trns ? 4u : 3u, static_cast<uint32_t>(depth)}; break;
        case 3: l = {has_trns ? "rgba" : "rgb", has_trns ? 4u : 3u, 8u}; break;
        case 4: l = {"gray_alpha", 2, static_cast<uint32_t>(depth)}; break;
        case 6: l = {"rgba", 4, static_cast<uint32_t>(depth)}; break;
        default: break;
    }
    return l;
}

int png_channels(int ctype) {
    switch (ctype) {
        case 0: return 1;
        case 2: return 3;
        case 3: return 1;
        case 4: return 2;
        case 6: return 4;
        default: return 0;
    }
}

struct ReadCtx {
    const ReadOnlyFile* f;
    uint64_t pos = 0;
};

// Adam7 pass geometry (PNG spec 8.2), pass 0..6. Computed here, independently
// of libpng, and cross-checked against libpng's pass/row position per row.
constexpr uint32_t kXStart[7] = {0, 4, 0, 2, 0, 1, 0};
constexpr uint32_t kXInc[7] = {8, 8, 4, 4, 2, 2, 1};
constexpr uint32_t kYStart[7] = {0, 0, 4, 0, 2, 0, 1};
constexpr uint32_t kYInc[7] = {8, 8, 8, 4, 4, 2, 2};
uint32_t pass_cols(uint32_t w, int p) { return w > kXStart[p] ? (w - kXStart[p] + kXInc[p] - 1) / kXInc[p] : 0; }
uint32_t pass_rows(uint32_t h, int p) { return h > kYStart[p] ? (h - kYStart[p] + kYInc[p] - 1) / kYInc[p] : 0; }

size_t read_cb(void* io, uint8_t* buf, size_t n) {
    auto* c = static_cast<ReadCtx*>(io);
    auto got = c->f->read_at(c->pos, buf, n);
    if (!got) return static_cast<size_t>(-1);
    c->pos += *got;
    return *got;
}

// ------------------------------------------------------------ colour
EffectiveColor effective_color(const PngScan& sc, bool decoder_kept_iccp) {
    EffectiveColor c;
    char b[160];
    if (sc.cicp) {
        snprintf(b, sizeof b, "cICP(%u,%u,%u,%u)", (*sc.cicp)[0], (*sc.cicp)[1], (*sc.cicp)[2], (*sc.cicp)[3]);
        c.signals.push_back(b);
    }
    if (sc.iccp_present) c.signals.push_back("iCCP(" + (sc.icc.sha256.empty() ? sc.icc.status : short_hash(sc.icc.sha256)) + ")");
    if (sc.srgb_intent) c.signals.push_back("sRGB(intent " + std::to_string(*sc.srgb_intent) + ")");
    if (sc.gama) c.signals.push_back("gAMA(" + std::to_string(*sc.gama) + ")");
    if (sc.chrm) {
        std::string v = "cHRM(";
        for (int i = 0; i < 8; ++i) v += (i ? "," : "") + std::to_string((*sc.chrm)[i]);
        c.signals.push_back(v + ")");
    }
    // PNG precedence (D4): cICP > iCCP > sRGB > cHRM/gAMA.
    if (sc.cicp) {
        c.kind = "cicp";
        snprintf(b, sizeof b, "cicp:%u/%u/%u/%u", (*sc.cicp)[0], (*sc.cicp)[1], (*sc.cicp)[2], (*sc.cicp)[3]);
        c.key = b;
        c.description = std::string("cICP primaries/transfer/matrix/range = ") + (b + 5);
    } else if (sc.iccp_present) {
        if (sc.icc.status == "embedded" && decoder_kept_iccp) {
            c.kind = "icc";
            c.key = "icc:" + sc.icc.sha256;
            c.description = "embedded ICC " + std::to_string(sc.icc.bytes ? sc.icc.bytes->v : 0) + " bytes";
        } else {
            c.kind = "icc_unreadable";
            c.description = "iCCP present but not usable (" + sc.icc.status + (decoder_kept_iccp ? "" : ", rejected by libpng") + ")";
        }
    } else if (sc.srgb_intent) {
        c.kind = "srgb";
        c.key = "srgb";
        c.description = "sRGB chunk";
    } else if (sc.gama && sc.chrm) {
        c.kind = "chrm_gama";
        std::string k = "gama:" + std::to_string(*sc.gama) + ";chrm:";
        for (int i = 0; i < 8; ++i) k += (i ? "," : "") + std::to_string((*sc.chrm)[i]);
        c.key = k;
        c.description = "explicit gAMA + cHRM";
    } else if (sc.gama || sc.chrm) {
        c.kind = "partial";
        c.description = sc.gama ? "gAMA without cHRM (primaries unknown)" : "cHRM without gAMA (transfer unknown)";
    } else {
        c.kind = "none";
        c.description = "no colour information (PNG has no specified default)";
    }
    return c;
}

// ------------------------------------------------------------ metadata items
void fill_metadata(FileReport& r, const PngScan& sc) {
    auto item = [&](const std::string& name, bool present, const std::string& status, const std::string& summary) {
        r.metadata[name] = MetaItem{present, present ? status : "absent", summary};
    };
    if (sc.exif_status == "over_limit") item("exif", true, "over_limit", "exceeds metadata budget");
    else if (sc.exif) {
        const int o = exif_orientation(*sc.exif);
        item("exif", true, o < 0 ? "parse_failed" : "present",
             "bytes=" + std::to_string(sc.exif->size()) + " sha256=" + short_hash(sha256_hex(sc.exif->data(), sc.exif->size())) +
                 (o > 0 ? " orientation=" + std::to_string(o) : ""));
    } else item("exif", false, "", "");

    const PngText* xmp = nullptr;
    std::vector<const PngText*> others;
    for (const auto& t : sc.texts) {
        if (t.keyword_raw == "XML:com.adobe.xmp") xmp = &t;
        else others.push_back(&t);
    }
    if (xmp) item("xmp", true, xmp->status, xmp->status == "present" ? "bytes=" + std::to_string(xmp->bytes) + " sha256=" + short_hash(xmp->sha256) : xmp->status);
    else item("xmp", false, "", "");

    if (!others.empty()) {
        std::string kw, status = "present";
        Sha256 h;
        for (const PngText* t : others) {
            kw += (kw.empty() ? "" : ",") + t->kind + ":" + t->keyword;
            h.update(t->keyword_raw.data(), t->keyword_raw.size());   // raw bytes, not the UTF-8 display form
            h.update(t->sha256.data(), t->sha256.size());
            if (t->status != "present") status = t->status;
        }
        item("text", true, status, "entries=" + std::to_string(others.size()) + " keys=[" + kw + "] values_sha256=" + short_hash(h.finish_hex()));
    } else item("text", false, "", "");

    item("resolution", sc.phys.has_value(), "present", sc.phys.value_or(""));
    item("time", sc.time.has_value(), "present", sc.time.value_or(""));
    std::string hdr;
    if (sc.cicp) hdr += "cICP=" + std::to_string((*sc.cicp)[0]) + "/" + std::to_string((*sc.cicp)[1]) + "/" +
                        std::to_string((*sc.cicp)[2]) + "/" + std::to_string((*sc.cicp)[3]) + " ";
    if (sc.mdcv_hex) hdr += "mDCV=" + *sc.mdcv_hex + " ";
    if (sc.clli_hex) hdr += "cLLI=" + *sc.clli_hex;
    item("hdr_signals", !hdr.empty(), "present", hdr);
    item("sbit", !sc.sbit.empty(), "present", hex(sc.sbit));
    item("ornt_private", sc.ornt.has_value(), "present", sc.ornt ? "orNT=" + std::to_string(*sc.ornt) : "");
    std::string other;
    for (const auto& t : sc.other_ancillary) other += (other.empty() ? "" : ",") + t;
    item("other_chunks", !other.empty(), "present", other);
}

// ------------------------------------------------------------ decoder
class PngDecoder final : public Decoder {
public:
    explicit PngDecoder(PngScan scan) : scan_(std::move(scan)) {}
    ~PngDecoder() override { pngs_destroy(dec_); }

    std::optional<uint64_t> buffer_estimate() const override {
        const auto rows = png_row_budget(scan_.width, scan_.bit_depth, scan_.color_type, scan_.has_trns);
        if (!interlaced() || !rows) return rows;
        // Adam7 adds one stored row (reconstruction) and the temp write buffer.
        const RowLayout l = layout_for(scan_.bit_depth, scan_.color_type, scan_.has_trns);
        const auto stored = checked_product({scan_.width, l.channels, l.bits > 8 ? 2u : 1u});
        if (!stored) return std::nullopt;
        return checked_sum({*rows, *stored, TempFile::kWriteBuffer});
    }

    bool open(Side& s, Report& r, const Limits& lim) override {
        FileReport& fr = *s.rep;
        const auto rowbytes = checked_mul(scan_.width, static_cast<uint64_t>(png_channels(scan_.color_type)) * scan_.bit_depth);
        const uint64_t rb = rowbytes ? (*rowbytes + 7) / 8 : 0;
        // libpng keeps two rows plus zlib state; chunk payloads are capped by metadata_item.
        const auto alloc_limit = checked_sum({checked_mul(rb + 64, 2).value_or(UINT64_MAX / 4), lim.metadata_item_bytes(), 1u << 20});
        io_ = ReadCtx{s.file, 0};
        dec_ = pngs_create(read_cb, &io_, static_cast<size_t>(std::min<uint64_t>(alloc_limit.value_or(UINT64_MAX), SIZE_MAX)),
                           static_cast<size_t>(lim.metadata_item_bytes()));
        if (!dec_) {
            fr.decode_status = "failed";
            fr.decode_status_reason = "decoder_init";
            add_issue(r, code::decode_failed, "error", s.scope, "libpng could not be initialised");
            return false;
        }
        const int rc = pngs_read_info(dec_, &info_);
        if (rc != PNGS_OK) {
            s.failed = true;
            // Stopped by a budget: the file is "not checked", never "failed".
            fr.decode_status = rc == PNGS_RESOURCE ? "not_checked" : "failed";
            fr.decode_status_reason = rc == PNGS_RESOURCE ? "resource_limit" : (rc == PNGS_IO ? "io_error" : "header");
            fr.decoder_errors.push_back(pngs_error(dec_));
            add_issue(r, rc == PNGS_RESOURCE ? code::resource_limit : code::decode_failed, "error", s.scope,
                      std::string("libpng: ") + pngs_error(dec_));
            return false;
        }
        // Cross-check libpng against the independent scanner.
        if (info_.width != scan_.width || info_.height != scan_.height || info_.bit_depth != scan_.bit_depth ||
            info_.color_type != scan_.color_type || info_.interlace != scan_.interlace ||
            info_.rowbytes != rb || info_.rowbytes_after_update != info_.rowbytes) {
            s.failed = true;
            fr.decode_status = "failed";
            fr.decode_status_reason = "decoder_disagrees_with_scan";
            add_issue(r, code::decode_failed, "error", s.scope, "libpng header/rowbytes disagree with the container scan");
            return false;
        }
        fr.icc.decoder_accepted = info_.has_iccp != 0;
        fr.color = effective_color(scan_, info_.has_iccp != 0);
        if (scan_.iccp_present && !info_.has_iccp)
            add_issue(r, code::icc_rejected_by_decoder, "warning", s.scope, "libpng rejected the embedded ICC profile");
        raw_.resize(static_cast<size_t>(rb));
        if (interlaced()) {
            // Reserve the whole canonical image in temp before decoding starts.
            bps_ = s.layout.bits > 8 ? 2u : 1u;
            uint64_t total = 0;
            for (int p = 0; p < 7; ++p) {
                pass_off_[p] = total;
                const uint32_t pw = pass_cols(scan_.width, p), ph = pass_rows(scan_.height, p);
                const auto bytes = checked_product({pw, ph, s.layout.channels, bps_});
                const auto sum = bytes ? checked_add(total, *bytes) : std::nullopt;
                if (!sum) {
                    s.failed = true;
                    fr.decode_status = "not_checked";
                    fr.decode_status_reason = "resource_limit";
                    add_issue(r, code::resource_limit, "error", s.scope, "Adam7 temporary size overflows 64-bit arithmetic");
                    return false;
                }
                total = *sum;
            }
            temp_total_ = total;
            TempError e;
            if (!s.temp || !s.temp->reserve(total, e)) {
                if (!s.temp) e = {"unavailable", "no temporary file for this side"};
                temp_failure(s, r, e);
                return false;
            }
            store_.resize(static_cast<size_t>(static_cast<uint64_t>(scan_.width) * s.layout.channels * bps_));
        }
        return true;
    }

    bool read_row(Side& s, Report& r) override {
        if (s.failed) return false;
        if (interlaced()) {
            if (!passes_done_ && !decode_passes(s, r)) return false;
            if (!reconstruct_row(s, r)) return false;
            ++s.rows;
            return true;
        }
        const int rc = pngs_read_row(dec_, raw_.data());
        if (rc != PNGS_OK) return libpng_failed(s, r, rc, "row " + std::to_string(s.rows), s.rows > 0);
        expand_row(s, scan_.width);
        ++s.rows;
        return true;
    }

    void finish(Side& s, Report& r) override {
        FileReport& fr = *s.rep;
        fr.coverage.rows_decoded = s.rows;
        if (!s.failed && s.rows == scan_.height) {
            const int rc = pngs_finish(dec_);
            if (rc != PNGS_OK) {
                s.failed = true;
                fr.decode_status = rc == PNGS_RESOURCE ? "not_checked" : "partial";
                fr.decode_status_reason = rc == PNGS_RESOURCE ? "resource_limit" : "after_image_data";
                fr.decoder_errors.push_back(pngs_error(dec_));
                add_issue(r, code::decode_failed, "error", s.scope, std::string("libpng after image data: ") + pngs_error(dec_));
            } else {
                fr.decode_status = "complete";
                fr.decode_status_reason.clear();
            }
        }
        record_decoder_messages(s, r);
        if (palette_oob_) {
            fr.decode_status = "failed";
            fr.decode_status_reason = "palette_index_out_of_range";
            add_issue(r, code::decode_failed, "error", s.scope, "palette index outside PLTE");
        }
        // A complete decode with a corrupt container (e.g. trailing chunk CRC) is not complete.
        if (fr.decode_status == "complete" && fr.container_status == "corrupt") {
            fr.decode_status = "partial";
            fr.decode_status_reason = "container_corrupt";
        }
    }

    void memory_metrics(std::map<std::string, uint64_t>& m) const override {
        m["tool_row_buffers"] += raw_.capacity() + store_.capacity();
        if (dec_) m["libpng_alloc_peak"] += pngs_alloc_peak(dec_);
    }

private:
    bool interlaced() const { return scan_.interlace == 1; }

    bool libpng_failed(Side& s, Report& r, int rc, const std::string& where, bool some_data) {
        s.failed = true;
        s.rep->decode_status = rc == PNGS_RESOURCE ? "not_checked" : (some_data ? "partial" : "failed");
        s.rep->decode_status_reason = rc == PNGS_RESOURCE ? "resource_limit" : (rc == PNGS_IO ? "io_error" : "decode_error");
        s.rep->decoder_errors.push_back(pngs_error(dec_));
        add_issue(r, rc == PNGS_RESOURCE ? code::resource_limit : code::decode_failed, "error", s.scope,
                  "libpng at " + where + ": " + pngs_error(dec_));
        return false;
    }

    // Adam7: decode every pass's reduced rows in file order, expand them to
    // canonical samples, and append them to temp (pass after pass).
    bool decode_passes(Side& s, Report& r) {
        const uint32_t ch = s.layout.channels;
        uint64_t pass_rows_read = 0;
        TempError e;
        for (int p = 0; p < 7; ++p) {
            const uint32_t pw = pass_cols(scan_.width, p), ph = pass_rows(scan_.height, p);
            if (pw == 0 || ph == 0) continue;  // libpng skips empty passes as well
            for (uint32_t ry = 0; ry < ph; ++ry) {
                int lp = -1;
                uint32_t lr = 0;
                pngs_position(dec_, &lp, &lr);
                if (lp != p || lr != ry) {
                    s.failed = true;
                    s.rep->decode_status = pass_rows_read ? "partial" : "failed";
                    s.rep->decode_status_reason = "decoder_disagrees_with_scan";
                    add_issue(r, code::decode_failed, "error", s.scope,
                              "libpng Adam7 position pass " + std::to_string(lp) + " row " + std::to_string(lr) +
                                  " differs from expected pass " + std::to_string(p) + " row " + std::to_string(ry));
                    return false;
                }
                const int rc = pngs_read_row(dec_, raw_.data());
                if (rc != PNGS_OK)
                    return libpng_failed(s, r, rc, "Adam7 pass " + std::to_string(p + 1) + " row " + std::to_string(ry),
                                         pass_rows_read > 0);
                ++pass_rows_read;
                expand_row(s, pw);  // into s.canon[0 .. pw*ch)
                pack(s.canon.data(), static_cast<size_t>(pw) * ch);
                if (!s.temp->append(store_.data(), static_cast<size_t>(pw) * ch * bps_, e)) {
                    temp_failure(s, r, e);
                    return false;
                }
            }
        }
        if (!s.temp->flush(e)) { temp_failure(s, r, e); return false; }
        if (s.temp->written() != temp_total_) {
            temp_failure(s, r, {"internal", "Adam7 passes wrote " + std::to_string(s.temp->written()) + " of " +
                                                std::to_string(temp_total_) + " bytes"});
            return false;
        }
        passes_done_ = true;
        pass_rows_read_ = pass_rows_read;
        return true;
    }

    // Rebuild image row s.rows from the passes that contain it. Every column
    // must be supplied exactly once; anything else is an internal error.
    bool reconstruct_row(Side& s, Report& r) {
        const uint32_t ch = s.layout.channels;
        const uint32_t y = static_cast<uint32_t>(s.rows);
        uint64_t filled = 0;
        TempError e;
        for (int p = 0; p < 7; ++p) {
            const uint32_t pw = pass_cols(scan_.width, p);
            if (pw == 0 || y < kYStart[p] || (y - kYStart[p]) % kYInc[p] != 0) continue;
            const uint64_t ry = (y - kYStart[p]) / kYInc[p];
            const size_t row_bytes = static_cast<size_t>(pw) * ch * bps_;
            if (!s.temp->read_at(pass_off_[p] + ry * row_bytes, store_.data(), row_bytes, e)) {
                temp_failure(s, r, e);
                return false;
            }
            for (uint32_t k = 0; k < pw; ++k) {
                uint16_t* dst = s.canon.data() + (static_cast<size_t>(kXStart[p]) + static_cast<size_t>(k) * kXInc[p]) * ch;
                for (uint32_t c = 0; c < ch; ++c) {
                    const size_t i = (static_cast<size_t>(k) * ch + c) * bps_;
                    dst[c] = bps_ == 2 ? static_cast<uint16_t>(store_[i] | (store_[i + 1] << 8)) : store_[i];
                }
            }
            filled += pw;
        }
        if (filled != scan_.width) {
            temp_failure(s, r, {"internal", "Adam7 row " + std::to_string(y) + " covered " + std::to_string(filled) +
                                                " of " + std::to_string(scan_.width) + " columns"});
            return false;
        }
        return true;
    }

    // Canonical samples -> stored bytes (1 byte up to 8 bits, else 2 bytes LE).
    void pack(const uint16_t* src, size_t n) {
        if (bps_ == 1)
            for (size_t i = 0; i < n; ++i) store_[i] = static_cast<uint8_t>(src[i]);
        else
            for (size_t i = 0; i < n; ++i) store_[2 * i] = static_cast<uint8_t>(src[i]), store_[2 * i + 1] = static_cast<uint8_t>(src[i] >> 8);
    }

    // Expand one libpng row (no transforms) into canonical samples.
    void expand_row(Side& s, uint32_t width) {
        const uint8_t* row = raw_.data();
        uint16_t* o = s.canon.data();
        const int depth = info_.bit_depth, ct = info_.color_type, in_ch = png_channels(ct);
        const uint32_t out_ch = s.layout.channels;
        auto packed = [&](size_t i) -> uint32_t {
            if (depth == 16) return (static_cast<uint32_t>(row[2 * i]) << 8) | row[2 * i + 1];
            if (depth == 8) return row[i];
            const size_t bit = i * static_cast<size_t>(depth);
            const unsigned shift = 8u - static_cast<unsigned>(depth) - static_cast<unsigned>(bit & 7u);
            return (row[bit >> 3] >> shift) & ((1u << depth) - 1u);
        };
        const uint32_t maxv = (1u << s.layout.bits) - 1u;
        for (uint32_t x = 0; x < width; ++x) {
            uint16_t* px = o + static_cast<size_t>(x) * out_ch;
            if (ct == 3) {
                const uint32_t idx = packed(x);
                if (idx >= static_cast<uint32_t>(info_.npal)) palette_oob_ = true;  // reported as decode failure
                const uint32_t k = idx < static_cast<uint32_t>(info_.npal) ? idx : 0;
                px[0] = info_.pal[k * 3];
                px[1] = info_.pal[k * 3 + 1];
                px[2] = info_.pal[k * 3 + 2];
                if (out_ch == 4) px[3] = static_cast<int>(idx) < info_.ntrns ? info_.trns_alpha[idx] : 255;
            } else {
                for (int c = 0; c < in_ch; ++c) px[c] = static_cast<uint16_t>(packed(static_cast<size_t>(x) * in_ch + c));
                if (static_cast<uint32_t>(in_ch) < out_ch) {  // tRNS colour key -> alpha
                    const bool key = ct == 0 ? px[0] == info_.trns_gray
                                             : (px[0] == info_.trns_r && px[1] == info_.trns_g && px[2] == info_.trns_b);
                    px[in_ch] = static_cast<uint16_t>(key ? 0 : maxv);
                }
            }
        }
    }

    void record_decoder_messages(Side& s, Report& r) {
        if (!dec_) return;
        for (int i = 0; i < pngs_warning_count(dec_); ++i) {
            const std::string w = pngs_warning(dec_, i);
            s.rep->decoder_warnings.push_back(w);
            // libpng reports a broken zTXt/iTXt/iCCP only as a warning (docs/decisions.md S3).
            if (w.find("zTXt") != std::string::npos || w.find("iTXt") != std::string::npos ||
                w.find("iCCP") != std::string::npos || w.find("tEXt") != std::string::npos) {
                if (s.rep->metadata_status == "valid") s.rep->metadata_status = "parse_failed";
                add_issue(r, code::metadata_parse_failed, "warning", s.scope, "libpng: " + w);
            } else {
                add_issue(r, code::decoder_warning, "warning", s.scope, "libpng: " + w);
            }
        }
    }

    PngScan scan_;
    pngs* dec_ = nullptr;
    pngs_info info_{};
    ReadCtx io_{};
    std::vector<uint8_t> raw_;  // libpng row
    bool palette_oob_ = false;  // index >= PLTE entries: invalid, never substituted
    // Adam7 only
    uint32_t bps_ = 1;
    uint64_t pass_off_[7] = {};
    uint64_t temp_total_ = 0, pass_rows_read_ = 0;
    bool passes_done_ = false;
    std::vector<uint8_t> store_;  // one stored row (pass row or reconstruction)
};

}  // namespace

std::optional<uint64_t> png_row_budget(uint32_t width, int bit_depth, int color_type, bool has_trns) {
    const RowLayout l = layout_for(bit_depth, color_type, has_trns);
    const auto bits = checked_product({width, static_cast<uint64_t>(png_channels(color_type)), static_cast<uint64_t>(bit_depth)});
    if (!bits || l.channels == 0) return std::nullopt;
    const uint64_t rb = (*bits + 7) / 8;
    // tool row buffer + libpng row and previous row + canonical uint16 row + fixed overhead
    const auto canon = checked_product({width, l.channels, 2});
    const auto rows3 = checked_mul(rb + 64, 3);
    if (!canon || !rows3) return std::nullopt;
    return checked_sum({*rows3, *canon, 256u << 10});
}

std::unique_ptr<Decoder> prepare_png(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    const auto t0 = Clock::now();
    PngScan sc = scan_png(*s.file, s.size, lim);
    s.scan_ms = ms_since(t0);
    for (Issue i : sc.issues) { i.scope = s.scope; r.issues.push_back(i); }
    fr.container_status = sc.container_status;
    fr.metadata_status = sc.metadata_status;
    fr.chunks = sc.chunk_order;
    fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
    if (sc.ended) fr.coverage.trailing_bytes = Count{sc.trailing_bytes};
    fr.icc = sc.icc;
    fill_metadata(fr, sc);
    if (!sc.has_ihdr) { fr.decode_status = "failed"; fr.decode_status_reason = "no_valid_ihdr"; return nullptr; }

    fr.width = s.width = sc.width;
    fr.height = s.height = sc.height;
    fr.interlace = sc.interlace;  // taken from IHDR, never inferred
    s.layout = layout_for(sc.bit_depth, sc.color_type, sc.has_trns);
    fr.coverage.rows_total = sc.height;
    SampleModel& sm = fr.sample_model;
    sm.model = s.layout.model;
    sm.container_bits = static_cast<uint32_t>(sc.bit_depth);
    sm.channel_bits.assign(s.layout.channels, s.layout.bits);
    sm.compare_type = s.layout.bits == 16 ? "uint16" : (s.layout.bits == 8 ? "uint8" : "native_codes");
    sm.alpha = has_alpha(s.layout.model) ? "straight" : "none";
    if (sc.color_type == 3) sm.expansion = std::string("palette") + (sc.has_trns ? "+tRNS" : "") + " -> " + s.layout.model + "8";
    else if (sc.has_trns) sm.expansion = "tRNS colour key -> alpha (key=0, else max)";
    fr.decode_path = sc.interlace == 1 ? "libpng-adam7-passes-no-transform+temp" : "libpng-rows-no-transform";

    // Orientation: Exif in eXIf, otherwise the PNG default (no rotation).
    if (sc.exif_status == "over_limit") {
        // Not read, so not known: never assume the PNG default (D21).
        fr.orientation = {"unknown", 0};
        add_issue(r, code::orientation_unknown, "warning", s.scope, "eXIf exceeds the metadata budget; orientation not read");
    } else if (sc.exif) {
        const int o = exif_orientation(*sc.exif);
        if (o < 0) { fr.orientation = {"unparseable", 0}; add_issue(r, code::orientation_unparseable, "warning", s.scope, "eXIf orientation could not be parsed"); }
        else if (o == 0) fr.orientation = {"default", 1};
        else fr.orientation = {"exif", static_cast<uint32_t>(o)};
    } else {
        fr.orientation = {"default", 1};
    }

    if (sc.animated) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "animation";
        add_issue(r, code::animation_not_supported, "error", s.scope, "APNG (acTL/fcTL/fdAT) is not supported");
        return nullptr;
    }
    if (sc.interlace != 0 && sc.interlace != 1) {  // the scanner already flags this; never guess
        fr.decode_status = "failed";
        fr.decode_status_reason = "invalid_interlace_method";
        return nullptr;
    }
    return std::make_unique<PngDecoder>(std::move(sc));
}

}  // namespace iat
