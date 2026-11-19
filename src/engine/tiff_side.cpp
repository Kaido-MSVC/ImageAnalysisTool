// TIFF / BigTIFF front end: independent container scan + libtiff strip/tile
// decoding (TIFFReadEncodedStrip/Tile, never the 8-bit RGBA helpers), expanded
// to canonical rows. libtiff is opened through TIFFClientOpenExt on our own
// read-only handle with memory mapping disabled and allocation limits set.
#include "../common/checked.hpp"
#include "../common/text.hpp"
#include "side.hpp"
#include "tiff_scan.hpp"

#include <tiffio.h>

#include <algorithm>
#include <cstdarg>
#include <cstring>

namespace iat {

namespace {

struct TiffIo {
    const ReadOnlyFile* f = nullptr;
    uint64_t size = 0, pos = 0;
    std::vector<std::string> errors, warnings;
    bool limit_hit = false;
};

tmsize_t on_read(thandle_t h, void* buf, tmsize_t n) {
    auto* c = static_cast<TiffIo*>(h);
    if (n <= 0) return 0;
    auto got = c->f->read_at(c->pos, buf, static_cast<size_t>(n));
    if (!got) return -1;
    c->pos += *got;
    return static_cast<tmsize_t>(*got);
}
tmsize_t on_write(thandle_t, void*, tmsize_t) { return -1; }  // read-only, always
toff_t on_seek(thandle_t h, toff_t off, int whence) {
    auto* c = static_cast<TiffIo*>(h);
    uint64_t base = whence == SEEK_CUR ? c->pos : (whence == SEEK_END ? c->size : 0);
    const auto t = checked_add(base, off);
    if (!t) return static_cast<toff_t>(-1);
    c->pos = *t;
    return *t;
}
int on_close(thandle_t) { return 0; }  // the handle belongs to the worker
toff_t on_size(thandle_t h) { return static_cast<TiffIo*>(h)->size; }
int on_map(thandle_t, void**, toff_t*) { return 0; }  // never memory-map the input
void on_unmap(thandle_t, void*, toff_t) {}

std::string vfmt(const char* module, const char* fmt, va_list ap) {
    char buf[512];
    vsnprintf(buf, sizeof buf, fmt, ap);
    // libtiff may print tag strings or bytes from the file (and truncation may cut a sequence).
    return file_text(std::string(module ? module : "") + ": " + buf);
}
int on_error(TIFF*, void* user, const char* module, const char* fmt, va_list ap) {
    auto* c = static_cast<TiffIo*>(user);
    std::string m = vfmt(module, fmt, ap);
    // libtiff 4.7.2 wording for its open-option allocation limits (docs/decisions.md S6).
    if (m.find("byte limit defined in open options") != std::string::npos) c->limit_hit = true;
    if (c->errors.size() < 32) c->errors.push_back(std::move(m));
    return 1;
}
int on_warning(TIFF*, void* user, const char* module, const char* fmt, va_list ap) {
    auto* c = static_cast<TiffIo*>(user);
    if (c->warnings.size() < 32) c->warnings.push_back(vfmt(module, fmt, ap));
    return 1;
}

bool compression_in_scope(uint16_t c) { return c == 1 || c == 5 || c == 8 || c == 32946 || c == 32773; }

void fill_metadata(FileReport& r, const TiffScan& sc) {
    auto item = [&](const std::string& name, const std::optional<std::string>& v, const std::string& status) {
        r.metadata[name] = MetaItem{v.has_value(), v ? status : "absent", v.value_or("")};
    };
    item("exif", sc.exif, sc.exif_status);
    item("xmp", sc.xmp, sc.xmp_status);
    item("text", sc.text, sc.text_status);
    item("resolution", sc.resolution, "present");
    item("time", sc.datetime, "present");
    item("hdr_signals", std::nullopt, "");
    item("sbit", std::nullopt, "");
    item("ornt_private", std::nullopt, "");
    std::string other;
    for (const auto& t : sc.other_tags) other += (other.empty() ? "" : ",") + t;
    item("other_chunks", other.empty() ? std::nullopt : std::optional<std::string>(other), "present");
}

EffectiveColor effective_color(const TiffScan& sc) {
    EffectiveColor c;
    if (sc.icc.status != "absent") c.signals.push_back("ICCProfile(" + (sc.icc.sha256.empty() ? sc.icc.status : short_hash(sc.icc.sha256)) + ")");
    for (const auto& t : sc.colorimetry) c.signals.push_back(t);
    if (sc.icc.status == "embedded") {
        c.kind = "icc";
        c.key = "icc:" + sc.icc.sha256;
        c.description = "embedded ICC " + std::to_string(sc.icc.bytes ? sc.icc.bytes->v : 0) + " bytes";
    } else if (sc.icc.status != "absent") {
        c.kind = "icc_unreadable";
        c.description = "ICC profile tag present but not usable (" + sc.icc.status + ")";
    } else if (!sc.colorimetry.empty()) {
        c.kind = "tiff_colorimetry";
        c.description = "TIFF colorimetry tags without ICC (no reliable equivalence)";
    } else {
        c.kind = "none";
        c.description = "no ICC profile (TIFF has no specified default colour space)";
    }
    return c;
}

enum class Mode { strips, tile_band, tile_temp };

class TiffDecoder final : public Decoder {
public:
    TiffDecoder(TiffScan scan, const Limits& lim) : scan_(std::move(scan)), lim_(lim) {}
    ~TiffDecoder() override {
        if (tif_) TIFFClose(tif_);
    }

