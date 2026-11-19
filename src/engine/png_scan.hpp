// Independent PNG container walker (does not use libpng).
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <array>
#include <optional>
#include <string>
#include <vector>

namespace iat {

struct PngText {
    std::string keyword;       // Latin-1 keyword as UTF-8 (display only)
    std::string keyword_raw;   // keyword bytes as stored in the file (hashed)
    std::string kind;         // tEXt | zTXt | iTXt
    std::string sha256;        // of the (decompressed) text value
    uint64_t bytes = 0;
    std::string status = "present";  // present | parse_failed | over_limit
};

struct PngScan {
    bool signature_ok = false;
    std::string container_status = "unverified";  // valid | warning | corrupt
    std::vector<Issue> issues;

    bool has_ihdr = false;
    uint32_t width = 0, height = 0;
    int bit_depth = 0, color_type = -1, compression = -1, filter = -1, interlace = -1;
    bool has_plte = false, has_trns = false, animated = false;

    bool ended = false;           // IEND reached with valid CRC
    uint64_t end_offset = 0;      // byte after IEND
    uint64_t trailing_bytes = 0;
    uint64_t idat_bytes = 0;
    uint32_t idat_chunks = 0;
    uint64_t bytes_scanned = 0;
    std::vector<std::string> chunk_order;

    // Colour signals (raw values, validated lengths).
    std::optional<std::array<uint8_t, 4>> cicp;
    std::optional<uint8_t> srgb_intent;
    std::optional<uint32_t> gama;
    std::optional<std::array<uint32_t, 8>> chrm;
    std::vector<uint8_t> sbit;
    IccInfo icc;                  // filled from iCCP (decompressed here, capped)
    bool iccp_present = false;
    std::optional<std::string> mdcv_hex, clli_hex;

    // Metadata
    std::optional<std::vector<uint8_t>> exif;
    std::string exif_status = "absent";  // present | absent | over_limit
    std::optional<std::string> phys;     // "x/y unit"
    std::optional<std::string> time;     // "YYYY-MM-DD hh:mm:ss"
    std::optional<int> ornt;             // ImageMagick private orientation chunk (metadata only)
    std::vector<PngText> texts;          // includes XMP (keyword XML:com.adobe.xmp)
    std::vector<std::string> other_ancillary;
    std::string metadata_status = "valid";  // valid | parse_failed | over_limit
};

PngScan scan_png(const ReadOnlyFile& f, uint64_t file_size, const Limits& limits);

// Exif orientation from an eXIf payload: value 1..8, 0 when the tag is absent,
// -1 when the structure cannot be parsed.
int exif_orientation(const std::vector<uint8_t>& exif);

// zlib inflate with an output cap. Returns "ok", "over_limit" or "corrupt"/"truncated".
std::string inflate_capped(const uint8_t* data, size_t n, uint64_t cap, std::vector<uint8_t>& out);

}  // namespace iat
