// Generates the table of ICC profiles that libjxl 0.12.0 synthesizes from JPEG XL colour enums
// (docs/decisions.md D-R18). For every non-custom combination of colour space x white point x
// primaries x transfer function x rendering intent it encodes a 1x1 lossless image with that
// enum and reads the profile back with JxlDecoderGetColorAsICCProfile(JXL_COLOR_PROFILE_TARGET_ORIGINAL),
// the call C++ iat 1.0.0 makes. Linked against the libjxl static libraries of the C++ build
// (read only); see build.ps1.
//
// Output (little-endian):
//   "IATJICC1", u32 entry count, then per entry
//     u8 color_space, u8 white_point, u8 primaries, u8 transfer_function, u8 rendering_intent,
//     u8[3] zero, u32 icc size, icc bytes
//   Keys are the enum values as the decoder reports them (JxlColorEncoding numbering), sorted.
//   primaries is 0 for grey images (libjxl leaves the field unused).
// A text log of skipped / failed combinations goes to stderr.
#include <jxl/decode.h>
#include <jxl/encode.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <tuple>
#include <vector>

namespace {

using Key = std::tuple<uint8_t, uint8_t, uint8_t, uint8_t, uint8_t>;

bool encode(const JxlColorEncoding& c, std::vector<uint8_t>& out) {
    JxlEncoder* enc = JxlEncoderCreate(nullptr);
    if (!enc) return false;
    const bool gray = c.color_space == JXL_COLOR_SPACE_GRAY;
    JxlBasicInfo bi;
    JxlEncoderInitBasicInfo(&bi);
    bi.xsize = bi.ysize = 1;
    bi.bits_per_sample = 16;
    bi.num_color_channels = gray ? 1 : 3;
    bi.uses_original_profile = JXL_TRUE;
    bool ok = JxlEncoderSetBasicInfo(enc, &bi) == JXL_ENC_SUCCESS && JxlEncoderSetColorEncoding(enc, &c) == JXL_ENC_SUCCESS;
    JxlEncoderFrameSettings* fs = ok ? JxlEncoderFrameSettingsCreate(enc, nullptr) : nullptr;
    ok = ok && fs && JxlEncoderSetFrameLossless(fs, JXL_TRUE) == JXL_ENC_SUCCESS;
    const uint16_t px[3] = {1000, 2000, 3000};
    const JxlPixelFormat pf{bi.num_color_channels, JXL_TYPE_UINT16, JXL_NATIVE_ENDIAN, 0};
    ok = ok && JxlEncoderAddImageFrame(fs, &pf, px, sizeof(uint16_t) * bi.num_color_channels) == JXL_ENC_SUCCESS;
    if (ok) JxlEncoderCloseInput(enc);
    out.assign(4096, 0);
    uint8_t* next = out.data();
    size_t avail = out.size();
    JxlEncoderStatus st = ok ? JXL_ENC_NEED_MORE_OUTPUT : JXL_ENC_ERROR;
    while (st == JXL_ENC_NEED_MORE_OUTPUT) {
        st = JxlEncoderProcessOutput(enc, &next, &avail);
        if (st == JXL_ENC_NEED_MORE_OUTPUT) {
            const size_t used = static_cast<size_t>(next - out.data());
            out.resize(out.size() * 2);
            next = out.data() + used;
            avail = out.size() - used;
        }
    }
    out.resize(static_cast<size_t>(next - out.data()));
    JxlEncoderDestroy(enc);
    return st == JXL_ENC_SUCCESS;
}

bool decode(const std::vector<uint8_t>& file, JxlColorEncoding& got, std::vector<uint8_t>& icc) {
    JxlDecoder* dec = JxlDecoderCreate(nullptr);
    if (!dec) return false;
    JxlDecoderSubscribeEvents(dec, JXL_DEC_COLOR_ENCODING);
    JxlDecoderSetInput(dec, file.data(), file.size());
    JxlDecoderCloseInput(dec);
    bool ok = false;
    for (;;) {
        const JxlDecoderStatus st = JxlDecoderProcessInput(dec);
        if (st == JXL_DEC_COLOR_ENCODING) {
            size_t n = 0;
            ok = JxlDecoderGetColorAsEncodedProfile(dec, JXL_COLOR_PROFILE_TARGET_ORIGINAL, &got) == JXL_DEC_SUCCESS &&
                 JxlDecoderGetICCProfileSize(dec, JXL_COLOR_PROFILE_TARGET_ORIGINAL, &n) == JXL_DEC_SUCCESS && n > 0;
            if (ok) {
                icc.resize(n);
                ok = JxlDecoderGetColorAsICCProfile(dec, JXL_COLOR_PROFILE_TARGET_ORIGINAL, icc.data(), n) == JXL_DEC_SUCCESS;
            }
            break;
        }
        if (st == JXL_DEC_ERROR || st == JXL_DEC_SUCCESS || st == JXL_DEC_NEED_MORE_INPUT) break;
    }
    JxlDecoderDestroy(dec);
    return ok;
}

void put_u32(FILE* f, uint32_t v) {
    const uint8_t b[4] = {uint8_t(v), uint8_t(v >> 8), uint8_t(v >> 16), uint8_t(v >> 24)};
    fwrite(b, 1, 4, f);
}

}  // namespace

