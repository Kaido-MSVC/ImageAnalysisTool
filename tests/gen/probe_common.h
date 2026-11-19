/* Shared helpers for stage-0 feasibility probes (usable from C and C++).
 * Probes print exactly one JSON object on stdout. */
#ifndef IAT_PROBE_COMMON_H
#define IAT_PROBE_COMMON_H

#include <windows.h>
#include <psapi.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Read-only input: GENERIC_READ + FILE_SHARE_READ, so nobody may write or
 * replace the file while it is open. */
typedef struct {
    HANDLE h;
    uint64_t size;
    uint64_t pos;
    uint64_t bytes_read;
} probe_file;

static inline int probe_open_ro(probe_file* f, const wchar_t* path) {
    LARGE_INTEGER sz;
    memset(f, 0, sizeof(*f));
    f->h = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING,
                       FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, NULL);
    if (f->h == INVALID_HANDLE_VALUE) return 0;
    if (!GetFileSizeEx(f->h, &sz)) { CloseHandle(f->h); f->h = INVALID_HANDLE_VALUE; return 0; }
    f->size = (uint64_t)sz.QuadPart;
    return 1;
}

/* Reads up to n bytes; returns bytes actually read (short only at EOF). */
static inline size_t probe_read(probe_file* f, void* buf, size_t n) {
    size_t done = 0;
    while (done < n) {
        DWORD chunk = (DWORD)((n - done) > 0x40000000u ? 0x40000000u : (n - done));
        DWORD got = 0;
        if (!ReadFile(f->h, (char*)buf + done, chunk, &got, NULL) || got == 0) break;
        done += got;
    }
    f->pos += done;
    f->bytes_read += done;
    return done;
}

static inline int probe_seek(probe_file* f, uint64_t off) {
    LARGE_INTEGER li;
    li.QuadPart = (LONGLONG)off;
    if (!SetFilePointerEx(f->h, li, NULL, FILE_BEGIN)) return 0;
    f->pos = off;
    return 1;
}

static inline void probe_close(probe_file* f) {
    if (f->h && f->h != INVALID_HANDLE_VALUE) CloseHandle(f->h);
    f->h = INVALID_HANDLE_VALUE;
}

/* Ground-truth file written by tools/gen_fixtures.py. */
typedef struct {
    uint32_t w, h, ch, bits;
    uint8_t* u8;   /* bits <= 8 */
    uint16_t* u16; /* bits == 16 */
} probe_truth;

static inline int probe_load_truth(probe_truth* t, const wchar_t* path) {
    probe_file f;
    unsigned char hdr[24];
    size_t n, bytes;
    memset(t, 0, sizeof(*t));
    if (!probe_open_ro(&f, path)) return 0;
    if (probe_read(&f, hdr, 24) != 24 || memcmp(hdr, "IATT", 4) != 0) { probe_close(&f); return 0; }
    memcpy(&t->w, hdr + 8, 4); memcpy(&t->h, hdr + 12, 4);
    memcpy(&t->ch, hdr + 16, 4); memcpy(&t->bits, hdr + 20, 4);
    n = (size_t)t->w * t->h * t->ch;
    bytes = n * (t->bits <= 8 ? 1 : 2);
    if (t->bits <= 8) t->u8 = (uint8_t*)malloc(bytes ? bytes : 1);
    else t->u16 = (uint16_t*)malloc(bytes ? bytes : 1);
    if (probe_read(&f, t->bits <= 8 ? (void*)t->u8 : (void*)t->u16, bytes) != bytes) {
        probe_close(&f); return 0;
    }
    probe_close(&f);
    return 1;
}

static inline uint32_t probe_truth_at(const probe_truth* t, size_t i) {
    return t->bits <= 8 ? t->u8[i] : t->u16[i];
}

/* Synthetic pattern used for large images (no truth file needed). */
static inline uint32_t probe_pattern(uint64_t x, uint64_t y, uint32_t c, uint32_t bits) {
    uint32_t mask = bits >= 32 ? 0xFFFFFFFFu : ((1u << bits) - 1u);
    return (uint32_t)((x * 7u + y * 13u + (uint64_t)c * 101u + ((x ^ y) >> 3)) & mask);
}

typedef struct {
    uint64_t peak_commit;  /* PeakPagefileUsage: peak private commit */
    uint64_t peak_ws;      /* PeakWorkingSetSize */
} probe_mem;

static inline probe_mem probe_memory(void) {
    PROCESS_MEMORY_COUNTERS_EX pmc;
    probe_mem m = {0, 0};
    if (GetProcessMemoryInfo(GetCurrentProcess(), (PROCESS_MEMORY_COUNTERS*)&pmc, sizeof(pmc))) {
        m.peak_commit = pmc.PeakPagefileUsage;
        m.peak_ws = pmc.PeakWorkingSetSize;
    }
    return m;
}

static inline double probe_ms(LARGE_INTEGER t0) {
    LARGE_INTEGER t1, fq;
    QueryPerformanceCounter(&t1);
    QueryPerformanceFrequency(&fq);
    return (double)(t1.QuadPart - t0.QuadPart) * 1000.0 / (double)fq.QuadPart;
}

/* Minimal JSON string escaping into a fixed buffer. */
static inline const char* probe_json_str(const char* s, char* out, size_t cap) {
    size_t o = 0;
    if (!s) s = "";
    for (; *s && o + 7 < cap; ++s) {
        unsigned char c = (unsigned char)*s;
        if (c == '"' || c == '\\') { out[o++] = '\\'; out[o++] = (char)c; }
        else if (c < 0x20) { o += (size_t)snprintf(out + o, cap - o, "\\u%04x", c); }
        else out[o++] = (char)c;
    }
    out[o] = 0;
    return out;
}

static inline char* probe_utf8(const wchar_t* w) {
    static char buf[2048];
    WideCharToMultiByte(CP_UTF8, 0, w, -1, buf, (int)sizeof(buf), NULL, NULL);
    return buf;
}

#ifdef __cplusplus
}
#endif

#endif
