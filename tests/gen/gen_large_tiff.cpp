// Writes a large contiguous RGB TIFF or BigTIFF whose samples follow probe_pattern(),
// the same pattern gen_large_png uses, so PNG <-> TIFF pairs must compare equal.
// The container is written here directly (zlib for Deflate blocks), independently
// of libtiff, so the decoder under test is checked against a separate writer.
//
// Usage: gen_large_tiff <out.tif> W H [--rgb8|--rgb16] [--be] [--big]
//          [--tile N | --rps N] [--deflate] [--level N] [--icc <profile>] [--gap-gib N]
//
// Layout: header, [sparse gap], image blocks, out-of-line tag values, IFD at the end.
// --gap-gib N marks the file sparse and leaves N GiB unwritten after the header, so
// every data offset and the IFD lie beyond 4 GiB (BigTIFF only) without using the disk.
// Tile padding (outside the image) is filled with 0xAB so it can never pass for data.
#include "probe_common.h"

#include <winioctl.h>
#include <zlib.h>

#include <algorithm>
#include <chrono>
#include <string>
#include <vector>

namespace {

bool g_be = false;

void put(std::vector<uint8_t>& v, uint64_t x, int n) {
    for (int i = 0; i < n; ++i) v.push_back(static_cast<uint8_t>(x >> (g_be ? 8 * (n - 1 - i) : 8 * i)));
}

struct Out {
    HANDLE h = INVALID_HANDLE_VALUE;
    uint64_t pos = 0;
    std::vector<uint8_t> buf;
    bool ok = true;

