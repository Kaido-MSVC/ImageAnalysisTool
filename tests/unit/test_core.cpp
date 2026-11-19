// Unit tests for the pure parts of the core (doctest).
#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest/doctest.h>

#include "common/checked.hpp"
#include "common/limits.hpp"
#include "common/text.hpp"
#include "engine/analyze.hpp"
#include "engine/avif_scan.hpp"
#include "engine/jxl_scan.hpp"
#include "engine/psd_scan.hpp"
#include "engine/png_scan.hpp"
#include "engine/webp_scan.hpp"
#include "engine/temp_store.hpp"
#include "engine/tiff_scan.hpp"
#include "core/core.hpp"

#include <algorithm>

#include <cstring>
#include "model/render.hpp"

#include <zlib.h>

using namespace iat;

TEST_CASE("checked arithmetic detects 64-bit overflow") {
    CHECK(checked_mul(1ull << 32, 1ull << 31).value() == (1ull << 63));
    CHECK_FALSE(checked_mul(1ull << 32, 1ull << 32).has_value());
    CHECK_FALSE(checked_add(UINT64_MAX, 1).has_value());
    CHECK_FALSE(checked_product({0x7FFFFFFFull, 0x7FFFFFFFull, 4, 2, 4}).has_value());  // ~2^67
    CHECK(checked_product({0x7FFFFFFFull, 0x7FFFFFFFull, 2}).has_value());              // ~2^63
    CHECK_FALSE(checked_product({0xFFFFFFFFull, 0xFFFFFFFFull, 2}).has_value());
}

TEST_CASE("U128 accumulates past 2^64") {
    U128 a;
    a.add(UINT64_MAX);
    a.add(2);
    CHECK(a.hi == 1);
    CHECK(a.lo == 1);
    // Whole 16-bit image of max squared error: 2^31 * 2^31 pixels would overflow 64-bit.
    U128 b;
    for (int i = 0; i < 8; ++i) b.add(0xFFFE0001ull * (1ull << 31));
    CHECK(b.hi > 0);
}

TEST_CASE("PNG row budget uses rows, never the whole image") {
    // big-png-261mp geometry: 12949 x 20167 RGB8 = 747 MiB of samples.
    const auto est = png_row_budget(12949, 8, 2, false);
    REQUIRE(est.has_value());
    CHECK(*est < (1ull << 20));                  // well under 1 MiB
    CHECK(*est < 64ull << 20);                   // fits the default 64 MiB buffer budget
    const auto rgb16 = png_row_budget(16384, 16, 2, false);
    CHECK(rgb16.value() < (1ull << 20));
    // Maximum legal PNG width stays representable in 64-bit arithmetic.
    CHECK(png_row_budget(0x7FFFFFFFu, 16, 6, false).has_value());
}

TEST_CASE("Exif orientation parsing") {
    std::vector<uint8_t> e = {'I', 'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0};
    CHECK(exif_orientation(e) == 6);
    e[18] = 9;  // out of range
    CHECK(exif_orientation(e) == -1);
    CHECK(exif_orientation({'X', 'X', 0, 0, 0, 0, 0, 0}) == -1);
    std::vector<uint8_t> none = {'M', 'M', 0, 42, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0};
    CHECK(exif_orientation(none) == 0);
}

TEST_CASE("inflate_capped enforces the output cap and detects truncation") {
    std::vector<uint8_t> src(100000, 'x'), comp(compressBound(static_cast<uLong>(src.size())));
    uLongf n = static_cast<uLongf>(comp.size());
    REQUIRE(compress2(comp.data(), &n, src.data(), static_cast<uLong>(src.size()), 9) == Z_OK);
    std::vector<uint8_t> out;
    CHECK(inflate_capped(comp.data(), n, 200000, out) == "ok");
    CHECK(out.size() == src.size());
    CHECK(inflate_capped(comp.data(), n, 1000, out) == "over_limit");
    CHECK(inflate_capped(comp.data(), n / 2, 200000, out) == "truncated");
}

TEST_CASE("limits: finite only, range checked") {
    Limits l;
    CHECK(l.job_commit_mib == 1024);
    CHECK(l.set("deadline_s=0") != "");
    CHECK(l.set("job_commit_mib=8") != "");
    CHECK(l.set("unlimited=1") != "");
    CHECK(l.set("buffer_mib=-1") != "");
    CHECK(l.set("deadline_s=1200") == "");
    CHECK(l.deadline_s == 1200);
}

TEST_CASE("exit code priority 5 > 4 > 3 > 1 > 2 > 0") {
    Report r;
    r.mode = "compare";
    r.output = FileReport{};
    r.original.decode_status = r.output->decode_status = "complete";
    r.original.container_status = r.output->container_status = "valid";
    r.strict.result = "match";
    CHECK(decide_exit(r).code == 0);
    r.strict.result = "indeterminate";
    CHECK(decide_exit(r).code == 2);
    r.strict.result = "mismatch";
    CHECK(decide_exit(r).code == 1);
    r.output->decode_status = "partial";
    CHECK(decide_exit(r).code == 3);
    r.original.decode_status = "not_checked";
    r.original.decode_status_reason = "deadline";
    CHECK(decide_exit(r).code == 4);
    r.issues.push_back(Issue{code::worker_crashed, "error", "run", ""});
    CHECK(decide_exit(r).code == 5);
}

namespace {
// A delete-on-close scratch file like the ones the parent creates.
HANDLE scratch_file() {
    wchar_t dir[MAX_PATH + 1], path[MAX_PATH + 1];
    GetTempPath2W(MAX_PATH + 1, dir);
    GetTempFileNameW(dir, L"iut", 0, path);
    return CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                       FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE, nullptr);
}
}  // namespace

