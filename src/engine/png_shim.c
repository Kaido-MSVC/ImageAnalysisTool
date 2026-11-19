/* libpng wrapper: no transforms (16-bit, packed low bit depths, palette
 * indices and tRNS are returned exactly as stored), CRC errors are fatal for
 * every chunk, allocations are tracked and capped. */
#include "png_shim.h"

#include <png.h>
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define PNGS_MAX_WARN 32

struct pngs {
    jmp_buf jb;
    png_structp png;
    png_infop info;
    png_infop end;
    pngs_read_fn read;
    void* io;
    int failed;      /* sticky: PNGS_* code of the first failure */
    int io_error;
    int alloc_fail;
    size_t alloc_cur, alloc_peak, alloc_limit;
    char err[256];
    int nwarn;
    char warn[PNGS_MAX_WARN][160];
};

static void on_error(png_structp png, png_const_charp msg) {
    pngs* s = (pngs*)png_get_error_ptr(png);
    snprintf(s->err, sizeof s->err, "%s", msg ? msg : "libpng error");
    longjmp(s->jb, 1);
}

static void on_warning(png_structp png, png_const_charp msg) {
    pngs* s = (pngs*)png_get_error_ptr(png);
    if (s->nwarn < PNGS_MAX_WARN) snprintf(s->warn[s->nwarn++], sizeof s->warn[0], "%s", msg ? msg : "");
}

static png_voidp on_malloc(png_structp png, png_alloc_size_t n) {
    pngs* s = (pngs*)png_get_mem_ptr(png);
    size_t* p;
    if (n > s->alloc_limit || s->alloc_cur > s->alloc_limit - n) {
        s->alloc_fail = 1;
        return NULL;
    }
    p = (size_t*)malloc(n + 16);
    if (!p) {
        s->alloc_fail = 1;
        return NULL;
    }
    p[0] = n;
    s->alloc_cur += n;
    if (s->alloc_cur > s->alloc_peak) s->alloc_peak = s->alloc_cur;
    return (char*)p + 16;
}

static void on_free(png_structp png, png_voidp ptr) {
    pngs* s = (pngs*)png_get_mem_ptr(png);
    size_t* p;
    if (!ptr) return;
    p = (size_t*)((char*)ptr - 16);
    s->alloc_cur -= p[0];
    free(p);
}

static void on_read(png_structp png, png_bytep data, size_t n) {
    pngs* s = (pngs*)png_get_io_ptr(png);
    size_t got = s->read(s->io, data, n);
    if (got == (size_t)-1) {
        s->io_error = 1;
        png_error(png, "I/O error while reading input");
    }
    if (got != n) png_error(png, "unexpected end of file");
}

static int fail_code(const pngs* s) {
    if (s->io_error) return PNGS_IO;
    if (s->alloc_fail) return PNGS_RESOURCE;
    return PNGS_ERROR;
}

pngs* pngs_create(pngs_read_fn read, void* io, size_t alloc_limit, size_t chunk_malloc_max) {
    pngs* s = (pngs*)calloc(1, sizeof(pngs));
    if (!s) return NULL;
    s->read = read;
    s->io = io;
    s->alloc_limit = alloc_limit;
    s->png = png_create_read_struct_2(PNG_LIBPNG_VER_STRING, s, on_error, on_warning, s, on_malloc, on_free);
    if (!s->png) {
        free(s);
        return NULL;
    }
    if (setjmp(s->jb)) {
        png_destroy_read_struct(&s->png, NULL, NULL);
        free(s);
        return NULL;
    }
    s->info = png_create_info_struct(s->png);
    s->end = png_create_info_struct(s->png);
    png_set_read_fn(s->png, s, on_read);
    png_set_crc_action(s->png, PNG_CRC_ERROR_QUIT, PNG_CRC_ERROR_QUIT);
    /* Dimension limits are enforced by the tool's own budget checks; lift
     * libpng's default 1,000,000 so legal PNGs (up to 2^31-1) are not refused. */
    png_set_user_limits(s->png, 0x7fffffffu, 0x7fffffffu);
    png_set_chunk_malloc_max(s->png, (png_alloc_size_t)chunk_malloc_max);
    return s;
}

