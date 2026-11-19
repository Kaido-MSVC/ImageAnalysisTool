#include "psd_scan.hpp"

#include "../common/checked.hpp"

#include <algorithm>
#include <cstring>
#include <set>

namespace iat {

namespace {

void issue(PsdScan& s, const std::string& code, const std::string& sev, const std::string& msg) {
    s.issues.push_back(Issue{code, sev, "", msg});
}
void corrupt(PsdScan& s, const std::string& code, const std::string& msg) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}
void warn(PsdScan& s, const std::string& msg) {
    if (s.container_status == "valid") s.container_status = "warning";
    issue(s, "psd_container_warning", "warning", msg);
}

uint16_t be16(const uint8_t* p) { return static_cast<uint16_t>((p[0] << 8) | p[1]); }
uint32_t be32(const uint8_t* p) {
    return (static_cast<uint32_t>(p[0]) << 24) | (static_cast<uint32_t>(p[1]) << 16) | (static_cast<uint32_t>(p[2]) << 8) | p[3];
}
uint64_t be64(const uint8_t* p) { return (static_cast<uint64_t>(be32(p)) << 32) | be32(p + 4); }

class Reader {
public:
    Reader(const ReadOnlyFile& f, uint64_t size, PsdScan& s) : f_(f), size_(size), s_(s) {}
    uint64_t size() const { return size_; }
    bool bytes(uint64_t off, void* dst, size_t n) {
        const auto end = checked_add(off, n);
        if (!end || *end > size_) return false;
        auto got = f_.read_at(off, dst, n);
        if (!got || *got != n) return false;
        s_.bytes_scanned += n;
        return true;
    }
    bool payload(uint64_t off, uint64_t n, std::vector<uint8_t>& out) {
        out.resize(static_cast<size_t>(n));
        return n == 0 || bytes(off, out.data(), out.size());
    }
    bool hash(uint64_t off, uint64_t n, std::string& hex) {
        Sha256 h;
        std::vector<uint8_t> buf(65536);
        while (n > 0) {
            const size_t take = static_cast<size_t>(n < buf.size() ? n : buf.size());
            if (!bytes(off, buf.data(), take)) return false;
            h.update(buf.data(), take);
            off += take;
            n -= take;
        }
        hex = h.finish_hex();
        return true;
    }
    // Bounds-checked length field (32 or 64 bits) at off.
    std::optional<uint64_t> length(uint64_t off, bool wide) {
        uint8_t b[8];
        if (!bytes(off, b, wide ? 8 : 4)) return std::nullopt;
        return wide ? be64(b) : be32(b);
    }

private:
    const ReadOnlyFile& f_;
    uint64_t size_;
    PsdScan& s_;
};

std::string fixed_text(uint32_t v) {   // 16.16 fixed point, 3 decimals
    const uint64_t milli = (static_cast<uint64_t>(v) * 1000 + 32768) >> 16;
    char b[48];
    snprintf(b, sizeof b, "%llu.%03llu", static_cast<unsigned long long>(milli / 1000), static_cast<unsigned long long>(milli % 1000));
    return b;
}

bool depth_valid(uint16_t mode, uint16_t depth) {
    switch (mode) {
        case 0: return depth == 1;
        case 2: return depth == 8;
        case 8: return depth == 8 || depth == 16;
        case 1: case 3: case 4: case 7: case 9: return depth == 8 || depth == 16 || depth == 32;
        default: return false;
    }
}

uint16_t min_channels(uint16_t mode) {
    switch (mode) {
        case 3: case 9: return 3;
        case 4: return 4;
        default: return 1;
    }
}

// Image resources (section 2). Returns false when the section is structurally broken.
bool scan_resources(Reader& rd, PsdScan& s, uint64_t start, uint64_t len, const Limits& lim) {
    const uint64_t end = start + len;
    uint64_t pos = start, meta_total = 0;
    auto over_meta = [&](uint64_t n) {
        const auto t = checked_add(meta_total, n);
        if (!t || n > lim.metadata_item_bytes() || *t > lim.metadata_file_bytes()) return true;
        meta_total = *t;
        return false;
    };
    while (pos < end) {
        uint8_t h[7];
        if (end - pos < 12 || !rd.bytes(pos, h, 7)) {
            corrupt(s, code::structure_error, "image resource header at offset " + std::to_string(pos) + " runs past the resource section");
            return false;
        }
        if (memcmp(h, "8BIM", 4) && memcmp(h, "MeSa", 4) && memcmp(h, "AgHg", 4) && memcmp(h, "PHUT", 4) && memcmp(h, "DCSR", 4)) {
            corrupt(s, code::structure_error, "image resource at offset " + std::to_string(pos) + " has an unknown signature");
            return false;
        }
        const uint16_t id = be16(h + 4);
        const uint64_t name_field = (static_cast<uint64_t>(h[6]) + 2) & ~1ull;   // length byte + name, padded to even
        const uint64_t size_at = pos + 6 + name_field;
        uint8_t sz[4];
        if (size_at + 4 > end || !rd.bytes(size_at, sz, 4)) {
            corrupt(s, code::structure_error, "image resource 0x" + std::to_string(id) + " name runs past the resource section");
            return false;
        }
        const uint64_t size = be32(sz), data = size_at + 4;
        const uint64_t next = data + size + (size & 1);
        if (data + size > end) {
            corrupt(s, code::structure_error, "image resource " + std::to_string(id) + " (" + std::to_string(size) + " bytes) runs past the resource section");
            return false;
        }
        ++s.resource_count;
        uint8_t b[16] = {};
        switch (id) {
            case 0x0421:   // version info: u32 version, u8 hasRealMergedData, ...
                if (size >= 5 && rd.bytes(data, b, 5)) {
                    s.version_info = true;
                    s.has_real_merged = b[4] != 0;
                } else warn(s, "version info resource (0x0421) is shorter than 5 bytes");
                break;
            case 0x040F:   // ICC profile
                if (over_meta(size)) s.icc_status = "over_budget";
                else if (rd.payload(data, size, s.icc)) s.icc_status = "embedded";
                break;
            case 0x0411:
                if (size >= 1 && rd.bytes(data, b, 1)) s.icc_untagged = b[0] == 1;
                break;
            case 0x0416:
                if (size >= 2 && rd.bytes(data, b, 2)) s.palette_count = be16(b);
                break;
            case 0x0417:
                if (size >= 2 && rd.bytes(data, b, 2)) s.transparent_index = be16(b);
                break;
            case 0x0422:   // EXIF data 1 (raw TIFF)
                if (over_meta(size)) s.exif_status = "over_limit";
                else if (rd.payload(data, size, s.exif)) s.exif_status = "present";
                break;
            case 0x0424:   // XMP
                if (rd.hash(data, size, s.xmp_sha256)) s.xmp_bytes = size;
                break;
            case 0x0404:   // IPTC-NAA
                if (rd.hash(data, size, s.iptc_sha256)) s.iptc_bytes = size;
                break;
            case 0x03ED:   // ResolutionInfo
                if (size >= 16 && rd.bytes(data, b, 16))
                    s.resolution = "h=" + fixed_text(be32(b)) + " unit=" + std::to_string(be16(b + 4)) + " v=" + fixed_text(be32(b + 8)) +
                                   " unit=" + std::to_string(be16(b + 12));
                break;
            default: break;
        }
        pos = next;
    }
    if (s.icc_status == "over_budget" || s.exif_status == "over_limit") {
        s.metadata_status = "over_limit";
        issue(s, code::metadata_over_limit, "warning", "an image resource exceeds the metadata budget (ICC or EXIF not read)");
    }
    return true;
}

// Layer and mask information (section 4): only counts are read. Layer records are skipped.
void scan_layers(Reader& rd, PsdScan& s, uint64_t start, uint64_t len) {
    const bool wide = s.version == 2;
    if (len == 0) return;
    const uint64_t end = start + len;
    uint64_t pos = start;
    auto li_len = rd.length(pos, wide);
    if (!li_len || *li_len > end - pos - (wide ? 8 : 4)) { s.layers_known = false; return; }
    pos += wide ? 8 : 4;
    if (*li_len >= 2) {
        uint8_t b[2];
        if (!rd.bytes(pos, b, 2)) { s.layers_known = false; return; }
        s.layer_count_field = static_cast<int16_t>(be16(b));
        s.layer_count = static_cast<uint32_t>(s.layer_count_field < 0 ? -s.layer_count_field : s.layer_count_field);
    }
    pos += *li_len;
    // Global layer mask info: u32 length + data.
    if (pos == end) return;
    auto gm = rd.length(pos, false);
    if (!gm || *gm > end - pos - 4) { s.layers_known = false; return; }
    pos += 4 + *gm;
    // Tagged blocks: 16/32-bit documents keep their layers in Lr16 / Lr32 (and Layr) here.
    static const std::set<std::string> wide_keys = {"LMsk", "Lr16", "Lr32", "Layr", "Mt16", "Mt32", "Mtrn",
                                                     "Alph", "FMsk", "lnk2", "FEid", "FXid", "PxSD"};
    while (end - pos >= 12) {
        uint8_t h[8];
        if (!rd.bytes(pos, h, 8)) { s.layers_known = false; return; }
        if (memcmp(h, "8BIM", 4) && memcmp(h, "8B64", 4)) {
            // Writers pad the section; padding is all zero.
            uint8_t z[4] = {};
            if (!memcmp(h, z, 4)) return;
            s.layers_known = false;
            return;
        }
        const std::string key(reinterpret_cast<const char*>(h + 4), 4);
        const bool w8 = wide && wide_keys.count(key);
        auto bl = rd.length(pos + 8, w8);
        const uint64_t data = pos + 8 + (w8 ? 8 : 4);
        if (!bl || data > end || *bl > end - data) { s.layers_known = false; return; }
        if ((key == "Lr16" || key == "Lr32" || key == "Layr") && *bl >= 2) {
            uint8_t b[2];
            if (rd.bytes(data, b, 2)) {
                const int16_t n = static_cast<int16_t>(be16(b));
                s.layer_count += static_cast<uint32_t>(n < 0 ? -n : n);
                if (n < 0 && s.layer_count_field == 0) s.layer_count_field = n;
            }
        }
        uint64_t next = data + *bl;
        if (next & 1) ++next;
        pos = next;
    }
}

}  // namespace

