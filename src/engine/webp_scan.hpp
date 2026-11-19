// Independent WebP RIFF walker (does not use libwebp). libwebp accepts a file
// whose final padding byte is missing (stage 0, S4) and lets the VP8L header
// hint override the VP8X alpha flag (stage 3), so container integrity and flag
// consistency are decided here, from the RIFF structure alone.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <optional>
#include <string>
#include <vector>

namespace iat {

struct WebpScan {
    std::string container_status = "unverified";  // valid | warning | corrupt
    std::string metadata_status = "valid";        // valid | over_limit
    std::vector<Issue> issues;
    uint64_t bytes_scanned = 0;
    std::optional<uint64_t> trailing_bytes;       // bytes after the RIFF payload (null if RIFF is truncated)
    std::vector<std::string> chunks;              // FourCCs in file order

    bool extended = false;                        // VP8X present
    uint8_t vp8x_flags = 0;
    uint32_t canvas_width = 0, canvas_height = 0;
    bool animated = false;                        // VP8X animation flag or ANIM/ANMF chunk
    std::string image_chunk;                      // "VP8L" | "VP8 " | "" (none found)
    bool has_alph = false;
    uint32_t width = 0, height = 0;               // from the VP8L / VP8 bitstream header
    bool vp8l_alpha_hint = false;                 // VP8L alpha_is_used bit

    std::string icc_status = "absent";            // embedded | absent | over_budget
    std::vector<uint8_t> icc;
    std::string exif_status = "absent";           // present | absent | over_limit
    std::vector<uint8_t> exif;
    std::optional<uint64_t> xmp_bytes;
    std::string xmp_sha256;
    std::vector<std::string> unknown_chunks;
};

WebpScan scan_webp(const ReadOnlyFile& f, uint64_t file_size, const Limits& limits);

}  // namespace iat
