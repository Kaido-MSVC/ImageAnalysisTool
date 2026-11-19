// Encodes a raw truth file into JPEG XL, WebP or AVIF with the libjxl / libwebp / libavif (aom) encoder APIs.
// The samples come from tools/gen_fixtures.py (the truth); this tool only transports
// them, and never decodes. Containers (VP8X/ICCP/EXIF, JXL boxes) and corruptions are
// built by gen_fixtures.py from the bare outputs written here.
//
//   gen_codec_fixture jxl  <raw> <out> W H CH BITS [--lossy] [--orient N] [--premul] [--depth]
//                          [--color srgb|pq|hlg|icc:<file>] [--effort N]
//                          [--anim <raw2>] [--layer <raw2> X Y LW LH]
//                          [--float-input] [--exp E]
//   gen_codec_fixture webp <raw> <out> W H CH [--lossy]
//   gen_codec_fixture avif <raw> <out> W H CH BITS [--lossy Q] [--yuv 444|422|420|400] [--matrix M]
//                          [--primaries P] [--transfer T] [--range full|limited] [--premul]
//                          [--irot A] [--imir X] [--speed S]
//
// AVIF: lossless unless --lossy; identity matrix + 4:4:4/4:0:0 + full range writes the truth
// samples straight into the planes (no RGB->YUV conversion). See encode_avif().
//
// raw: interleaved samples, uint8 when BITS <= 8, else little-endian uint16; with
// --float-input little-endian float32 in 0..1 (JXL_TYPE_FLOAT). BITS and --exp describe
// what the codestream declares (e.g. 32 + --exp 8 for float32, 24 for 24-bit integer);
// used only for out-of-range sample models the tool must reject. CH includes
// alpha (last) for 2 and 4. --anim adds a second animation frame, --layer a cropped
// REPLACE layer of LW x LH at (X, Y) composited into a still image.
#include "probe_common.h"

#include <jxl/encode.h>
#include <webp/encode.h>
#include <avif/avif.h>

#include <string>
#include <vector>

namespace {

bool read_all(const wchar_t* path, std::vector<uint8_t>& out) {
    FILE* f = _wfopen(path, L"rb");
    if (!f) return false;
    uint8_t buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) out.insert(out.end(), buf, buf + n);
    fclose(f);
    return true;
}

bool write_all(const wchar_t* path, const uint8_t* p, size_t n) {
    FILE* f = _wfopen(path, L"wb");
    if (!f) return false;
    const bool ok = fwrite(p, 1, n, f) == n;
    fclose(f);
    return ok;
}

struct JxlOpts {
    uint32_t w = 0, h = 0, ch = 3, bits = 16, orient = 1, exp = 0;
    int effort = 3;
    bool lossy = false, premul = false, depth = false, float_in = false;
    std::string color = "srgb";
    std::vector<uint8_t> icc, anim;
    std::vector<uint8_t> layer;
    uint32_t lx = 0, ly = 0, lw = 0, lh = 0;
};

bool jxl_frame(JxlEncoderFrameSettings* fs, const JxlOpts& o, const std::vector<uint8_t>& raw, uint32_t w, uint32_t h) {
    JxlPixelFormat pf{o.ch, o.float_in ? JXL_TYPE_FLOAT : (o.bits <= 8 ? JXL_TYPE_UINT8 : JXL_TYPE_UINT16), JXL_LITTLE_ENDIAN, 0};
    const size_t need = static_cast<size_t>(w) * h * o.ch * (o.float_in ? 4 : (o.bits <= 8 ? 1 : 2));
    if (raw.size() != need) { fprintf(stderr, "raw size %zu, expected %zu\n", raw.size(), need); return false; }
    // Integer depths other than 8/16: the truth values are codestream values (0 .. 2^bits-1).
    // libjxl's default (FROM_PIXEL_FORMAT) would rescale UINT16 input by 65535/(2^bits-1)
    // (encode.h, JxlEncoderSetFrameBitDepth), which does not transport the truth.
    if (!o.float_in && o.bits != 8 && o.bits != 16) {
        JxlBitDepth bd{JXL_BIT_DEPTH_FROM_CODESTREAM, 0, 0};
        if (JxlEncoderSetFrameBitDepth(fs, &bd) != JXL_ENC_SUCCESS) return false;
    }
    if (JxlEncoderAddImageFrame(fs, &pf, raw.data(), raw.size()) != JXL_ENC_SUCCESS) return false;
    if (o.depth) {
        std::vector<uint16_t> z(static_cast<size_t>(w) * h, 0x1234);
        JxlPixelFormat zf{1, JXL_TYPE_UINT16, JXL_LITTLE_ENDIAN, 0};
        if (JxlEncoderSetExtraChannelBuffer(fs, &zf, z.data(), z.size() * 2, (o.ch == 2 || o.ch == 4) ? 1 : 0) != JXL_ENC_SUCCESS)
            return false;
    }
    return true;
}

