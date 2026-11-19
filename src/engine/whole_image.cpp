#include "whole_image.hpp"

#include "../common/checked.hpp"

#include <cstring>
#include <new>

namespace iat {

namespace {
// Interval lists of rows still partial: a fixed allowance, independent of image
// size (libjxl keeps at most a band of groups in flight).
constexpr uint64_t kCoverageAllowance = 1ull << 20;
}  // namespace

void WholeImageDecoder::set_geometry(uint32_t width, uint32_t height, uint32_t channels, uint32_t bits) {
    width_ = width;
    height_ = height;
    channels_ = out_channels_ = channels;
    bytes_per_sample_ = bits <= 8 ? 1u : 2u;
}

void WholeImageDecoder::set_output_channels(Side& s, uint32_t channels, const std::string& model) {
    if (channels == 0 || channels > channels_) return;
    out_channels_ = channels;
    s.layout.channels = channels;
    s.layout.model = model;
}

std::optional<uint64_t> WholeImageDecoder::buffer_estimate() const {
    const auto canon = checked_product({width_, channels_, 2});
    const auto temp_row = checked_product({width_, channels_, bytes_per_sample_});
    if (!canon || !temp_row) return std::nullopt;
    // canonical row + temp row (read back) + span conversion buffer (at most one row)
    // + one state byte per row + allowance for partial rows.
    return checked_sum({*canon, *temp_row, *temp_row, RowCoverage::state_bytes(height_), kCoverageAllowance});
}

void WholeImageDecoder::fail_decode(Side& s, Report& r, const std::string& reason, const std::string& msg, bool some_data) {
    s.failed = true;
    s.rep->decode_status = some_data ? "partial" : "failed";
    s.rep->decode_status_reason = reason;
    add_issue(r, code::decode_failed, "error", s.scope, msg);
}

void WholeImageDecoder::fail_resource(Side& s, Report& r, const std::string& msg) {
    s.failed = true;
    s.rep->decode_status = "not_checked";
    s.rep->decode_status_reason = "resource_limit";
    add_issue(r, code::resource_limit, "error", s.scope, msg);
}

bool WholeImageDecoder::open(Side& s, Report& r, const Limits& lim) {
    side_ = &s;
    const auto rb = checked_product({width_, channels_, bytes_per_sample_});
    const auto total = rb ? checked_mul(*rb, height_) : std::nullopt;
    if (!rb || !total) {
        fail_resource(s, r, "decoded image size overflows 64-bit arithmetic");
        return false;
    }
    row_bytes_ = *rb;
    TempError e;
    if (!s.temp || !s.temp->reserve(*total, e)) {
        if (!s.temp) e = {"unavailable", "no temporary file for this side"};
        temp_failure(s, r, e);
        return false;
    }
    try {
        cov_ = RowCoverage(width_, height_);
        row_.resize(static_cast<size_t>(row_bytes_));
        span_.reserve(static_cast<size_t>(row_bytes_));
    } catch (const std::bad_alloc&) {
        fail_resource(s, r, "tool buffers for the decoded image could not be allocated");
        return false;
    }
    s.rep->decode_path += "+temp";
    if (!decode_all(s, r, lim)) {
        if (!s.failed) fail_decode(s, r, "decode_error", "decoder failed", cov_.full_rows() > 0);
        return false;
    }
    if (put_failed()) {
        if (!put_temp_error_.kind.empty()) temp_failure(s, r, put_temp_error_);
        else fail_decode(s, r, "decoder_output_invalid", "decoder output rejected: " + put_error_, true);
        return false;
    }
    if (!cov_.complete()) {
        fail_decode(s, r, "decoder_output_incomplete", "decoder reported success but " + cov_.problem(), cov_.full_rows() > 0);
        return false;
    }
    if (!s.temp->seal(e)) {
        temp_failure(s, r, e);
        return false;
    }
    decoded_ = true;
    return true;
}

void WholeImageDecoder::put(uint32_t y, uint32_t x, uint32_t n, const uint16_t* samples) {
    if (!put_error_.empty() || !side_) return;
    if (!cov_.insert(y, x, n)) {
        put_error_ = cov_.problem();
        return;
    }
    const size_t count = static_cast<size_t>(n) * channels_;
    span_.resize(count * bytes_per_sample_);
    if (bytes_per_sample_ == 2) memcpy(span_.data(), samples, count * 2);
    else
        for (size_t i = 0; i < count; ++i) span_[i] = static_cast<uint8_t>(samples[i]);
    // Within the reservation: y < height and x + n <= width were checked by insert().
    const uint64_t off = static_cast<uint64_t>(y) * row_bytes_ + static_cast<uint64_t>(x) * channels_ * bytes_per_sample_;
    if (!side_->temp->write_at(off, span_.data(), span_.size(), put_temp_error_))
        put_error_ = put_temp_error_.message;
}

bool WholeImageDecoder::read_row(Side& s, Report& r) {
    if (s.failed || !decoded_) return false;
    TempError e;
    if (!s.temp->read_at(s.rows * row_bytes_, row_.data(), row_.size(), e)) {
        temp_failure(s, r, e);
        return false;
    }
    if (out_channels_ == channels_) {
        const size_t count = static_cast<size_t>(width_) * channels_;
        if (bytes_per_sample_ == 2) memcpy(s.canon.data(), row_.data(), count * 2);
        else
            for (size_t i = 0; i < count; ++i) s.canon[i] = row_[i];
    } else {
        const uint16_t* p16 = reinterpret_cast<const uint16_t*>(row_.data());
        for (size_t x = 0; x < width_; ++x)
            for (uint32_t c = 0; c < out_channels_; ++c) {
                const size_t src = x * channels_ + c;
                s.canon[x * out_channels_ + c] = bytes_per_sample_ == 2 ? p16[src] : row_[src];
            }
    }
    ++s.rows;
    return true;
}

void WholeImageDecoder::finish(Side& s, Report&) {
    FileReport& fr = *s.rep;
    fr.coverage.rows_decoded = s.rows;
    if (!s.failed && decoded_ && s.rows == height_) {
        fr.decode_status = "complete";
        fr.decode_status_reason.clear();
    }
}

void WholeImageDecoder::memory_metrics(std::map<std::string, uint64_t>& m) const {
    m["tool_row_buffers"] += row_.capacity() + span_.capacity();
    m["coverage_state"] += RowCoverage::state_bytes(height_);
    if (codec_estimate_) m["codec_estimate"] += *codec_estimate_;
    codec_metrics(m);
}

}  // namespace iat
