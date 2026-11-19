// iat.exe — CLI front end of iat-core. Parses arguments, runs one analysis
// through the core library, writes/prints the reports and decides the process
// exit code. Holds no image decoder (decoding runs in iat-worker.exe).
#include "common/limits.hpp"
#include "common/text.hpp"
#include "common/win_util.hpp"
#include "core/core.hpp"

#include <algorithm>
#include <atomic>
#include <string>
#include <vector>

namespace {

using namespace iat;

std::atomic<bool> g_cancel{false};

BOOL WINAPI on_ctrl(DWORD type) {
    if (type == CTRL_C_EVENT || type == CTRL_BREAK_EVENT || type == CTRL_CLOSE_EVENT) {
        g_cancel = true;
        return TRUE;
    }
    return FALSE;
}

void err(const std::string& s) {
    const std::wstring w = wide(s + "\n");
    DWORD n;
    HANDLE h = GetStdHandle(STD_ERROR_HANDLE);
    if (GetFileType(h) == FILE_TYPE_CHAR) WriteConsoleW(h, w.data(), static_cast<DWORD>(w.size()), &n, nullptr);
    else WriteFile(h, s.data(), static_cast<DWORD>(s.size()), &n, nullptr), WriteFile(h, "\n", 1, &n, nullptr);
}

void write_stdout(const std::string& s) {
    HANDLE h = GetStdHandle(STD_OUTPUT_HANDLE);
    size_t done = 0;
    while (done < s.size()) {
        DWORD n = 0;
        if (!WriteFile(h, s.data() + done, static_cast<DWORD>(std::min<size_t>(s.size() - done, 1u << 20)), &n, nullptr) || !n) return;
        done += n;
    }
}

const char* kUsage =
    "usage:\n"
    "  iat compare <original> <output> [--report-md <path>] [--report-json <path>] [--stdout md|json]\n"
    "              [--full-paths] [--temp-dir <dir>] [--limits name=value ...]\n"
    "  iat inspect <file> [--report-md <path>] [--report-json <path>] [--stdout md|json] [--full-paths]\n"
    "              [--temp-dir <dir>] [--limits name=value ...]\n"
    "  iat --version\n"
    "temp: decoded samples may be written to delete-on-close files in --temp-dir (default: system temp)\n"
    "limits: job_commit_mib (1024), buffer_mib (64), metadata_item_mib (8), metadata_file_mib (32),\n"
    "        temp_gib (8), deadline_s (600)\n"
    "exit codes: 0 match/ok, 1 mismatch, 2 indeterminate/unsupported, 3 decode/structure failure,\n"
    "            4 resource limit/deadline/cancel (not checked), 5 argument/I-O/worker/environment error";

struct Args {
    RunRequest req;
    std::wstring report_md, report_json;
    std::string stdout_fmt;
};

std::string parse_args(int argc, wchar_t** argv, Args& args) {
    RunRequest& a = args.req;
    if (argc < 2) return "missing command";
    a.mode = utf8(argv[1]);
    if (a.mode != "compare" && a.mode != "inspect") return "unknown command: " + a.mode;
    std::vector<std::wstring> pos;
    for (int i = 2; i < argc; ++i) {
        const std::wstring s = argv[i];
        auto need = [&](const char* what) -> const wchar_t* {
            if (i + 1 >= argc) return nullptr;
            (void)what;
            return argv[++i];
        };
        if (s == L"--report-md") { auto v = need("path"); if (!v) return "--report-md needs a path"; args.report_md = v; }
        else if (s == L"--report-json") { auto v = need("path"); if (!v) return "--report-json needs a path"; args.report_json = v; }
        else if (s == L"--stdout") {
            auto v = need("format");
            if (!v || (std::wstring(v) != L"md" && std::wstring(v) != L"json")) return "--stdout needs md or json";
            args.stdout_fmt = utf8(v);
        } else if (s == L"--full-paths") a.full_paths = true;
        else if (s == L"--temp-dir") { auto v = need("dir"); if (!v) return "--temp-dir needs a directory"; a.temp_dir = v; }
        else if (s == L"--test-codec-estimate-pct") {
            // Test-only (not in the usage text): scales whole-image decoder estimates down so the
            // allocator hard cap can be exercised end to end. Always disclosed in the report.
            auto v = need("percent");
            const std::wstring t = v ? v : L"";
            if (t.empty() || t.size() > 3 || t.find_first_not_of(L"0123456789") != std::wstring::npos)
                return "--test-codec-estimate-pct needs 1..100";
            const uint64_t pct = std::stoull(t);
            if (pct < 1 || pct > 100) return "--test-codec-estimate-pct needs 1..100";
            a.limits.test_codec_estimate_pct = pct;
        } else if (s == L"--limits") {
            bool any = false;
            while (i + 1 < argc && std::wstring(argv[i + 1]).find(L'=') != std::wstring::npos &&
                   std::wstring(argv[i + 1]).rfind(L"--", 0) != 0) {
                const std::string e = a.limits.set(utf8(argv[++i]));
                if (!e.empty()) return e;
                any = true;
            }
            if (!any) return "--limits needs name=value";
        } else if (s.rfind(L"--", 0) == 0) return "unknown option: " + utf8(s);
        else pos.push_back(s);
    }
    const size_t want = a.mode == "compare" ? 2 : 1;
    if (pos.size() != want) return a.mode + " needs " + std::to_string(want) + " input path(s)";
    a.original = pos[0];
    if (want == 2) a.output = pos[1];
    if (args.report_md.empty() && args.report_json.empty() && args.stdout_fmt.empty())
        return "no report output requested (use --report-md, --report-json or --stdout)";
    if (!a.temp_dir.empty() && !is_directory(a.temp_dir)) return "--temp-dir is not an existing directory: " + utf8(a.temp_dir);
    return {};
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc == 2 && std::wstring(argv[1]) == L"--version") {
        write_stdout(std::string("iat ") + version() + "\n");
        return 0;
    }
    Args a;
    if (const std::string e = parse_args(argc, argv, a); !e.empty()) {
        err("error: " + e);
        err(kUsage);
        return 5;
    }
    if (const std::string e = validate_report_targets(a.req, a.report_md, a.report_json); !e.empty()) {
        err("error: " + e);
        return 5;
    }
    SetConsoleCtrlHandler(on_ctrl, TRUE);

    const Report r = run_analysis(a.req, g_cancel);

    std::vector<std::string> written, failed;
    auto emit = [&](const std::wstring& path, const std::string& data) {
        std::string e;
        if (write_new_file(path, data, r.run_id, e)) written.push_back(utf8(full_path(path)));
        else failed.push_back(utf8(path) + " (" + e + ")");
    };
    const std::string json_text = report_json_text(r);
    const std::string md_text = report_markdown_text(r);
    if (!a.report_json.empty()) emit(a.report_json, json_text);
    if (!a.report_md.empty()) emit(a.report_md, md_text);
    if (a.stdout_fmt == "json") write_stdout(json_text);
    else if (a.stdout_fmt == "md") write_stdout(md_text);

    err("iat: strict=" + r.strict.result + " exit=" + std::to_string(r.exit_code) + " (" + r.exit_reason + ") run_id=" + r.run_id);
    for (const auto& p : written) err("iat: report written: " + p);
    // A report that could not be written makes the process exit 5, ahead of the report's own exit code.
    if (!failed.empty()) {
        for (const auto& p : failed) err("iat: report NOT written: " + p);
        return 5;
    }
    return r.exit_code;
}
