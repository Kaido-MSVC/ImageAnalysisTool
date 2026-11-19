// Report targets, report texts and no-overwrite file writing.
#include "core/core.hpp"

#include "common/text.hpp"
#include "common/win_util.hpp"

#include <nlohmann/json.hpp>

#include <algorithm>
#include <string>
#include <vector>

#ifndef IAT_VERSION
#error IAT_VERSION must come from CMake project(VERSION)
#endif

namespace iat {

const char* version() { return IAT_VERSION; }

std::string validate_report_targets(const RunRequest& req, const std::wstring& report_md, const std::wstring& report_json) {
    std::vector<std::wstring> reports;
    if (!report_md.empty()) reports.push_back(report_md);
    if (!report_json.empty()) reports.push_back(report_json);
    if (reports.size() == 2 && same_path(reports[0], reports[1])) return "report paths refer to the same file";
    for (const auto& r : reports) {
        if (same_path(r, req.original) || (!req.output.empty() && same_path(r, req.output)))
            return "report path is an input file: " + utf8(r);
        if (path_exists(r)) return "report already exists (never overwritten): " + utf8(r);
        if (!is_directory(parent_dir_of(r))) return "report directory does not exist: " + utf8(parent_dir_of(r));
    }
    return {};
}

// Last line of defence for strings that came from files: file strings are converted where
// they are read; anything that still is not UTF-8 becomes U+FFFD instead of failing the report.
std::string report_json_text(const Report& r) {
    return nlohmann::json(r).dump(1, ' ', false, nlohmann::json::error_handler_t::replace) + "\n";
}

std::string report_markdown_text(const Report& r) { return utf8_sanitize(render_markdown(r), true); }

bool write_new_file(const std::wstring& target, const std::string& data, const std::string& run_id, std::string& error) {
    const std::wstring tmp = full_path(target) + L".iat-tmp-" + wide(run_id.substr(0, 8));
    HANDLE h = CreateFileW(tmp.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (h == INVALID_HANDLE_VALUE) { error = "cannot create temporary file: " + win_error_text(GetLastError()); return false; }
    size_t done = 0;
    bool ok = true;
    while (done < data.size()) {
        DWORD n = 0;
        if (!WriteFile(h, data.data() + done, static_cast<DWORD>(std::min<size_t>(data.size() - done, 1u << 20)), &n, nullptr) || !n) {
            ok = false;
            break;
        }
        done += n;
    }
    ok = ok && FlushFileBuffers(h);
    const DWORD werr = ok ? 0 : GetLastError();
    CloseHandle(h);
    if (!ok) { DeleteFileW(tmp.c_str()); error = "write failed: " + win_error_text(werr); return false; }
    if (!MoveFileExW(tmp.c_str(), full_path(target).c_str(), MOVEFILE_WRITE_THROUGH)) {
        const DWORD e = GetLastError();
        DeleteFileW(tmp.c_str());
        error = (e == ERROR_ALREADY_EXISTS || e == ERROR_FILE_EXISTS ? "target appeared meanwhile, not overwritten: " : "rename failed: ") +
                win_error_text(e);
        return false;
    }
    return true;
}

}  // namespace iat