TEST_CASE("temp quota: limit and free-disk margin are both enforced") {
    TempQuota q(1000, 5000 + TempQuota::kDiskMargin);
    CHECK(q.take(600).empty());
    CHECK(q.take(500) == "quota");  // 1100 > 1000
    CHECK(q.take(400).empty());
    TempQuota d(1ull << 40, 100 + TempQuota::kDiskMargin);
    CHECK(d.take(101) == "disk");   // would eat into the margin
    CHECK(d.take(100).empty());
    TempQuota o(UINT64_MAX, UINT64_MAX);
    CHECK(o.take(1ull << 62).empty());
    CHECK(o.take(UINT64_MAX) == "quota");          // reserved + n overflows: refused, not wrapped
    CHECK(o.take(UINT64_MAX - (1ull << 62)) == "disk");  // + margin overflows: refused as well
}

TEST_CASE("temp file: reserved, append-only, no read past written data") {
    HANDLE h = scratch_file();
    REQUIRE(h != INVALID_HANDLE_VALUE);
    TempQuota q(1 << 20, 1ull << 40);
    TempFile t(h, &q);
    TempError e;
    const uint8_t data[8] = {1, 2, 3, 4, 5, 6, 7, 8};
    CHECK_FALSE(t.append(data, 8, e));  // nothing reserved yet
    CHECK(e.kind == "internal");
    REQUIRE(t.reserve(16, e));
    REQUIRE(t.append(data, 8, e));
    uint8_t back[8] = {};
    CHECK_FALSE(t.read_at(0, back, 8, e));  // still in the write buffer: not readable yet
    REQUIRE(t.flush(e));
    REQUIRE(t.read_at(0, back, 8, e));
    CHECK(memcmp(back, data, 8) == 0);
    CHECK_FALSE(t.read_at(4, back, 8, e));  // 4..12 crosses the written end (a hole)
    REQUIRE(t.append(data, 8, e));
    CHECK_FALSE(t.append(data, 1, e));      // beyond the reservation
    TempFile none;
    CHECK_FALSE(none.reserve(1, e));
    CHECK(e.kind == "unavailable");
    CloseHandle(h);
}

TEST_CASE("markdown escaping and length limit") {
    CHECK(md_escape("a|b*c_d") == "a\\|b\\*c\\_d");
    CHECK(md_escape("# title") == "\\# title");
    CHECK(md_escape("x\ny") == "x y");
    CHECK(md_escape("中文檔名.png", 2) == "中文…");
}

// ------------------------------------------------------------ stage 3: coverage, positional temp, codec cap
TEST_CASE("row coverage: out-of-order spans merge into complete rows") {
    RowCoverage c(600, 2);
    // libjxl-like order: 256-wide group pieces, both rows of the first group column first.
    CHECK(c.insert(0, 0, 256));
    CHECK(c.insert(1, 0, 256));
    CHECK(c.insert(0, 512, 88));
    CHECK(c.insert(1, 256, 256));
    CHECK(c.insert(0, 256, 256));   // fills the gap: row 0 becomes one interval [0, 600)
    CHECK_FALSE(c.complete());
    CHECK(c.problem().find("row 1 has 512 of 600") != std::string::npos);
    CHECK(c.insert(1, 512, 88));
    CHECK(c.complete());
    CHECK(c.full_rows() == 2);
}

TEST_CASE("row coverage: a duplicate fails immediately") {
    RowCoverage c(10, 1);
    CHECK(c.insert(0, 0, 4));
    CHECK_FALSE(c.insert(0, 3, 2));   // overlaps pixel 3
    CHECK(c.problem().find("overlaps") != std::string::npos);
    CHECK_FALSE(c.complete());
    RowCoverage full(10, 1);
    CHECK(full.insert(0, 0, 10));
    CHECK_FALSE(full.insert(0, 9, 1));  // any write into a full row is a duplicate
}

TEST_CASE("row coverage: a missing pixel is reported") {
    RowCoverage c(10, 2);
    CHECK(c.insert(0, 0, 10));
    CHECK(c.insert(1, 0, 4));
    CHECK(c.insert(1, 5, 5));         // pixel 4 of row 1 never written
    CHECK_FALSE(c.complete());
    CHECK(c.problem().find("row 1 has 9 of 10") != std::string::npos);
}

TEST_CASE("row coverage: a duplicate and a gap cannot cancel out") {
    // Same total pixel count as a full row (10), but pixel 2 twice and pixel 9 never.
    RowCoverage c(10, 1);
    CHECK(c.insert(0, 0, 3));
    CHECK_FALSE(c.insert(0, 2, 7));
    CHECK_FALSE(c.complete());
    CHECK(c.problem().find("overlaps") != std::string::npos);  // the first problem is kept
}

TEST_CASE("row coverage: spans outside the raster or empty are rejected") {
    RowCoverage c(10, 2);
    CHECK_FALSE(c.insert(2, 0, 1));
    RowCoverage d(10, 2);
    CHECK_FALSE(d.insert(0, 8, 3));
    RowCoverage e(10, 2);
    CHECK_FALSE(e.insert(0, 0, 0));
    RowCoverage f(10, 2);
    CHECK_FALSE(f.insert(0, 0xFFFFFFFFu, 2));  // x + n would wrap 32 bits
}

