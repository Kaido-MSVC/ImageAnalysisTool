// One analysis run: opens inputs read-only, hashes them, creates the
// delete-on-close temporary files, runs iat-worker.exe inside a Job Object with
// resource limits, and assembles the final Report. No console output here.
#include "core/core.hpp"

#include "common/text.hpp"
#include "common/win_util.hpp"

#include <nlohmann/json.hpp>

#include <chrono>
#include <cstdio>
#include <optional>
#include <string>
#include <thread>
#include <vector>

namespace iat {
namespace {

using Clock = std::chrono::steady_clock;

constexpr size_t kMaxWorkerStdout = 32u << 20;
constexpr size_t kMaxWorkerStderr = 64u << 10;

double ms_since(Clock::time_point t0) {
    return std::chrono::duration<double, std::milli>(Clock::now() - t0).count();
}

// ---- temporary storage
// Two files per run (one per side), created here so the worker never opens a
// path. CREATE_NEW + share mode 0 + FILE_FLAG_DELETE_ON_CLOSE: nobody else can
// open them, and the file system deletes them when the last handle closes,
// which also happens when this process or the worker is killed.
constexpr const wchar_t* kTempPrefix = L"iat-tmp-";

struct TempFiles {
    std::wstring dir;
    bool user_specified = false;
    std::wstring path[2];
    HANDLE h[2] = {nullptr, nullptr};
    uint64_t free_bytes = 0;
    int stale_removed = 0;
    std::string error;

