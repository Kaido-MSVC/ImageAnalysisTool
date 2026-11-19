// Windows helpers shared by iat.exe and iat-worker.exe.
#pragma once

#include <windows.h>

#include <cstdint>
#include <atomic>
#include <optional>
#include <span>
#include <string>
#include <vector>

namespace iat {

std::string utf8(const std::wstring& w);
std::wstring wide(const std::string& s);

// Read-only input file. Opened GENERIC_READ + FILE_SHARE_READ, so no other
// process can open it for writing or deletion while this handle is open.
// Reads are positional (OVERLAPPED offset) and never move a shared pointer.
class ReadOnlyFile {
public:
    ReadOnlyFile() = default;
    explicit ReadOnlyFile(HANDLE h, bool owned) : h_(h), owned_(owned) {}
    ReadOnlyFile(const ReadOnlyFile&) = delete;
    ReadOnlyFile& operator=(const ReadOnlyFile&) = delete;
    ReadOnlyFile(ReadOnlyFile&& o) noexcept { *this = std::move(o); }
    ReadOnlyFile& operator=(ReadOnlyFile&& o) noexcept;
    ~ReadOnlyFile();

    // inheritable: handle may be passed to a child via an explicit handle list.
    static std::optional<ReadOnlyFile> open(const std::wstring& path, bool inheritable, DWORD* error);

    bool valid() const { return h_ != nullptr && h_ != INVALID_HANDLE_VALUE; }
    HANDLE handle() const { return h_; }
    std::optional<uint64_t> size() const;
    // Reads up to n bytes at offset; returns bytes read (short only at EOF) or nullopt on I/O error.
    std::optional<size_t> read_at(uint64_t offset, void* buf, size_t n) const;
    uint64_t bytes_read() const { return bytes_read_; }

private:
    HANDLE h_ = nullptr;
    bool owned_ = false;
    mutable uint64_t bytes_read_ = 0;
};

std::string hex(std::span<const uint8_t> bytes);

// SHA-256 helpers (Windows CNG).
class Sha256 {
public:
    Sha256();
    ~Sha256();
    Sha256(const Sha256&) = delete;
    Sha256& operator=(const Sha256&) = delete;
    void update(const void* data, size_t n);
    std::string finish_hex();

private:
    void* alg_ = nullptr;
    void* hash_ = nullptr;
};
std::string sha256_hex(const void* data, size_t n);
// nullopt on a read error, or when *cancel becomes true (checked before every 4 MiB block).
std::optional<std::string> sha256_file(const ReadOnlyFile& f, const std::atomic<bool>* cancel = nullptr);

std::wstring full_path(const std::wstring& p);
bool same_path(const std::wstring& a, const std::wstring& b);  // normalized, case-insensitive
std::wstring file_name_of(const std::wstring& p);
std::wstring parent_dir_of(const std::wstring& p);
bool path_exists(const std::wstring& p);
bool is_directory(const std::wstring& p);

struct FileIdentity {
    uint64_t volume = 0;
    uint8_t id[16] = {};
    bool operator==(const FileIdentity& o) const;
};
std::optional<FileIdentity> file_identity(HANDLE h);

std::string new_run_id();             // random UUID v4 text
std::string win_error_text(DWORD e);  // "code: message"

}  // namespace iat