bool PackBitsRow::feed(const uint8_t* in, size_t n, std::string& why) {
    size_t i = 0;
    while (i < n) {
        if (literal_left_) {
            const size_t k = std::min(literal_left_, n - i);
            memcpy(out_ + o_, in + i, k);
            i += k;
            o_ += k;
            literal_left_ -= k;
            continue;
        }
        if (repeat_count_) {
            memset(out_ + o_, in[i++], repeat_count_);
            o_ += repeat_count_;
            repeat_count_ = 0;
            continue;
        }
        const int8_t h = static_cast<int8_t>(in[i++]);
        if (h >= 0) {
            const size_t k = static_cast<size_t>(h) + 1;
            if (k > size_ - o_) { why = "literal run overflows the row"; return false; }
            literal_left_ = k;
        } else if (h != -128) {   // -128 is a no-op header
            const size_t k = static_cast<size_t>(1 - h);
            if (k > size_ - o_) { why = "repeat run overflows the row"; return false; }
            repeat_count_ = k;
        }
    }
    return true;
}

bool PackBitsRow::finish(std::string& why) {
    if (literal_left_) { why = "literal run passes the end of the row data"; return false; }
    if (repeat_count_) { why = "repeat run without its byte"; return false; }
    if (o_ != size_) { why = "row decodes to " + std::to_string(o_) + " of " + std::to_string(size_) + " bytes"; return false; }
    return true;
}