    TempFiles() = default;
    TempFiles(const TempFiles&) = delete;
    TempFiles& operator=(const TempFiles&) = delete;
    ~TempFiles() { close(); }
    void close() {
        for (HANDLE& x : h)
            if (x) CloseHandle(x), x = nullptr;
    }
    // After close(): paths that still exist (deletion did not happen).
    std::vector<std::wstring> leftovers() const {
        std::vector<std::wstring> v;
        for (const auto& p : path)
            if (!p.empty() && GetFileAttributesW(p.c_str()) != INVALID_FILE_ATTRIBUTES) v.push_back(p);
        return v;
    }
};

std::wstring trim_dir(std::wstring d) {
    while (d.size() > 3 && (d.back() == L'\\' || d.back() == L'/')) d.pop_back();
    return d;
}

std::wstring join_dir(const std::wstring& dir, const std::wstring& name) {
    return (!dir.empty() && (dir.back() == L'\\' || dir.back() == L'/')) ? dir + name : dir + L"\\" + name;
}

std::wstring system_temp_dir() {
    wchar_t buf[MAX_PATH + 2];
    DWORD n = GetTempPath2W(static_cast<DWORD>(std::size(buf)), buf);
    if (n == 0 || n >= std::size(buf)) return {};
    return trim_dir(std::wstring(buf, n));
}

// Only names this tool creates: iat-tmp-<run id>-a.bin / -b.bin, where the run id is a
// UUID in 8-4-4-4-12 hexadecimal form. Leftovers of earlier runs (crash, power loss) are
// still removed; a same-prefix name of any other shape is never touched.
bool is_own_temp_name(const std::wstring& n) {
    const size_t pre = wcslen(kTempPrefix);
    if (n.size() != pre + 36 + 6 || n.rfind(kTempPrefix, 0) != 0) return false;
    if (n.compare(pre + 36, 6, L"-a.bin") != 0 && n.compare(pre + 36, 6, L"-b.bin") != 0) return false;
    for (size_t i = 0; i < 36; ++i) {
        const wchar_t c = n[pre + i];
        if (i == 8 || i == 13 || i == 18 || i == 23) {
            if (c != L'-') return false;
        } else if (!((c >= L'0' && c <= L'9') || (c >= L'a' && c <= L'f') || (c >= L'A' && c <= L'F'))) {
            return false;
        }
    }
    return true;
}

// Leftovers exist only after an OS crash or power loss (delete-on-close never
// ran). A file still held by a running instance cannot be deleted (it is not
// shared), so DeleteFileW fails for it and it is left alone.
int remove_stale_temp(const std::wstring& dir) {
    WIN32_FIND_DATAW fd;
    HANDLE f = FindFirstFileW(join_dir(dir, std::wstring(kTempPrefix) + L"*.bin").c_str(), &fd);
    if (f == INVALID_HANDLE_VALUE) return 0;
    int n = 0;
    do {
        if ((fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) || !is_own_temp_name(fd.cFileName)) continue;
        if (DeleteFileW(join_dir(dir, fd.cFileName).c_str())) ++n;
    } while (FindNextFileW(f, &fd));
    FindClose(f);
    return n;
}

bool create_temp(const RunRequest& a, const std::string& run_id, TempFiles& t) {
    t.user_specified = !a.temp_dir.empty();
    t.dir = t.user_specified ? trim_dir(full_path(a.temp_dir)) : system_temp_dir();
    if (t.dir.empty() || !is_directory(t.dir)) { t.error = "temporary directory is not available"; return false; }
    t.stale_removed = remove_stale_temp(t.dir);
    ULARGE_INTEGER avail{};
    if (!GetDiskFreeSpaceExW(t.dir.c_str(), &avail, nullptr, nullptr)) {
        t.error = "cannot query free space of the temporary directory: " + win_error_text(GetLastError());
        return false;
    }
    t.free_bytes = avail.QuadPart;
    SECURITY_ATTRIBUTES sa{sizeof sa, nullptr, TRUE};  // inherited only through the explicit handle list
    for (int i = 0; i < 2; ++i) {
        t.path[i] = join_dir(t.dir, kTempPrefix + wide(run_id) + (i == 0 ? L"-a.bin" : L"-b.bin"));
        t.h[i] = CreateFileW(t.path[i].c_str(), GENERIC_READ | GENERIC_WRITE, 0, &sa, CREATE_NEW,
                             FILE_ATTRIBUTE_TEMPORARY | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED | FILE_FLAG_DELETE_ON_CLOSE, nullptr);
        if (t.h[i] == INVALID_HANDLE_VALUE) {
            t.h[i] = nullptr;
            t.error = "cannot create a temporary file: " + win_error_text(GetLastError());
            t.close();
            return false;
        }
    }
    return true;
}

std::string ext_format(const std::wstring& name) {
    const size_t dot = name.find_last_of(L'.');
    std::wstring e = dot == std::wstring::npos ? L"" : name.substr(dot + 1);
    for (auto& c : e) c = static_cast<wchar_t>(towlower(c));
    if (e == L"png") return "png";
    if (e == L"tif" || e == L"tiff") return "tiff";
    if (e == L"jxl") return "jxl";
    if (e == L"webp") return "webp";
    if (e == L"psd") return "psd";
    if (e == L"psb") return "psb";
    if (e == L"avif") return "avif";
    return "other";
}

void set_not_checked(FileReport& f, const std::string& reason) {
    f.decode_status = "not_checked";
    f.decode_status_reason = reason;
    if (f.container_status.empty()) f.container_status = "unverified";
}

struct WorkerRun {
    std::string outcome = "exited";   // exited | deadline | cancelled | launch_failed
    DWORD exit_code = 0;
    std::string stdout_data, stderr_data;
    bool stdout_overflow = false;
    bool memory_limit_signalled = false;
    uint64_t peak_job_commit = 0;
    uint64_t io_read = 0;
    double wall_ms = 0;
    std::string launch_error;
};

void drain(HANDLE h, std::string& sink, size_t cap, bool* overflow) {
    char buf[64 * 1024];
    DWORD n = 0;
    while (ReadFile(h, buf, sizeof buf, &n, nullptr) && n > 0) {
        if (sink.size() + n <= cap) sink.append(buf, n);
        else if (overflow) *overflow = true;
    }
}

WorkerRun run_worker(const RunRequest& a, const ReadOnlyFile& fo, const ReadOnlyFile* fp, const TempFiles& tmp,
                     const std::atomic<bool>& cancel) {
    WorkerRun w;
    wchar_t self[MAX_PATH * 4];
    GetModuleFileNameW(nullptr, self, static_cast<DWORD>(std::size(self)));
    const std::wstring worker = parent_dir_of(self) + L"\\iat-worker.exe";
    if (GetFileAttributesW(worker.c_str()) == INVALID_FILE_ATTRIBUTES) {
        w.outcome = "launch_failed";
        w.launch_error = "iat-worker.exe not found next to iat.exe";
        return w;
    }

    HANDLE job = CreateJobObjectW(nullptr, nullptr);
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION li{};
    li.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_PROCESS_MEMORY |
                                          JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION |
                                          JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    li.BasicLimitInformation.ActiveProcessLimit = 1;
    li.JobMemoryLimit = static_cast<SIZE_T>(a.limits.job_commit_mib) << 20;
    li.ProcessMemoryLimit = li.JobMemoryLimit;
    HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, nullptr, 0, 1);
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT acp{reinterpret_cast<PVOID>(1), port};
    if (!job || !port || !SetInformationJobObject(job, JobObjectExtendedLimitInformation, &li, sizeof li) ||
        !SetInformationJobObject(job, JobObjectAssociateCompletionPortInformation, &acp, sizeof acp)) {
        w.outcome = "launch_failed";
        w.launch_error = "Job Object could not be configured: " + win_error_text(GetLastError());
        if (job) CloseHandle(job);
        if (port) CloseHandle(port);
        return w;  // never run the decoder without limits
    }

