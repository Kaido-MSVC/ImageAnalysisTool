// Resource limits. Every limit is finite; there is no "unlimited" setting.
#pragma once

#include <cstdint>
#include <string>

namespace iat {

struct Limits {
    uint64_t job_commit_mib = 1024;    // Job commit limit for the worker process
    uint64_t buffer_mib = 64;          // tool-managed row/band buffers (both sides together)
    uint64_t metadata_item_mib = 8;    // one metadata item (e.g. decompressed ICC)
    uint64_t metadata_file_mib = 32;   // all metadata kept for one file
    uint64_t temp_gib = 8;             // temporary storage (stage 2+)
    uint64_t deadline_s = 600;         // wall-clock deadline for the whole worker run

    // Test-only (stage 3): scales whole-image decoder memory estimates down so the
    // allocator hard cap can be reached in end-to-end tests. Not settable through
    // --limits; only the hidden --test-codec-estimate-pct flag changes it, and the
    // report then says so. 100 = normal operation.
    uint64_t test_codec_estimate_pct = 100;

    // Parses "name=value"; returns an error message or empty on success.
    std::string set(const std::string& assignment);

    uint64_t buffer_bytes() const { return buffer_mib << 20; }
    uint64_t metadata_item_bytes() const { return metadata_item_mib << 20; }
    uint64_t metadata_file_bytes() const { return metadata_file_mib << 20; }

    // Hard cap for whole-image decoders (libjxl allocator, WebP estimate): the Job
    // commit limit minus a fixed reserve for the worker itself (decision D10).
    static constexpr uint64_t kCodecReserve = 64ull << 20;
    uint64_t codec_cap_bytes() const {
        const uint64_t job = job_commit_mib << 20;
        return job > kCodecReserve ? job - kCodecReserve : 0;
    }
};

inline std::string Limits::set(const std::string& a) {
    const size_t eq = a.find('=');
    if (eq == std::string::npos) return "expected name=value: " + a;
    const std::string name = a.substr(0, eq), val = a.substr(eq + 1);
    if (val.empty() || val.size() > 12 || val.find_first_not_of("0123456789") != std::string::npos)
        return "value must be a positive integer: " + a;
    const uint64_t v = std::stoull(val);
    struct Spec { const char* name; uint64_t Limits::*field; uint64_t lo, hi; };
    static const Spec specs[] = {
        {"job_commit_mib", &Limits::job_commit_mib, 16, 1u << 20},
        {"buffer_mib", &Limits::buffer_mib, 1, 1u << 16},
        {"metadata_item_mib", &Limits::metadata_item_mib, 1, 1024},
        {"metadata_file_mib", &Limits::metadata_file_mib, 1, 4096},
        {"temp_gib", &Limits::temp_gib, 1, 1u << 16},
        {"deadline_s", &Limits::deadline_s, 1, 7 * 24 * 3600},
    };
    for (const Spec& s : specs) {
        if (name == s.name) {
            if (v < s.lo || v > s.hi)
                return name + " must be between " + std::to_string(s.lo) + " and " + std::to_string(s.hi);
            this->*s.field = v;
            return {};
        }
    }
    return "unknown limit: " + name;
}

}  // namespace iat