TEST_CASE("temp file: positional mode is unreadable until sealed and never mixes with append") {
    HANDLE h = scratch_file();
    REQUIRE(h != INVALID_HANDLE_VALUE);
    TempQuota q(1 << 20, 1ull << 40);
    TempFile t(h, &q);
    TempError e;
    const uint8_t a[4] = {1, 2, 3, 4}, b[4] = {5, 6, 7, 8};
    CHECK_FALSE(t.write_at(0, a, 4, e));    // nothing reserved yet
    REQUIRE(t.reserve(8, e));
    REQUIRE(t.write_at(4, b, 4, e));        // out of order is allowed
    REQUIRE(t.write_at(0, a, 4, e));
    CHECK_FALSE(t.write_at(6, a, 4, e));    // beyond the reservation
    uint8_t back[8] = {};
    CHECK_FALSE(t.read_at(0, back, 8, e));  // not sealed: coverage not proven yet
    CHECK_FALSE(t.append(a, 1, e));         // modes cannot be mixed
    REQUIRE(t.seal(e));
    REQUIRE(t.read_at(0, back, 8, e));
    CHECK(memcmp(back, a, 4) == 0);
    CHECK(memcmp(back + 4, b, 4) == 0);
    CHECK_FALSE(t.write_at(0, a, 4, e));    // sealed: no more writes
    CHECK_FALSE(t.read_at(4, back, 8, e));  // beyond the reservation
    CloseHandle(h);
}

TEST_CASE("codec hard cap is job commit minus 64 MiB") {
    Limits l;
    CHECK(l.codec_cap_bytes() == (1024ull - 64) << 20);
    CHECK(l.set("job_commit_mib=128").empty());
    CHECK(l.codec_cap_bytes() == 64ull << 20);
    CHECK(l.set("job_commit_mib=16").empty());   // smaller than the reserve: nothing left for a codec
    CHECK(l.codec_cap_bytes() == 0);
    CHECK_FALSE(l.set("test_codec_estimate_pct=10").empty());  // test knob is not a --limits name
}

// ------------------------------------------------------------ TIFF container scanner
namespace {
struct TagSpec {
    uint16_t tag, type;
    uint32_t count, value;  // value: inline (SHORT in the low half) or an offset
};

void le(std::vector<uint8_t>& v, uint64_t x, int n) {
    for (int i = 0; i < n; ++i) v.push_back(static_cast<uint8_t>(x >> (8 * i)));
}

// Classic little-endian TIFF: header, `data` at offset 8, then the IFD.
std::vector<uint8_t> tiny_tiff(const std::vector<uint8_t>& data, const std::vector<TagSpec>& tags) {
    std::vector<uint8_t> f = {'I', 'I', 42, 0};
    const uint32_t ifd = static_cast<uint32_t>(8 + data.size() + (data.size() & 1));
    le(f, ifd, 4);
    f.insert(f.end(), data.begin(), data.end());
    if (data.size() & 1) f.push_back(0);
    le(f, tags.size(), 2);
    for (const auto& t : tags) {
        le(f, t.tag, 2);
        le(f, t.type, 2);
        le(f, t.count, 4);
        le(f, t.value, 4);
    }
    le(f, 0, 4);
    return f;
}

// 2x2 gray8, one uncompressed strip of 4 bytes at offset 8; strip offset/count overridable.
std::vector<TagSpec> gray2x2(uint32_t strip_off = 8, uint32_t strip_cnt = 4) {
    return {{256, 3, 1, 2}, {257, 3, 1, 2}, {258, 3, 1, 8}, {259, 3, 1, 1}, {262, 3, 1, 1},
            {273, 4, 1, strip_off}, {277, 3, 1, 1}, {278, 3, 1, 2}, {279, 4, 1, strip_cnt}};
}

TiffScan scan_bytes(const std::vector<uint8_t>& bytes) {
    HANDLE h = scratch_file();
    REQUIRE(h != INVALID_HANDLE_VALUE);
    DWORD n = 0;
    REQUIRE(WriteFile(h, bytes.data(), static_cast<DWORD>(bytes.size()), &n, nullptr));
    ReadOnlyFile f(h, true);  // closes (and deletes) the scratch file
    return scan_tiff(f, bytes.size(), Limits{});
}

bool has_code(const TiffScan& s, const std::string& c) {
    return std::any_of(s.issues.begin(), s.issues.end(), [&](const Issue& i) { return i.code == c; });
}
}  // namespace

TEST_CASE("TIFF scan: minimal valid file") {
    const auto s = scan_bytes(tiny_tiff({1, 2, 3, 4}, gray2x2()));
    CHECK(s.container_status == "valid");
    CHECK(s.width == 2);
    CHECK(s.height == 2);
    CHECK(s.bits == 8);
    CHECK(s.blocks == 1);
    CHECK(s.data_end == 12);
    CHECK_FALSE(s.tiled);
}

TEST_CASE("TIFF scan: strip table checked against the file size") {
    // Starts inside the file but runs past its end: truncated image data.
    auto s = scan_bytes(tiny_tiff({1, 2, 3, 4}, gray2x2(8, 400)));
    CHECK(s.container_status == "corrupt");
    CHECK(has_code(s, code::truncated));
    // Starts beyond the end: a structural error, not truncation.
    s = scan_bytes(tiny_tiff({1, 2, 3, 4}, gray2x2(1u << 30, 4)));
    CHECK(s.container_status == "corrupt");
    CHECK(has_code(s, code::structure_error));
    CHECK_FALSE(has_code(s, code::truncated));
    // offset + count wraps 32 bits in the file but not in 64-bit arithmetic: still outside.
    s = scan_bytes(tiny_tiff({1, 2, 3, 4}, gray2x2(0xFFFFFFF0u, 0x20)));
    CHECK(has_code(s, code::structure_error));
    // Byte count 0 means missing image data.
    s = scan_bytes(tiny_tiff({1, 2, 3, 4}, gray2x2(8, 0)));
    CHECK(has_code(s, code::structure_error));
}