    // Opens libtiff (directory only; allocations capped) and derives the
    // block geometry libtiff will actually use (strip chopping may apply).
    bool start(Side& s, Report& r) {
        FileReport& fr = *s.rep;
        io_.f = s.file;
        io_.size = s.size;
        TIFFOpenOptions* opts = TIFFOpenOptionsAlloc();
        TIFFOpenOptionsSetErrorHandlerExtR(opts, on_error, &io_);
        TIFFOpenOptionsSetWarningHandlerExtR(opts, on_warning, &io_);
        // Single libtiff allocation: at most the row/band budget (or one metadata item);
        // all libtiff allocations together: budget + per-file metadata allowance.
        const uint64_t single = std::max(lim_.buffer_bytes(), lim_.metadata_item_bytes());
        const uint64_t total = lim_.buffer_bytes() + lim_.metadata_file_bytes();
        TIFFOpenOptionsSetMaxSingleMemAlloc(opts, static_cast<tmsize_t>(single));
        TIFFOpenOptionsSetMaxCumulatedMemAlloc(opts, static_cast<tmsize_t>(total));
        // "r" read-only, "m" no memory mapping (on_map also refuses).
        tif_ = TIFFClientOpenExt("input", "rm", &io_, on_read, on_write, on_seek, on_close, on_size, on_map, on_unmap, opts);
        TIFFOpenOptionsFree(opts);
        if (!tif_) return fail(s, r, "libtiff could not open the first directory", false);

        uint32_t w = 0, h = 0;
        uint16_t bits = 0, spp = 0, comp = 0, photo = 0, planar = 0;
        TIFFGetField(tif_, TIFFTAG_IMAGEWIDTH, &w);
        TIFFGetField(tif_, TIFFTAG_IMAGELENGTH, &h);
        TIFFGetFieldDefaulted(tif_, TIFFTAG_BITSPERSAMPLE, &bits);
        TIFFGetFieldDefaulted(tif_, TIFFTAG_SAMPLESPERPIXEL, &spp);
        TIFFGetFieldDefaulted(tif_, TIFFTAG_COMPRESSION, &comp);
        TIFFGetField(tif_, TIFFTAG_PHOTOMETRIC, &photo);
        TIFFGetFieldDefaulted(tif_, TIFFTAG_PLANARCONFIG, &planar);
        const bool tiled = TIFFIsTiled(tif_) != 0;
        if (w != scan_.width || h != scan_.height || bits != scan_.bits || spp != scan_.spp || comp != scan_.compression ||
            photo != scan_.photometric || planar != scan_.planar || tiled != scan_.tiled ||
            (TIFFIsBigTIFF(tif_) != 0) != scan_.big) {
            fr.decode_status = "failed";
            fr.decode_status_reason = "decoder_disagrees_with_scan";
            add_issue(r, code::decode_failed, "error", s.scope, "libtiff directory disagrees with the container scan");
            return false;
        }
        bps_ = bits / 8u;
        ch_ = spp;
        px_ = static_cast<uint64_t>(ch_) * bps_;
        const auto row_bytes = checked_mul(w, px_);
        if (!row_bytes) return overflow(s, r);
        row_bytes_ = *row_bytes;
        if (tiled) {
            TIFFGetField(tif_, TIFFTAG_TILEWIDTH, &tw_);
            TIFFGetField(tif_, TIFFTAG_TILELENGTH, &tl_);
            if (tw_ != scan_.tile_width || tl_ != scan_.tile_length) {
                fr.decode_status = "failed";
                fr.decode_status_reason = "decoder_disagrees_with_scan";
                add_issue(r, code::decode_failed, "error", s.scope, "libtiff tile size disagrees with the container scan");
                return false;
            }
            across_ = (w + tw_ - 1) / tw_;
            down_ = (h + tl_ - 1) / tl_;
            const auto tb = checked_product({tw_, tl_, px_});
            if (!tb || TIFFTileSize64(tif_) != *tb) return disagree(s, r, "tile byte size");
            block_bytes_ = *tb;
            const auto band = checked_mul(*tb, across_);
            if (!band) return overflow(s, r);
            // One band (a full row of tiles) in memory if it fits half the budget; otherwise
            // tiles go to temp one at a time and rows are rebuilt from there.
            mode_ = *band <= lim_.buffer_bytes() / 2 ? Mode::tile_band : Mode::tile_temp;
            band_bytes_ = *band;
            total_blocks_ = static_cast<uint64_t>(across_) * down_;
            if (TIFFNumberOfTiles(tif_) != total_blocks_) return disagree(s, r, "tile count");
        } else {
            uint32_t rps = 0;
            TIFFGetFieldDefaulted(tif_, TIFFTAG_ROWSPERSTRIP, &rps);
            rps_ = std::min<uint32_t>(rps == 0 ? h : rps, h);  // after libtiff's strip chopping
            nstrips_ = TIFFNumberOfStrips(tif_);
            if (nstrips_ != (h + rps_ - 1) / rps_) return disagree(s, r, "strip count");
            const auto sb = checked_mul(row_bytes_, rps_);
            if (!sb || TIFFStripSize64(tif_) != *sb) return disagree(s, r, "strip byte size");
            block_bytes_ = *sb;
            mode_ = Mode::strips;
            total_blocks_ = nstrips_;
        }
        fr.coverage.rows_total = h;
        return true;
    }

