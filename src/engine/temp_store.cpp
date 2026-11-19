#include "temp_store.hpp"

#include "../common/checked.hpp"
#include "../common/win_util.hpp"

#include <algorithm>
#include <chrono>
#include <cstring>

namespace iat {

namespace {
using Clock = std::chrono::steady_clock;
double ms(Clock::time_point t0) { return std::chrono::duration<double, std::milli>(Clock::now() - t0).count(); }

bool is_space_error(DWORD e) { return e == ERROR_DISK_FULL || e == ERROR_HANDLE_DISK_FULL || e == ERROR_DISK_QUOTA_EXCEEDED; }
}  // namespace

std::string TempQuota::take(uint64_t n) {
    const auto total = checked_add(reserved_, n);
    if (!total || *total > limit_) return "quota";
    const auto with_margin = checked_add(*total, kDiskMargin);
    if (!with_margin || *with_margin > free_) return "disk";
    reserved_ = *total;
    return {};
}

bool TempFile::reserve(uint64_t bytes, TempError& e) {
    if (!available()) { e = {"unavailable", "no temporary file was provided to the worker"}; return false; }
    if (reserved_ != 0) { e = {"internal", "temporary file reserved twice"}; return false; }
    if (const std::string k = q_->take(bytes); !k.empty()) {
        e.kind = k;
        e.message = k == "quota"
            ? "temporary storage needs " + std::to_string(bytes) + " bytes, temp_gib allows " + std::to_string(q_->limit()) +
                  " (already reserved " + std::to_string(q_->reserved()) + "; raise with --limits temp_gib=)"
            : "temporary storage needs " + std::to_string(bytes) + " bytes but the temp volume has " +
                  std::to_string(q_->free_disk()) + " bytes free (margin " + std::to_string(TempQuota::kDiskMargin) +
                  "; choose another volume with --temp-dir)";
        return false;
    }
    const auto t0 = Clock::now();
    FILE_ALLOCATION_INFO fa{};
    fa.AllocationSize.QuadPart = static_cast<LONGLONG>(bytes);
    if (bytes > 0 && !SetFileInformationByHandle(h_, FileAllocationInfo, &fa, sizeof fa)) {
        const DWORD err = GetLastError();
        e = {is_space_error(err) ? "disk" : "io", "preallocating temporary storage failed: " + win_error_text(err)};
        io_ms_ += ms(t0);
        return false;
    }
    io_ms_ += ms(t0);
    reserved_ = bytes;
    buf_.reserve(kWriteBuffer);
    return true;
}

bool TempFile::write_raw(uint64_t off, const void* data, size_t n, TempError& e) {
    const auto t0 = Clock::now();
    const uint8_t* p = static_cast<const uint8_t*>(data);
    while (n > 0) {
        OVERLAPPED ov{};
        ov.Offset = static_cast<DWORD>(off);
        ov.OffsetHigh = static_cast<DWORD>(off >> 32);
        const DWORD chunk = static_cast<DWORD>(n > (1u << 30) ? (1u << 30) : n);
        DWORD done = 0;
        if (!WriteFile(h_, p, chunk, &done, &ov) || done == 0) {
            const DWORD err = GetLastError();
            e = {is_space_error(err) ? "disk" : "io", "writing temporary storage failed: " + win_error_text(err)};
            io_ms_ += ms(t0);
            return false;
        }
        off += done;
        p += done;
        n -= done;
    }
    io_ms_ += ms(t0);
    return true;
}

bool TempFile::append(const void* data, size_t n, TempError& e) {
    if (mode_ == Mode::positional) { e = {"internal", "append on a positional temporary file"}; return false; }
    mode_ = Mode::append;
    const auto end = checked_add(written_, n);
    if (!end || *end > reserved_) { e = {"internal", "temporary write beyond the reserved size"}; return false; }
    const uint8_t* p = static_cast<const uint8_t*>(data);
    while (n > 0) {
        const size_t take = std::min(n, kWriteBuffer - buf_.size());
        buf_.insert(buf_.end(), p, p + take);
        p += take;
        n -= take;
        written_ += take;
        if (buf_.size() == kWriteBuffer && !flush(e)) return false;
    }
    return true;
}

bool TempFile::flush(TempError& e) {
    if (buf_.empty()) return true;
    const bool ok = write_raw(on_disk_, buf_.data(), buf_.size(), e);
    if (ok) on_disk_ += buf_.size();
    buf_.clear();
    return ok;
}

bool TempFile::write_at(uint64_t off, const void* data, size_t n, TempError& e) {
    if (mode_ == Mode::append) { e = {"internal", "positional write on an append-mode temporary file"}; return false; }
    if (sealed_) { e = {"internal", "write after the temporary file was sealed"}; return false; }
    if (reserved_ == 0 && n > 0) { e = {"internal", "temporary write before reservation"}; return false; }
    const auto end = checked_add(off, n);
    if (!end || *end > reserved_) { e = {"internal", "temporary write beyond the reserved size"}; return false; }
    mode_ = Mode::positional;
    if (!write_raw(off, data, n, e)) return false;
    written_ += n;
    return true;
}

bool TempFile::seal(TempError& e) {
    if (mode_ == Mode::append) { e = {"internal", "seal on an append-mode temporary file"}; return false; }
    mode_ = Mode::positional;
    sealed_ = true;
    return true;
}

bool TempFile::read_at(uint64_t off, void* data, size_t n, TempError& e) {
    const auto end = checked_add(off, n);
    if (mode_ == Mode::positional) {
        if (!sealed_) { e = {"internal", "temporary read before coverage was proven"}; return false; }
        if (!end || *end > reserved_) { e = {"internal", "temporary read outside the reserved range"}; return false; }
    } else if (!end || *end > on_disk_) {
        e = {"internal", "temporary read outside the written range"};
        return false;
    }
    const auto t0 = Clock::now();
    uint8_t* p = static_cast<uint8_t*>(data);
    while (n > 0) {
        OVERLAPPED ov{};
        ov.Offset = static_cast<DWORD>(off);
        ov.OffsetHigh = static_cast<DWORD>(off >> 32);
        const DWORD chunk = static_cast<DWORD>(n > (1u << 30) ? (1u << 30) : n);
        DWORD done = 0;
        if (!ReadFile(h_, p, chunk, &done, &ov) || done == 0) {
            e = {"io", "reading temporary storage failed: " + win_error_text(GetLastError())};
            io_ms_ += ms(t0);
            return false;
        }
        off += done;
        p += done;
        n -= done;
        read_ += done;
    }
    io_ms_ += ms(t0);
    return true;
}

// ------------------------------------------------------------ RowCoverage
bool RowCoverage::fail(const std::string& why) {
    if (problem_.empty()) problem_ = why;
    return false;
}

bool RowCoverage::insert(uint32_t y, uint32_t x, uint32_t n) {
    if (n == 0) return fail("empty span at row " + std::to_string(y));
    if (y >= state_.size() || x >= width_ || n > width_ - x)
        return fail("span row " + std::to_string(y) + " [" + std::to_string(x) + ", +" + std::to_string(n) +
                    ") lies outside the " + std::to_string(width_) + "x" + std::to_string(state_.size()) + " raster");
    const uint32_t a = x, b = x + n;  // [a, b)
    auto overlap = [&]() {
        return fail("row " + std::to_string(y) + " span [" + std::to_string(a) + ", " + std::to_string(b) +
                    ") overlaps pixels already written");
    };
    uint8_t& st = state_[y];
    if (st == kFull) return overlap();
    if (st == kEmpty) {
        if (a == 0 && b == width_) {
            st = kFull;
            ++full_rows_;
            return true;
        }
        st = kPartial;
        partial_[y].push_back({a, b});
        return true;
    }
    auto& iv = partial_[y];
    // First interval starting at or after b. Intervals are sorted and disjoint, so
    // only the one just before it can reach into [a, b).
    auto it = std::lower_bound(iv.begin(), iv.end(), std::make_pair(b, 0u));
    if (it != iv.begin() && std::prev(it)->second > a) return overlap();
    // Merge with touching neighbours so a complete row collapses to one interval.
    uint32_t na = a, nb = b;
    auto first = it, last = it;
    if (it != iv.begin() && std::prev(it)->second == a) { first = std::prev(it); na = first->first; }
    if (it != iv.end() && it->first == b) { nb = it->second; last = std::next(it); }
    it = iv.erase(first, last);
    iv.insert(it, {na, nb});
    if (iv.size() == 1 && iv.front().first == 0 && iv.front().second == width_) {
        partial_.erase(y);
        st = kFull;
        ++full_rows_;
    }
    return true;
}

std::string RowCoverage::problem() const {
    if (!problem_.empty()) return problem_;
    for (size_t y = 0; y < state_.size(); ++y) {
        if (state_[y] == kFull) continue;
        uint64_t have = 0;
        if (state_[y] == kPartial) {
            auto it = partial_.find(static_cast<uint32_t>(y));
            if (it != partial_.end())
                for (const auto& [a, b] : it->second) have += b - a;
        }
        return "row " + std::to_string(y) + " has " + std::to_string(have) + " of " + std::to_string(width_) +
               " pixels written";
    }
    return {};
}

}  // namespace iat
