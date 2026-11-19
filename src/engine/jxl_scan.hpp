// Independent JPEG XL container walker (ISO/IEC 18181-2 box structure; does not
// use libjxl). A bare codestream has no boxes: its end is established by the
// decoder (unconsumed bytes after success), see jxl_side.cpp.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <optional>
#include <string>
#include <vector>

namespace iat {

struct JxlScan {
    std::string container_status = "unverified";  // valid | warning | corrupt
    std::string metadata_status = "valid";        // valid | over_limit
    std::vector<Issue> issues;
    uint64_t bytes_scanned = 0;
    bool container = false;                       // false: bare codestream (FF 0A)
    std::vector<std::string> boxes;               // box types in file order ("brob:Exif" for compressed boxes)
    uint64_t codestream_bytes = 0;                // jxlc payload or sum of jxlp payloads (container only)
    uint32_t jxlp_parts = 0;
    bool level_box = false;                       // jxll present
    int level = 5;                                // jxll value, 5 when absent
    std::string exif_status = "absent";           // present | absent | over_limit | compressed
    std::vector<uint8_t> exif;                    // TIFF data (the 4-byte offset prefix removed)
    std::optional<uint64_t> xmp_bytes;
    std::string xmp_sha256;
    bool jbrd = false;                            // JPEG reconstruction data present
    std::vector<std::string> other_boxes;
};

JxlScan scan_jxl(const ReadOnlyFile& f, uint64_t file_size, const Limits& limits);

}  // namespace iat