bool packbits_row(const uint8_t* in, size_t in_size, uint8_t* out, size_t out_size, std::string& why) {
    PackBitsRow row(out, out_size);
    return row.feed(in, in_size, why) && row.finish(why);
}

PsdScan scan_psd(const ReadOnlyFile& f, uint64_t file_size, const Limits& lim) {
    PsdScan s;
    Reader rd(f, file_size, s);
    uint8_t h[26];
    if (!rd.bytes(0, h, 26)) { corrupt(s, code::truncated, "file shorter than the 26-byte PSD header"); return s; }
    if (memcmp(h, "8BPS", 4)) { corrupt(s, code::structure_error, "missing 8BPS signature"); return s; }
    s.version = be16(h + 4);
    if (s.version != 1 && s.version != 2) {
        corrupt(s, code::structure_error, "PSD version " + std::to_string(s.version) + " (expected 1 = PSD or 2 = PSB)");
        return s;
    }
    s.container_status = "valid";
    s.sections.push_back(s.version == 2 ? "header(PSB)" : "header(PSD)");
    for (int i = 6; i < 12; ++i)
        if (h[i]) { warn(s, "reserved header bytes are not zero"); break; }
    s.channels = be16(h + 12);
    s.height = be32(h + 14);
    s.width = be32(h + 18);
    s.depth = be16(h + 22);
    s.mode = be16(h + 24);
    const uint32_t max_side = s.version == 2 ? 300000u : 30000u;
    if (s.width == 0 || s.height == 0 || s.width > max_side || s.height > max_side) {
        corrupt(s, code::structure_error, std::to_string(s.width) + "x" + std::to_string(s.height) + " is outside 1.." +
                                              std::to_string(max_side) + " (" + (s.version == 2 ? "PSB" : "PSD") + " limit)");
        return s;
    }
    if (s.channels < 1 || s.channels > 56) { corrupt(s, code::structure_error, std::to_string(s.channels) + " channels (1..56 allowed)"); return s; }
    if (!depth_valid(s.mode, s.depth)) {
        corrupt(s, code::structure_error, "colour mode " + std::to_string(s.mode) + " with depth " + std::to_string(s.depth) + " is not defined");
        return s;
    }
    if (s.channels < min_channels(s.mode)) {
        corrupt(s, code::structure_error, "colour mode " + std::to_string(s.mode) + " needs at least " + std::to_string(min_channels(s.mode)) + " channels");
        return s;
    }
    s.header_ok = true;
    const auto bits = checked_mul(s.width, s.depth);
    s.row_bytes = bits ? (*bits + 7) / 8 : 0;
    const bool wide = s.version == 2;

    // ---- colour mode data
    uint64_t pos = 26;
    auto cm = rd.length(pos, false);
    if (!cm) { corrupt(s, code::truncated, "colour mode data length missing"); return s; }
    pos += 4;
    if (*cm > file_size - pos) { corrupt(s, code::truncated, "colour mode data (" + std::to_string(*cm) + " bytes) runs past the end of the file"); return s; }
    if (s.mode == 2) {
        if (*cm != 768) { corrupt(s, code::structure_error, "indexed colour needs 768 bytes of palette, found " + std::to_string(*cm)); return s; }
        if (!rd.payload(pos, 768, s.palette)) { corrupt(s, code::truncated, "palette could not be read"); return s; }
    }
    pos += *cm;
    s.sections.push_back("color_mode_data");

    // ---- image resources
    auto rl = rd.length(pos, false);
    if (!rl) { corrupt(s, code::truncated, "image resource section length missing"); return s; }
    pos += 4;
    if (*rl > file_size - pos) { corrupt(s, code::truncated, "image resource section (" + std::to_string(*rl) + " bytes) runs past the end of the file"); return s; }
    if (!scan_resources(rd, s, pos, *rl, lim)) return s;
    pos += *rl;
    s.sections.push_back("image_resources");

    // ---- layer and mask information (only lengths and counts)
    auto lm = rd.length(pos, wide);
    if (!lm) { corrupt(s, code::truncated, "layer and mask section length missing"); return s; }
    pos += wide ? 8 : 4;
    if (*lm > file_size - pos) {
        corrupt(s, code::truncated, "layer and mask section (" + std::to_string(*lm) + " bytes) runs past the end of the file");
        return s;
    }
    s.layer_section_bytes = *lm;
    scan_layers(rd, s, pos, *lm);
    pos += *lm;
    s.sections.push_back("layer_and_mask");

    // ---- image data
    s.image_data_offset = pos;
    uint8_t c[2];
    if (!rd.bytes(pos, c, 2)) { corrupt(s, code::truncated, "image data section is missing (file ends after the layer section)"); return s; }
    s.compression = be16(c);
    pos += 2;
    s.sections.push_back("image_data");
    const auto plane_rows = checked_mul(s.channels, s.height);
    if (s.compression == 0) {
        const auto total = plane_rows ? checked_mul(*plane_rows, s.row_bytes) : std::nullopt;
        if (!total) { corrupt(s, code::structure_error, "raw image data size overflows 64-bit arithmetic"); return s; }
        s.data_offset = pos;
        s.data_bytes = *total;
    } else if (s.compression == 1) {
        const uint64_t entry = wide ? 4 : 2;
        const auto table = plane_rows ? checked_mul(*plane_rows, entry) : std::nullopt;
        if (!table || *table > file_size - pos) { corrupt(s, code::truncated, "RLE row byte-count table runs past the end of the file"); return s; }
        s.rle_table_offset = pos;
        // Sum the counts in 1 MiB chunks (the table can be tens of MB for a large PSB).
        std::vector<uint8_t> buf(1 << 20);
        uint64_t sum = 0, off = pos, left = *table;
        while (left) {
            const size_t take = static_cast<size_t>(std::min<uint64_t>(left, buf.size()));
            if (!rd.bytes(off, buf.data(), take)) { corrupt(s, code::truncated, "RLE row byte-count table could not be read"); return s; }
            for (size_t i = 0; i < take; i += entry) sum += wide ? be32(buf.data() + i) : be16(buf.data() + i);
            off += take;
            left -= take;
        }
        s.data_offset = pos + *table;
        s.data_bytes = sum;
    } else if (s.compression == 2 || s.compression == 3) {
        // ZIP streams cannot be bounded without inflating; the image data is not checked.
        s.data_offset = pos;
        return s;
    } else {
        corrupt(s, code::structure_error, "image data compression " + std::to_string(s.compression) + " is not defined");
        return s;
    }
    const auto data_end = checked_add(s.data_offset, s.data_bytes);
    if (!data_end || *data_end > file_size) {
        corrupt(s, code::truncated, "image data needs " + std::to_string(s.data_bytes) + " bytes from offset " +
                                        std::to_string(s.data_offset) + ", file has " + std::to_string(file_size));
        return s;
    }
    s.trailing_bytes = file_size - *data_end;
    if (*s.trailing_bytes) {
        if (s.container_status == "valid") s.container_status = "warning";
        issue(s, code::trailing_data, "warning", std::to_string(*s.trailing_bytes) + " bytes after the image data");
    }
    return s;
}

}  // namespace iat
