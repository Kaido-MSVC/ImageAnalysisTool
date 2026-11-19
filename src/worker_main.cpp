// iat-worker.exe — decodes untrusted input inside the parent's Job Object.
// Inputs arrive only as inherited read-only handles; the worker never opens
// paths. The result is one JSON document on stdout.
//
// iat-worker --mode compare|inspect --original <handle> [--output <handle>] [--limit name=value]...
//            [--temp-a <handle>] [--temp-b <handle>] [--temp-free <bytes>] [--test-codec-estimate-pct <1..100>]
// Temporary files are parent-created, delete-on-close handles; the worker never opens a path.
#include "common/win_util.hpp"
#include "engine/analyze.hpp"

#include <nlohmann/json.hpp>

#include <cstdio>
#include <new>
#include <string>

namespace {

void write_stdout(const std::string& s) {
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    size_t done = 0;
    while (done < s.size()) {
        DWORD n = 0;
        const DWORD chunk = static_cast<DWORD>(std::min<size_t>(s.size() - done, 1u << 20));
        if (!WriteFile(out, s.data() + done, chunk, &n, nullptr) || n == 0) return;
        done += n;
    }
}

HANDLE parse_handle(const wchar_t* s) {
    return reinterpret_cast<HANDLE>(static_cast<uintptr_t>(_wcstoui64(s, nullptr, 10)));
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    // The parent owns cancellation; console signals go to the parent only.
    SetConsoleCtrlHandler(nullptr, TRUE);
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);

    iat::AnalysisInput in;
    HANDLE ho = nullptr, hp = nullptr;
    for (int i = 1; i < argc; ++i) {
        const std::wstring a = argv[i];
        if (a == L"--mode" && i + 1 < argc) in.mode = iat::utf8(argv[++i]);
        else if (a == L"--original" && i + 1 < argc) ho = parse_handle(argv[++i]);
        else if (a == L"--output" && i + 1 < argc) hp = parse_handle(argv[++i]);
        else if (a == L"--temp-a" && i + 1 < argc) in.temp[0] = parse_handle(argv[++i]);
        else if (a == L"--temp-b" && i + 1 < argc) in.temp[1] = parse_handle(argv[++i]);
        else if (a == L"--temp-free" && i + 1 < argc) in.temp_free_bytes = _wcstoui64(argv[++i], nullptr, 10);
        else if (a == L"--test-codec-estimate-pct" && i + 1 < argc) {
            const uint64_t pct = _wcstoui64(argv[++i], nullptr, 10);
            if (pct < 1 || pct > 100) {
                write_stdout(nlohmann::json{{"worker_error", "bad_argument"}, {"message", "test-codec-estimate-pct out of range"}}.dump());
                return 2;
            }
            in.limits.test_codec_estimate_pct = pct;
        }
        else if (a == L"--limit" && i + 1 < argc) {
            const std::string err = in.limits.set(iat::utf8(argv[++i]));
            if (!err.empty()) { write_stdout(nlohmann::json{{"worker_error", "bad_argument"}, {"message", err}}.dump()); return 2; }
        }
    }
    if ((in.mode != "compare" && in.mode != "inspect") || !ho || (in.mode == "compare" && !hp)) {
        write_stdout(nlohmann::json{{"worker_error", "bad_argument"}, {"message", "missing mode or handles"}}.dump());
        return 2;
    }
    iat::ReadOnlyFile original(ho, false), output(hp, false);
    in.original = &original;
    in.output = in.mode == "compare" ? &output : nullptr;
    try {
        iat::Report r = iat::analyze(in);
        // File strings are converted where they are read; replace is the backstop (never throw here).
        write_stdout(nlohmann::json(r).dump(-1, ' ', false, nlohmann::json::error_handler_t::replace));
        return 0;
    } catch (const std::bad_alloc&) {
        // Tool-side allocation failed (Job commit limit): report it, never crash.
        write_stdout(nlohmann::json{{"worker_error", "resource_limit"}, {"message", "allocation failed in worker"}}.dump());
        return 3;
    } catch (const std::exception& e) {
        write_stdout(nlohmann::json{{"worker_error", "exception"}, {"message", e.what()}}.dump(-1, ' ', false, nlohmann::json::error_handler_t::replace));
        return 4;
    }
}