int encode_jxl(const std::vector<uint8_t>& raw, const wchar_t* out, const JxlOpts& o) {
    JxlEncoder* enc = JxlEncoderCreate(nullptr);
    JxlBasicInfo bi;
    JxlEncoderInitBasicInfo(&bi);
    bi.xsize = o.w;
    bi.ysize = o.h;
    bi.bits_per_sample = o.bits;
    bi.exponent_bits_per_sample = o.exp;
    const bool alpha = o.ch == 2 || o.ch == 4;
    bi.num_color_channels = o.ch >= 3 ? 3 : 1;
    bi.num_extra_channels = (alpha ? 1 : 0) + (o.depth ? 1 : 0);
    bi.alpha_bits = alpha ? o.bits : 0;
    bi.alpha_premultiplied = o.premul ? JXL_TRUE : JXL_FALSE;
    bi.uses_original_profile = o.lossy ? JXL_FALSE : JXL_TRUE;
    bi.orientation = static_cast<JxlOrientation>(o.orient);
    if (!o.anim.empty()) {
        bi.have_animation = JXL_TRUE;
        bi.animation.tps_numerator = 10;
        bi.animation.tps_denominator = 1;
    }
    bool ok = JxlEncoderSetBasicInfo(enc, &bi) == JXL_ENC_SUCCESS;
    if (ok && alpha) {
        JxlExtraChannelInfo ei;
        JxlEncoderInitExtraChannelInfo(JXL_CHANNEL_ALPHA, &ei);
        ei.bits_per_sample = o.bits;
        ei.alpha_premultiplied = bi.alpha_premultiplied;
        ok = JxlEncoderSetExtraChannelInfo(enc, 0, &ei) == JXL_ENC_SUCCESS;
    }
    if (ok && o.depth) {
        JxlExtraChannelInfo ei;
        JxlEncoderInitExtraChannelInfo(JXL_CHANNEL_DEPTH, &ei);
        ei.bits_per_sample = 16;
        ok = JxlEncoderSetExtraChannelInfo(enc, alpha ? 1 : 0, &ei) == JXL_ENC_SUCCESS;
    }
    if (ok) {
        if (o.color.rfind("icc:", 0) == 0) {
            ok = JxlEncoderSetICCProfile(enc, o.icc.data(), o.icc.size()) == JXL_ENC_SUCCESS;
        } else {
            JxlColorEncoding ce;
            JxlColorEncodingSetToSRGB(&ce, bi.num_color_channels == 1 ? JXL_TRUE : JXL_FALSE);
            if (o.color == "pq" || o.color == "hlg") {
                ce.primaries = JXL_PRIMARIES_2100;
                ce.transfer_function = o.color == "pq" ? JXL_TRANSFER_FUNCTION_PQ : JXL_TRANSFER_FUNCTION_HLG;
                ce.rendering_intent = JXL_RENDERING_INTENT_RELATIVE;
            }
            ok = JxlEncoderSetColorEncoding(enc, &ce) == JXL_ENC_SUCCESS;
        }
    }
    JxlEncoderFrameSettings* fs = JxlEncoderFrameSettingsCreate(enc, nullptr);
    JxlEncoderFrameSettingsSetOption(fs, JXL_ENC_FRAME_SETTING_EFFORT, o.effort);
    if (o.lossy) ok = ok && JxlEncoderSetFrameDistance(fs, 1.0f) == JXL_ENC_SUCCESS;
    else ok = ok && JxlEncoderSetFrameLossless(fs, JXL_TRUE) == JXL_ENC_SUCCESS;
    const bool frames = !o.anim.empty() || !o.layer.empty();
    if (ok && frames) {
        JxlFrameHeader fh;
        JxlEncoderInitFrameHeader(&fh);
        fh.duration = o.anim.empty() ? 0 : 1;
        ok = JxlEncoderSetFrameHeader(fs, &fh) == JXL_ENC_SUCCESS;
    }
    ok = ok && jxl_frame(fs, o, raw, o.w, o.h);
    if (ok && !o.anim.empty()) {
        JxlFrameHeader fh;
        JxlEncoderInitFrameHeader(&fh);
        fh.duration = 1;
        ok = JxlEncoderSetFrameHeader(fs, &fh) == JXL_ENC_SUCCESS && jxl_frame(fs, o, o.anim, o.w, o.h);
    }
    if (ok && !o.layer.empty()) {
        JxlFrameHeader fh;
        JxlEncoderInitFrameHeader(&fh);
        fh.layer_info.have_crop = JXL_TRUE;
        fh.layer_info.crop_x0 = static_cast<int32_t>(o.lx);
        fh.layer_info.crop_y0 = static_cast<int32_t>(o.ly);
        fh.layer_info.xsize = o.lw;
        fh.layer_info.ysize = o.lh;
        fh.layer_info.blend_info.blendmode = JXL_BLEND_REPLACE;
        ok = JxlEncoderSetFrameHeader(fs, &fh) == JXL_ENC_SUCCESS && jxl_frame(fs, o, o.layer, o.lw, o.lh);
    }
    JxlEncoderCloseInput(enc);
    std::vector<uint8_t> buf(1 << 16);
    uint8_t* next = buf.data();
    size_t avail = buf.size();
    JxlEncoderStatus st = JXL_ENC_NEED_MORE_OUTPUT;
    while (ok && st == JXL_ENC_NEED_MORE_OUTPUT) {
        st = JxlEncoderProcessOutput(enc, &next, &avail);
        if (st == JXL_ENC_NEED_MORE_OUTPUT) {
            const size_t used = next - buf.data();
            buf.resize(buf.size() * 2);
            next = buf.data() + used;
            avail = buf.size() - used;
        }
    }
    ok = ok && st == JXL_ENC_SUCCESS;
    if (!ok) fprintf(stderr, "jxl encode failed: error=%d\n", static_cast<int>(JxlEncoderGetError(enc)));
    JxlEncoderDestroy(enc);
    if (!ok) return 4;
    buf.resize(next - buf.data());
    if (!write_all(out, buf.data(), buf.size())) return 3;
    printf("{\"tool\":\"gen_codec_fixture\",\"format\":\"jxl\",\"bytes\":%zu}\n", buf.size());
    return 0;
}

