// iat-core public API: one analysis run and its report, without any console
// output or process exit. Used by the iat CLI; meant to be callable from other
// front ends as well.
//
// Decoding runs in iat-worker.exe (next to the calling executable) inside a Job
// Object with the requested limits; this library holds no image decoder.
#pragma once

#include "common/limits.hpp"
#include "model/render.hpp"
#include "model/report.hpp"

#include <atomic>
#include <string>
#include <vector>

namespace iat {

// Tool version, from CMake project(VERSION) (single source).
const char* version();

struct RunRequest {
    std::string mode;            // compare | inspect
    std::wstring original;
    std::wstring output;         // compare only
    std::wstring temp_dir;       // empty: system temporary directory; otherwise an existing directory
    bool full_paths = false;     // record full paths in the report
    Limits limits;
};

// Runs one analysis. Never throws for input, worker or environment problems:
// every outcome (including cancellation through `cancel`) is expressed in the
// returned Report, whose exit_code / exit_reason are already decided
// (decide_exit). Report files are not written here.
Report run_analysis(const RunRequest& req, const std::atomic<bool>& cancel);

// Report targets must be new files in an existing directory and must not alias
// an input or each other. Returns an empty string when they are acceptable,
// otherwise the reason. Empty paths are ignored.
std::string validate_report_targets(const RunRequest& req, const std::wstring& report_md, const std::wstring& report_json);

// The exact report texts the CLI writes and prints (JSON ends with a newline).
std::string report_json_text(const Report& r);
std::string report_markdown_text(const Report& r);

// Writes `data` to a temporary file next to `target`, flushes, then renames it
// without replacement: an existing target is never overwritten. On failure the
// reason is stored in `error` and false is returned.
bool write_new_file(const std::wstring& target, const std::string& data, const std::string& run_id, std::string& error);

}  // namespace iat