    void flush() {
        size_t done = 0;
        while (ok && done < buf.size()) {
            DWORD n = 0;
            const DWORD chunk = static_cast<DWORD>((std::min<size_t>)(buf.size() - done, 1u << 30));
            if (!WriteFile(h, buf.data() + done, chunk, &n, nullptr) || n == 0) ok = false;
            done += n;
        }
        buf.clear();
    }
    void write(const void* p, size_t n) {
        const uint8_t* b = static_cast<const uint8_t*>(p);
        buf.insert(buf.end(), b, b + n);
        pos += n;
        if (buf.size() >= (8u << 20)) flush();
    }
    void write(const std::vector<uint8_t>& v) { write(v.data(), v.size()); }
    void seek(uint64_t off) {
        flush();
        LARGE_INTEGER li;
        li.QuadPart = static_cast<LONGLONG>(off);
        if (!SetFilePointerEx(h, li, nullptr, FILE_BEGIN)) ok = false;
        pos = off;
    }
    void align2() {
        if (pos & 1) {
            const uint8_t z = 0;
            write(&z, 1);
        }
    }
};

struct Entry {
    uint16_t tag, type;
    uint64_t count;
    std::vector<uint8_t> payload;  // already in file byte order
    uint64_t offset = 0;           // where an out-of-line payload was written
};

Entry num(uint16_t tag, uint16_t type, const std::vector<uint64_t>& vals) {
    Entry e{tag, type, vals.size(), {}};
    const int n = type == 3 ? 2 : (type == 4 ? 4 : 8);
    for (uint64_t v : vals) put(e.payload, v, n);
    return e;
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: gen_large_tiff <out> W H [--rgb8|--rgb16] [--be] [--big] [--tile N|--rps N] "
                        "[--deflate] [--level N] [--icc file] [--gap-gib N]\n");
        return 2;
    }
    const auto t0 = std::chrono::steady_clock::now();
    const uint32_t w = static_cast<uint32_t>(_wtoi(argv[2])), h = static_cast<uint32_t>(_wtoi(argv[3]));
    uint32_t bits = 16, tile = 0, rps = 16;
    int level = 1;
    bool big = false, deflate = false;
    uint64_t gap_gib = 0;
    std::wstring icc_path;
    for (int i = 4; i < argc; ++i) {
        if (!wcscmp(argv[i], L"--rgb8")) bits = 8;
        else if (!wcscmp(argv[i], L"--rgb16")) bits = 16;
        else if (!wcscmp(argv[i], L"--be")) g_be = true;
        else if (!wcscmp(argv[i], L"--big")) big = true;
        else if (!wcscmp(argv[i], L"--deflate")) deflate = true;
        else if (!wcscmp(argv[i], L"--tile") && i + 1 < argc) tile = static_cast<uint32_t>(_wtoi(argv[++i]));
        else if (!wcscmp(argv[i], L"--rps") && i + 1 < argc) rps = static_cast<uint32_t>(_wtoi(argv[++i]));
        else if (!wcscmp(argv[i], L"--level") && i + 1 < argc) level = _wtoi(argv[++i]);
        else if (!wcscmp(argv[i], L"--icc") && i + 1 < argc) icc_path = argv[++i];
        else if (!wcscmp(argv[i], L"--gap-gib") && i + 1 < argc) gap_gib = static_cast<uint64_t>(_wtoi(argv[++i]));
        else { fwprintf(stderr, L"unknown option %ls\n", argv[i]); return 2; }
    }
    if (w == 0 || h == 0 || (tile && tile % 16) || rps == 0 || (gap_gib && !big)) {
        fprintf(stderr, "invalid geometry (tile must be a multiple of 16; --gap-gib needs --big)\n");
        return 2;
    }
    const uint32_t ch = 3, bps = bits / 8;

    std::vector<uint8_t> icc;
    if (!icc_path.empty()) {
        FILE* fi = _wfopen(icc_path.c_str(), L"rb");
        if (!fi) { fprintf(stderr, "cannot read ICC profile\n"); return 3; }
        uint8_t tmp[4096];
        size_t n;
        while ((n = fread(tmp, 1, sizeof tmp, fi)) > 0) icc.insert(icc.end(), tmp, tmp + n);
        fclose(fi);
    }

    Out o;
    o.h = CreateFileW(argv[1], GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (o.h == INVALID_HANDLE_VALUE) { fprintf(stderr, "cannot create output (exists already?)\n"); return 3; }
    if (gap_gib) {
        DWORD ret = 0;
        if (!DeviceIoControl(o.h, FSCTL_SET_SPARSE, nullptr, 0, nullptr, 0, &ret, nullptr)) {
            fprintf(stderr, "FSCTL_SET_SPARSE failed (%lu)\n", GetLastError());
            CloseHandle(o.h);
            DeleteFileW(argv[1]);
            return 3;
        }
    }

    // Header with a placeholder IFD pointer, patched at the end.
    std::vector<uint8_t> hdr;
    hdr.push_back(g_be ? 'M' : 'I');
    hdr.push_back(g_be ? 'M' : 'I');
    if (big) { put(hdr, 43, 2); put(hdr, 8, 2); put(hdr, 0, 2); put(hdr, 0, 8); }
    else { put(hdr, 42, 2); put(hdr, 0, 4); }
    o.write(hdr);
    const uint64_t ifd_ptr_pos = big ? 8 : 4;
    if (gap_gib) o.seek(o.pos + (gap_gib << 30));
    const uint64_t first_data = o.pos;

    // Image blocks.
    const uint32_t bw = tile ? tile : w, bh = tile ? tile : (std::min)(rps, h);
    const uint32_t across = (w + bw - 1) / bw, down = (h + bh - 1) / bh;
    const size_t raw_len = static_cast<size_t>(bw) * bh * ch * bps;
    std::vector<uint8_t> raw(raw_len), comp(deflate ? compressBound(static_cast<uLong>(raw_len)) : 0);
    std::vector<uint64_t> offsets, counts;
    for (uint32_t by = 0; by < down; ++by) {
        for (uint32_t bx = 0; bx < across; ++bx) {
            const uint32_t x0 = bx * bw, y0 = by * bh;
            const uint32_t rows = tile ? bh : (std::min)(bh, h - y0);  // last strip may be short; tiles never
            uint8_t* p = raw.data();
            for (uint32_t y = y0; y < y0 + rows; ++y)
                for (uint32_t x = x0; x < x0 + bw; ++x)
                    for (uint32_t c = 0; c < ch; ++c) {
                        if (x >= w || y >= h) {
                            for (uint32_t k = 0; k < bps; ++k) *p++ = 0xAB;
                            continue;
                        }
                        const uint32_t v = probe_pattern(x, y, c, bits);
                        if (bps == 1) *p++ = static_cast<uint8_t>(v);
                        else if (g_be) { *p++ = static_cast<uint8_t>(v >> 8); *p++ = static_cast<uint8_t>(v); }
                        else { *p++ = static_cast<uint8_t>(v); *p++ = static_cast<uint8_t>(v >> 8); }
                    }
            const size_t len = static_cast<size_t>(p - raw.data());
            offsets.push_back(o.pos);
            if (deflate) {
                uLongf dlen = static_cast<uLongf>(comp.size());
                if (compress2(comp.data(), &dlen, raw.data(), static_cast<uLong>(len), level) != Z_OK) {
                    fprintf(stderr, "deflate failed\n");
                    return 4;
                }
                o.write(comp.data(), dlen);
                counts.push_back(dlen);
            } else {
                o.write(raw.data(), len);
                counts.push_back(len);
            }
            o.align2();
        }
    }

    // Tags (sorted by tag number).
    const uint16_t off_type = big ? 16 : 4;
    std::vector<Entry> e;
    e.push_back(num(256, 4, {w}));
    e.push_back(num(257, 4, {h}));
    e.push_back(num(258, 3, {bits, bits, bits}));
    e.push_back(num(259, 3, {deflate ? 8u : 1u}));
    e.push_back(num(262, 3, {2}));
    if (!tile) e.push_back(num(273, off_type, offsets));
    e.push_back(num(277, 3, {ch}));
    if (!tile) {
        e.push_back(num(278, 4, {bh}));
        e.push_back(num(279, off_type, counts));
    }
    e.push_back(num(284, 3, {1}));
    if (tile) {
        e.push_back(num(322, 4, {tile}));
        e.push_back(num(323, 4, {tile}));
        e.push_back(num(324, off_type, offsets));
        e.push_back(num(325, off_type, counts));
    }
    if (!icc.empty()) e.push_back(Entry{34675, 7, icc.size(), icc});

    const size_t inline_n = big ? 8 : 4;
    for (auto& x : e) {
        if (x.payload.size() > inline_n) {
            o.align2();
            x.offset = o.pos;
            o.write(x.payload);
        }
    }
    o.align2();
    const uint64_t ifd_pos = o.pos;
    std::vector<uint8_t> ifd;
    put(ifd, e.size(), big ? 8 : 2);
    for (const auto& x : e) {
        put(ifd, x.tag, 2);
        put(ifd, x.type, 2);
        put(ifd, x.count, big ? 8 : 4);
        if (x.payload.size() > inline_n) put(ifd, x.offset, static_cast<int>(inline_n));
        else {
            ifd.insert(ifd.end(), x.payload.begin(), x.payload.end());
            ifd.insert(ifd.end(), inline_n - x.payload.size(), 0);
        }
    }
    put(ifd, 0, big ? 8 : 4);  // no further image
    o.write(ifd);
    const uint64_t file_bytes = o.pos;
    if (!big && file_bytes > 0xFFFFFFFFull) {
        fprintf(stderr, "classic TIFF would exceed 4 GiB; use --big\n");
        o.flush();
        CloseHandle(o.h);
        DeleteFileW(argv[1]);
        return 4;
    }
    std::vector<uint8_t> ptr;
    put(ptr, ifd_pos, big ? 8 : 4);
    o.seek(ifd_ptr_pos);
    o.write(ptr);
    o.flush();
    const bool ok = o.ok;
    CloseHandle(o.h);
    if (!ok) { fprintf(stderr, "write failed\n"); DeleteFileW(argv[1]); return 3; }

    const double secs = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    printf("{\"tool\":\"gen_large_tiff\",\"width\":%u,\"height\":%u,\"bits\":%u,\"channels\":%u,\"big\":%d,\"be\":%d,"
           "\"tile\":%u,\"rows_per_strip\":%u,\"deflate\":%d,\"blocks\":%zu,\"raw_bytes\":%llu,\"file_bytes\":%llu,"
           "\"first_data_offset\":%llu,\"ifd_offset\":%llu,\"icc_bytes\":%zu,\"seconds\":%.1f}\n",
           w, h, bits, ch, big ? 1 : 0, g_be ? 1 : 0, tile, tile ? 0 : bh, deflate ? 1 : 0, offsets.size(),
           static_cast<unsigned long long>(static_cast<uint64_t>(w) * h * ch * bps), static_cast<unsigned long long>(file_bytes),
           static_cast<unsigned long long>(first_data), static_cast<unsigned long long>(ifd_pos), icc.size(), secs);
    return 0;
}