    std::optional<uint64_t> buffer_estimate() const override {
        const auto canon_row = checked_product({scan_.width, ch_, 2});
        if (!canon_row) return std::nullopt;
        switch (mode_) {
            case Mode::strips: return checked_sum({block_bytes_, *canon_row, 256u << 10});
            case Mode::tile_band: return checked_sum({band_bytes_, *canon_row, 256u << 10});
            case Mode::tile_temp: return checked_sum({block_bytes_, row_bytes_, *canon_row, TempFile::kWriteBuffer, 256u << 10});
        }
        return std::nullopt;
    }

    bool open(Side& s, Report& r, const Limits&) override {
        if (mode_ == Mode::tile_temp) {
            const auto total = checked_mul(row_bytes_, scan_.height);
            TempError e;
            if (!total) return overflow(s, r);
            if (!s.temp || !s.temp->reserve(*total, e)) {
                if (!s.temp) e = {"unavailable", "no temporary file for this side"};
                temp_failure(s, r, e);
                return false;
            }
            row_.resize(static_cast<size_t>(row_bytes_));
            s.rep->decode_path += "+temp";
        }
        buf_.resize(static_cast<size_t>(mode_ == Mode::tile_band ? band_bytes_ : block_bytes_));
        return true;
    }

    bool read_row(Side& s, Report& r) override {
        if (s.failed) return false;
        const uint32_t y = static_cast<uint32_t>(s.rows);
        const uint8_t* src = nullptr;
        if (mode_ == Mode::strips) {
            const uint32_t strip = y / rps_;
            if (strip != loaded_) {
                const uint32_t rows = std::min<uint32_t>(rps_, scan_.height - strip * rps_);
                const tmsize_t want = static_cast<tmsize_t>(row_bytes_ * rows);
                const tmsize_t got = TIFFReadEncodedStrip(tif_, strip, buf_.data(), want);
                if (got != want) return block_failed(s, r, "strip " + std::to_string(strip), got);
                loaded_ = strip;
                ++blocks_read_;
            }
            src = buf_.data() + static_cast<size_t>(y - loaded_ * rps_) * row_bytes_;
            convert(s, src, 0, scan_.width);
        } else if (mode_ == Mode::tile_band) {
            const uint32_t ty = y / tl_;
            if (ty != loaded_) {
                for (uint32_t tx = 0; tx < across_; ++tx) {
                    const tmsize_t got = TIFFReadEncodedTile(tif_, ty * across_ + tx, buf_.data() + tx * block_bytes_,
                                                             static_cast<tmsize_t>(block_bytes_));
                    if (got != static_cast<tmsize_t>(block_bytes_)) return block_failed(s, r, "tile " + std::to_string(ty * across_ + tx), got);
                    ++blocks_read_;
                }
                loaded_ = ty;
            }
            const uint32_t yy = y - ty * tl_;
            for (uint32_t tx = 0; tx < across_; ++tx) {
                const uint32_t x0 = tx * tw_, vw = std::min(tw_, scan_.width - x0);  // padding columns excluded
                convert(s, buf_.data() + tx * block_bytes_ + static_cast<size_t>(yy) * tw_ * px_, x0, vw);
            }
        } else {
            if (!tiles_to_temp_ && !tiles_to_temp(s, r)) return false;
            if (!row_from_temp(s, r, y)) return false;
        }
        ++s.rows;
        return true;
    }

