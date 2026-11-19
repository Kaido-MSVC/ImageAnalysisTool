// AVIF: own ISOBMFF scan (avif_scan.cpp) + libavif 1.4.2 with the aom 3.15.2 decoder.
//
// Lossless is judged in two separate steps (docs/decisions.md D30):
//  * metadata: 4:4:4 with matrix_coefficients 0 (identity, GBR) or 4:0:0, full range,
//    straight alpha, one still image. These are only necessary conditions; whether
//    the AV1 frames were coded losslessly is not visible through libavif.
//  * pixels: compare mode only, set by the engine after the sample comparison.
// Files whose metadata is not consistent with lossless are not decoded (as lossy WebP):
// decoding would need chroma upsampling or a YUV->RGB matrix, i.e. a colour conversion.
// Decoding never calls avifImageYUVToRGB: the planes are read directly (identity:
// Y = G, U = B, V = R; 4:0:0: Y = gray), 8-bit as uint8 and 10/12-bit as uint16 values.
#include "../common/checked.hpp"
#include "avif_scan.hpp"
#include "png_scan.hpp"   // exif_orientation
#include "side.hpp"
#include "whole_image.hpp"

#include <avif/avif.h>

#include <algorithm>
#include <cstring>
#include <new>

namespace iat {

namespace {

// avifIO over the read-only input handle: positional reads, nothing loaded up front.
struct FileIO {
    avifIO io{};
    const ReadOnlyFile* f = nullptr;
    uint64_t size = 0;
    std::vector<uint8_t> buf;
    bool io_error = false;

