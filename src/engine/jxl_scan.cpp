#include "jxl_scan.hpp"

#include "../common/checked.hpp"

#include <cstring>

namespace iat {

namespace {

void issue(JxlScan& s, const std::string& code, const std::string& sev, const std::string& msg) {
    s.issues.push_back(Issue{code, sev, "", msg});
}
void corrupt(JxlScan& s, const std::string& code, const std::string& msg) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}
void warn(JxlScan& s, const std::string& msg) { issue(s, "jxl_container_warning", "warning", msg); }

uint64_t be(const uint8_t* p, int n) {
    uint64_t v = 0;
    for (int i = 0; i < n; ++i) v = (v << 8) | p[i];
    return v;
}

std::string type_text(const uint8_t* p) {
    std::string t;
    for (int i = 0; i < 4; ++i) t += (p[i] >= 0x20 && p[i] < 0x7F) ? static_cast<char>(p[i]) : '?';
    return t;
}

class Reader {
public:
    Reader(const ReadOnlyFile& f, uint64_t size, JxlScan& s) : f_(f), size_(size), s_(s) {}
    bool bytes(uint64_t off, void* dst, size_t n) {
        const auto end = checked_add(off, n);
        if (!end || *end > size_) return false;
        auto got = f_.read_at(off, dst, n);
        if (!got || *got != n) return false;
        s_.bytes_scanned += n;
        return true;
    }
    bool hash(uint64_t off, uint64_t n, std::string& hex) {
        Sha256 h;
        uint8_t buf[65536];
        while (n > 0) {
            const size_t take = static_cast<size_t>(n < sizeof buf ? n : sizeof buf);
            if (!bytes(off, buf, take)) return false;
            h.update(buf, take);
            off += take;
            n -= take;
        }
        hex = h.finish_hex();
        return true;
    }

private:
    const ReadOnlyFile& f_;
    uint64_t size_;
    JxlScan& s_;
};

}  // namespace