    void finish(Side& s, Report& r) override {
        FileReport& fr = *s.rep;
        fr.coverage.rows_decoded = s.rows;
        for (const auto& w : io_.warnings) {
            fr.decoder_warnings.push_back(w);
            add_issue(r, code::decoder_warning, "warning", s.scope, "libtiff: " + w);
        }
        if (!s.failed && s.rows == scan_.height) {
            if (blocks_read_ != total_blocks_) {
                fr.decode_status = "partial";
                fr.decode_status_reason = "blocks_not_covered";
                add_issue(r, code::decode_failed, "error", s.scope,
                          "read " + std::to_string(blocks_read_) + " of " + std::to_string(total_blocks_) + " strips/tiles");
                return;
            }
            fr.decode_status = "complete";
            fr.decode_status_reason.clear();
        }
    }

    void memory_metrics(std::map<std::string, uint64_t>& m) const override {
        m["tool_row_buffers"] += buf_.capacity() + row_.capacity();
    }

private:
    bool fail(Side& s, Report& r, const std::string& what, bool some_data) {
        s.failed = true;
        FileReport& fr = *s.rep;
        for (const auto& e : io_.errors) fr.decoder_errors.push_back(e);
        if (io_.limit_hit) {
            fr.decode_status = "not_checked";
            fr.decode_status_reason = "resource_limit";
            add_issue(r, code::resource_limit, "error", s.scope, "libtiff allocation limit: " + (io_.errors.empty() ? what : io_.errors.back()));
            return false;
        }
        fr.decode_status = some_data ? "partial" : "failed";
        fr.decode_status_reason = "decode_error";
        add_issue(r, code::decode_failed, "error", s.scope, "libtiff: " + what + (io_.errors.empty() ? "" : " (" + io_.errors.back() + ")"));
        return false;
    }
    bool block_failed(Side& s, Report& r, const std::string& block, tmsize_t got) {
        return fail(s, r, block + " decoded " + std::to_string(got) + " bytes, expected a complete block", s.rows > 0 || blocks_read_ > 0);
    }
    bool disagree(Side& s, Report& r, const std::string& what) {
        s.rep->decode_status = "failed";
        s.rep->decode_status_reason = "decoder_disagrees_with_scan";
        add_issue(r, code::decode_failed, "error", s.scope, "libtiff " + what + " disagrees with the expected layout");
        return false;
    }
    bool overflow(Side& s, Report& r) {
        s.rep->decode_status = "not_checked";
        s.rep->decode_status_reason = "resource_limit";
        add_issue(r, code::resource_limit, "error", s.scope, "TIFF buffer size overflows 64-bit arithmetic");
        return false;
    }

