#include "win_util.hpp"

#include <bcrypt.h>

#include <stdexcept>

namespace iat {

std::string utf8(const std::wstring& w) {
    if (w.empty()) return {};
    int n = WideCharToMultiByte(CP_UTF8, 0, w.data(), static_cast<int>(w.size()), nullptr, 0, nullptr, nullptr);
    std::string s(static_cast<size_t>(n), '\0');
    WideCharToMultiByte(CP_UTF8, 0, w.data(), static_cast<int>(w.size()), s.data(), n, nullptr, nullptr);
    return s;
}

std::wstring wide(const std::string& s) {
    if (s.empty()) return {};
    int n = MultiByteToWideChar(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), nullptr, 0);
    std::wstring w(static_cast<size_t>(n), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), w.data(), n);
    return w;
}

ReadOnlyFile& ReadOnlyFile::operator=(ReadOnlyFile&& o) noexcept {
    if (this != &o) {
        if (owned_ && valid()) CloseHandle(h_);
        h_ = o.h_;
        owned_ = o.owned_;
        bytes_read_ = o.bytes_read_;
        o.h_ = nullptr;
        o.owned_ = false;
    }
    return *this;
}

ReadOnlyFile::~ReadOnlyFile() {
    if (owned_ && valid()) CloseHandle(h_);
}

std::optional<ReadOnlyFile> ReadOnlyFile::open(const std::wstring& path, bool inheritable, DWORD* error) {
    SECURITY_ATTRIBUTES sa{sizeof sa, nullptr, inheritable ? TRUE : FALSE};
    HANDLE h = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, &sa, OPEN_EXISTING,
                           FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
        if (error) *error = GetLastError();
        return std::nullopt;
    }
    BY_HANDLE_FILE_INFORMATION info{};
    if (GetFileInformationByHandle(h, &info) && (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) {
        CloseHandle(h);
        if (error) *error = ERROR_DIRECTORY;
        return std::nullopt;
    }
    return ReadOnlyFile(h, true);
}

std::optional<uint64_t> ReadOnlyFile::size() const {
    LARGE_INTEGER li{};
    if (!GetFileSizeEx(h_, &li)) return std::nullopt;
    return static_cast<uint64_t>(li.QuadPart);
}

std::optional<size_t> ReadOnlyFile::read_at(uint64_t offset, void* buf, size_t n) const {
    size_t done = 0;
    while (done < n) {
        const DWORD want = static_cast<DWORD>(std::min<size_t>(n - done, 1u << 30));
        OVERLAPPED ov{};
        const uint64_t off = offset + done;
        ov.Offset = static_cast<DWORD>(off & 0xFFFFFFFFu);
        ov.OffsetHigh = static_cast<DWORD>(off >> 32);
        DWORD got = 0;
        if (!ReadFile(h_, static_cast<char*>(buf) + done, want, &got, &ov)) {
            if (GetLastError() == ERROR_HANDLE_EOF) break;
            return std::nullopt;
        }
        if (got == 0) break;
        done += got;
    }
    bytes_read_ += done;
    return done;
}

std::string hex(std::span<const uint8_t> bytes) {
    static const char* d = "0123456789abcdef";
    std::string s;
    s.reserve(bytes.size() * 2);
    for (uint8_t b : bytes) {
        s += d[b >> 4];
        s += d[b & 15];
    }
    return s;
}

Sha256::Sha256() {
    BCRYPT_ALG_HANDLE a = nullptr;
    BCRYPT_HASH_HANDLE h = nullptr;
    if (BCryptOpenAlgorithmProvider(&a, BCRYPT_SHA256_ALGORITHM, nullptr, 0) != 0 ||
        BCryptCreateHash(a, &h, nullptr, 0, nullptr, 0, 0) != 0)
        throw std::runtime_error("SHA-256 provider unavailable");
    alg_ = a;
    hash_ = h;
}

Sha256::~Sha256() {
    if (hash_) BCryptDestroyHash(static_cast<BCRYPT_HASH_HANDLE>(hash_));
    if (alg_) BCryptCloseAlgorithmProvider(static_cast<BCRYPT_ALG_HANDLE>(alg_), 0);
}

