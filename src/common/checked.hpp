// 64-bit checked arithmetic and a 128-bit accumulator. All size, offset and
// count computations in the tool go through these helpers.
#pragma once

#include <intrin.h>

#include <cmath>
#include <cstdint>
#include <initializer_list>
#include <optional>

namespace iat {

inline std::optional<uint64_t> checked_mul(uint64_t a, uint64_t b) {
    uint64_t hi = 0;
    const uint64_t lo = _umul128(a, b, &hi);
    if (hi != 0) return std::nullopt;
    return lo;
}

inline std::optional<uint64_t> checked_add(uint64_t a, uint64_t b) {
    const uint64_t r = a + b;
    if (r < a) return std::nullopt;
    return r;
}

// Product of all factors, or nullopt on overflow.
inline std::optional<uint64_t> checked_product(std::initializer_list<uint64_t> factors) {
    uint64_t r = 1;
    for (uint64_t f : factors) {
        auto m = checked_mul(r, f);
        if (!m) return std::nullopt;
        r = *m;
    }
    return r;
}

inline std::optional<uint64_t> checked_sum(std::initializer_list<uint64_t> terms) {
    uint64_t r = 0;
    for (uint64_t t : terms) {
        auto s = checked_add(r, t);
        if (!s) return std::nullopt;
        r = *s;
    }
    return r;
}

// Unsigned 128-bit accumulator for sums of absolute and squared errors
// (a whole 16-bit image can exceed 2^64 in squared error).
struct U128 {
    uint64_t lo = 0;
    uint64_t hi = 0;

    void add(uint64_t v) {
        const uint64_t old = lo;
        lo += v;
        if (lo < old) ++hi;
    }
    void add(const U128& o) {
        add(o.lo);
        hi += o.hi;
    }
    bool is_zero() const { return lo == 0 && hi == 0; }
    // Approximate value for presentation only (equality never uses this).
    double approx() const { return static_cast<double>(hi) * 18446744073709551616.0 + static_cast<double>(lo); }
};

}  // namespace iat