TEST_CASE("TIFF scan: strip count must match the geometry") {
    auto tags = gray2x2();
    tags[7].value = 1;  // RowsPerStrip 1 -> 2 strips needed, IFD lists 1
    const auto s = scan_bytes(tiny_tiff({1, 2, 3, 4}, tags));
    CHECK(s.container_status == "corrupt");
    CHECK(has_code(s, code::structure_error));
}

TEST_CASE("TIFF scan: tag value ranges") {
    // Out-of-line value (3 SHORTs = 6 bytes) pointing past the end of the file.
    auto tags = gray2x2();
    tags[2] = {258, 3, 3, 0x7FFFFFF0u};
    auto s = scan_bytes(tiny_tiff({1, 2, 3, 4}, tags));
    CHECK(s.container_status == "corrupt");
    CHECK(has_code(s, code::structure_error));
    // Count large enough that count * size reaches 4 GiB: outside any small file, no wrap-around.
    tags = gray2x2();
    tags.push_back({34675, 4, 0x40000000u, 8});  // ICC as LONG x 2^30 = 4 GiB at offset 8
    s = scan_bytes(tiny_tiff({1, 2, 3, 4}, tags));
    CHECK(s.container_status == "corrupt");
    CHECK(has_code(s, code::structure_error));
}

TEST_CASE("TIFF scan: tag order problems are warnings, not corruption") {
    auto tags = gray2x2();
    std::swap(tags[0], tags[1]);  // 257 before 256
    const auto s = scan_bytes(tiny_tiff({1, 2, 3, 4}, tags));
    CHECK(s.container_status == "warning");
    CHECK(has_code(s, "tiff_tag_order"));
    CHECK(s.width == 2);
}

// ------------------------------------------------------------ stage 3: WebP RIFF and JXL box scans
namespace {
template <class Scan, class Fn>
Scan scan_with(const std::vector<uint8_t>& bytes, Fn fn) {
    HANDLE h = scratch_file();
    REQUIRE(h != INVALID_HANDLE_VALUE);
    DWORD n = 0;
    REQUIRE(WriteFile(h, bytes.data(), static_cast<DWORD>(bytes.size()), &n, nullptr));
    ReadOnlyFile f(h, true);
    return fn(f, bytes.size());
}
WebpScan webp_bytes(const std::vector<uint8_t>& b) {
    return scan_with<WebpScan>(b, [](const ReadOnlyFile& f, uint64_t n) { return scan_webp(f, n, Limits{}); });
}
JxlScan jxl_bytes(const std::vector<uint8_t>& b) {
    return scan_with<JxlScan>(b, [](const ReadOnlyFile& f, uint64_t n) { return scan_jxl(f, n, Limits{}); });
}
template <class S>
bool has_issue(const S& s, const std::string& c) {
    return std::any_of(s.issues.begin(), s.issues.end(), [&](const Issue& i) { return i.code == c; });
}

using Bytes = std::vector<uint8_t>;
using Chunk = std::pair<std::string, Bytes>;

// VP8L header only (signature + 32-bit size/alpha/version word); the scan never decodes further.
Bytes vp8l_header(uint32_t w, uint32_t h, bool alpha) {
    Bytes b = {0x2F};
    le(b, (w - 1) | ((h - 1) << 14) | (alpha ? 1u << 28 : 0u), 4);
    b.push_back(0);   // padding to an even length is added by riff()
    return b;
}
Bytes vp8x_payload(uint8_t flags, uint32_t w, uint32_t h) {
    Bytes b = {flags, 0, 0, 0};
    le(b, w - 1, 3);
    le(b, h - 1, 3);
    return b;
}
Bytes riff(const std::vector<Chunk>& chunks, bool pad = true) {
    Bytes body = {'W', 'E', 'B', 'P'};
    for (const auto& [tag, p] : chunks) {
        body.insert(body.end(), tag.begin(), tag.end());
        le(body, p.size(), 4);
        body.insert(body.end(), p.begin(), p.end());
        if (pad && (p.size() & 1)) body.push_back(0);
    }
    Bytes f = {'R', 'I', 'F', 'F'};
    le(f, body.size(), 4);
    f.insert(f.end(), body.begin(), body.end());
    return f;
}

void be32(Bytes& v, uint64_t x) {
    for (int i = 3; i >= 0; --i) v.push_back(static_cast<uint8_t>(x >> (8 * i)));
}
Bytes jbox(const std::string& type, const Bytes& payload) {
    Bytes b;
    be32(b, 8 + payload.size());
    b.insert(b.end(), type.begin(), type.end());
    b.insert(b.end(), payload.begin(), payload.end());
    return b;
}
Bytes cat(std::initializer_list<Bytes> parts) {
    Bytes out;
    for (const auto& p : parts) out.insert(out.end(), p.begin(), p.end());
    return out;
}
const Bytes kJxlSig = {0, 0, 0, 0x0C, 'J', 'X', 'L', ' ', 0x0D, 0x0A, 0x87, 0x0A};
const Bytes kFtyp = jbox("ftyp", {'j', 'x', 'l', ' ', 0, 0, 0, 0, 'j', 'x', 'l', ' '});
const Bytes kCs = {0xFF, 0x0A, 1, 2, 3, 4, 5};   // the box scan does not read codestream contents
Bytes jxlp(uint32_t idx, const Bytes& part, bool last) {
    Bytes p;
    be32(p, idx | (last ? 0x80000000u : 0u));
    p.insert(p.end(), part.begin(), part.end());
    return jbox("jxlp", p);
}
}  // namespace