int pngs_read_info(pngs* s, pngs_info* out) {
    png_uint_32 w, h;
    int depth, ctype, ilace;
    if (s->failed) return s->failed;
    memset(out, 0, sizeof *out);
    if (setjmp(s->jb)) return s->failed = fail_code(s);
    png_read_info(s->png, s->info);
    png_get_IHDR(s->png, s->info, &w, &h, &depth, &ctype, &ilace, NULL, NULL);
    out->width = w;
    out->height = h;
    out->bit_depth = depth;
    out->color_type = ctype;
    out->interlace = ilace;
    out->channels = png_get_channels(s->png, s->info);
    out->rowbytes = png_get_rowbytes(s->png, s->info);
    if (ctype == PNG_COLOR_TYPE_PALETTE) {
        png_colorp pal = NULL;
        int n = 0, i;
        if (png_get_PLTE(s->png, s->info, &pal, &n) && pal) {
            out->npal = n;
            for (i = 0; i < n && i < 256; ++i) {
                out->pal[i * 3] = pal[i].red;
                out->pal[i * 3 + 1] = pal[i].green;
                out->pal[i * 3 + 2] = pal[i].blue;
            }
        }
    }
    if (png_get_valid(s->png, s->info, PNG_INFO_tRNS)) {
        png_bytep ta = NULL;
        int nt = 0;
        png_color_16p tc = NULL;
        png_get_tRNS(s->png, s->info, &ta, &nt, &tc);
        out->has_trns = 1;
        if (ctype == PNG_COLOR_TYPE_PALETTE && ta) {
            out->ntrns = nt > 256 ? 256 : nt;
            memcpy(out->trns_alpha, ta, (size_t)out->ntrns);
        } else if (tc) {
            out->trns_gray = tc->gray;
            out->trns_r = tc->red;
            out->trns_g = tc->green;
            out->trns_b = tc->blue;
        }
    }
    out->has_iccp = png_get_valid(s->png, s->info, PNG_INFO_iCCP) != 0;
    /* No transform is configured (in particular no png_set_interlace_handling):
     * update_info must not change the row size. For Adam7, png_read_row then
     * returns each pass's reduced rows in file order (libpng 1.6.59
     * png_read_finish_row skips empty passes). */
    png_read_update_info(s->png, s->info);
    out->rowbytes_after_update = png_get_rowbytes(s->png, s->info);
    return PNGS_OK;
}

void pngs_position(const pngs* s, int* pass, uint32_t* row) {
    *pass = png_get_current_pass_number(s->png);
    *row = png_get_current_row_number(s->png);
}

int pngs_read_row(pngs* s, uint8_t* row) {
    if (s->failed) return s->failed;
    if (setjmp(s->jb)) return s->failed = fail_code(s);
    png_read_row(s->png, row, NULL);
    return PNGS_OK;
}

int pngs_finish(pngs* s) {
    if (s->failed) return s->failed;
    if (setjmp(s->jb)) return s->failed = fail_code(s);
    png_read_end(s->png, s->end);
    return PNGS_OK;
}

const char* pngs_error(const pngs* s) { return s->err; }
int pngs_warning_count(const pngs* s) { return s->nwarn; }
const char* pngs_warning(const pngs* s, int i) { return (i >= 0 && i < s->nwarn) ? s->warn[i] : ""; }
size_t pngs_alloc_peak(const pngs* s) { return s->alloc_peak; }

void pngs_destroy(pngs* s) {
    if (!s) return;
    if (s->png) png_destroy_read_struct(&s->png, &s->info, &s->end);
    free(s);
}

const char* pngs_version(void) { return png_get_libpng_ver(NULL); }