int main(int argc, char** argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: gen_jxl_icc_table <out.bin>\n");
        return 2;
    }
    const uint32_t v = JxlDecoderVersion();
    fprintf(stderr, "libjxl %u.%u.%u\n", v / 1000000, v / 1000 % 1000, v % 1000);
    const JxlColorSpace spaces[] = {JXL_COLOR_SPACE_RGB, JXL_COLOR_SPACE_GRAY, JXL_COLOR_SPACE_XYB, JXL_COLOR_SPACE_UNKNOWN};
    const JxlWhitePoint whites[] = {JXL_WHITE_POINT_D65, JXL_WHITE_POINT_E, JXL_WHITE_POINT_DCI};
    const JxlPrimaries prims[] = {JXL_PRIMARIES_SRGB, JXL_PRIMARIES_2100, JXL_PRIMARIES_P3};
    const JxlTransferFunction tfs[] = {JXL_TRANSFER_FUNCTION_709, JXL_TRANSFER_FUNCTION_UNKNOWN, JXL_TRANSFER_FUNCTION_LINEAR,
                                       JXL_TRANSFER_FUNCTION_SRGB, JXL_TRANSFER_FUNCTION_PQ,      JXL_TRANSFER_FUNCTION_DCI,
                                       JXL_TRANSFER_FUNCTION_HLG};
    const JxlRenderingIntent intents[] = {JXL_RENDERING_INTENT_PERCEPTUAL, JXL_RENDERING_INTENT_RELATIVE,
                                          JXL_RENDERING_INTENT_SATURATION, JXL_RENDERING_INTENT_ABSOLUTE};
    std::map<Key, std::vector<uint8_t>> table;
    int tried = 0, failed = 0, renamed = 0;
    for (JxlColorSpace cs : spaces)
        for (JxlWhitePoint wp : whites)
            for (JxlPrimaries pr : prims) {
                if (cs != JXL_COLOR_SPACE_RGB && pr != JXL_PRIMARIES_SRGB) continue;  // primaries unused off RGB
                for (JxlTransferFunction tf : tfs)
                    for (JxlRenderingIntent ri : intents) {
                        ++tried;
                        JxlColorEncoding c{};
                        c.color_space = cs;
                        c.white_point = wp;
                        c.primaries = pr;
                        c.transfer_function = tf;
                        c.rendering_intent = ri;
                        std::vector<uint8_t> file, icc;
                        JxlColorEncoding got{};
                        if (!encode(c, file) || !decode(file, got, icc)) {
                            ++failed;
                            fprintf(stderr, "skip space=%d white=%d primaries=%d transfer=%d intent=%d\n", cs, wp, pr, tf, ri);
                            continue;
                        }
                        const uint8_t gp = got.color_space == JXL_COLOR_SPACE_RGB ? uint8_t(got.primaries) : 0;
                        const Key k{uint8_t(got.color_space), uint8_t(got.white_point), gp, uint8_t(got.transfer_function),
                                    uint8_t(got.rendering_intent)};
                        if (got.color_space != cs || got.white_point != wp || got.transfer_function != tf || got.rendering_intent != ri ||
                            (cs == JXL_COLOR_SPACE_RGB && got.primaries != pr)) {
                            ++renamed;
                            fprintf(stderr, "decoded as space=%d white=%d primaries=%d transfer=%d intent=%d (asked %d %d %d %d %d)\n",
                                    got.color_space, got.white_point, gp, got.transfer_function, got.rendering_intent, cs, wp, pr, tf, ri);
                        }
                        auto it = table.find(k);
                        if (it != table.end() && it->second != icc) {
                            fprintf(stderr, "CONFLICT for decoded key; keeping the first profile\n");
                            continue;
                        }
                        table[k] = icc;
                    }
            }
    FILE* f = fopen(argv[1], "wb");
    if (!f) return 1;
    fwrite("IATJICC1", 1, 8, f);
    put_u32(f, uint32_t(table.size()));
    size_t bytes = 0;
    for (const auto& [k, icc] : table) {
        const uint8_t h[8] = {std::get<0>(k), std::get<1>(k), std::get<2>(k), std::get<3>(k), std::get<4>(k), 0, 0, 0};
        fwrite(h, 1, 8, f);
        put_u32(f, uint32_t(icc.size()));
        fwrite(icc.data(), 1, icc.size(), f);
        bytes += icc.size();
    }
    fclose(f);
    fprintf(stderr, "tried %d, failed %d, decoded under another key %d, entries %zu, profile bytes %zu\n", tried, failed, renamed,
            table.size(), bytes);
    return 0;
}