TEST_CASE("WebP scan: simple VP8L file") {
    const auto s = webp_bytes(riff({{"VP8L", vp8l_header(300, 200, true)}}));
    CHECK(s.container_status == "valid");
    CHECK(s.image_chunk == "VP8L");
    CHECK(s.width == 300);
    CHECK(s.height == 200);
    CHECK(s.vp8l_alpha_hint);
    CHECK(s.trailing_bytes.value() == 0);
}

TEST_CASE("WebP scan: RIFF size, chunk size and padding are checked against the file") {
    auto f = riff({{"VP8L", vp8l_header(4, 4, false)}});
    f[4] += 10;   // RIFF claims 10 bytes more than the file holds
    auto s = webp_bytes(f);
    CHECK(s.container_status == "corrupt");
    CHECK(has_issue(s, code::truncated));
    CHECK_FALSE(s.trailing_bytes.has_value());

    Bytes odd = vp8l_header(4, 4, false);
    odd.push_back(0);   // 7 bytes: odd payload, then the padding byte is left out
    s = webp_bytes(riff({{"VP8L", odd}}, false));
    CHECK(s.container_status == "corrupt");
    CHECK(has_issue(s, code::truncated));

    f = riff({{"VP8L", vp8l_header(4, 4, false)}});
    f.insert(f.end(), {'J', 'U', 'N', 'K'});
    s = webp_bytes(f);
    CHECK(s.container_status == "warning");
    CHECK(has_issue(s, code::trailing_data));
    CHECK(s.trailing_bytes.value() == 4);
}

TEST_CASE("WebP scan: VP8X flags are compared with the chunks and the VP8L hint") {
    // Alpha flag set, VP8L hint clear: only this scan sees it (libwebp lets the hint win).
    auto s = webp_bytes(riff({{"VP8X", vp8x_payload(0x10, 4, 4)}, {"VP8L", vp8l_header(4, 4, false)}}));
    CHECK(s.container_status == "warning");
    CHECK(has_issue(s, code::alpha_hint_mismatch));
    // ICC flag without an ICCP chunk.
    s = webp_bytes(riff({{"VP8X", vp8x_payload(0x20, 4, 4)}, {"VP8L", vp8l_header(4, 4, false)}}));
    CHECK(s.container_status == "warning");
    CHECK(has_issue(s, "webp_container_warning"));
    // ICCP read when present and flagged.
    s = webp_bytes(riff({{"VP8X", vp8x_payload(0x20, 4, 4)}, {"ICCP", {1, 2, 3}}, {"VP8L", vp8l_header(4, 4, false)}}));
    CHECK(s.container_status == "valid");
    CHECK(s.icc_status == "embedded");
    CHECK(s.icc == Bytes{1, 2, 3});
    // Canvas size must equal the still image size.
    s = webp_bytes(riff({{"VP8X", vp8x_payload(0, 5, 4)}, {"VP8L", vp8l_header(4, 4, false)}}));
    CHECK(s.container_status == "corrupt");
    CHECK(has_issue(s, code::structure_error));
}

TEST_CASE("WebP scan: animation is recognised without a top-level image chunk") {
    const auto s = webp_bytes(riff({{"VP8X", vp8x_payload(0x02, 7, 5)}, {"ANIM", Bytes(6, 0)}, {"ANMF", Bytes(16, 0)}}));
    CHECK(s.container_status != "corrupt");
    CHECK(s.animated);
    CHECK(s.canvas_width == 7);
    CHECK(s.canvas_height == 5);
    CHECK(s.image_chunk.empty());
}

TEST_CASE("WebP scan: wrong first chunk and missing image") {
    auto s = webp_bytes(riff({{"ICCP", {1, 2}}, {"VP8L", vp8l_header(4, 4, false)}}));
    CHECK(s.container_status == "corrupt");
    CHECK(has_issue(s, code::structure_error));
    s = webp_bytes(riff({}));
    CHECK(s.container_status == "corrupt");
}

TEST_CASE("JXL scan: bare codestream and minimal container") {
    auto s = jxl_bytes(kCs);
    CHECK(s.container_status == "valid");
    CHECK_FALSE(s.container);
    s = jxl_bytes(cat({kJxlSig, kFtyp, jbox("jxlc", kCs)}));
    CHECK(s.container_status == "valid");
    CHECK(s.container);
    CHECK(s.codestream_bytes == kCs.size());
    CHECK(s.boxes == std::vector<std::string>{"JXL ", "ftyp", "jxlc"});
}

TEST_CASE("JXL scan: jxlp parts must be in order and end with the last-part bit") {
    const Bytes a = {0xFF, 0x0A, 1}, b = {2, 3}, c = {4};
    auto s = jxl_bytes(cat({kJxlSig, kFtyp, jxlp(0, a, false), jxlp(1, b, false), jxlp(2, c, true)}));
    CHECK(s.container_status == "valid");
    CHECK(s.jxlp_parts == 3);
    CHECK(s.codestream_bytes == 6);
    s = jxl_bytes(cat({kJxlSig, kFtyp, jxlp(0, a, false), jxlp(1, b, false)}));
    CHECK(s.container_status == "corrupt");
    CHECK(has_issue(s, code::truncated));
    s = jxl_bytes(cat({kJxlSig, kFtyp, jxlp(0, a, false), jxlp(2, c, true)}));
    CHECK(has_issue(s, code::structure_error));
    s = jxl_bytes(cat({kJxlSig, kFtyp, jbox("jxlc", kCs), jxlp(0, a, true)}));
    CHECK(has_issue(s, code::structure_error));
}

