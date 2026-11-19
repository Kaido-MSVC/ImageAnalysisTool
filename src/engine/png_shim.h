/* Thin C boundary around libpng. libpng reports errors with longjmp; every
 * setjmp lives in png_shim.c so the jump never crosses C++ frames. */
#ifndef IAT_PNG_SHIM_H
#define IAT_PNG_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Returns the number of bytes copied into buf (short only at EOF), or
 * (size_t)-1 on I/O error. Must not throw. */
typedef size_t (*pngs_read_fn)(void* io, uint8_t* buf, size_t n);

typedef struct pngs pngs;

typedef struct {
    uint32_t width, height;
    int bit_depth, color_type, interlace, channels;
    size_t rowbytes;              /* after png_read_info */
    size_t rowbytes_after_update; /* after png_read_update_info with no transform */
    int npal;
    uint8_t pal[256 * 3];
    int ntrns;                    /* palette alpha entries */
    uint8_t trns_alpha[256];
    int has_trns;
    uint16_t trns_gray, trns_r, trns_g, trns_b;
    int has_iccp;                 /* libpng accepted an iCCP profile */
} pngs_info;

enum { PNGS_OK = 0, PNGS_ERROR = 1, PNGS_RESOURCE = 2, PNGS_IO = 3 };

/* alloc_limit: hard cap for libpng's own allocations (bytes).
 * chunk_malloc_max: cap for one ancillary chunk (bytes). */
pngs* pngs_create(pngs_read_fn read, void* io, size_t alloc_limit, size_t chunk_malloc_max);
int pngs_read_info(pngs* s, pngs_info* info);
int pngs_read_row(pngs* s, uint8_t* row);
/* Pass (0..6, 0 when not interlaced) and row within that pass that the next
 * pngs_read_row will return; used to cross-check the Adam7 pass geometry. */
void pngs_position(const pngs* s, int* pass, uint32_t* row);
int pngs_finish(pngs* s); /* png_read_end: chunks after IDAT, IEND, CRCs */
const char* pngs_error(const pngs* s);
int pngs_warning_count(const pngs* s);
const char* pngs_warning(const pngs* s, int i);
size_t pngs_alloc_peak(const pngs* s);
void pngs_destroy(pngs* s);
const char* pngs_version(void);

#ifdef __cplusplus
}
#endif

#endif
