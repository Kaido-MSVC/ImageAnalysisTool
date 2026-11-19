// Base for decoders that cannot produce rows top to bottom with bounded memory
// (libjxl delivers 256-wide group pieces out of raster order; libwebp decodes the
// whole picture at once). open() decodes the entire image, writes the canonical
// samples into this side's temporary file in positional mode, proves complete and
// non-overlapping coverage, seals the file and lets the codec release its memory.
// read_row() then serves rows in raster order from the temporary file, so the
// engine's lockstep comparison is unchanged and two whole-image sides are never
// resident at the same time (stage 0 decision; plan 4.1 point 3).
#pragma once

#include "side.hpp"

#include <map>
#include <optional>
#include <string>
#include <vector>

namespace iat {

class WholeImageDecoder : public Decoder {
public:
    // Tool-managed buffers: one canonical row, one temp row and the coverage state.
    std::optional<uint64_t> buffer_estimate() const override;
    std::optional<uint64_t> codec_estimate() const override { return codec_estimate_; }
    bool open(Side& s, Report& r, const Limits& lim) override;
    bool read_row(Side& s, Report& r) override;
    void finish(Side& s, Report& r) override;
    void memory_metrics(std::map<std::string, uint64_t>& m) const override;

protected:
    // Decode the whole image, delivering canonical samples through put(). Return
    // false after recording the reason (fail_decode / fail_resource) when the codec
    // fails; spans rejected by put() are detected by the base afterwards.
    virtual bool decode_all(Side& s, Report& r, const Limits& lim) = 0;
    // Codec-specific metrics, e.g. allocator peak.
    virtual void codec_metrics(std::map<std::string, uint64_t>&) const {}

    // Canonical samples for [x, x + n) of row y (n * channels values). Safe to call
    // from a codec callback: failures are recorded, never thrown.
    void put(uint32_t y, uint32_t x, uint32_t n, const uint16_t* samples);
    bool put_failed() const { return !put_error_.empty(); }

    void fail_decode(Side& s, Report& r, const std::string& reason, const std::string& msg, bool some_data);
    void fail_resource(Side& s, Report& r, const std::string& msg);

    // Set by the front end once the header is known, before the engine asks for estimates.
    void set_geometry(uint32_t width, uint32_t height, uint32_t channels, uint32_t bits);
    // After decode_all(): serve only the first `channels` of each stored pixel (e.g. a
    // WebP whose alpha proved fully opaque is compared as RGB). Updates s.layout.
    void set_output_channels(Side& s, uint32_t channels, const std::string& model);
    std::optional<uint64_t> codec_estimate_ = uint64_t{0};

private:
    uint32_t width_ = 0, height_ = 0, channels_ = 0, out_channels_ = 0;
    Side* side_ = nullptr;
    RowCoverage cov_;
    uint32_t bytes_per_sample_ = 2;
    uint64_t row_bytes_ = 0;
    std::vector<uint8_t> span_, row_;
    std::string put_error_;
    TempError put_temp_error_;
    bool decoded_ = false;
};

}  // namespace iat
