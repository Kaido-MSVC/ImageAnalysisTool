// Writes a large non-interlaced PNG whose samples follow probe_pattern().
// Uses zlib directly (not libpng), so the libpng decoder is checked against an
// independent writer. Also used for the public, reproducible stand-in of the
// private big-png-261mp sample.
//
// Usage: gen_large_png <out.png> W H [--rgb8|--rgb16|--rgba8] [--srgb] [--icc <profile>] [--level N] [--adam7]
// --adam7 writes the same samples as Adam7 passes (pass geometry from the PNG
// specification, written out here independently of libpng).
// --icc embeds the profile file as iCCP (name "icc"), e.g. for PNG <-> TIFF pairs.
#include "probe_common.h"

#include <zlib.h>

#include <string>
#include <vector>

namespace {

void be32(uint8_t* p, uint32_t v) {
    for (int i = 0; i < 4; ++i) p[i] = static_cast<uint8_t>(v >> (24 - 8 * i));
}

void write_chunk(FILE* f, const char* type, const uint8_t* data, uint32_t n) {
    uint8_t hdr[8];
    be32(hdr, n);
    memcpy(hdr + 4, type, 4);
    uLong crc = crc32(0, hdr + 4, 4);
    if (n) crc = crc32(crc, data, n);
    uint8_t c[4];
    be32(c, static_cast<uint32_t>(crc));
    fwrite(hdr, 1, 8, f);
    if (n) fwrite(data, 1, n, f);
    fwrite(c, 1, 4, f);
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc < 4) { fprintf(stderr, "usage: gen_large_png <out> W H [--rgb8|--rgb16|--rgba8] [--srgb] [--level N]\n"); return 2; }
    const uint32_t w = static_cast<uint32_t>(_wtoi(argv[2])), h = static_cast<uint32_t>(_wtoi(argv[3]));
    uint32_t ch = 3, bits = 16;
    int ctype = 2, level = 1;
    bool srgb = false, adam7 = false;
    std::wstring icc_path;
    for (int i = 4; i < argc; ++i) {
        if (!wcscmp(argv[i], L"--adam7")) adam7 = true;
        if (!wcscmp(argv[i], L"--icc") && i + 1 < argc) icc_path = argv[++i];
        if (!wcscmp(argv[i], L"--rgba8")) { ch = 4; bits = 8; ctype = 6; }
        else if (!wcscmp(argv[i], L"--rgb8")) { ch = 3; bits = 8; ctype = 2; }
        else if (!wcscmp(argv[i], L"--rgb16")) { ch = 3; bits = 16; ctype = 2; }
        else if (!wcscmp(argv[i], L"--srgb")) srgb = true;
        else if (!wcscmp(argv[i], L"--level") && i + 1 < argc) level = _wtoi(argv[++i]);
    }
    const uint32_t bps = bits / 8;
    FILE* f = _wfopen(argv[1], L"wb");
    if (!f) return 3;
    static const uint8_t sig[8] = {0x89, 'P', 'N', 'G', 0x0D, 0x0A, 0x1A, 0x0A};
    fwrite(sig, 1, 8, f);
    uint8_t ihdr[13];
    be32(ihdr, w);
    be32(ihdr + 4, h);
    ihdr[8] = static_cast<uint8_t>(bits);
    ihdr[9] = static_cast<uint8_t>(ctype);
    ihdr[10] = ihdr[11] = 0;  // deflate, adaptive filtering
    ihdr[12] = adam7 ? 1 : 0;
    write_chunk(f, "IHDR", ihdr, 13);
    if (srgb) {
        const uint8_t intent = 0;
        write_chunk(f, "sRGB", &intent, 1);
    }
    size_t icc_bytes = 0;
    if (!icc_path.empty()) {
        FILE* fi = _wfopen(icc_path.c_str(), L"rb");
        if (!fi) { fprintf(stderr, "cannot read ICC profile\n"); fclose(f); return 3; }
        std::vector<uint8_t> prof;
        uint8_t tmp[4096];
        size_t n;
        while ((n = fread(tmp, 1, sizeof tmp, fi)) > 0) prof.insert(prof.end(), tmp, tmp + n);
        fclose(fi);
        icc_bytes = prof.size();
        uLongf zlen = compressBound(static_cast<uLong>(prof.size()));
        std::vector<uint8_t> chunkdata = {'i', 'c', 'c', 0, 0};  // name, NUL, compression method 0
        chunkdata.resize(5 + zlen);
        // Level 0 (stored), as in gen_fixtures.py: libpng 1.6.59 rejects iCCP payloads
        // shorter than 81 + 11 bytes ("too short"), which a compressed small profile would be.
        if (compress2(chunkdata.data() + 5, &zlen, prof.data(), static_cast<uLong>(prof.size()), 0) != Z_OK) {
            fclose(f);
            return 4;
        }
        chunkdata.resize(5 + zlen);
        write_chunk(f, "iCCP", chunkdata.data(), static_cast<uint32_t>(chunkdata.size()));
    }