    SECURITY_ATTRIBUTES sa{sizeof sa, nullptr, TRUE};
    HANDLE out_r, out_w, err_r, err_w;
    CreatePipe(&out_r, &out_w, &sa, 0);
    CreatePipe(&err_r, &err_w, &sa, 0);
    SetHandleInformation(out_r, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(err_r, HANDLE_FLAG_INHERIT, 0);
    HANDLE nul = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, nullptr);

    std::wstring cmd = L"\"" + worker + L"\" --mode " + wide(a.mode) + L" --original " +
                       std::to_wstring(reinterpret_cast<uintptr_t>(fo.handle()));
    if (fp) cmd += L" --output " + std::to_wstring(reinterpret_cast<uintptr_t>(fp->handle()));
    const Limits& L = a.limits;
    // job_commit_mib also goes to the worker: it derives the codec hard cap from it (D10).
    for (const auto& [k, v] : std::initializer_list<std::pair<const wchar_t*, uint64_t>>{
             {L"job_commit_mib", L.job_commit_mib}, {L"buffer_mib", L.buffer_mib}, {L"metadata_item_mib", L.metadata_item_mib},
             {L"metadata_file_mib", L.metadata_file_mib}, {L"temp_gib", L.temp_gib}})
        cmd += L" --limit " + std::wstring(k) + L"=" + std::to_wstring(v);
    if (L.test_codec_estimate_pct != 100)
        cmd += L" --test-codec-estimate-pct " + std::to_wstring(L.test_codec_estimate_pct);
    cmd += L" --temp-a " + std::to_wstring(reinterpret_cast<uintptr_t>(tmp.h[0])) + L" --temp-b " +
           std::to_wstring(reinterpret_cast<uintptr_t>(tmp.h[1])) + L" --temp-free " + std::to_wstring(tmp.free_bytes);

    std::vector<HANDLE> inherit{fo.handle(), out_w, err_w, nul, tmp.h[0], tmp.h[1]};
    if (fp && fp->handle() != fo.handle()) inherit.push_back(fp->handle());
    SIZE_T attr_size = 0;
    InitializeProcThreadAttributeList(nullptr, 1, 0, &attr_size);
    std::vector<uint8_t> attr_buf(attr_size);
    auto attrs = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(attr_buf.data());
    InitializeProcThreadAttributeList(attrs, 1, 0, &attr_size);
    UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherit.data(), inherit.size() * sizeof(HANDLE),
                              nullptr, nullptr);
    STARTUPINFOEXW si{};
    si.StartupInfo.cb = sizeof si;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = nul;
    si.StartupInfo.hStdOutput = out_w;
    si.StartupInfo.hStdError = err_w;
    si.lpAttributeList = attrs;
    PROCESS_INFORMATION pi{};
    const auto t0 = Clock::now();
    const BOOL created = CreateProcessW(nullptr, cmd.data(), nullptr, nullptr, TRUE,
                                        CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | EXTENDED_STARTUPINFO_PRESENT |
                                            CREATE_NO_WINDOW,
                                        nullptr, nullptr, &si.StartupInfo, &pi);
    const DWORD cerr = GetLastError();
    DeleteProcThreadAttributeList(attrs);
    CloseHandle(out_w);
    CloseHandle(err_w);
    CloseHandle(nul);
    if (!created || !AssignProcessToJobObject(job, pi.hProcess)) {
        const DWORD e = created ? GetLastError() : cerr;
        if (created) TerminateProcess(pi.hProcess, 1), CloseHandle(pi.hThread), CloseHandle(pi.hProcess);
        CloseHandle(out_r);
        CloseHandle(err_r);
        CloseHandle(job);
        CloseHandle(port);
        w.outcome = "launch_failed";
        w.launch_error = std::string(created ? "AssignProcessToJobObject" : "CreateProcess") + ": " + win_error_text(e);
        return w;
    }
    std::thread t_out(drain, out_r, std::ref(w.stdout_data), kMaxWorkerStdout, &w.stdout_overflow);
    std::thread t_err(drain, err_r, std::ref(w.stderr_data), kMaxWorkerStderr, nullptr);
    ResumeThread(pi.hThread);  // limits are in place before any worker code runs

