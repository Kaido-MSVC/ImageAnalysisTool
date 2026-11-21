// Encodes an 8-bit PNG into AVIF with libavif 1.4.2 + aom 3.15.2 of the C++ iat 1.0.0 build (read
// only), for the stage 6 decode-time measurement (docs/decisions.md D-R19). Not used for fixtures.
//
//   encode_avif lossless|lossy <in.png> <out.avif>
//
// lossless: 4:4:4, identity matrix, full range, AVIF_QUALITY_LOSSLESS (what IMB-C writes);
// lossy: 4:2:0, BT.601 matrix, quality 60. Speed 6, all threads.
#include <avif/avif.h>
#include <png.h>

#include <cstdio>
#include <cstring>
#include <thread>
#include <vector>

int main(int argc, char** argv) {
    if (argc != 4 || (std::strcmp(argv[1], "lossless") && std::strcmp(argv[1], "lossy"))) {
        std::fprintf(stderr, "usage: encode_avif lossless|lossy <in.png> <out.avif>\n");
        return 2;
    }
    const bool lossless = !std::strcmp(argv[1], "lossless");
    png_image pi;
    std::memset(&pi, 0, sizeof pi);
    pi.version = PNG_IMAGE_VERSION;
    if (!png_image_begin_read_from_file(&pi, argv[2])) { std::fprintf(stderr, "png: %s\n", pi.message); return 1; }
    pi.format = PNG_FORMAT_RGB;
    std::vector<uint8_t> rgb(PNG_IMAGE_SIZE(pi));
    if (!png_image_finish_read(&pi, nullptr, rgb.data(), 0, nullptr)) { std::fprintf(stderr, "png: %s\n", pi.message); return 1; }

    avifImage* im = avifImageCreate(pi.width, pi.height, 8, lossless ? AVIF_PIXEL_FORMAT_YUV444 : AVIF_PIXEL_FORMAT_YUV420);
    im->yuvRange = AVIF_RANGE_FULL;
    im->colorPrimaries = AVIF_COLOR_PRIMARIES_BT709;
    im->transferCharacteristics = AVIF_TRANSFER_CHARACTERISTICS_SRGB;
    im->matrixCoefficients = lossless ? AVIF_MATRIX_COEFFICIENTS_IDENTITY : AVIF_MATRIX_COEFFICIENTS_BT601;
    avifRGBImage r;
    avifRGBImageSetDefaults(&r, im);
    r.format = AVIF_RGB_FORMAT_RGB;
    r.pixels = rgb.data();
    r.rowBytes = pi.width * 3;
    avifResult res = avifImageRGBToYUV(im, &r);
    if (res != AVIF_RESULT_OK) { std::fprintf(stderr, "RGBToYUV: %s\n", avifResultToString(res)); return 1; }

    avifEncoder* e = avifEncoderCreate();
    e->speed = 6;
    e->maxThreads = static_cast<int>(std::thread::hardware_concurrency());
    e->quality = lossless ? AVIF_QUALITY_LOSSLESS : 60;
    e->qualityAlpha = AVIF_QUALITY_LOSSLESS;
    avifRWData out = AVIF_DATA_EMPTY;
    res = avifEncoderWrite(e, im, &out);
    if (res != AVIF_RESULT_OK) { std::fprintf(stderr, "encode: %s (%s)\n", avifResultToString(res), e->diag.error); return 1; }
    FILE* f = std::fopen(argv[3], "wb");
    const bool ok = f && std::fwrite(out.data, 1, out.size, f) == out.size && std::fclose(f) == 0;
    std::printf("%s: %ux%u %s, %zu bytes\n", argv[3], pi.width, pi.height, argv[1], out.size);
    avifRWDataFree(&out);
    avifEncoderDestroy(e);
    avifImageDestroy(im);
    return ok ? 0 : 1;
}