    // Native samples (libtiff returns host byte order) -> canonical row at x0.
    void convert(Side& s, const uint8_t* src, uint32_t x0, uint32_t n) {
        uint16_t* dst = s.canon.data() + static_cast<size_t>(x0) * ch_;
        const size_t count = static_cast<size_t>(n) * ch_;
        if (bps_ == 2) memcpy(dst, src, count * 2);
        else for (size_t i = 0; i < count; ++i) dst[i] = src[i];
        if (scan_.photometric == 0) {  // MINISWHITE -> MINISBLACK at the same depth (alpha untouched)
            const uint16_t maxv = static_cast<uint16_t>((1u << (bps_ * 8)) - 1u);
            for (size_t i = 0; i < count; i += ch_) dst[i] = static_cast<uint16_t>(maxv - dst[i]);
        }
    }

    // Large tiles: every tile's valid area is appended to temp in file order.
    bool tiles_to_temp(Side& s, Report& r) {
        TempError e;
        for (uint32_t ty = 0; ty < down_; ++ty)
            for (uint32_t tx = 0; tx < across_; ++tx) {
                const tmsize_t got = TIFFReadEncodedTile(tif_, ty * across_ + tx, buf_.data(), static_cast<tmsize_t>(block_bytes_));
                if (got != static_cast<tmsize_t>(block_bytes_)) return block_failed(s, r, "tile " + std::to_string(ty * across_ + tx), got);
                ++blocks_read_;
                const uint32_t vw = std::min(tw_, scan_.width - tx * tw_), vh = std::min(tl_, scan_.height - ty * tl_);
                for (uint32_t yy = 0; yy < vh; ++yy)
                    if (!s.temp->append(buf_.data() + static_cast<size_t>(yy) * tw_ * px_, static_cast<size_t>(vw * px_), e)) {
                        temp_failure(s, r, e);
                        return false;
                    }
            }
        if (!s.temp->flush(e)) { temp_failure(s, r, e); return false; }
        if (s.temp->written() != s.temp->reserved()) {
            temp_failure(s, r, {"internal", "tiles wrote " + std::to_string(s.temp->written()) + " of " +
                                                std::to_string(s.temp->reserved()) + " bytes"});
            return false;
        }
        tiles_to_temp_ = true;
        return true;
    }

    // Temp layout: band ty starts at ty*tl*row_bytes; inside a band, tile tx
    // starts at vh*x0*px (all earlier tiles in the band are full width).
    bool row_from_temp(Side& s, Report& r, uint32_t y) {
        const uint32_t ty = y / tl_, yy = y - ty * tl_, vh = std::min(tl_, scan_.height - ty * tl_);
        const uint64_t band = static_cast<uint64_t>(ty) * tl_ * row_bytes_;
        TempError e;
        for (uint32_t tx = 0; tx < across_; ++tx) {
            const uint32_t x0 = tx * tw_, vw = std::min(tw_, scan_.width - x0);
            const uint64_t off = band + static_cast<uint64_t>(vh) * x0 * px_ + static_cast<uint64_t>(yy) * vw * px_;
            if (!s.temp->read_at(off, row_.data() + x0 * px_, static_cast<size_t>(vw * px_), e)) {
                temp_failure(s, r, e);
                return false;
            }
        }
        convert(s, row_.data(), 0, scan_.width);
        return true;
    }