    z_stream zs{};
    deflateInit(&zs, level);
    const size_t rowlen = 1 + static_cast<size_t>(w) * ch * bps;
    std::vector<uint8_t> row(rowlen), out(1 << 20);
    uint64_t total = 0;
    auto flush_out = [&](int mode) {
        do {
            zs.next_out = out.data();
            zs.avail_out = static_cast<uInt>(out.size());
            deflate(&zs, mode);
            uint32_t n = static_cast<uint32_t>(out.size() - zs.avail_out);
            if (n) { write_chunk(f, "IDAT", out.data(), n); total += n; }
        } while (zs.avail_out == 0);
    };
    // (x start, x step, y start, y step) per pass; one pass when not interlaced.
    static const uint32_t a7[7][4] = {{0, 8, 0, 8}, {4, 8, 0, 8}, {0, 4, 4, 8}, {2, 4, 0, 4},
                                      {0, 2, 2, 4}, {1, 2, 0, 2}, {0, 1, 1, 2}};
    static const uint32_t plain[1][4] = {{0, 1, 0, 1}};
    const uint32_t(*passes)[4] = adam7 ? a7 : plain;
    const int npass = adam7 ? 7 : 1;
    for (int ps = 0; ps < npass; ++ps) {
        const uint32_t xs = passes[ps][0], xi = passes[ps][1], ys = passes[ps][2], yi = passes[ps][3];
        if (xs >= w) continue;  // empty pass: no rows at all
        const uint32_t cols = (w - xs + xi - 1) / xi;
        const size_t len = 1 + static_cast<size_t>(cols) * ch * bps;
        for (uint32_t y = ys; y < h; y += yi) {
            row[0] = 0;  // filter: none
            uint8_t* p = row.data() + 1;
            for (uint32_t x = xs; x < w; x += xi)
                for (uint32_t c = 0; c < ch; ++c) {
                    uint32_t v = probe_pattern(x, y, c, bits);
                    if (bps == 2) { *p++ = static_cast<uint8_t>(v >> 8); *p++ = static_cast<uint8_t>(v); }
                    else *p++ = static_cast<uint8_t>(v);
                }
            zs.next_in = row.data();
            zs.avail_in = static_cast<uInt>(len);
            flush_out(Z_NO_FLUSH);
        }
    }
    flush_out(Z_FINISH);
    deflateEnd(&zs);
    write_chunk(f, "IEND", nullptr, 0);
    fclose(f);
    printf("{\"tool\":\"gen_large_png\",\"width\":%u,\"height\":%u,\"bits\":%u,\"channels\":%u,\"srgb\":%d,\"adam7\":%d,"
           "\"icc_bytes\":%zu,\"raw_bytes\":%llu,\"idat_bytes\":%llu}\n",
           w, h, bits, ch, srgb ? 1 : 0, adam7 ? 1 : 0, icc_bytes,
           static_cast<unsigned long long>(static_cast<uint64_t>(w) * h * ch * bps), static_cast<unsigned long long>(total));
    return 0;
}