int encode_webp(const std::vector<uint8_t>& raw, const wchar_t* out, int w, int h, int ch, bool lossy) {
    if (raw.size() != static_cast<size_t>(w) * h * ch || (ch != 3 && ch != 4)) { fprintf(stderr, "bad raw size\n"); return 2; }
    WebPConfig cfg;
    WebPConfigInit(&cfg);
    if (!lossy) {
        cfg.lossless = 1;
        cfg.exact = 1;      // keep RGB under A=0
        cfg.method = 4;
        cfg.quality = 75;
    } else {
        cfg.quality = 80;
    }
    WebPPicture pic;
    WebPPictureInit(&pic);
    pic.use_argb = 1;
    pic.width = w;
    pic.height = h;
    WebPMemoryWriter wr;
    WebPMemoryWriterInit(&wr);
    pic.writer = WebPMemoryWrite;
    pic.custom_ptr = &wr;
    const int imported = ch == 4 ? WebPPictureImportRGBA(&pic, raw.data(), w * 4) : WebPPictureImportRGB(&pic, raw.data(), w * 3);
    const bool ok = imported && WebPEncode(&cfg, &pic);
    WebPPictureFree(&pic);
    if (!ok) { fprintf(stderr, "webp encode failed\n"); WebPMemoryWriterClear(&wr); return 4; }
    const bool written = write_all(out, wr.mem, wr.size);
    printf("{\"tool\":\"gen_codec_fixture\",\"format\":\"webp\",\"bytes\":%zu}\n", wr.size);
    WebPMemoryWriterClear(&wr);
    return written ? 0 : 3;
}

}  // namespace