    const double deadline_ms = static_cast<double>(a.limits.deadline_s) * 1000.0;
    for (;;) {
        const DWORD wr = WaitForSingleObject(pi.hProcess, 50);
        DWORD msg;
        ULONG_PTR key;
        LPOVERLAPPED ov;
        while (GetQueuedCompletionStatus(port, &msg, &key, &ov, 0))
            if (msg == JOB_OBJECT_MSG_JOB_MEMORY_LIMIT || msg == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT) w.memory_limit_signalled = true;
        if (wr == WAIT_OBJECT_0) break;
        if (cancel && w.outcome == "exited") { w.outcome = "cancelled"; TerminateJobObject(job, 0xC000013A); }
        else if (ms_since(t0) > deadline_ms && w.outcome == "exited") { w.outcome = "deadline"; TerminateJobObject(job, 0x5A5A); }
    }
    w.wall_ms = ms_since(t0);
    t_out.join();
    t_err.join();
    DWORD msg;
    ULONG_PTR key;
    LPOVERLAPPED ov;
    while (GetQueuedCompletionStatus(port, &msg, &key, &ov, 20))
        if (msg == JOB_OBJECT_MSG_JOB_MEMORY_LIMIT || msg == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT) w.memory_limit_signalled = true;
    GetExitCodeProcess(pi.hProcess, &w.exit_code);
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION q{};
    if (QueryInformationJobObject(job, JobObjectExtendedLimitInformation, &q, sizeof q, nullptr))
        w.peak_job_commit = q.PeakJobMemoryUsed;
    JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION io{};
    if (QueryInformationJobObject(job, JobObjectBasicAndIoAccountingInformation, &io, sizeof io, nullptr))
        w.io_read = io.IoInfo.ReadTransferCount;
    CloseHandle(out_r);
    CloseHandle(err_r);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(port);
    CloseHandle(job);
    return w;
}

void fill_identity(FileReport& f, const std::wstring& path, bool full_paths) {
    f.name = utf8(file_name_of(path));
    f.path = full_paths ? utf8(full_path(path)) : "";
}

}  // namespace