TEST_CASE("JXL scan: boxes are checked against the end of the file") {
    Bytes f = cat({kJxlSig, kFtyp, jbox("jxlc", kCs)});
    f.pop_back();   // jxlc runs one byte past the end
    auto s = jxl_bytes(f);
    CHECK(s.container_status == "corrupt");
    CHECK(has_issue(s, code::truncated));
    f = cat({kJxlSig, kFtyp, jbox("jxlc", kCs), Bytes{0, 0, 0}});   // incomplete box header at the end
    s = jxl_bytes(f);
    CHECK(has_issue(s, code::truncated));
    // Size 0: the last box runs to the end of the file.
    f = cat({kJxlSig, kFtyp, Bytes{0, 0, 0, 0, 'j', 'x', 'l', 'c'}, kCs});
    s = jxl_bytes(f);
    CHECK(s.container_status == "valid");
    // Missing ftyp.
    s = jxl_bytes(cat({kJxlSig, jbox("jxlc", kCs)}));
    CHECK(has_issue(s, code::structure_error));
}

TEST_CASE("JXL scan: Exif box drops its 4-byte offset, xml is hashed") {
    const Bytes tiff = {'I', 'I', 42, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    Bytes exif = {0, 0, 0, 2, 0xAA, 0xBB};   // offset 2: two bytes before the TIFF header
    exif.insert(exif.end(), tiff.begin(), tiff.end());
    const auto s = jxl_bytes(cat({kJxlSig, kFtyp, jbox("Exif", exif), jbox("xml ", {'<', 'x', '/', '>'}), jbox("jxlc", kCs)}));
    CHECK(s.container_status == "valid");
    CHECK(s.exif_status == "present");
    CHECK(s.exif == tiff);
    CHECK(s.xmp_bytes.value() == 4);
    CHECK(s.xmp_sha256.size() == 64);
}

// ------------------------------------------------------------ stage 3: JXL colour equivalence (D12)
namespace {
FileReport jxl_enum(int space, int white, int prim, int tf, int intent = 1) {
    FileReport f;
    f.color.kind = "jxl_enum";
    f.color.key = "jxl_enum:space=" + std::to_string(space) + ";white=" + std::to_string(white) + ";primaries=" + std::to_string(prim) +
                  ";transfer=" + std::to_string(tf);
    f.color.signals = {"enum(" + f.color.key.substr(9) + ";intent=" + std::to_string(intent) + ")"};
    return f;
}
FileReport png_color(const std::string& kind, const std::string& key, std::vector<std::string> signals = {}) {
    FileReport f;
    f.color.kind = kind;
    f.color.key = key;
    f.color.signals = std::move(signals);
    return f;
}
}  // namespace

TEST_CASE("jxl_equivalence: enum against PNG cICP needs the same CICP codes") {
    const auto pq = jxl_enum(0, 1, 9, 16);
    CHECK(jxl_equivalence(pq, png_color("cicp", "cicp:9/16/0/1")).equal);
    CHECK(jxl_equivalence(png_color("cicp", "cicp:9/16/0/1"), pq).equal);     // either side
    CHECK_FALSE(jxl_equivalence(pq, png_color("cicp", "cicp:9/18/0/1")).equal);
    CHECK_FALSE(jxl_equivalence(pq, png_color("cicp", "cicp:9/16/0/0")).equal); // limited range
    CHECK_FALSE(jxl_equivalence(jxl_enum(0, 2, 9, 16), png_color("cicp", "cicp:9/16/0/1")).equal);  // not D65
    CHECK_FALSE(jxl_equivalence(jxl_enum(0, 1, 9, 17), png_color("cicp", "cicp:9/17/0/1")).equal); // DCI: not accepted
    CHECK(jxl_equivalence(jxl_enum(0, 1, 9, 18), png_color("cicp", "cicp:9/18/0/1")).equal);       // HLG
}

TEST_CASE("jxl_equivalence: sRGB enum against PNG sRGB records the intent only") {
    const auto e = jxl_equivalence(jxl_enum(0, 1, 1, 13, 1), png_color("srgb", "srgb", {"sRGB(intent 0)"}));
    CHECK(e.equal);
    CHECK_FALSE(e.note.empty());
    const auto same = jxl_equivalence(jxl_enum(0, 1, 1, 13, 0), png_color("srgb", "srgb", {"sRGB(intent 0)"}));
    CHECK(same.equal);
    CHECK(same.note.empty());
    CHECK(jxl_equivalence(jxl_enum(1, 1, 0, 13), png_color("srgb", "srgb", {"sRGB(intent 0)"})).equal);    // gray
    CHECK_FALSE(jxl_equivalence(jxl_enum(0, 1, 9, 13), png_color("srgb", "srgb", {"sRGB(intent 0)"})).equal);  // BT.2100 primaries
}

TEST_CASE("jxl_equivalence: synthesized ICC equals only an embedded, byte-identical ICC") {
    auto j = jxl_enum(0, 1, 9, 16);
    j.icc.status = "synthesized";
    j.icc.sha256 = std::string(64, 'a');
    auto o = png_color("icc", "icc:" + std::string(64, 'a'));
    o.icc.status = "embedded";
    o.icc.sha256 = std::string(64, 'a');
    CHECK(jxl_equivalence(j, o).equal);
    o.icc.sha256 = std::string(64, 'b');
    CHECK_FALSE(jxl_equivalence(j, o).equal);
    // Two JXL enums: not handled here (the plain key comparison applies).
    auto j2 = j;
    CHECK_FALSE(jxl_equivalence(j, j2).equal);
    // No JXL enum on either side.
    CHECK_FALSE(jxl_equivalence(o, o).equal);
}

TEST_CASE("file strings shown in reports are valid UTF-8") {
    const uint8_t cafe[] = {'C', 'a', 'f', 0xE9};
    CHECK(latin1_text(cafe, 4) == "Caf\xC3\xA9");
    const uint8_t ctl[] = {'a', 0x0A, 0x85};
    CHECK(latin1_text(ctl, 3) == "a\xEF\xBF\xBD\xEF\xBF\xBD");
    CHECK(file_text("2026:01:02 \xC3\xA9") == "2026:01:02 \xC3\xA9");          // valid UTF-8 kept
    CHECK(file_text("x\xE9y") == "x\xEF\xBF\xBDy");                              // lone Latin-1 byte
    CHECK(file_text("\xC0\xAF") == "\xEF\xBF\xBD\xEF\xBF\xBD");                  // overlong
    CHECK(file_text("\xED\xA0\x80") == "\xEF\xBF\xBD\xEF\xBF\xBD\xEF\xBF\xBD");  // surrogate
    CHECK(file_text("\xE4\xB8") == "\xEF\xBF\xBD\xEF\xBF\xBD");                  // cut sequence
    CHECK(file_text("a\nb") == "a\xEF\xBF\xBD" "b");
    CHECK(utf8_sanitize("a\nb", true) == "a\nb");
}

TEST_CASE("PSD PackBits rows must decode to exactly the row size") {
    std::string why;
    uint8_t out[8] = {};
    const uint8_t lit[] = {0x02, 'a', 'b', 'c', 0xFE, 'z'};   // literal 3 + repeat 3 ('z' x 3)
    REQUIRE(packbits_row(lit, sizeof lit, out, 6, why));
    CHECK(memcmp(out, "abczzz", 6) == 0);
    const uint8_t nop[] = {0x80, 0x00, 'q'};                  // -128 is a no-op
    CHECK(packbits_row(nop, sizeof nop, out, 1, why));
    CHECK_FALSE(packbits_row(lit, sizeof lit, out, 5, why));  // decodes past the row
    CHECK_FALSE(packbits_row(lit, sizeof lit, out, 7, why));  // decodes short of the row
    CHECK(why.find("6 of 7") != std::string::npos);
    const uint8_t cut[] = {0x05, 'a', 'b'};                   // literal run past the data
    CHECK_FALSE(packbits_row(cut, sizeof cut, out, 6, why));
    const uint8_t norep[] = {0xFD};                           // repeat without its byte
    CHECK_FALSE(packbits_row(norep, sizeof norep, out, 4, why));
}

TEST_CASE("PSD PackBits accepts valid non-optimal encodings (0.5.1, D33)") {
    std::string why;
    uint8_t out[8] = {};
    // ImageMagick: split literal + trailing no-op; 11 bytes for an 8-byte row (optimal maximum 9).
    const uint8_t im_tail[] = {0x04, 1, 2, 3, 4, 5, 0x02, 6, 7, 8, 0x80};
    REQUIRE(packbits_row(im_tail, sizeof im_tail, out, 8, why));
    CHECK(memcmp(out, "\x01\x02\x03\x04\x05\x06\x07\x08", 8) == 0);
    // No-ops at the start, in the middle and repeated.
    const uint8_t mid[] = {0x80, 0x01, 'a', 'b', 0x80, 0x80, 0xFF, 'c', 0x80};
    REQUIRE(packbits_row(mid, sizeof mid, out, 4, why));
    CHECK(memcmp(out, "abcc", 4) == 0);
    // Non-optimal: one-byte literals and two-byte repeats, twice the row size.
    const uint8_t slow[] = {0x00, 'x', 0x00, 'y', 0xFF, 'z', 0x00, 'w'};
    REQUIRE(packbits_row(slow, sizeof slow, out, 5, why));
    CHECK(memcmp(out, "xyzzw", 5) == 0);
    // Fed in pieces that split headers, literals and repeat bytes: same result.
    {
        PackBitsRow row(out, 8);
        for (uint8_t b : im_tail) REQUIRE(row.feed(&b, 1, why));
        CHECK(row.finish(why));
    }
    {
        memset(out, 0, sizeof out);
        PackBitsRow row(out, 4);
        REQUIRE(row.feed(mid, 3, why));
        REQUIRE(row.feed(mid + 3, 4, why));
        REQUIRE(row.feed(mid + 7, 2, why));
        CHECK(row.finish(why));
        CHECK(memcmp(out, "abcc", 4) == 0);
    }
}

TEST_CASE("PSD PackBits still rejects rows that overflow or do not match the byte count") {
    std::string why;
    uint8_t out[8] = {};
    // A complete row followed by a run (not a no-op) writes past the row.
    const uint8_t extra_lit[] = {0x01, 'a', 'b', 0x80, 0x00, 'c'};
    CHECK_FALSE(packbits_row(extra_lit, sizeof extra_lit, out, 2, why));
    CHECK(why.find("overflows the row") != std::string::npos);
    const uint8_t extra_rep[] = {0x01, 'a', 'b', 0xFF, 'c'};
    CHECK_FALSE(packbits_row(extra_rep, sizeof extra_rep, out, 2, why));
    CHECK(why.find("overflows the row") != std::string::npos);
    // Byte count shorter than the encoded row: the run is cut / the row is short.
    const uint8_t row4[] = {0x03, 'a', 'b', 'c', 'd'};
    CHECK_FALSE(packbits_row(row4, 3, out, 4, why));          // count 3 of 5
    CHECK(why.find("passes the end of the row data") != std::string::npos);
    const uint8_t two[] = {0x01, 'a', 'b', 0x01, 'c', 'd'};
    CHECK_FALSE(packbits_row(two, 3, out, 4, why));           // stops after 2 of 4 bytes
    CHECK(why.find("2 of 4") != std::string::npos);
    const uint8_t rep[] = {0xFD, 'q'};
    CHECK_FALSE(packbits_row(rep, 1, out, 4, why));           // repeat byte outside the count
    CHECK(why.find("without its byte") != std::string::npos);
    // Byte count longer than the encoded row: the next row's header is read as part of this row.
    const uint8_t next[] = {0x01, 'a', 'b', 0x01, 'c', 'd'};
    CHECK_FALSE(packbits_row(next, sizeof next, out, 2, why));
}

TEST_CASE("AVIF irot / imir map to EXIF orientation") {
    CHECK(avif_orientation(0, -1) == 1);
    CHECK(avif_orientation(1, -1) == 8);   // 90 degrees anticlockwise
    CHECK(avif_orientation(2, -1) == 3);
    CHECK(avif_orientation(3, -1) == 6);
    CHECK(avif_orientation(0, 0) == 4);    // libavif: axis 0 corresponds to EXIF 4
    CHECK(avif_orientation(0, 1) == 2);    // axis 1 corresponds to EXIF 2
    CHECK(avif_orientation(1, 0) == 5);    // transpose
    CHECK(avif_orientation(3, 0) == 7);    // transverse
    CHECK(avif_orientation(2, 0) == 2);
    CHECK(avif_orientation(2, 1) == 4);
}

TEST_CASE("resource limit suggestion: smallest rounded value whose usable bytes hold the estimate") {
    const uint64_t mib = 1ull << 20;
    // job_commit_mib: cap = value - 64 MiB, multiples of 256.
    CHECK(suggest_limit_mib(1932 * mib, 64 * mib, 256, 1u << 20).value() == 2048);   // 12000x8637 AVIF estimate
    CHECK(suggest_limit_mib(960 * mib, 64 * mib, 256, 1u << 20).value() == 1024);    // exactly the default cap
    CHECK(suggest_limit_mib(960 * mib + 1, 64 * mib, 256, 1u << 20).value() == 1280);
    CHECK_FALSE(suggest_limit_mib(uint64_t{1} << 41, 64 * mib, 256, 1u << 20).has_value());   // past the 1 TiB maximum
    CHECK_FALSE(suggest_limit_mib(UINT64_MAX, 64 * mib, 256, 1u << 20).has_value());          // overflow
    // buffer_mib: no reserve, multiples of 16.
    CHECK(suggest_limit_mib(70 * mib, 0, 16, 1u << 16).value() == 80);
    CHECK(suggest_limit_mib(1, 0, 16, 1u << 16).value() == 16);
}

// ------------------------------------------------------------ 1.0.0: iat-core API (CLI front end split)
TEST_CASE("core: version comes from CMake project(VERSION)") {
    CHECK(std::string(iat::version()) == IAT_EXPECTED_VERSION);
}

TEST_CASE("core: report targets and write_new_file never overwrite") {
    wchar_t tmp[MAX_PATH + 1];
    REQUIRE(GetTempPath2W(MAX_PATH + 1, tmp) > 0);
    const std::wstring dir = std::wstring(tmp) + L"iat-unit-core-" + iat::wide(iat::new_run_id());
    REQUIRE(CreateDirectoryW(dir.c_str(), nullptr));
    const std::wstring in = dir + L"\\in.png", rep = dir + L"\\r.json";
    std::string e;
    REQUIRE(iat::write_new_file(in, "input", "00000000-0000-4000-8000-000000000000", e));

    iat::RunRequest req;
    req.mode = "inspect";
    req.original = in;
    CHECK(iat::validate_report_targets(req, L"", rep).empty());
    CHECK(iat::validate_report_targets(req, rep, rep) == "report paths refer to the same file");
    CHECK(iat::validate_report_targets(req, L"", in).rfind("report path is an input file", 0) == 0);
    CHECK(iat::validate_report_targets(req, L"", dir + L"\\nodir\\r.json").rfind("report directory does not exist", 0) == 0);

    REQUIRE(iat::write_new_file(rep, "first", "11111111-0000-4000-8000-000000000000", e));
    CHECK(iat::validate_report_targets(req, L"", rep).rfind("report already exists", 0) == 0);
    CHECK_FALSE(iat::write_new_file(rep, "second", "22222222-0000-4000-8000-000000000000", e));
    CHECK(e.rfind("target appeared meanwhile, not overwritten", 0) == 0);
    HANDLE h = CreateFileW(rep.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, 0, nullptr);
    REQUIRE(h != INVALID_HANDLE_VALUE);
    char buf[16] = {};
    DWORD n = 0;
    ReadFile(h, buf, sizeof buf, &n, nullptr);
    CloseHandle(h);
    CHECK(std::string(buf, n) == "first");
    // The temporary file of the refused write is gone.
    CHECK(GetFileAttributesW((rep + L".iat-tmp-22222222").c_str()) == INVALID_FILE_ATTRIBUTES);

    DeleteFileW(rep.c_str());
    DeleteFileW(in.c_str());
    RemoveDirectoryW(dir.c_str());
}