void Sha256::update(const void* data, size_t n) {
    const auto* p = static_cast<const uint8_t*>(data);
    while (n > 0) {
        const ULONG chunk = static_cast<ULONG>(std::min<size_t>(n, 1u << 30));
        BCryptHashData(static_cast<BCRYPT_HASH_HANDLE>(hash_), const_cast<PUCHAR>(p), chunk, 0);
        p += chunk;
        n -= chunk;
    }
}

std::string Sha256::finish_hex() {
    uint8_t d[32];
    BCryptFinishHash(static_cast<BCRYPT_HASH_HANDLE>(hash_), d, 32, 0);
    return hex(d);
}

std::string sha256_hex(const void* data, size_t n) {
    Sha256 s;
    s.update(data, n);
    return s.finish_hex();
}

std::optional<std::string> sha256_file(const ReadOnlyFile& f, const std::atomic<bool>* cancel) {
    Sha256 s;
    std::vector<uint8_t> buf(4u << 20);
    uint64_t off = 0;
    for (;;) {
        if (cancel && cancel->load()) return std::nullopt;
        auto got = f.read_at(off, buf.data(), buf.size());
        if (!got) return std::nullopt;
        if (*got == 0) break;
        s.update(buf.data(), *got);
        off += *got;
    }
    return s.finish_hex();
}

std::wstring full_path(const std::wstring& p) {
    DWORD n = GetFullPathNameW(p.c_str(), 0, nullptr, nullptr);
    if (n == 0) return p;
    std::wstring out(n, L'\0');
    n = GetFullPathNameW(p.c_str(), n, out.data(), nullptr);
    out.resize(n);
    return out;
}

bool same_path(const std::wstring& a, const std::wstring& b) {
    const std::wstring fa = full_path(a), fb = full_path(b);
    return CompareStringOrdinal(fa.c_str(), static_cast<int>(fa.size()), fb.c_str(), static_cast<int>(fb.size()),
                                TRUE) == CSTR_EQUAL;
}

std::wstring file_name_of(const std::wstring& p) {
    const size_t k = p.find_last_of(L"\\/");
    return k == std::wstring::npos ? p : p.substr(k + 1);
}

std::wstring parent_dir_of(const std::wstring& p) {
    const std::wstring f = full_path(p);
    const size_t k = f.find_last_of(L"\\/");
    return k == std::wstring::npos ? L"." : f.substr(0, k);
}

bool path_exists(const std::wstring& p) {
    return GetFileAttributesW(p.c_str()) != INVALID_FILE_ATTRIBUTES || GetLastError() == ERROR_SHARING_VIOLATION;
}

bool is_directory(const std::wstring& p) {
    const DWORD a = GetFileAttributesW(p.c_str());
    return a != INVALID_FILE_ATTRIBUTES && (a & FILE_ATTRIBUTE_DIRECTORY);
}

bool FileIdentity::operator==(const FileIdentity& o) const {
    return volume == o.volume && memcmp(id, o.id, 16) == 0;
}

std::optional<FileIdentity> file_identity(HANDLE h) {
    FILE_ID_INFO info{};
    if (!GetFileInformationByHandleEx(h, FileIdInfo, &info, sizeof info)) return std::nullopt;
    FileIdentity fi;
    fi.volume = info.VolumeSerialNumber;
    memcpy(fi.id, info.FileId.Identifier, 16);
    return fi;
}

std::string new_run_id() {
    uint8_t b[16];
    BCryptGenRandom(nullptr, b, sizeof b, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    b[6] = static_cast<uint8_t>((b[6] & 0x0F) | 0x40);
    b[8] = static_cast<uint8_t>((b[8] & 0x3F) | 0x80);
    const std::string h = hex(b);
    return h.substr(0, 8) + "-" + h.substr(8, 4) + "-" + h.substr(12, 4) + "-" + h.substr(16, 4) + "-" + h.substr(20);
}

std::string win_error_text(DWORD e) {
    wchar_t* msg = nullptr;
    FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                   nullptr, e, MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US), reinterpret_cast<LPWSTR>(&msg), 0,
                   nullptr);
    std::wstring w = msg ? msg : L"";
    if (msg) LocalFree(msg);
    while (!w.empty() && (w.back() == L'\n' || w.back() == L'\r' || w.back() == L' ')) w.pop_back();
    return std::to_string(e) + ": " + utf8(w);
}

}  // namespace iat
