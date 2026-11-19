#include "tiff_scan.hpp"

#include "../common/checked.hpp"
#include "../common/text.hpp"

#include <algorithm>
#include <cstring>
#include <map>

namespace iat {

namespace {

constexpr uint64_t kMaxEntries = 4096;          // real IFDs have well under 100

uint32_t type_size(uint16_t t) {
    switch (t) {
        case 1: case 2: case 6: case 7: return 1;   // BYTE ASCII SBYTE UNDEFINED
        case 3: case 8: return 2;                    // SHORT SSHORT
        case 4: case 9: case 11: case 13: return 4;  // LONG SLONG FLOAT IFD
        case 5: case 10: case 12: case 16: case 17: case 18: return 8;  // RATIONAL SRATIONAL DOUBLE LONG8 SLONG8 IFD8
        default: return 0;
    }
}

struct Entry {
    uint16_t tag = 0, type = 0;
    uint64_t count = 0;
    uint64_t value_offset = 0;   // file offset of the value bytes (inline or not)
    uint64_t bytes = 0;          // count * type size
};

class Reader {
public:
    Reader(const ReadOnlyFile& f, uint64_t size, TiffScan& s) : f_(f), size_(size), s_(s) {}

    bool bytes(uint64_t off, void* dst, size_t n) {
        const auto end = checked_add(off, n);
        if (!end || *end > size_) return false;
        auto got = f_.read_at(off, dst, n);
        if (!got || *got != n) return false;
        s_.bytes_scanned += n;
        return true;
    }
    uint64_t u(const uint8_t* p, int n) const {
        uint64_t v = 0;
        for (int i = 0; i < n; ++i) {
            const int k = s_.little_endian ? n - 1 - i : i;
            v = (v << 8) | p[k];
        }
        return v;
    }
    // One element of an integer array value (SHORT, LONG, LONG8, BYTE, IFD/IFD8).
    std::optional<uint64_t> element(const Entry& e, uint64_t i) {
        const uint32_t ts = type_size(e.type);
        if (i >= e.count || !(e.type == 1 || e.type == 3 || e.type == 4 || e.type == 16 || e.type == 13 || e.type == 18))
            return std::nullopt;
        uint8_t b[8];
        if (!bytes(e.value_offset + i * ts, b, ts)) return std::nullopt;
        return u(b, static_cast<int>(ts));
    }
    std::optional<uint64_t> scalar(const Entry& e) { return e.count >= 1 ? element(e, 0) : std::nullopt; }

private:
    const ReadOnlyFile& f_;
    uint64_t size_;
    TiffScan& s_;
};

void issue(TiffScan& s, const std::string& code, const std::string& sev, const std::string& msg) {
    s.issues.push_back(Issue{code, sev, "", msg});
}

void corrupt(TiffScan& s, const std::string& code, const std::string& msg) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}

std::string short_hex(const std::string& h) { return h.size() > 16 ? h.substr(0, 16) : h; }

}  // namespace

std::string tiff_compression_name(uint16_t c) {
    switch (c) {
        case 1: return "none";
        case 2: return "CCITT RLE";
        case 3: return "CCITT G3";
        case 4: return "CCITT G4";
        case 5: return "LZW";
        case 6: return "old-style JPEG";
        case 7: return "JPEG";
        case 8: return "Deflate";
        case 32946: return "Deflate (legacy code)";
        case 32773: return "PackBits";
        case 34925: return "LZMA";
        case 50000: return "ZSTD";
        case 50001: return "WebP";
        case 50002: return "JPEG XL";
        case 34887: return "LERC";
        default: return "code " + std::to_string(c);
    }
}

