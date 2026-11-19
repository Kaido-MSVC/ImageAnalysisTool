// Worker-side analysis: structure scan, row decoding, strict comparison.
#pragma once

#include "../common/limits.hpp"
#include "../common/win_util.hpp"
#include "../model/report.hpp"

#include <optional>

namespace iat {

struct AnalysisInput {
    std::string mode;                    // compare | inspect
    const ReadOnlyFile* original = nullptr;
    const ReadOnlyFile* output = nullptr;  // compare only
    Limits limits;
    // Temporary files created by the parent (one per side, may be null) and
    // the free space of their volume measured by the parent.
    HANDLE temp[2] = {nullptr, nullptr};
    uint64_t temp_free_bytes = 0;
};

// Never throws for input problems; every outcome is expressed in the Report.
Report analyze(const AnalysisInput& in);

// Pure helpers exposed for unit tests.
struct RowLayout {
    std::string model;       // gray | gray_alpha | rgb | rgba
    uint32_t channels = 0;
    uint32_t bits = 0;       // canonical bits per channel
};
// Estimated tool-managed buffer bytes for row decoding one PNG side, or nullopt on overflow.
std::optional<uint64_t> png_row_budget(uint32_t width, int bit_depth, int color_type, bool has_trns);

// Smallest limit value (in MiB, rounded up to a multiple of step_mib) whose usable bytes
// (value MiB - reserve_bytes) hold needed_bytes; nullopt when that exceeds max_mib or the
// arithmetic overflows. Used for job_commit_mib (reserve 64 MiB, step 256) and buffer_mib.
std::optional<uint64_t> suggest_limit_mib(uint64_t needed_bytes, uint64_t reserve_bytes, uint64_t step_mib, uint64_t max_mib);

// D12: is a JXL colour enum on one side equal to the other side's colour description?
// Not applicable (equal=false, empty basis) when neither or both sides are JXL enums.
struct JxlEquivalence {
    bool equal = false;
    std::string basis, note;   // note: recorded-only differences (rendering intent)
};
JxlEquivalence jxl_equivalence(const FileReport& a, const FileReport& b);

}  // namespace iat
