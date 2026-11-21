// Writes a lossy JPEG XL that is NOT XYB (VarDCT with uses_original_profile = true), so C++ iat
// 1.0.0 decodes and compares it instead of rejecting it as xyb_not_native (docs/decisions.md D-R18).
// Input: an IATT truth file (tests/fixtures/generated/s3_jxl_rgb16_600x40_srgb.truth: 600x40 RGB
// 16-bit, little-endian samples). Output: <dir>/v_jxl_rgb16_lossy_vardct_noxyb.jxl
// (sRGB colour enum, distance 1.0, effort 7). Built by build.ps1 against the C++ build's libjxl.
#include <jxl/encode.h>

#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

int main(int argc, char** argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: gen_jxl_lossy <truth> <out dir>\n");
        return 2;
    }
    FILE* in = fopen(argv[1], "rb");
    if (!in) return 1;
    uint8_t head[24];
    if (fread(head, 1, 24, in) != 24) return 1;
    auto u32 = [&](int o) { return uint32_t(head[o]) | uint32_t(head[o + 1]) << 8 | uint32_t(head[o + 2]) << 16 | uint32_t(head[o + 3]) << 24; };
    const uint32_t w = u32(8), h = u32(12), ch = u32(16), bits = u32(20);
    if (ch != 3 || bits != 16) return 1;
    std::vector<uint16_t> px(size_t(w) * h * ch);
    if (fread(px.data(), 2, px.size(), in) != px.size()) return 1;
    fclose(in);

    JxlEncoder* enc = JxlEncoderCreate(nullptr);
    JxlBasicInfo bi;
    JxlEncoderInitBasicInfo(&bi);
    bi.xsize = w;
    bi.ysize = h;
    bi.bits_per_sample = 16;
    bi.num_color_channels = 3;
    bi.uses_original_profile = JXL_TRUE;  // no XYB
    JxlColorEncoding c;
    JxlColorEncodingSetToSRGB(&c, JXL_FALSE);
    JxlEncoderFrameSettings* fs = nullptr;
    bool ok = JxlEncoderSetBasicInfo(enc, &bi) == JXL_ENC_SUCCESS && JxlEncoderSetColorEncoding(enc, &c) == JXL_ENC_SUCCESS &&
              (fs = JxlEncoderFrameSettingsCreate(enc, nullptr)) != nullptr &&
              JxlEncoderSetFrameDistance(fs, 1.0f) == JXL_ENC_SUCCESS &&
              JxlEncoderFrameSettingsSetOption(fs, JXL_ENC_FRAME_SETTING_EFFORT, 7) == JXL_ENC_SUCCESS &&
              JxlEncoderFrameSettingsSetOption(fs, JXL_ENC_FRAME_SETTING_MODULAR, 0) == JXL_ENC_SUCCESS;
    const JxlPixelFormat pf{3, JXL_TYPE_UINT16, JXL_LITTLE_ENDIAN, 0};
    ok = ok && JxlEncoderAddImageFrame(fs, &pf, px.data(), px.size() * 2) == JXL_ENC_SUCCESS;
    if (ok) JxlEncoderCloseInput(enc);
    std::vector<uint8_t> out(1 << 20);
    uint8_t* next = out.data();
    size_t avail = out.size();
    JxlEncoderStatus st = ok ? JXL_ENC_NEED_MORE_OUTPUT : JXL_ENC_ERROR;
    while (st == JXL_ENC_NEED_MORE_OUTPUT) {
        st = JxlEncoderProcessOutput(enc, &next, &avail);
        if (st == JXL_ENC_NEED_MORE_OUTPUT) {
            const size_t used = size_t(next - out.data());
            out.resize(out.size() * 2);
            next = out.data() + used;
            avail = out.size() - used;
        }
    }
    JxlEncoderDestroy(enc);
    if (st != JXL_ENC_SUCCESS) {
        fprintf(stderr, "encoding failed\n");
        return 1;
    }
    out.resize(size_t(next - out.data()));
    const std::string path = std::string(argv[2]) + "/v_jxl_rgb16_lossy_vardct_noxyb.jxl";
    FILE* f = fopen(path.c_str(), "wb");
    if (!f) return 1;
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
    fprintf(stderr, "wrote %s (%zu bytes)\n", path.c_str(), out.size());
    return 0;
}