Report run_analysis(const RunRequest& a, const std::atomic<bool>& cancel) {
    const auto t_all = Clock::now();
    const bool compare = a.mode == "compare";

    Report r;
    r.run_id = new_run_id();
    r.mode = a.mode;
    r.tool = {{"name", "iat"}, {"version", version()}, {"compiler", "MSVC " + std::to_string(_MSC_FULL_VER)}};
    r.settings = {{"limits",
                   {{"job_commit_mib", a.limits.job_commit_mib}, {"buffer_mib", a.limits.buffer_mib},
                    {"metadata_item_mib", a.limits.metadata_item_mib}, {"metadata_file_mib", a.limits.metadata_file_mib},
                    {"temp_gib", a.limits.temp_gib}, {"deadline_s", a.limits.deadline_s}}},
                  {"codec_threads", 1},
                  {"full_paths", a.full_paths}};
    if (a.limits.test_codec_estimate_pct != 100) {
        r.settings["test"] = {{"codec_estimate_pct", a.limits.test_codec_estimate_pct}};
        r.issues.push_back(Issue{code::test_option_used, "warning", "run",
                                 "test-only --test-codec-estimate-pct " + std::to_string(a.limits.test_codec_estimate_pct) +
                                     " scaled decoder memory estimates down; results show limit handling, not normal operation"});
    }

    // ---- open inputs read-only (share read only: nobody can write or replace them meanwhile)
    const auto t_open = Clock::now();
    DWORD e1 = 0, e2 = 0;
    auto fo = ReadOnlyFile::open(a.original, true, &e1);
    std::optional<ReadOnlyFile> fp;
    if (compare) fp = ReadOnlyFile::open(a.output, true, &e2);
    bool open_failed = false;
    auto describe = [&](FileReport& f, const std::wstring& path, std::optional<ReadOnlyFile>& h, DWORD e, const char* scope) {
        fill_identity(f, path, a.full_paths);
        if (!h) {
            open_failed = true;
            set_not_checked(f, "input_open_failed");
            r.issues.push_back(Issue{code::input_open_failed, "error", scope, "cannot open input: " + win_error_text(e)});
            return;
        }
        f.size_bytes = Count{h->size().value_or(0)};
        f.sha256 = sha256_file(*h, &cancel).value_or("");   // empty when cancelled
    };
    describe(r.original, a.original, fo, e1, "original");
    if (compare) {
        r.output = FileReport{};
        describe(*r.output, a.output, fp, e2, "output");
        if (fo && fp) {
            auto ia = file_identity(fo->handle()), ib = file_identity(fp->handle());
            if (ia && ib && *ia == *ib)
                r.issues.push_back(Issue{code::same_file, "info", "pair", "original and output are the same file"});
        }
    }
    r.metrics.time_ms["open_and_hash"] = ms_since(t_open);

    // Cancelled while hashing: stop here, no temp files and no worker.
    const bool cancelled_early = !open_failed && cancel.load();
    if (cancelled_early) {
        set_not_checked(r.original, "cancelled");
        if (r.output) set_not_checked(*r.output, "cancelled");
        r.issues.push_back(Issue{code::cancelled, "error", "run", "cancelled by user (Ctrl+C / Ctrl+Break) while hashing the inputs"});
    }

    TempFiles tmp;
    const bool temp_ok = open_failed || cancelled_early || create_temp(a, r.run_id, tmp);
    r.settings["temp"] = {{"location", tmp.user_specified ? "user_specified" : "system_temp"},
                          {"free_bytes_at_start", std::to_string(tmp.free_bytes)},
                          {"delete_on_close", true}};
    if (a.full_paths && !tmp.dir.empty()) r.settings["temp"]["dir"] = utf8(tmp.dir);
    if (tmp.stale_removed > 0)
        r.issues.push_back(Issue{"temp_stale_removed", "info", "run",
                                 std::to_string(tmp.stale_removed) + " leftover temporary file(s) of an earlier run removed"});
    if (!temp_ok) {
        set_not_checked(r.original, "environment_error");
        if (r.output) set_not_checked(*r.output, "environment_error");
        r.issues.push_back(Issue{code::environment_error, "error", "run", tmp.error});
    }

    if (!open_failed && !cancelled_early && temp_ok) {
        WorkerRun w = run_worker(a, *fo, compare ? &*fp : nullptr, tmp, cancel);
        // The worker is gone (exited or killed with its Job): closing our
        // handles is the last close, so the files must vanish now.
        tmp.close();
        for (const auto& left : tmp.leftovers())
            r.issues.push_back(Issue{code::temp_cleanup_failed, "warning", "run",
                                     "temporary file still exists after close: " + utf8(file_name_of(left))});
        r.metrics.time_ms["worker_wall"] = w.wall_ms;
        r.metrics.memory_bytes["job_peak_commit"] = Count{w.peak_job_commit};
        r.metrics.memory_bytes["job_commit_limit"] = Count{a.limits.job_commit_mib << 20};
        r.metrics.io_read_bytes = Count{w.io_read};
        std::vector<FileReport*> fs{&r.original};
        if (r.output) fs.push_back(&*r.output);
        auto all_not_checked = [&](const std::string& reason) {
            for (FileReport* f : fs) set_not_checked(*f, reason);
        };

        nlohmann::json j;
        bool parsed = false;
        if (w.outcome == "exited" && !w.stdout_overflow) {
            try { j = nlohmann::json::parse(w.stdout_data); parsed = j.is_object(); } catch (...) { parsed = false; }
        }
        if (w.outcome == "launch_failed") {
            all_not_checked("environment_error");
            r.issues.push_back(Issue{code::environment_error, "error", "run", w.launch_error});
        } else if (w.outcome == "cancelled") {
            all_not_checked("cancelled");
            r.issues.push_back(Issue{code::cancelled, "error", "run", "cancelled by user (Ctrl+C / Ctrl+Break)"});
        } else if (w.outcome == "deadline") {
            all_not_checked("deadline");
            r.issues.push_back(Issue{code::deadline_exceeded, "error", "run",
                                     "worker exceeded deadline of " + std::to_string(a.limits.deadline_s) +
                                         " s (raise with --limits deadline_s=)"});
        } else if (parsed && j.contains("worker_error")) {
            const std::string we = j.value("worker_error", std::string());
            if (we == "resource_limit") {
                all_not_checked("resource_limit");
                r.issues.push_back(Issue{code::resource_limit, "error", "run",
                                         "worker allocation failed under Job commit limit " +
                                             std::to_string(a.limits.job_commit_mib) + " MiB" +
                                             (w.memory_limit_signalled ? " (Job memory-limit notification received)" : "")});
            } else {
                all_not_checked("worker_error");
                r.issues.push_back(Issue{code::worker_crashed, "error", "run", "worker error: " + j.value("message", we)});
            }
        } else if (!parsed || w.exit_code != 0) {
            all_not_checked("worker_crashed");
            char b[96];
            snprintf(b, sizeof b, "worker exit 0x%08lX%s%s", w.exit_code, w.stdout_overflow ? ", result too large" : "",
                     w.memory_limit_signalled ? ", Job memory-limit notification received" : "");
            // A crash is never guessed to be a resource limit, even with a notification.
            r.issues.push_back(Issue{w.stdout_overflow ? code::worker_protocol_error : code::worker_crashed, "error", "run", b});
        } else {
            Report wr;
            try {
                wr = j.get<Report>();
            } catch (const std::exception& ex) {
                all_not_checked("worker_protocol_error");
                r.issues.push_back(Issue{code::worker_protocol_error, "error", "run", ex.what()});
                parsed = false;
            }
            if (parsed) {
                // Keep parent-owned identity (names, sizes, hashes) and run-level data.
                auto merge = [&](FileReport& dst, FileReport src) {
                    src.name = dst.name;
                    src.path = dst.path;
                    src.size_bytes = dst.size_bytes;
                    src.sha256 = dst.sha256;
                    dst = std::move(src);
                };
                merge(r.original, wr.original);
                if (compare && wr.output) merge(*r.output, *wr.output);
                r.codecs = wr.codecs;
                r.geometry = wr.geometry;
                r.samples = wr.samples;
                r.color = wr.color;
                r.icc = wr.icc;
                r.metadata = wr.metadata;
                r.size = wr.size;
                r.strict = wr.strict;
                for (auto& i : wr.issues) r.issues.push_back(i);
                for (auto& [k, v] : wr.metrics.time_ms) r.metrics.time_ms[k] = v;
                for (auto& [k, v] : wr.metrics.memory_bytes) r.metrics.memory_bytes[k] = v;
                r.metrics.timing_note = wr.metrics.timing_note;
            }
        }
        if (!w.stderr_data.empty()) r.issues.push_back(Issue{"worker_stderr", "info", "run", utf8_sanitize(w.stderr_data.substr(0, 2000), true)});
    }

    // Extension check (format detected by content, never by extension).
    for (auto [f, path] : {std::pair<FileReport*, std::wstring>{&r.original, a.original},
                           std::pair<FileReport*, std::wstring>{r.output ? &*r.output : nullptr, a.output}}) {
        if (!f || f->format.empty() || f->format == "unknown") continue;
        const std::string ef = ext_format(file_name_of(path));
        const bool ok = ef == f->format || (ef == "tiff" && f->format == "bigtiff") || (ef == "jxl" && f->format.rfind("jxl", 0) == 0);
        if (!ok) {
            f->extension_mismatch = true;
            r.issues.push_back(Issue{code::extension_mismatch, "warning", f == &r.original ? "original" : "output",
                                     "content is " + f->format + " but the extension does not match"});
        }
    }
    if (r.size.original_bytes.v == 0) r.size.original_bytes = r.original.size_bytes;
    if (compare && !r.size.output_bytes && r.output) r.size.output_bytes = r.output->size_bytes;

    ExitDecision d = decide_exit(r);
    r.exit_code = d.code;
    r.exit_reason = d.reason;
    r.metrics.time_ms["total"] = ms_since(t_all);
    return r;
}

}  // namespace iat
