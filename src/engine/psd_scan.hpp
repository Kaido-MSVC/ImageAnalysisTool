// Independent PSD / PSB structure scanner (Adobe Photoshop File Formats
// Specification). Reads the header, colour mode data, image resources, the
// lengths of the layer and mask section and the merged ("composite") image data
// section; it never interprets layer pixels. All lengths and offsets are 64-bit
// and checked against the file size. PSB (version 2) differs only in field widths:
// 64-bit section lengths and 32-bit RLE row byte counts.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <optional>
#include <string>
#include <vector>

namespace iat {

struct PsdScan {
    std::string container_status = "unverified";  // valid | warning | corrupt
    std::string metadata_status = "valid";        // valid | over_limit | parse_failed
    std::vector<Issue> issues;
    uint64_t bytes_scanned = 0;
    std::optional<uint64_t> trailing_bytes;       // after the image data (null when the data is truncated)
    std::vector<std::string> sections;            // reached sections, for the report

    bool header_ok = false;
    uint16_t version = 0;                         // 1 = PSD, 2 = PSB
    uint16_t channels = 0;
    uint32_t width = 0, height = 0;
    uint16_t depth = 0;                           // 1 | 8 | 16 | 32
    uint16_t mode = 0;                            // 0 bitmap, 1 gray, 2 indexed, 3 RGB, 4 CMYK, 7 multichannel, 8 duotone, 9 Lab

    std::vector<uint8_t> palette;                 // indexed: 768 bytes, R[256] G[256] B[256]
    std::optional<uint16_t> palette_count;        // resource 0x0416 (number of colours)
    std::optional<uint16_t> transparent_index;    // resource 0x0417

    // Version info (0x0421): hasRealMergedData. Absent in files written by many non-Adobe tools.
    bool version_info = false;
    bool has_real_merged = false;
    bool icc_untagged = false;                    // resource 0x0411 = 1
    std::string icc_status = "absent";            // embedded | absent | over_budget
    std::vector<uint8_t> icc;
    std::string exif_status = "absent";           // present | absent | over_limit
    std::vector<uint8_t> exif;                    // raw TIFF (resource 0x0422)
    std::optional<uint64_t> xmp_bytes;            // resource 0x0424
    std::string xmp_sha256;
    std::optional<uint64_t> iptc_bytes;           // resource 0x0404
    std::string iptc_sha256;
    std::optional<std::string> resolution;        // resource 0x03ED summary
    uint32_t resource_count = 0;

    // Layer and mask information. layers_known is false when tagged blocks could not be
    // walked; the composite is then never treated as "flattened".
    uint64_t layer_section_bytes = 0;
    int32_t layer_count_field = 0;                // signed: < 0 => first extra channel is merged transparency
    uint32_t layer_count = 0;                     // |field| (+ Lr16 / Lr32 / Layr blocks)
    bool layers_known = true;

    // Image data section.
    uint64_t image_data_offset = 0;               // position of the compression field
    uint16_t compression = 0xFFFF;                // 0 raw, 1 RLE (PackBits), 2 ZIP, 3 ZIP+prediction
    uint64_t row_bytes = 0;                       // bytes per channel row (uncompressed)
    uint64_t data_offset = 0;                     // first byte after the compression field (+ RLE table)
    uint64_t rle_table_offset = 0;                // RLE only
    uint64_t data_bytes = 0;                      // raw: channels*height*row_bytes; RLE: sum of row counts
};

PsdScan scan_psd(const ReadOnlyFile& f, uint64_t file_size, const Limits& limits);

// Incremental PackBits decoder for one row (0.5.1, D33). The row's packed bytes may be fed
// in any number of pieces; finish() then requires that the last run is complete and that the
// row decoded to exactly `out_size` bytes. Any valid PackBits is accepted: -128 (0x80) no-op
// headers anywhere, and non-optimal runs (split literals, 2-byte repeats), so the packed size
// of a row has no upper bound other than its byte count. A run that would write past the row
// is an error at once.
class PackBitsRow {
public:
    PackBitsRow(uint8_t* out, size_t out_size) : out_(out), size_(out_size) {}
    bool feed(const uint8_t* in, size_t n, std::string& why);
    bool finish(std::string& why);

private:
    uint8_t* out_;
    size_t size_, o_ = 0;
    size_t literal_left_ = 0;   // literal bytes still to copy
    size_t repeat_count_ = 0;   // > 0: the next byte is the value to repeat
};

// PackBits decode of exactly one row: `in` must decode to exactly `out_size` bytes using
// every input byte. Returns false (and leaves a reason) otherwise. Exposed for unit tests.
bool packbits_row(const uint8_t* in, size_t in_size, uint8_t* out, size_t out_size, std::string& why);

}  // namespace iat
