// Independent TIFF / BigTIFF container walker (does not use libtiff).
// Validates the header, the first IFD, every tag's value range, every
// strip/tile offset + byte count against the file size, and the IFD chain.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <optional>
#include <string>
#include <vector>

namespace iat {

struct TiffScan {
    std::string container_status = "unverified";  // valid | warning | corrupt
    std::string metadata_status = "valid";        // valid | parse_failed | over_limit
    std::vector<Issue> issues;
    uint64_t bytes_scanned = 0;

    bool header_ok = false;
    bool big = false;              // BigTIFF (64-bit offsets)
    bool little_endian = true;
    bool has_ifd = false;          // first IFD readable
    uint64_t ifd_offset = 0;
    uint64_t next_ifd = 0;         // 0 when this is the only image
    bool subifds = false;          // tag 330 present

    uint32_t width = 0, height = 0;
    uint16_t bits = 0;             // common BitsPerSample, 0 when channels differ
    uint16_t spp = 1, compression = 1, photometric = 0xFFFF, planar = 1, sample_format = 1;
    uint16_t predictor = 1, fill_order = 1;
    std::optional<uint16_t> orientation;  // tag 274 as stored
    std::vector<uint16_t> extra_samples;
    bool tiled = false;
    uint32_t tile_width = 0, tile_length = 0, rows_per_strip = 0;
    uint64_t blocks = 0;           // strips or tiles referenced by the IFD
    uint64_t data_end = 0;         // highest byte referenced by image data

    IccInfo icc;
    std::vector<std::string> colorimetry;   // WhitePoint / PrimaryChromaticities / TransferFunction present

    // Metadata summaries (hashes, sizes, small values only).
    std::optional<std::string> exif, xmp, text, resolution, datetime;
    std::string exif_status = "present", xmp_status = "present", text_status = "present";
    std::vector<std::string> other_tags;
    std::vector<std::string> tag_list;      // "256,257,..." in file order (for the report)
};

TiffScan scan_tiff(const ReadOnlyFile& f, uint64_t file_size, const Limits& limits);

std::string tiff_compression_name(uint16_t c);

}  // namespace iat