// AVIF (libavif + aom). Without --lossy, and with identity matrix 4:4:4 or 4:0:0 at full range,
// the truth samples go straight into the planes (G -> Y, B -> U, R -> V; gray -> Y) and are
// encoded losslessly: no RGB->YUV conversion is involved. --lossy Q uses quality Q on the same
// planes. A non-identity --matrix or --yuv 420/422 converts with avifImageRGBToYUV (lossy cases).
struct AvifOpts {
    uint32_t w = 0, h = 0, ch = 0, bits = 8;
    int quality = -1;                  // -1 = lossless
    std::wstring yuv = L"auto";        // auto | 444 | 422 | 420 | 400
    int matrix = 0, primaries = 1, transfer = 13;
    bool full = true, premul = false;
    int irot = -1, imir = -1, speed = 6;
};

int encode_avif(const std::vector<uint8_t>& raw, const wchar_t* out, const AvifOpts& o) {
    const bool hi = o.bits > 8;
    const size_t n = static_cast<size_t>(o.w) * o.h * o.ch;
    if (raw.size() != n * (hi ? 2 : 1) || o.ch < 1 || o.ch > 4) { fprintf(stderr, "bad raw size\n"); return 2; }
    const bool gray = o.ch <= 2, alpha = o.ch == 2 || o.ch == 4;
    const uint32_t color_ch = gray ? 1 : 3;
    avifPixelFormat fmt = gray ? AVIF_PIXEL_FORMAT_YUV400 : AVIF_PIXEL_FORMAT_YUV444;
    if (o.yuv == L"420") fmt = AVIF_PIXEL_FORMAT_YUV420;
    else if (o.yuv == L"422") fmt = AVIF_PIXEL_FORMAT_YUV422;
    else if (o.yuv == L"400") fmt = AVIF_PIXEL_FORMAT_YUV400;
    avifImage* im = avifImageCreate(o.w, o.h, o.bits, fmt);
    if (!im) return 4;
    im->colorPrimaries = static_cast<avifColorPrimaries>(o.primaries);
    im->transferCharacteristics = static_cast<avifTransferCharacteristics>(o.transfer);
    // AV1 forbids the identity matrix with chroma subsampling, and 4:0:0 counts as subsampled:
    // gray images use matrix 2 (unspecified); Y is the gray sample either way.
    const int matrix = fmt == AVIF_PIXEL_FORMAT_YUV400 && o.matrix == 0 ? 2 : o.matrix;
    im->matrixCoefficients = static_cast<avifMatrixCoefficients>(matrix);
    im->yuvRange = o.full ? AVIF_RANGE_FULL : AVIF_RANGE_LIMITED;
    im->alphaPremultiplied = o.premul ? AVIF_TRUE : AVIF_FALSE;
    if (o.irot >= 0) { im->transformFlags |= AVIF_TRANSFORM_IROT; im->irot.angle = static_cast<uint8_t>(o.irot); }
    if (o.imir >= 0) { im->transformFlags |= AVIF_TRANSFORM_IMIR; im->imir.axis = static_cast<uint8_t>(o.imir); }
    auto sample = [&](size_t i) -> uint32_t { return hi ? (raw[2 * i] | (raw[2 * i + 1] << 8)) : raw[i]; };
    const bool direct = o.full && ((fmt == AVIF_PIXEL_FORMAT_YUV444 && o.matrix == 0) || fmt == AVIF_PIXEL_FORMAT_YUV400);
    avifResult res = AVIF_RESULT_OK;
    if (direct) {
        res = avifImageAllocatePlanes(im, alpha ? AVIF_PLANES_ALL : AVIF_PLANES_YUV);
        if (res != AVIF_RESULT_OK) { fprintf(stderr, "allocate: %s\n", avifResultToString(res)); return 4; }
        const int plane_of[3] = {2, 0, 1};   // R -> V, G -> Y, B -> U
        for (uint32_t y = 0; y < o.h; ++y)
            for (uint32_t x = 0; x < o.w; ++x)
                for (uint32_t c = 0; c < o.ch; ++c) {
                    const uint32_t v = sample((static_cast<size_t>(y) * o.w + x) * o.ch + c);
                    const bool is_a = alpha && c == o.ch - 1;
                    uint8_t* row = is_a ? im->alphaPlane + static_cast<size_t>(y) * im->alphaRowBytes
                                        : im->yuvPlanes[gray ? 0 : plane_of[c]] + static_cast<size_t>(y) * im->yuvRowBytes[gray ? 0 : plane_of[c]];
                    if (hi) reinterpret_cast<uint16_t*>(row)[x] = static_cast<uint16_t>(v);
                    else row[x] = static_cast<uint8_t>(v);
                }
    } else {
        avifRGBImage rgb;
        avifRGBImageSetDefaults(&rgb, im);
        rgb.format = gray ? (alpha ? AVIF_RGB_FORMAT_GRAYA : AVIF_RGB_FORMAT_GRAY) : (alpha ? AVIF_RGB_FORMAT_RGBA : AVIF_RGB_FORMAT_RGB);
        rgb.depth = o.bits;
        std::vector<uint8_t> px(raw);
        rgb.pixels = px.data();
        rgb.rowBytes = o.w * o.ch * (hi ? 2 : 1);
        (void)color_ch;
        res = avifImageRGBToYUV(im, &rgb);
        if (res != AVIF_RESULT_OK) { fprintf(stderr, "RGBToYUV: %s\n", avifResultToString(res)); return 4; }
    }
    avifEncoder* e = avifEncoderCreate();
    e->maxThreads = 1;
    e->speed = o.speed;
    e->quality = o.quality < 0 ? AVIF_QUALITY_LOSSLESS : o.quality;
    e->qualityAlpha = o.quality < 0 ? AVIF_QUALITY_LOSSLESS : o.quality;
    avifRWData enc = AVIF_DATA_EMPTY;
    res = avifEncoderWrite(e, im, &enc);
    avifEncoderDestroy(e);
    avifImageDestroy(im);
    if (res != AVIF_RESULT_OK) { fprintf(stderr, "avif encode failed: %s\n", avifResultToString(res)); return 4; }
    const bool ok = write_all(out, enc.data, enc.size);
    printf("{\"tool\":\"gen_codec_fixture\",\"format\":\"avif\",\"libavif\":\"%s\",\"bytes\":%zu}\n", avifVersion(), enc.size);
    avifRWDataFree(&enc);
    return ok ? 0 : 3;
}