    TiffScan scan_;
    Limits lim_;
    TiffIo io_;
    TIFF* tif_ = nullptr;
    Mode mode_ = Mode::strips;
    uint32_t bps_ = 1, ch_ = 1;
    uint64_t px_ = 1, row_bytes_ = 0, block_bytes_ = 0, band_bytes_ = 0;
    uint32_t rps_ = 0, nstrips_ = 0, tw_ = 0, tl_ = 0, across_ = 0, down_ = 0;
    uint32_t loaded_ = UINT32_MAX;
    uint64_t blocks_read_ = 0, total_blocks_ = 0;
    bool tiles_to_temp_ = false;
    std::vector<uint8_t> buf_, row_;
};

void unsupported(Side& s, Report& r, const std::string& reason, const char* issue_code, const std::string& msg) {
    s.rep->decode_status = "unsupported";
    s.rep->decode_status_reason = reason;
    add_issue(r, issue_code, "error", s.scope, msg);
}

}  // namespace

std::string libtiff_version() {
    std::string v = TIFFGetVersion();
    return v.substr(0, v.find('\n'));
}

std::unique_ptr<Decoder> prepare_tiff(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    const auto t0 = Clock::now();
    TiffScan sc = scan_tiff(*s.file, s.size, lim);
    s.scan_ms = ms_since(t0);
    for (Issue i : sc.issues) { i.scope = s.scope; r.issues.push_back(i); }
    fr.container_status = sc.container_status;
    fr.metadata_status = sc.metadata_status;
    fr.chunks = sc.tag_list;
    fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
    fr.icc = sc.icc;
    fill_metadata(fr, sc);
    if (!sc.has_ifd || sc.container_status == "corrupt" || sc.width == 0) {
        fr.decode_status = "failed";
        fr.decode_status_reason = "structure_error";
        return nullptr;
    }
    fr.width = s.width = sc.width;
    fr.height = s.height = sc.height;
    fr.coverage.rows_total = sc.height;
    fr.color = effective_color(sc);
    fr.decode_path = std::string("libtiff-") + (sc.tiled ? "tiles" : "strips") + "-encoded-no-rgba";
    if (!sc.orientation) fr.orientation = {"default", 1};
    else if (*sc.orientation >= 1 && *sc.orientation <= 8) fr.orientation = {"tiff_tag", *sc.orientation};
    else {
        fr.orientation = {"unparseable", 0};
        add_issue(r, code::orientation_unparseable, "warning", s.scope, "Orientation tag value " + std::to_string(*sc.orientation) + " is invalid");
    }

    // ---- support matrix (docs/capability-matrix.md), checked before any codec runs
    if (sc.next_ifd != 0) {
        unsupported(s, r, "multi_image", code::multi_image_not_supported, "TIFF has more than one image (next IFD present)");
        return nullptr;
    }
    if (sc.subifds) {
        unsupported(s, r, "subifd", code::multi_image_not_supported, "TIFF has SubIFDs (tag 330); not supported");
        return nullptr;
    }
    if (!compression_in_scope(sc.compression) || !TIFFIsCODECConfigured(sc.compression)) {
        // Not decoded at all: the file is "not checked", with the reason stated.
        fr.decode_status = "not_checked";
        fr.decode_status_reason = "compression_not_supported";
        add_issue(r, code::compression_not_supported, "error", s.scope,
                  "TIFF compression " + tiff_compression_name(sc.compression) + " (" + std::to_string(sc.compression) +
                      ") is outside the supported set (none, LZW, Deflate, PackBits); image data was not checked");
        return nullptr;
    }
    std::string why;
    const uint32_t base = sc.photometric == 2 ? 3u : 1u;
    if (sc.planar != 1) why = "separate planes (PlanarConfiguration=2)";
    else if (sc.sample_format != 1) why = "SampleFormat " + std::to_string(sc.sample_format) + " (only unsigned integer)";
    else if (sc.bits != 8 && sc.bits != 16) why = "BitsPerSample " + std::to_string(sc.bits) + " (only 8 or 16, equal per channel)";
    else if (sc.photometric > 2) why = "Photometric " + std::to_string(sc.photometric) + " (only MinIsWhite, MinIsBlack, RGB)";
    else if (sc.spp < base || sc.spp > base + 1 || sc.extra_samples.size() != sc.spp - base)
        why = "SamplesPerPixel " + std::to_string(sc.spp) + " with " + std::to_string(sc.extra_samples.size()) + " extra samples";
    else if (sc.predictor != 1 && sc.predictor != 2) why = "Predictor " + std::to_string(sc.predictor);
    if (!why.empty()) {
        unsupported(s, r, "sample_model", code::unsupported_sample_model, "TIFF " + why + " is not supported");
        return nullptr;
    }
    const bool alpha = sc.spp == base + 1;
    s.layout = {base == 3 ? (alpha ? "rgba" : "rgb") : (alpha ? "gray_alpha" : "gray"), sc.spp, sc.bits};
    SampleModel& sm = fr.sample_model;
    sm.model = s.layout.model;
    sm.container_bits = sc.bits;
    sm.channel_bits.assign(sc.spp, sc.bits);
    sm.compare_type = sc.bits == 16 ? "uint16" : "uint8";
    sm.alpha = !alpha ? "none" : (sc.extra_samples[0] == 2 ? "straight" : (sc.extra_samples[0] == 1 ? "associated" : "unspecified"));
    if (sc.photometric == 0) sm.expansion = "MinIsWhite -> MinIsBlack at the same depth (max - v)";

    auto dec = std::make_unique<TiffDecoder>(std::move(sc), lim);
    if (!dec->start(s, r)) return nullptr;
    return dec;
}

}  // namespace iat