JxlScan scan_jxl(const ReadOnlyFile& f, uint64_t file_size, const Limits& lim) {
    JxlScan s;
    Reader rd(f, file_size, s);
    uint8_t h[12];
    if (!rd.bytes(0, h, 2)) { corrupt(s, code::truncated, "file shorter than a JPEG XL signature"); return s; }
    if (h[0] == 0xFF && h[1] == 0x0A) {
        s.container_status = "valid";   // the decoder decides about the end of a bare codestream
        return s;
    }
    if (!rd.bytes(0, h, 12) || memcmp(h, "\0\0\0\x0CJXL \r\n\x87\n", 12) != 0) {
        corrupt(s, code::structure_error, "not a JPEG XL codestream or container");
        return s;
    }
    s.container = true;
    uint64_t pos = 0, meta_total = 0;
    bool have_jxlc = false, last_part_seen = false, ftyp_ok = false;
    uint32_t expected_part = 0;
    int exifs = 0, xmps = 0;
    while (pos < file_size) {
        uint8_t bh[16];
        if (file_size - pos < 8 || !rd.bytes(pos, bh, 8)) {
            corrupt(s, code::truncated, "box header at offset " + std::to_string(pos) + " is incomplete (" +
                                            std::to_string(file_size - pos) + " bytes left)");
            return s;
        }
        uint64_t size = be(bh, 4);
        std::string type(reinterpret_cast<const char*>(bh + 4), 4);
        uint64_t header = 8;
        if (size == 1) {
            if (file_size - pos < 16 || !rd.bytes(pos + 8, bh + 8, 8)) { corrupt(s, code::truncated, "box largesize field is incomplete"); return s; }
            size = be(bh + 8, 8);
            header = 16;
            if (size < 16) { corrupt(s, code::structure_error, "box '" + type_text(bh + 4) + "' largesize " + std::to_string(size) + " is too small"); return s; }
        } else if (size == 0) {
            size = file_size - pos;        // box runs to the end of the file (last box only)
        } else if (size < 8) {
            corrupt(s, code::structure_error, "box '" + type_text(bh + 4) + "' size " + std::to_string(size) + " is too small");
            return s;
        }
        const auto end = checked_add(pos, size);
        if (!end || *end > file_size) {
            corrupt(s, code::truncated, "box '" + type_text(bh + 4) + "' at offset " + std::to_string(pos) + " (" +
                                            std::to_string(size) + " bytes) runs past the end of the file");
            return s;
        }
        const uint64_t data = pos + header, len = size - header;
        std::string shown = type_text(bh + 4);
        if (pos == 0) {
            // signature box, checked above
        } else if (type == "ftyp") {
            uint8_t b[4];
            if (s.boxes.size() != 1) warn(s, "ftyp is not the second box");
            if (len < 4 || !rd.bytes(data, b, 4) || memcmp(b, "jxl ", 4) != 0) { corrupt(s, code::structure_error, "ftyp major brand is not 'jxl '"); return s; }
            ftyp_ok = true;
        } else if (type == "jxll") {
            uint8_t b = 0;
            if (len != 1 || !rd.bytes(data, &b, 1)) warn(s, "jxll box does not hold exactly one byte");
            else { s.level_box = true; s.level = b; }
        } else if (type == "jxlc") {
            if (have_jxlc || s.jxlp_parts) { corrupt(s, code::structure_error, "more than one codestream box (jxlc/jxlp)"); return s; }
            have_jxlc = true;
            s.codestream_bytes = len;
        } else if (type == "jxlp") {
            uint8_t b[4];
            if (have_jxlc) { corrupt(s, code::structure_error, "jxlp together with jxlc"); return s; }
            if (last_part_seen) { corrupt(s, code::structure_error, "jxlp after the part marked last"); return s; }
            if (len < 4 || !rd.bytes(data, b, 4)) { corrupt(s, code::truncated, "jxlp index is missing"); return s; }
            const uint32_t idx = static_cast<uint32_t>(be(b, 4));
            if ((idx & 0x7FFFFFFFu) != expected_part) {
                corrupt(s, code::structure_error, "jxlp index " + std::to_string(idx & 0x7FFFFFFFu) + ", expected " + std::to_string(expected_part));
                return s;
            }
            ++expected_part;
            ++s.jxlp_parts;
            last_part_seen = (idx & 0x80000000u) != 0;
            s.codestream_bytes += len - 4;
        } else if (type == "brob") {
            uint8_t b[4];
            if (len < 4 || !rd.bytes(data, b, 4)) { corrupt(s, code::truncated, "brob inner type is missing"); return s; }
            const std::string inner(reinterpret_cast<const char*>(b), 4);
            shown = "brob:" + type_text(b);
            if (inner == "jxlc" || inner == "jxlp" || inner == "brob" || inner == "JXL " || inner == "ftyp" || inner == "jxll") {
                corrupt(s, code::structure_error, "brob may not compress a '" + type_text(b) + "' box");
                return s;
            }
            if (inner == "Exif") { ++exifs; if (s.exif_status == "absent") s.exif_status = "compressed"; }
            else if (inner == "xml ") { ++xmps; }
        } else if (type == "Exif") {
            if (++exifs > 1) warn(s, "more than one Exif box (the first is used)");
            else if (len < 4) { s.exif_status = "present"; warn(s, "Exif box shorter than its 4-byte offset"); }
            else if (len > lim.metadata_item_bytes() || meta_total + len > lim.metadata_file_bytes()) {
                s.exif_status = "over_limit";
                s.metadata_status = "over_limit";
            } else {
                std::vector<uint8_t> e(static_cast<size_t>(len));
                if (!rd.bytes(data, e.data(), e.size())) { corrupt(s, code::truncated, "Exif payload unreadable"); return s; }
                const uint64_t off = be(e.data(), 4);   // offset of the TIFF header after the 4-byte field
                if (off > len - 4) { s.exif_status = "present"; warn(s, "Exif TIFF header offset lies outside the box"); }
                else { s.exif.assign(e.begin() + 4 + static_cast<size_t>(off), e.end()); s.exif_status = "present"; }
                meta_total += len;
            }
        } else if (type == "xml ") {
            if (++xmps > 1) warn(s, "more than one xml box (the first is used)");
            else if (!rd.hash(data, len, s.xmp_sha256)) { corrupt(s, code::truncated, "xml payload unreadable"); return s; }
            else s.xmp_bytes = len;
        } else if (type == "jbrd") {
            s.jbrd = true;
        } else {
            s.other_boxes.push_back(shown);
        }
        s.boxes.push_back(shown);
        if (!end || *end == pos) break;
        pos = *end;
    }
    if (!ftyp_ok) { corrupt(s, code::structure_error, "ftyp box missing"); return s; }
    if (!have_jxlc && s.jxlp_parts == 0) { corrupt(s, code::structure_error, "no codestream box (jxlc or jxlp)"); return s; }
    if (s.jxlp_parts && !last_part_seen) {
        corrupt(s, code::truncated, "the last jxlp part (index with the high bit set) is missing");
        return s;
    }
    bool warned = false;
    for (const Issue& i : s.issues) warned = warned || i.severity == "warning";
    s.container_status = warned ? "warning" : "valid";
    return s;
}

}  // namespace iat