TiffScan scan_tiff(const ReadOnlyFile& f, uint64_t file_size, const Limits& lim) {
    TiffScan s;
    Reader rd(f, file_size, s);
    uint8_t h[16] = {};
    if (!rd.bytes(0, h, 8)) { corrupt(s, code::truncated, "file shorter than a TIFF header"); return s; }
    if (h[0] == 'I' && h[1] == 'I') s.little_endian = true;
    else if (h[0] == 'M' && h[1] == 'M') s.little_endian = false;
    else { corrupt(s, code::structure_error, "bad byte-order mark"); return s; }
    const uint64_t magic = rd.u(h + 2, 2);
    if (magic == 42) {
        s.ifd_offset = rd.u(h + 4, 4);
    } else if (magic == 43) {
        s.big = true;
        if (!rd.bytes(8, h + 8, 8)) { corrupt(s, code::truncated, "file shorter than a BigTIFF header"); return s; }
        if (rd.u(h + 4, 2) != 8 || rd.u(h + 6, 2) != 0) { corrupt(s, code::structure_error, "BigTIFF offset size must be 8"); return s; }
        s.ifd_offset = rd.u(h + 8, 8);
    } else {
        corrupt(s, code::structure_error, "bad TIFF magic " + std::to_string(magic));
        return s;
    }
    s.header_ok = true;

    // ---- first IFD
    const int cnt_size = s.big ? 8 : 2, ent_size = s.big ? 20 : 12, off_size = s.big ? 8 : 4;
    uint8_t cb[8];
    if (s.ifd_offset < (s.big ? 16u : 8u) || !rd.bytes(s.ifd_offset, cb, cnt_size)) {
        corrupt(s, code::structure_error, "first IFD offset " + std::to_string(s.ifd_offset) + " is outside the file");
        return s;
    }
    const uint64_t n = rd.u(cb, cnt_size);
    if (n == 0 || n > kMaxEntries) { corrupt(s, code::structure_error, "IFD entry count " + std::to_string(n) + " is not plausible"); return s; }
    std::vector<uint8_t> raw(static_cast<size_t>(n * ent_size + off_size));
    if (!rd.bytes(s.ifd_offset + cnt_size, raw.data(), raw.size())) {
        corrupt(s, code::truncated, "first IFD runs past the end of the file");
        return s;
    }
    std::map<uint16_t, Entry> tags;
    uint16_t prev = 0;
    for (uint64_t i = 0; i < n; ++i) {
        const uint8_t* p = raw.data() + i * ent_size;
        Entry e;
        e.tag = static_cast<uint16_t>(rd.u(p, 2));
        e.type = static_cast<uint16_t>(rd.u(p + 2, 2));
        e.count = rd.u(p + 4, s.big ? 8 : 4);
        const uint32_t ts = type_size(e.type);
        s.tag_list.push_back(std::to_string(e.tag));
        if (i > 0 && e.tag <= prev) issue(s, "tiff_tag_order", "warning", "tag " + std::to_string(e.tag) + " is out of ascending order or repeated");
        prev = e.tag;
        if (ts == 0) { issue(s, "tiff_unknown_type", "warning", "tag " + std::to_string(e.tag) + " has unknown type " + std::to_string(e.type)); continue; }
        const auto bytes = checked_mul(e.count, ts);
        if (!bytes) { corrupt(s, code::structure_error, "tag " + std::to_string(e.tag) + " value size overflows"); return s; }
        e.bytes = *bytes;
        const uint64_t inline_pos = s.ifd_offset + cnt_size + i * ent_size + (s.big ? 12 : 8);
        if (e.bytes <= static_cast<uint64_t>(off_size)) {
            e.value_offset = inline_pos;
        } else {
            e.value_offset = rd.u(p + (s.big ? 12 : 8), off_size);
            const auto end = checked_add(e.value_offset, e.bytes);
            if (!end || *end > file_size) {
                corrupt(s, code::structure_error, "tag " + std::to_string(e.tag) + " value lies outside the file");
                return s;
            }
        }
        if (!tags.count(e.tag)) tags[e.tag] = e;
    }
    s.next_ifd = rd.u(raw.data() + n * ent_size, off_size);
    s.has_ifd = true;

    auto get = [&](uint16_t tag) -> const Entry* {
        auto it = tags.find(tag);
        return it == tags.end() ? nullptr : &it->second;
    };
    auto scalar = [&](uint16_t tag, uint64_t dflt) -> uint64_t {
        const Entry* e = get(tag);
        if (!e) return dflt;
        return rd.scalar(*e).value_or(dflt);
    };

    s.width = static_cast<uint32_t>(std::min<uint64_t>(scalar(256, 0), UINT32_MAX));
    s.height = static_cast<uint32_t>(std::min<uint64_t>(scalar(257, 0), UINT32_MAX));
    s.spp = static_cast<uint16_t>(scalar(277, 1));
    s.compression = static_cast<uint16_t>(scalar(259, 1));
    s.photometric = static_cast<uint16_t>(scalar(262, 0xFFFF));
    s.planar = static_cast<uint16_t>(scalar(284, 1));
    s.sample_format = static_cast<uint16_t>(scalar(339, 1));
    s.predictor = static_cast<uint16_t>(scalar(317, 1));
    s.fill_order = static_cast<uint16_t>(scalar(266, 1));
    if (const Entry* e = get(274)) s.orientation = static_cast<uint16_t>(rd.scalar(*e).value_or(0));
    if (const Entry* e = get(258)) {
        s.bits = static_cast<uint16_t>(rd.element(*e, 0).value_or(0));
        for (uint64_t i = 1; i < e->count && i < 64; ++i)
            if (rd.element(*e, i).value_or(0) != s.bits) s.bits = 0;  // channels differ
    } else {
        s.bits = 1;  // TIFF default
    }
    if (const Entry* e = get(338))
        for (uint64_t i = 0; i < e->count && i < 64; ++i) s.extra_samples.push_back(static_cast<uint16_t>(rd.element(*e, i).value_or(0)));
    s.subifds = get(330) != nullptr;
    if (s.width == 0 || s.height == 0) { corrupt(s, code::structure_error, "ImageWidth/ImageLength missing or zero"); return s; }

    // ---- strips / tiles: count must match the geometry, every block must lie inside the file
    const Entry *offs = nullptr, *cnts = nullptr;
    uint64_t expected = 0;
    s.tiled = get(322) || get(323) || get(324);
    if (s.tiled) {
        s.tile_width = static_cast<uint32_t>(scalar(322, 0));
        s.tile_length = static_cast<uint32_t>(scalar(323, 0));
        offs = get(324);
        cnts = get(325);
        if (s.tile_width == 0 || s.tile_length == 0 || s.tile_width % 16 || s.tile_length % 16) {
            corrupt(s, code::structure_error, "tile size " + std::to_string(s.tile_width) + "x" + std::to_string(s.tile_length) +
                                                  " is invalid (must be a non-zero multiple of 16)");
            return s;
        }
        expected = (static_cast<uint64_t>(s.width) + s.tile_width - 1) / s.tile_width *
                   ((static_cast<uint64_t>(s.height) + s.tile_length - 1) / s.tile_length);
    } else {
        const uint64_t rps = scalar(278, UINT32_MAX);
        s.rows_per_strip = static_cast<uint32_t>(std::min<uint64_t>(rps == 0 ? UINT32_MAX : rps, UINT32_MAX));
        offs = get(273);
        cnts = get(279);
        const uint64_t eff = std::min<uint64_t>(s.rows_per_strip, s.height);
        expected = (s.height + eff - 1) / eff;
    }
    if (s.planar == 2) expected *= s.spp;
    if (!offs || !cnts) { corrupt(s, code::structure_error, "strip/tile offsets or byte counts are missing"); return s; }
    if (offs->count != expected || cnts->count != expected) {
        corrupt(s, code::structure_error, "IFD lists " + std::to_string(offs->count) + " offsets and " + std::to_string(cnts->count) +
                                              " byte counts, geometry needs " + std::to_string(expected));
        return s;
    }
    s.blocks = expected;
    for (uint64_t i = 0; i < expected; ++i) {
        const auto o = rd.element(*offs, i), c = rd.element(*cnts, i);
        if (!o || !c) { corrupt(s, code::structure_error, "strip/tile table entry " + std::to_string(i) + " is unreadable"); return s; }
        if (*c == 0) { corrupt(s, code::structure_error, "block " + std::to_string(i) + " has byte count 0 (missing image data)"); return s; }
        const auto end = checked_add(*o, *c);
        if (!end || *end > file_size) {
            corrupt(s, *o < file_size ? code::truncated : code::structure_error,
                    "block " + std::to_string(i) + " at offset " + std::to_string(*o) + " (+" + std::to_string(*c) +
                        " bytes) lies outside the file of " + std::to_string(file_size) + " bytes");
            return s;
        }
        s.data_end = std::max(s.data_end, *end);
    }

    // ---- further images
    if (s.next_ifd != 0) {
        uint8_t nb[8];
        if (!rd.bytes(s.next_ifd, nb, cnt_size)) {
            corrupt(s, code::structure_error, "next IFD offset " + std::to_string(s.next_ifd) + " is outside the file");
            return s;
        }
    }

    // ---- colour: ICC (34675) and colorimetry tags
    if (const Entry* e = get(34675)) {
        if (e->bytes > lim.metadata_item_bytes()) {
            s.icc.status = "over_budget";
            s.icc.bytes = Count{e->bytes};
            s.metadata_status = "over_limit";
            issue(s, code::metadata_over_limit, "warning", "ICC profile of " + std::to_string(e->bytes) + " bytes exceeds metadata_item_mib");
        } else {
            std::vector<uint8_t> icc(static_cast<size_t>(e->bytes));
            if (rd.bytes(e->value_offset, icc.data(), icc.size())) {
                s.icc.status = "embedded";
                s.icc.bytes = Count{e->bytes};
                s.icc.sha256 = sha256_hex(icc.data(), icc.size());
                s.icc.decoder_accepted = true;
            } else {
                s.icc.status = "unreadable";
            }
        }
    }
    if (get(318)) s.colorimetry.push_back("WhitePoint");
    if (get(319)) s.colorimetry.push_back("PrimaryChromaticities");
    if (get(301)) s.colorimetry.push_back("TransferFunction");

    // ---- metadata (hashes and sizes only; values are not copied into the report)
    auto blob_hash = [&](const Entry& e, std::string& status) -> std::string {
        if (e.bytes > lim.metadata_item_bytes()) { status = "over_limit"; return "bytes=" + std::to_string(e.bytes) + " (over limit)"; }
        std::vector<uint8_t> b(static_cast<size_t>(e.bytes));
        if (!rd.bytes(e.value_offset, b.data(), b.size())) { status = "parse_failed"; return "unreadable"; }
        return "bytes=" + std::to_string(e.bytes) + " sha256=" + short_hex(sha256_hex(b.data(), b.size()));
    };
    if (const Entry* e = get(700)) s.xmp = blob_hash(*e, s.xmp_status);
    if (const Entry* e = get(34665)) {
        const auto off = rd.scalar(*e);
        uint8_t eb[8];
        if (off && rd.bytes(*off, eb, cnt_size)) {
            const uint64_t en = rd.u(eb, cnt_size);
            const auto len = checked_mul(en, ent_size);
            std::vector<uint8_t> body;
            if (en <= kMaxEntries && len) body.resize(static_cast<size_t>(*len));
            if (!body.empty() && rd.bytes(*off + cnt_size, body.data(), body.size()))
                s.exif = "exif_ifd entries=" + std::to_string(en) + " sha256=" + short_hex(sha256_hex(body.data(), body.size()));
            else { s.exif = "exif_ifd unreadable"; s.exif_status = "parse_failed"; }
        } else {
            s.exif = "exif_ifd offset outside the file";
            s.exif_status = "parse_failed";
        }
    }
    // Tag tables are aggregates (not std::pair): brace-initialising uint16_t from these
    // constants is checked for narrowing at compile time, so an out-of-range tag cannot compile.
    struct NamedTag {
        uint16_t tag;
        const char* name;
    };
    {
        static const NamedTag text_tags[] = {{270, "ImageDescription"}, {271, "Make"}, {272, "Model"},
                                             {305, "Software"}, {315, "Artist"}, {33432, "Copyright"}};
        std::string keys;
        Sha256 hs;
        bool any = false;
        for (const auto& [tag, name] : text_tags) {
            const Entry* e = get(tag);
            if (!e) continue;
            any = true;
            keys += (keys.empty() ? "" : ",") + std::string(name);
            std::string st = "present";
            const std::string hsum = blob_hash(*e, st);
            if (st != "present") s.text_status = st;
            hs.update(name, strlen(name));
            hs.update(hsum.data(), hsum.size());
        }
        if (any) s.text = "keys=[" + keys + "] values_sha256=" + short_hex(hs.finish_hex());
    }
    if (const Entry *xr = get(282), *yr = get(283); xr && yr && xr->type == 5 && yr->type == 5) {
        uint8_t xb[8], yb[8];
        if (rd.bytes(xr->value_offset, xb, 8) && rd.bytes(yr->value_offset, yb, 8))
            s.resolution = std::to_string(rd.u(xb, 4)) + "/" + std::to_string(rd.u(xb + 4, 4)) + " x " +
                           std::to_string(rd.u(yb, 4)) + "/" + std::to_string(rd.u(yb + 4, 4)) + " unit=" + std::to_string(scalar(296, 2));
    }
    if (const Entry* e = get(306); e && e->type == 2 && e->bytes <= 64) {
        std::string t(static_cast<size_t>(e->bytes), '\0');
        if (rd.bytes(e->value_offset, t.data(), t.size())) {
            t.erase(std::find(t.begin(), t.end(), '\0'), t.end());
            s.datetime = file_text(t);   // ASCII by specification; anything else is shown as U+FFFD
        }
    }
    static const NamedTag other[] = {{33723, "IPTC"}, {34377, "Photoshop"}, {34853, "GPS IFD"},
                                     {37724, "ImageSourceData"}, {50706, "DNGVersion"}};
    for (const auto& [tag, name] : other)
        if (get(tag)) s.other_tags.push_back(name);
    for (const std::string* st : {&s.exif_status, &s.xmp_status, &s.text_status})
        if (*st == "parse_failed") s.metadata_status = "parse_failed";
        else if (*st == "over_limit" && s.metadata_status == "valid") s.metadata_status = "over_limit";

    if (s.container_status == "unverified")
        s.container_status = std::any_of(s.issues.begin(), s.issues.end(), [](const Issue& i) { return i.severity == "warning"; })
                                 ? "warning" : "valid";
    return s;
}

}  // namespace iat
