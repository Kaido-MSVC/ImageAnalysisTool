// Worker-side temporary storage. The parent creates the files (CREATE_NEW,
// FILE_FLAG_DELETE_ON_CLOSE, not shared) and passes only the handles, so the
// worker never touches a path and the files disappear when the last handle
// closes, including after a crash or termination of either process.
//
// Rules enforced here:
// - every byte is reserved (quota + free disk space) and preallocated before
//   decoding starts, so exhaustion is reported up front, not half-way;
// - append mode: writes are sequential, reads may only cover bytes already
//   written, so a hole can never be read back as zero-valued samples;
// - positional mode (stage 3, whole-image decoders whose output arrives out of
//   raster order): writes may land anywhere inside the reservation, and nothing
//   is readable until the caller seals the file after RowCoverage has proven
//   that every row was written exactly once. The two modes cannot be mixed.
#pragma once

#include <windows.h>

#include <cstdint>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace iat {

class TempQuota {
public:
    TempQuota(uint64_t limit_bytes, uint64_t free_disk_bytes) : limit_(limit_bytes), free_(free_disk_bytes) {}
    // Returns "" when granted, else "quota" or "disk".
    std::string take(uint64_t n);
    uint64_t limit() const { return limit_; }
    uint64_t free_disk() const { return free_; }
    uint64_t reserved() const { return reserved_; }

    static constexpr uint64_t kDiskMargin = 64ull << 20;  // always leave this much free

private:
    uint64_t limit_, free_, reserved_ = 0;
};

struct TempError {
    std::string kind;     // quota | disk | io | unavailable | internal
    std::string message;
};

class TempFile {
public:
    TempFile() = default;
    TempFile(HANDLE h, TempQuota* q) : h_(h), q_(q) {}
    TempFile(const TempFile&) = delete;
    TempFile& operator=(const TempFile&) = delete;

    bool available() const { return h_ != nullptr && h_ != INVALID_HANDLE_VALUE && q_ != nullptr; }
    // Reserve against quota and free space, then preallocate on disk.
    bool reserve(uint64_t bytes, TempError& e);
    // Append mode: sequential append through a fixed write buffer.
    bool append(const void* data, size_t n, TempError& e);
    bool flush(TempError& e);
    // Positional mode: write anywhere inside the reservation (unbuffered).
    bool write_at(uint64_t off, const void* data, size_t n, TempError& e);
    // Positional mode: make the whole reservation readable. The caller must have
    // proven complete, non-overlapping coverage first (RowCoverage::complete()).
    bool seal(TempError& e);
    // Positional read. Append mode: the range must lie within bytes already
    // written. Positional mode: only after seal(), within the reservation.
    bool read_at(uint64_t off, void* data, size_t n, TempError& e);

    uint64_t reserved() const { return reserved_; }
    uint64_t written() const { return written_; }
    uint64_t bytes_written() const { return written_; }
    uint64_t bytes_read() const { return read_; }
    double io_ms() const { return io_ms_; }
    static constexpr size_t kWriteBuffer = 1u << 20;

private:
    enum class Mode { none, append, positional };
    bool write_raw(uint64_t off, const void* data, size_t n, TempError& e);

    HANDLE h_ = nullptr;
    TempQuota* q_ = nullptr;
    Mode mode_ = Mode::none;
    bool sealed_ = false;
    uint64_t reserved_ = 0, written_ = 0, on_disk_ = 0, read_ = 0;
    double io_ms_ = 0;
    std::vector<uint8_t> buf_;
};

// Coverage of a width x height raster by horizontal spans, for decoders that
// deliver rows in pieces and out of order (libjxl image-out callback).
// Each row keeps a sorted list of disjoint half-open intervals; inserting a span
// that overlaps anything already present fails (a duplicate is never merged
// away), and complete() requires every row to be exactly [0, width). A missing
// pixel and a duplicated pixel therefore cannot cancel each other out.
// Memory: one state byte per row plus interval lists for rows still partial.
class RowCoverage {
public:
    RowCoverage() = default;
    RowCoverage(uint32_t width, uint32_t height) : width_(width), state_(height, 0) {}

    // Records [x, x + n) on row y. Returns false on an empty span, a span outside
    // the raster, or any overlap; the first problem is kept in problem().
    bool insert(uint32_t y, uint32_t x, uint32_t n);
    bool complete() const { return problem_.empty() && full_rows_ == state_.size(); }
    // First problem, or a description of the first incomplete row.
    std::string problem() const;
    uint64_t full_rows() const { return full_rows_; }
    // Bytes needed for the per-row state of a raster with this many rows.
    static uint64_t state_bytes(uint64_t height) { return height; }

private:
    enum : uint8_t { kEmpty = 0, kPartial = 1, kFull = 2 };
    bool fail(const std::string& why);

    uint32_t width_ = 0;
    std::vector<uint8_t> state_;
    std::unordered_map<uint32_t, std::vector<std::pair<uint32_t, uint32_t>>> partial_;
    uint64_t full_rows_ = 0;
    std::string problem_;
};

}  // namespace iat
