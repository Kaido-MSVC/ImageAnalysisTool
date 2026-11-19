// Independent ISOBMFF walk for AVIF (does not use libavif): top-level box bounds,
// truncation, the ftyp brands, and the colour property boxes ('colr' nclx / prof)
// under meta/iprp/ipco. libavif then parses items and decodes; this scan decides
// container integrity on its own, as for the other formats.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <optional>
#include <string>
#include <vector>

namespace iat {

struct AvifNclx {
    uint16_t primaries = 2, transfer = 2, matrix = 2;
    bool full_range = false;
};

struct AvifScan {
    std::string container_status = "unverified";  // valid | warning | corrupt
    std::vector<Issue> issues;
    uint64_t bytes_scanned = 0;
    std::vector<std::string> boxes;               // top-level box types in file order
    std::string major_brand;
    std::vector<std::string> brands;              // major + compatible
    bool is_avif = false;                         // 'avif' or 'avis' brand present
    bool sequence = false;                        // 'avis' brand or a 'moov' box
    bool has_meta = false;
    std::vector<AvifNclx> nclx;                   // every 'colr' nclx property, in ipco order
    uint32_t colr_prof = 0;                       // 'colr' prof / rICC properties
};

AvifScan scan_avif(const ReadOnlyFile& f, uint64_t file_size);

// EXIF orientation (1..8) equivalent to AVIF irot (anticlockwise quarter turns, applied first)
// followed by imir (axis 0 = top-bottom, EXIF 4; axis 1 = left-right, EXIF 2). axis -1 = no imir.
uint32_t avif_orientation(uint32_t irot_angle, int imir_axis);

}  // namespace iat
