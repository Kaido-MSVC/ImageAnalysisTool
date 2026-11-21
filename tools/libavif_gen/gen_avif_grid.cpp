// Writes the stage 6 grid test images with libavif 1.4.2 + aom 3.15.2 of the C++ iat 1.0.0 build
// (read only), and the PNGs holding the same samples (docs/decisions.md D-R19).
//
//   gen_avif_grid <out-dir>
//
// v_avif_grid_rgb8.avif   2x2 grid of 64x64 lossless tiles (4:4:4, identity, full range), 128x128
// v_avif_grid_rgba8.avif  the same with an alpha grid (alpha 0 over some pixels, RGB kept)
// v_avif_grid_ref_rgb8.png / v_avif_grid_ref_rgba8.png  the source samples
// The samples are a fixed pattern, so the files are reproducible with this libavif build.
#include <avif/avif.h>
#include <png.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

constexpr uint32_t kTile = 64, kCols = 2, kRows = 2, kW = kTile * kCols, kH = kTile * kRows;

uint8_t sample(uint32_t x, uint32_t y, uint32_t c) {
    // Deterministic, non-smooth content so that a lossy or shifted decode cannot pass.
    return static_cast<uint8_t>((x * 7 + y * 13 + c * 61 + ((x * y) >> 3)) & 0xFF);
}

uint8_t alpha(uint32_t x, uint32_t y) {
    if (x < 8) return 0;   // A=0 columns: their RGB must survive
    return static_cast<uint8_t>(255 - ((x + y) & 0x7F));
}

bool write_png(const std::string& path, bool with_alpha) {
    const uint32_t ch = with_alpha ? 4 : 3;
    std::vector<uint8_t> px(static_cast<size_t>(kW) * kH * ch);
    for (uint32_t y = 0; y < kH; ++y)
        for (uint32_t x = 0; x < kW; ++x) {
            uint8_t* p = &px[(static_cast<size_t>(y) * kW + x) * ch];
            for (uint32_t c = 0; c < 3; ++c) p[c] = sample(x, y, c);
            if (with_alpha) p[3] = alpha(x, y);
        }
    png_image img;
    std::memset(&img, 0, sizeof img);
    img.version = PNG_IMAGE_VERSION;
    img.width = kW;
    img.height = kH;
    img.format = with_alpha ? PNG_FORMAT_RGBA : PNG_FORMAT_RGB;
    if (!png_image_write_to_file(&img, path.c_str(), 0, px.data(), 0, nullptr)) {
        std::fprintf(stderr, "png: %s\n", img.message);
        return false;
    }
    return true;
}

bool write_grid(const std::string& path, bool with_alpha) {
    std::vector<avifImage*> cells;
    for (uint32_t r = 0; r < kRows; ++r)
        for (uint32_t c = 0; c < kCols; ++c) {
            avifImage* im = avifImageCreate(kTile, kTile, 8, AVIF_PIXEL_FORMAT_YUV444);
            im->yuvRange = AVIF_RANGE_FULL;
            im->colorPrimaries = AVIF_COLOR_PRIMARIES_BT709;
            im->transferCharacteristics = AVIF_TRANSFER_CHARACTERISTICS_SRGB;
            im->matrixCoefficients = AVIF_MATRIX_COEFFICIENTS_IDENTITY;
            if (avifImageAllocatePlanes(im, with_alpha ? AVIF_PLANES_ALL : AVIF_PLANES_YUV) != AVIF_RESULT_OK) return false;
            for (uint32_t y = 0; y < kTile; ++y)
                for (uint32_t x = 0; x < kTile; ++x) {
                    const uint32_t gx = c * kTile + x, gy = r * kTile + y;
                    // identity: Y = G, U = B, V = R
                    im->yuvPlanes[0][y * im->yuvRowBytes[0] + x] = sample(gx, gy, 1);
                    im->yuvPlanes[1][y * im->yuvRowBytes[1] + x] = sample(gx, gy, 2);
                    im->yuvPlanes[2][y * im->yuvRowBytes[2] + x] = sample(gx, gy, 0);
                    if (with_alpha) im->alphaPlane[y * im->alphaRowBytes + x] = alpha(gx, gy);
                }
            cells.push_back(im);
        }
    avifEncoder* e = avifEncoderCreate();
    e->speed = 6;
    e->maxThreads = 1;
    e->quality = AVIF_QUALITY_LOSSLESS;
    e->qualityAlpha = AVIF_QUALITY_LOSSLESS;
    avifResult res = avifEncoderAddImageGrid(e, kCols, kRows, const_cast<const avifImage* const*>(cells.data()),
                                             AVIF_ADD_IMAGE_FLAG_SINGLE);
    avifRWData out = AVIF_DATA_EMPTY;
    if (res == AVIF_RESULT_OK) res = avifEncoderFinish(e, &out);
    if (res != AVIF_RESULT_OK) {
        std::fprintf(stderr, "grid: %s (%s)\n", avifResultToString(res), e->diag.error);
        return false;
    }
    FILE* f = std::fopen(path.c_str(), "wb");
    const bool ok = f && std::fwrite(out.data, 1, out.size, f) == out.size && std::fclose(f) == 0;
    std::printf("%s: %zu bytes\n", path.c_str(), out.size);
    avifRWDataFree(&out);
    avifEncoderDestroy(e);
    for (avifImage* im : cells) avifImageDestroy(im);
    return ok;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: gen_avif_grid <out-dir>\n");
        return 2;
    }
    const std::string d = argv[1];
    const bool ok = write_grid(d + "\\v_avif_grid_rgb8.avif", false) && write_grid(d + "\\v_avif_grid_rgba8.avif", true) &&
                    write_png(d + "\\v_avif_grid_ref_rgb8.png", false) && write_png(d + "\\v_avif_grid_ref_rgba8.png", true);
    return ok ? 0 : 1;
}