int wmain(int argc, wchar_t** argv) {
    if (argc < 7) { fprintf(stderr, "usage: see the header of gen_codec_fixture.cpp\n"); return 2; }
    std::vector<uint8_t> raw;
    if (!read_all(argv[2], raw)) { fprintf(stderr, "cannot read raw\n"); return 3; }
    if (!wcscmp(argv[1], L"avif")) {
        if (argc < 8) { fprintf(stderr, "avif needs W H CH BITS\n"); return 2; }
        AvifOpts o;
        o.w = static_cast<uint32_t>(_wtoi(argv[4]));
        o.h = static_cast<uint32_t>(_wtoi(argv[5]));
        o.ch = static_cast<uint32_t>(_wtoi(argv[6]));
        o.bits = static_cast<uint32_t>(_wtoi(argv[7]));
        for (int i = 8; i < argc; ++i) {
            const std::wstring a = argv[i];
            const bool val = i + 1 < argc;
            if (a == L"--lossy" && val) o.quality = _wtoi(argv[++i]);
            else if (a == L"--yuv" && val) o.yuv = argv[++i];
            else if (a == L"--matrix" && val) o.matrix = _wtoi(argv[++i]);
            else if (a == L"--primaries" && val) o.primaries = _wtoi(argv[++i]);
            else if (a == L"--transfer" && val) o.transfer = _wtoi(argv[++i]);
            else if (a == L"--range" && val) o.full = std::wstring(argv[++i]) != L"limited";
            else if (a == L"--premul") o.premul = true;
            else if (a == L"--irot" && val) o.irot = _wtoi(argv[++i]);
            else if (a == L"--imir" && val) o.imir = _wtoi(argv[++i]);
            else if (a == L"--speed" && val) o.speed = _wtoi(argv[++i]);
            else { fwprintf(stderr, L"unknown option %ls\n", argv[i]); return 2; }
        }
        return encode_avif(raw, argv[3], o);
    }
    if (!wcscmp(argv[1], L"webp")) {
        bool lossy = argc >= 8 && !wcscmp(argv[7], L"--lossy");
        return encode_webp(raw, argv[3], _wtoi(argv[4]), _wtoi(argv[5]), _wtoi(argv[6]), lossy);
    }
    if (wcscmp(argv[1], L"jxl") || argc < 8) { fprintf(stderr, "unknown format\n"); return 2; }
    JxlOpts o;
    o.w = static_cast<uint32_t>(_wtoi(argv[4]));
    o.h = static_cast<uint32_t>(_wtoi(argv[5]));
    o.ch = static_cast<uint32_t>(_wtoi(argv[6]));
    o.bits = static_cast<uint32_t>(_wtoi(argv[7]));
    for (int i = 8; i < argc; ++i) {
        const std::wstring a = argv[i];
        if (a == L"--lossy") o.lossy = true;
        else if (a == L"--premul") o.premul = true;
        else if (a == L"--depth") o.depth = true;
        else if (a == L"--float-input") o.float_in = true;
        else if (a == L"--exp" && i + 1 < argc) o.exp = static_cast<uint32_t>(_wtoi(argv[++i]));
        else if (a == L"--orient" && i + 1 < argc) o.orient = static_cast<uint32_t>(_wtoi(argv[++i]));
        else if (a == L"--effort" && i + 1 < argc) o.effort = _wtoi(argv[++i]);
        else if (a == L"--color" && i + 1 < argc) {
            const std::wstring c = argv[++i];
            if (c.rfind(L"icc:", 0) == 0) {
                if (!read_all(c.substr(4).c_str(), o.icc)) { fprintf(stderr, "cannot read ICC\n"); return 3; }
                o.color = "icc:";
            } else o.color = c == L"pq" ? "pq" : (c == L"hlg" ? "hlg" : "srgb");
        } else if (a == L"--anim" && i + 1 < argc) {
            if (!read_all(argv[++i], o.anim)) return 3;
        } else if (a == L"--layer" && i + 5 < argc) {
            if (!read_all(argv[++i], o.layer)) return 3;
            o.lx = static_cast<uint32_t>(_wtoi(argv[++i]));
            o.ly = static_cast<uint32_t>(_wtoi(argv[++i]));
            o.lw = static_cast<uint32_t>(_wtoi(argv[++i]));
            o.lh = static_cast<uint32_t>(_wtoi(argv[++i]));
        } else { fwprintf(stderr, L"unknown option %ls\n", argv[i]); return 2; }
    }
    return encode_jxl(raw, argv[3], o);
}
