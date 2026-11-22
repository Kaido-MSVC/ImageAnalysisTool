// Dumps the planes libavif 1.4.2 + aom 3.15.2 (the C++ iat 1.0.0 build, read only) decodes from
// each AVIF given, for the stage 6 rav1d check (docs/decisions.md D-R19).
//
//   dump_avif_planes <out-dir> <file.avif>...
//
// Decoder settings are those of the C++ iat (src/engine/avif_side.cpp): one thread, strict flags,
// colour and alpha only, the default size limits. Planes are what avifDecoderNextImage leaves in
// avifImage (no avifImageYUVToRGB), so a limited-range alpha is already converted to full range.
//
// Output <out-dir>/<file name>.planes, little endian:
//   "IATAVPL1", u32 width, height, depth, yuvFormat, yuvRange, matrixCoefficients,
//   alphaPresent, alphaPremultiplied, plane count, then per plane (Y, U, V, A as present):
//   u32 plane width, plane height, then width * height u16 samples, row by row.
// The decode time (open, parse, first image) is printed to stdout.
// A file libavif refuses gets "<file name>.error" holding avifResultToString and the diagnostics.
#include <avif/avif.h>

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

namespace {

void put32(std::vector<uint8_t>& o, uint32_t v) {
    for (int i = 0; i < 4; ++i) o.push_back(static_cast<uint8_t>(v >> (8 * i)));
}

std::string base_name(const std::string& p) {
    const size_t s = p.find_last_of("\\/");
    return s == std::string::npos ? p : p.substr(s + 1);
}

bool write_all(const std::string& path, const std::vector<uint8_t>& data) {
    FILE* f = std::fopen(path.c_str(), "wb");
    if (!f) return false;
    const bool ok = std::fwrite(data.data(), 1, data.size(), f) == data.size();
    return std::fclose(f) == 0 && ok;
}

void put_plane(std::vector<uint8_t>& o, const uint8_t* base, uint32_t row_bytes, uint32_t w, uint32_t h, bool hi) {
    put32(o, w);
    put32(o, h);
    for (uint32_t y = 0; y < h; ++y) {
        const uint8_t* row = base + static_cast<size_t>(y) * row_bytes;
        for (uint32_t x = 0; x < w; ++x) {
            const uint16_t v = hi ? reinterpret_cast<const uint16_t*>(row)[x] : row[x];
            o.push_back(static_cast<uint8_t>(v));
            o.push_back(static_cast<uint8_t>(v >> 8));
        }
    }
}

int dump(const std::string& out_dir, const std::string& path) {
    const std::string name = base_name(path);
    avifDecoder* d = avifDecoderCreate();
    if (!d) return 1;
    d->maxThreads = 1;
    d->strictFlags = AVIF_STRICT_ENABLED;
    d->imageContentToDecode = AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA;
    d->imageSizeLimit = AVIF_DEFAULT_IMAGE_SIZE_LIMIT;
    d->imageDimensionLimit = AVIF_DEFAULT_IMAGE_DIMENSION_LIMIT;
    const auto t0 = std::chrono::steady_clock::now();
    avifResult r = avifDecoderSetIOFile(d, path.c_str());
    if (r == AVIF_RESULT_OK) r = avifDecoderParse(d);
    if (r == AVIF_RESULT_OK) r = avifDecoderNextImage(d);
    const double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
    if (r != AVIF_RESULT_OK) {
        const std::string msg = std::string(avifResultToString(r)) + (d->diag.error[0] ? std::string(" (") + d->diag.error + ")" : "") + "\n";
        avifDecoderDestroy(d);
        std::printf("%s: %s", name.c_str(), msg.c_str());
        return write_all(out_dir + "\\" + name + ".error", std::vector<uint8_t>(msg.begin(), msg.end())) ? 0 : 1;
    }
    const avifImage* im = d->image;
    std::vector<uint8_t> o{'I', 'A', 'T', 'A', 'V', 'P', 'L', '1'};
    put32(o, im->width);
    put32(o, im->height);
    put32(o, im->depth);
    put32(o, im->yuvFormat);
    put32(o, im->yuvRange);
    put32(o, im->matrixCoefficients);
    put32(o, d->alphaPresent ? 1 : 0);
    put32(o, im->alphaPremultiplied ? 1 : 0);
    avifPixelFormatInfo info;
    avifGetPixelFormatInfo(im->yuvFormat, &info);
    const bool hi = im->depth > 8;
    const uint32_t planes = (im->yuvFormat == AVIF_PIXEL_FORMAT_YUV400 ? 1 : 3) + (im->alphaPlane ? 1 : 0);
    put32(o, planes);
    for (int p = 0; p < (im->yuvFormat == AVIF_PIXEL_FORMAT_YUV400 ? 1 : 3); ++p) {
        const uint32_t w = p == 0 ? im->width : (im->width + info.chromaShiftX) >> info.chromaShiftX;
        const uint32_t h = p == 0 ? im->height : (im->height + info.chromaShiftY) >> info.chromaShiftY;
        put_plane(o, im->yuvPlanes[p], im->yuvRowBytes[p], w, h, hi);
    }
    if (im->alphaPlane) put_plane(o, im->alphaPlane, im->alphaRowBytes, im->width, im->height, hi);
    std::printf("%s: %ux%u depth %u format %d range %d matrix %u alpha %d premultiplied %d | decode %.1f ms\n", name.c_str(), im->width,
                im->height, im->depth, im->yuvFormat, im->yuvRange, im->matrixCoefficients, d->alphaPresent, im->alphaPremultiplied, ms);
    avifDecoderDestroy(d);
    return write_all(out_dir + "\\" + name + ".planes", o) ? 0 : 1;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 3) {
        std::fprintf(stderr, "usage: dump_avif_planes <out-dir> <file.avif>...\n");
        return 2;
    }
    std::printf("libavif %s (%s)\n", avifVersion(), avifCodecName(AVIF_CODEC_CHOICE_AUTO, AVIF_CODEC_FLAG_CAN_DECODE));
    int rc = 0;
    for (int i = 2; i < argc; ++i) rc |= dump(argv[1], argv[i]);
    return rc;
}