    static avifResult read(avifIO* io, uint32_t, uint64_t offset, size_t n, avifROData* out) {
        auto* self = reinterpret_cast<FileIO*>(io);
        if (offset > self->size) return AVIF_RESULT_IO_ERROR;
        const size_t take = static_cast<size_t>(std::min<uint64_t>(n, self->size - offset));
        try {
            self->buf.resize(take);
        } catch (const std::bad_alloc&) {
            return AVIF_RESULT_OUT_OF_MEMORY;
        }
        if (take) {
            auto got = self->f->read_at(offset, self->buf.data(), take);
            if (!got || *got != take) { self->io_error = true; return AVIF_RESULT_IO_ERROR; }
        }
        out->data = self->buf.data();
        out->size = take;
        return AVIF_RESULT_OK;
    }
    void init(const ReadOnlyFile* file, uint64_t file_size) {
        f = file;
        size = file_size;
        io.destroy = nullptr;   // owned here, not by the decoder
        io.read = &FileIO::read;
        io.write = nullptr;
        io.sizeHint = file_size;
        io.persistent = AVIF_FALSE;
        io.data = nullptr;
    }
};

struct DecoderHandle {
    avifDecoder* d = nullptr;
    FileIO fio;
    ~DecoderHandle() {
        if (d) {
            d->io = nullptr;   // fio is not heap-owned by libavif
            avifDecoderDestroy(d);
        }
    }
    bool create(const ReadOnlyFile* f, uint64_t size) {
        d = avifDecoderCreate();
        if (!d) return false;
        d->maxThreads = 1;
        d->strictFlags = AVIF_STRICT_ENABLED;
        d->imageContentToDecode = AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA;   // no gain map, no sample transforms
        d->imageSizeLimit = AVIF_DEFAULT_IMAGE_SIZE_LIMIT;
        d->imageDimensionLimit = AVIF_DEFAULT_IMAGE_DIMENSION_LIMIT;
        fio.init(f, size);
        avifDecoderSetIO(d, &fio.io);
        return true;
    }
};

const char* yuv_name(avifPixelFormat f) {
    switch (f) {
        case AVIF_PIXEL_FORMAT_YUV444: return "444";
        case AVIF_PIXEL_FORMAT_YUV422: return "422";
        case AVIF_PIXEL_FORMAT_YUV420: return "420";
        case AVIF_PIXEL_FORMAT_YUV400: return "400";
        default: return "none";
    }
}

class AvifDecoder final : public WholeImageDecoder {
public:
    AvifDecoder(uint64_t file_size, uint32_t w, uint32_t h, uint32_t planes, uint32_t depth, bool alpha)
        : file_size_(file_size), alpha_(alpha), planes_(planes) {
        set_geometry(w, h, planes + (alpha ? 1 : 0), depth);
        // libavif keeps the item payload (bounded by the file size), the AV1 decoder's frame
        // buffers and libavif's own copy of the planes. 6 bytes per sample covers a 16-bit
        // frame in both places plus aom's reference buffer; checked against the Job limit
        // in the large-image e2e (D31).
        const uint64_t samples_per_px = planes + (alpha ? 1 : 0);
        const auto px = checked_mul(w, h);
        const auto body = px ? checked_product({*px, samples_per_px, 6}) : std::nullopt;
        codec_estimate_ = body ? checked_sum({*body, file_size_, 48ull << 20}) : std::nullopt;
    }

protected:
    bool decode_all(Side& s, Report& r, const Limits&) override {
        DecoderHandle h;
        if (!h.create(s.file, file_size_)) { fail_resource(s, r, "avifDecoderCreate failed (out of memory)"); return false; }
        avifResult res = avifDecoderParse(h.d);
        if (res == AVIF_RESULT_OK) res = avifDecoderNextImage(h.d);
        if (res != AVIF_RESULT_OK) {
            s.rep->decoder_errors.push_back(std::string("libavif: ") + avifResultToString(res) +
                                            (h.d->diag.error[0] ? std::string(" (") + h.d->diag.error + ")" : ""));
            if (res == AVIF_RESULT_OUT_OF_MEMORY) fail_resource(s, r, std::string("libavif: ") + avifResultToString(res));
            else fail_decode(s, r, res == AVIF_RESULT_TRUNCATED_DATA ? "truncated" : (h.fio.io_error ? "io_error" : "decode_error"),
                             std::string("libavif: ") + avifResultToString(res), false);
            return false;
        }
        const avifImage* im = h.d->image;
        const uint32_t w = s.width, hh = s.height;
        if (im->width != w || im->height != hh || !im->yuvPlanes[0] || (planes_ == 3 && (!im->yuvPlanes[1] || !im->yuvPlanes[2])) ||
            (alpha_ && !im->alphaPlane)) {
            fail_decode(s, r, "decoder_disagrees_with_parse", "decoded planes do not match the parsed header", false);
            return false;
        }
        const bool hi = im->depth > 8;
        const uint32_t ch = planes_ + (alpha_ ? 1 : 0);
        std::vector<uint16_t> row(static_cast<size_t>(w) * ch);
        // canonical order: Gray or R, G, B, then A. identity: R = V (plane 2), G = Y (0), B = U (1).
        const int map3[3] = {2, 0, 1};
        for (uint32_t y = 0; y < hh; ++y) {
            for (uint32_t c = 0; c < ch; ++c) {
                const bool is_a = alpha_ && c == ch - 1;
                const int p = is_a ? -1 : (planes_ == 3 ? map3[c] : 0);
                const uint8_t* base = is_a ? im->alphaPlane + static_cast<size_t>(y) * im->alphaRowBytes
                                           : im->yuvPlanes[p] + static_cast<size_t>(y) * im->yuvRowBytes[p];
                for (uint32_t x = 0; x < w; ++x)
                    row[static_cast<size_t>(x) * ch + c] = hi ? reinterpret_cast<const uint16_t*>(base)[x] : base[x];
            }
            put(y, 0, w, row.data());
        }
        return true;
    }

private:
    uint64_t file_size_;
    bool alpha_;
    uint32_t planes_;
};

void fill_metadata(FileReport& fr, const avifImage* im, const AvifScan& sc) {
    auto item = [&](const std::string& name, bool present, const std::string& status, const std::string& summary) {
        fr.metadata[name] = MetaItem{present, present ? status : "absent", summary};
    };
    if (im->exif.size) {
        std::vector<uint8_t> e(im->exif.data, im->exif.data + im->exif.size);
        const int o = exif_orientation(e);
        item("exif", true, o < 0 ? "parse_failed" : "present",
             "bytes=" + std::to_string(e.size()) + " sha256=" + short_hash(sha256_hex(e.data(), e.size())) +
                 (o > 0 ? " orientation=" + std::to_string(o) : ""));
    } else item("exif", false, "", "");
    if (im->xmp.size) item("xmp", true, "present", "bytes=" + std::to_string(im->xmp.size) + " sha256=" + short_hash(sha256_hex(im->xmp.data, im->xmp.size)));
    else item("xmp", false, "", "");
    std::string hdr;
    if (im->clli.maxCLL || im->clli.maxPALL) hdr = "clli=" + std::to_string(im->clli.maxCLL) + "/" + std::to_string(im->clli.maxPALL);
    item("hdr_signals", !hdr.empty(), "present", hdr);
    std::string boxes;
    for (const auto& b : sc.boxes) boxes += (boxes.empty() ? "" : ",") + b;
    item("other_chunks", !boxes.empty(), "present", "boxes=" + boxes);
}

}  // namespace

std::string libavif_version() {
    return std::string(avifVersion()) + " (" + avifCodecName(AVIF_CODEC_CHOICE_AUTO, AVIF_CODEC_FLAG_CAN_DECODE) + ")";
}

std::unique_ptr<Decoder> prepare_avif(Side& s, Report& r, const Limits& lim) {
    FileReport& fr = *s.rep;
    const auto t0 = Clock::now();
    AvifScan sc = scan_avif(*s.file, s.size);
    for (Issue i : sc.issues) { i.scope = s.scope; r.issues.push_back(i); }
    fr.container_status = sc.container_status;
    fr.chunks = sc.boxes;
    fr.coverage.bytes_scanned = Count{sc.bytes_scanned};
    if (sc.container_status != "corrupt") fr.coverage.trailing_bytes = Count{0};   // boxes run to the end of the file
    fr.metadata_status = "valid";
    if (sc.container_status == "corrupt") {
        s.scan_ms = ms_since(t0);
        fr.decode_status = "failed";
        fr.decode_status_reason = "structure_error";
        return nullptr;
    }
    // Header parse (no pixels) for the lossless metadata judgement.
    DecoderHandle h;
    if (!h.create(s.file, s.size)) {
        fr.decode_status = "not_checked";
        fr.decode_status_reason = "resource_limit";
        add_issue(r, code::resource_limit, "error", s.scope, "avifDecoderCreate failed");
        return nullptr;
    }
    const avifResult pr = avifDecoderParse(h.d);
    s.scan_ms = ms_since(t0);
    if (pr != AVIF_RESULT_OK) {
        const std::string msg = std::string("libavif: ") + avifResultToString(pr) + (h.d->diag.error[0] ? std::string(" (") + h.d->diag.error + ")" : "");
        fr.decoder_errors.push_back(msg);
        if (pr == AVIF_RESULT_OUT_OF_MEMORY) {
            fr.decode_status = "not_checked";
            fr.decode_status_reason = "resource_limit";
            add_issue(r, code::resource_limit, "error", s.scope, msg);
        } else {
            fr.container_status = "corrupt";
            fr.decode_status = "failed";
            fr.decode_status_reason = pr == AVIF_RESULT_TRUNCATED_DATA ? "truncated" : "structure_error";
            add_issue(r, pr == AVIF_RESULT_TRUNCATED_DATA ? code::truncated : code::structure_error, "error", s.scope, msg);
        }
        return nullptr;
    }
    const avifImage* im = h.d->image;
    fr.width = s.width = im->width;
    fr.height = s.height = im->height;
    fr.coverage.rows_total = im->height;
    fill_metadata(fr, im, sc);

    AvifInfo ai;
    ai.yuv_format = yuv_name(im->yuvFormat);
    ai.depth = im->depth;
    ai.color_primaries = im->colorPrimaries;
    ai.transfer_characteristics = im->transferCharacteristics;
    ai.matrix_coefficients = im->matrixCoefficients;
    ai.full_range = im->yuvRange == AVIF_RANGE_FULL;
    ai.cicp_source = sc.nclx.empty() ? "av1_sequence_header_or_default" : "colr_nclx";
    ai.alpha = h.d->alphaPresent != 0;
    ai.alpha_premultiplied = im->alphaPremultiplied != 0;
    ai.image_count = static_cast<uint32_t>(std::max(h.d->imageCount, 0));

    // Colour: ICC wins over CICP (AVIF / HEIF: a 'colr' prof replaces nclx for colour).
    char key[64];
    snprintf(key, sizeof key, "cicp:%u/%u/%u/%u", ai.color_primaries, ai.transfer_characteristics, ai.matrix_coefficients, ai.full_range ? 1 : 0);
    std::vector<std::string> signals{std::string("CICP(") + (key + 5) + ", " + ai.cicp_source + ")"};
    if (im->icc.size) {
        if (im->icc.size > lim.metadata_item_bytes()) {
            fr.icc.status = "over_budget";
            fr.color = {"icc_unreadable", "", "ICC exceeds the metadata budget", signals};
        } else {
            fr.icc.status = "embedded";
            fr.icc.bytes = Count{im->icc.size};
            fr.icc.sha256 = sha256_hex(im->icc.data, im->icc.size);
            signals.push_back("ICC(" + short_hash(fr.icc.sha256) + ")");
            fr.color = {"icc", "icc:" + fr.icc.sha256, "embedded ICC " + std::to_string(im->icc.size) + " bytes ('colr' prof)", signals};
        }
    } else if (!sc.nclx.empty()) {
        // Explicit nclx: compared as a CICP key with the matrix forced to 0 / full range, as PNG cICP
        // only ever describes RGB (identity matrix, full range). The real values stay in avif.*.
        char k2[64];
        snprintf(k2, sizeof k2, "cicp:%u/%u/0/1", ai.color_primaries, ai.transfer_characteristics);
        fr.color = {"cicp", k2, std::string("AVIF nclx primaries/transfer = ") + std::to_string(ai.color_primaries) + "/" +
                                    std::to_string(ai.transfer_characteristics) + " (RGB after identity decode)", signals};
    } else {
        fr.color = {"none", "", "no 'colr' box (AV1 sequence header values only; not an explicit declaration)", signals};
    }

    // Orientation: irot / imir are authoritative in AVIF; EXIF orientation stays metadata.
    const bool rot = (im->transformFlags & AVIF_TRANSFORM_IROT) != 0, mir = (im->transformFlags & AVIF_TRANSFORM_IMIR) != 0;
    if (rot || mir) fr.orientation = {"avif_irot_imir", avif_orientation(rot ? im->irot.angle : 0, mir ? im->imir.axis : -1)};
    else fr.orientation = {"default", 1};

    // Lossless metadata judgement (necessary conditions only).
    std::vector<std::string>& why = ai.lossless_metadata_reasons;
    if (im->yuvFormat == AVIF_PIXEL_FORMAT_YUV420 || im->yuvFormat == AVIF_PIXEL_FORMAT_YUV422) why.push_back("chroma_subsampled");
    if (im->yuvFormat == AVIF_PIXEL_FORMAT_YUV444 && im->matrixCoefficients != AVIF_MATRIX_COEFFICIENTS_IDENTITY)
        why.push_back(im->matrixCoefficients == AVIF_MATRIX_COEFFICIENTS_YCGCO_RE || im->matrixCoefficients == AVIF_MATRIX_COEFFICIENTS_YCGCO_RO
                          ? "matrix_ycgco_r_not_supported"
                          : "matrix_not_identity");
    if (im->yuvRange != AVIF_RANGE_FULL) why.push_back("limited_range");
    if (ai.alpha_premultiplied) why.push_back("alpha_premultiplied");
    ai.lossless_metadata = why.empty() ? "consistent" : "inconsistent";
    ai.pixel_verified_basis = "needs_reference";   // the engine sets the verdict in compare mode
    fr.avif = ai;

    if (ai.image_count > 1 || sc.sequence) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "animation";
        add_issue(r, code::animation_not_supported, "error", s.scope, "AVIF image sequence (avis) is not supported");
        return nullptr;
    }
    if (im->transformFlags & AVIF_TRANSFORM_CLAP) {
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "clap_not_supported";
        add_issue(r, "avif_clap_not_supported", "error", s.scope, "clean aperture ('clap') crop is not supported; stored and displayed sizes differ");
        return nullptr;
    }
    if (!why.empty()) {
        std::string list;
        for (const auto& w : why) list += (list.empty() ? "" : ", ") + w;
        fr.decode_status = "unsupported";
        fr.decode_status_reason = "not_lossless_by_metadata";
        add_issue(r, "avif_not_lossless_by_metadata", "error", s.scope,
                  "metadata is not consistent with lossless (" + list + "; " + ai.yuv_format + ", matrix " +
                      std::to_string(ai.matrix_coefficients) + ", " + (ai.full_range ? "full" : "limited") +
                      " range); not decoded, decoding would need a colour conversion");
        return nullptr;
    }
    const bool gray = im->yuvFormat == AVIF_PIXEL_FORMAT_YUV400;
    const uint32_t planes = gray ? 1 : 3;
    s.layout = {gray ? (ai.alpha ? "gray_alpha" : "gray") : (ai.alpha ? "rgba" : "rgb"), planes + (ai.alpha ? 1u : 0u), im->depth};
    SampleModel& sm = fr.sample_model;
    sm.model = s.layout.model;
    sm.container_bits = im->depth;
    sm.channel_bits.assign(s.layout.channels, im->depth);
    sm.compare_type = im->depth > 8 ? "native_codes" : "uint8";
    sm.alpha = ai.alpha ? "straight" : "none";
    sm.expansion = gray ? "" : "identity matrix: G = Y, B = U, R = V (planes read directly)";
    fr.decode_path = gray ? "libavif-aom-planes-400" : "libavif-aom-planes-identity";
    add_issue(r, "avif_lossless_metadata_only", "info", s.scope,
              "metadata is consistent with lossless (necessary conditions only; AV1 lossless coding is not visible). "
              "Pixel verification needs a compare against the original.");
    return std::make_unique<AvifDecoder>(s.size, im->width, im->height, planes, im->depth, ai.alpha);
}

}  // namespace iat
