#include "webp_scan.hpp"

#include "../common/checked.hpp"

#include <cstring>

namespace iat {

namespace {

void issue(WebpScan& s, const std::string& code, const std::string& sev, const std::string& msg) {
    s.issues.push_back(Issue{code, sev, "", msg});
}
void corrupt(WebpScan& s, const std::string& code, const std::string& msg) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}
void warn(WebpScan& s, const std::string& msg) { issue(s, "webp_container_warning", "warning", msg); }

uint32_t le32(const uint8_t* p) { return p[0] | (p[1] << 8) | (p[2] << 16) | (static_cast<uint32_t>(p[3]) << 24); }
uint32_t le24(const uint8_t* p) { return p[0] | (p[1] << 8) | (p[2] << 16); }

class Reader {
public:
    Reader(const ReadOnlyFile& f, uint64_t size, WebpScan& s) : f_(f), size_(size), s_(s) {}
    bool bytes(uint64_t off, void* dst, size_t n) {
        const auto end = checked_add(off, n);
        if (!end || *end > size_) return false;
        auto got = f_.read_at(off, dst, n);
        if (!got || *got != n) return false;
        s_.bytes_scanned += n;
        return true;
    }
    // Reads a whole payload when it fits the cap; returns false on I/O failure.
    bool payload(uint64_t off, uint64_t n, std::vector<uint8_t>& out) {
        out.resize(static_cast<size_t>(n));
        return n == 0 || bytes(off, out.data(), out.size());
    }
    // Streams a payload through SHA-256 (no copy kept).
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
    WebpScan& s_;
};

std::string fourcc_text(const uint8_t* p) {
    std::string t;
    for (int i = 0; i < 4; ++i) t += (p[i] >= 0x20 && p[i] < 0x7F) ? static_cast<char>(p[i]) : '?';
    return t;
}

}  // namespace

WebpScan scan_webp(const ReadOnlyFile& f, uint64_t file_size, const Limits& lim) {
    WebpScan s;
    Reader rd(f, file_size, s);
    uint8_t h[12];
    if (!rd.bytes(0, h, 12)) { corrupt(s, code::truncated, "file shorter than a RIFF/WEBP header"); return s; }
    if (memcmp(h, "RIFF", 4) || memcmp(h + 8, "WEBP", 4)) { corrupt(s, code::structure_error, "not a RIFF WEBP file"); return s; }
    const uint64_t riff_size = le32(h + 4);
    const uint64_t riff_end = 8 + riff_size;
    if (riff_size < 4) { corrupt(s, code::structure_error, "RIFF size " + std::to_string(riff_size) + " is too small"); return s; }
    if (riff_size & 1) warn(s, "RIFF size " + std::to_string(riff_size) + " is odd");
    if (riff_end > file_size) {
        corrupt(s, code::truncated, "RIFF declares " + std::to_string(riff_end) + " bytes, file has " + std::to_string(file_size));
        return s;
    }
    s.trailing_bytes = file_size - riff_end;
    if (*s.trailing_bytes > 0) issue(s, code::trailing_data, "warning", std::to_string(*s.trailing_bytes) + " bytes after the RIFF payload");

    uint64_t pos = 12, meta_total = 0;
    int images = 0, iccp = 0, exifs = 0, xmps = 0, anims = 0;
    bool first = true, after_image = false;
    while (pos < riff_end) {
        uint8_t ch[8];
        if (riff_end - pos < 8 || !rd.bytes(pos, ch, 8)) {
            corrupt(s, code::truncated, "chunk header at offset " + std::to_string(pos) + " runs past the RIFF payload");
            return s;
        }
        const std::string tag(reinterpret_cast<const char*>(ch), 4);
        const uint64_t size = le32(ch + 4);
        const uint64_t data = pos + 8, end = data + size, padded = end + (size & 1);
        s.chunks.push_back(fourcc_text(ch));
        if (end > riff_end) {
            corrupt(s, code::truncated, "chunk '" + fourcc_text(ch) + "' at offset " + std::to_string(pos) + " (" +
                                            std::to_string(size) + " bytes) runs past the RIFF payload");
            return s;
        }
        if (padded > riff_end) {  // S4: libwebp tolerates this, the container rules do not
            corrupt(s, code::truncated, "chunk '" + fourcc_text(ch) + "' has an odd size but its padding byte is missing");
            return s;
        }
        if (first) {
            first = false;
            if (tag != "VP8 " && tag != "VP8L" && tag != "VP8X") {
                corrupt(s, code::structure_error, "first chunk is '" + fourcc_text(ch) + "', expected VP8, VP8L or VP8X");
                return s;
            }
        } else if (!s.extended) {
            warn(s, "chunk '" + fourcc_text(ch) + "' after the image in a simple-format file");
        }
        if (tag == "VP8X") {
            if (s.extended || pos != 12) { corrupt(s, code::structure_error, "VP8X is not the first chunk or appears twice"); return s; }
            uint8_t x[10];
            if (size < 10 || !rd.bytes(data, x, 10)) { corrupt(s, code::structure_error, "VP8X chunk shorter than 10 bytes"); return s; }
            s.extended = true;
            s.vp8x_flags = x[0];
            s.canvas_width = le24(x + 4) + 1;
            s.canvas_height = le24(x + 7) + 1;
            if (static_cast<uint64_t>(s.canvas_width) * s.canvas_height > 0xFFFFFFFFull)
                corrupt(s, code::structure_error, "VP8X canvas width x height exceeds 2^32 - 1");
        } else if (tag == "VP8L" || tag == "VP8 ") {
            ++images;
            after_image = true;
            if (images == 1) {
                s.image_chunk = tag;
                uint8_t b[10] = {};
                const size_t need = tag == "VP8L" ? 5 : 10;
                if (size < need || !rd.bytes(data, b, need)) {
                    corrupt(s, code::truncated, tag + " chunk too short for its header");
                    return s;
                }
                if (tag == "VP8L") {
                    if (b[0] != 0x2F) { corrupt(s, code::structure_error, "VP8L signature is not 0x2F"); return s; }
                    const uint32_t v = le32(b + 1);
                    s.width = (v & 0x3FFF) + 1;
                    s.height = ((v >> 14) & 0x3FFF) + 1;
                    s.vp8l_alpha_hint = ((v >> 28) & 1) != 0;
                    if ((v >> 29) != 0) { corrupt(s, code::structure_error, "VP8L version " + std::to_string(v >> 29) + " (must be 0)"); return s; }
                } else {
                    if (b[3] != 0x9D || b[4] != 0x01 || b[5] != 0x2A) { corrupt(s, code::structure_error, "VP8 start code missing"); return s; }
                    s.width = (b[6] | (b[7] << 8)) & 0x3FFF;
                    s.height = (b[8] | (b[9] << 8)) & 0x3FFF;
                }
            }
        } else if (tag == "ALPH") {
            s.has_alph = true;
            if (after_image) warn(s, "ALPH chunk after the image data");
        } else if (tag == "ANIM" || tag == "ANMF") {
            ++anims;
            s.animated = true;
        } else if (tag == "ICCP") {
            if (++iccp > 1) { warn(s, "more than one ICCP chunk (the first is used)"); }
            else {
                if (s.chunks.size() != 2) warn(s, "ICCP is not directly after VP8X");
                if (size > lim.metadata_item_bytes() || meta_total + size > lim.metadata_file_bytes()) {
                    s.icc_status = "over_budget";
                    s.metadata_status = "over_limit";
                    issue(s, code::metadata_over_limit, "warning", "ICC profile of " + std::to_string(size) + " bytes exceeds the metadata budget");
                } else if (!rd.payload(data, size, s.icc)) {
                    corrupt(s, code::truncated, "ICCP payload unreadable");
                    return s;
                } else {
                    s.icc_status = "embedded";
                    meta_total += size;
                }
            }
        } else if (tag == "EXIF") {
            if (++exifs > 1) warn(s, "more than one EXIF chunk (the first is used)");
            else if (size > lim.metadata_item_bytes() || meta_total + size > lim.metadata_file_bytes()) {
                s.exif_status = "over_limit";
                s.metadata_status = "over_limit";
            } else if (!rd.payload(data, size, s.exif)) {
                corrupt(s, code::truncated, "EXIF payload unreadable");
                return s;
            } else {
                s.exif_status = "present";
                meta_total += size;
            }
        } else if (tag == "XMP ") {
            if (++xmps > 1) warn(s, "more than one XMP chunk (the first is used)");
            else if (!rd.hash(data, size, s.xmp_sha256)) { corrupt(s, code::truncated, "XMP payload unreadable"); return s; }
            else s.xmp_bytes = size;
        } else {
            s.unknown_chunks.push_back(fourcc_text(ch));
        }
        pos = padded;
    }

    if (s.container_status == "corrupt") return s;
    if (images == 0 && !s.animated) { corrupt(s, code::structure_error, "no VP8 or VP8L image chunk"); return s; }
    if (images > 1 && !s.animated) { corrupt(s, code::structure_error, std::to_string(images) + " image chunks in a still image"); return s; }
    if (s.extended) {
        const uint8_t fl = s.vp8x_flags;
        if (fl & 0x02) s.animated = true;
        if (((fl & 0x20) != 0) != (iccp > 0)) warn(s, "VP8X ICC flag does not match the presence of an ICCP chunk");
        if (((fl & 0x08) != 0) != (exifs > 0)) warn(s, "VP8X EXIF flag does not match the presence of an EXIF chunk");
        if (((fl & 0x04) != 0) != (xmps > 0)) warn(s, "VP8X XMP flag does not match the presence of an XMP chunk");
        if (((fl & 0x02) != 0) != (anims > 0)) warn(s, "VP8X animation flag does not match the presence of ANIM/ANMF chunks");
        if (!s.animated && images == 1 && (s.canvas_width != s.width || s.canvas_height != s.height)) {
            corrupt(s, code::structure_error, "VP8X canvas " + std::to_string(s.canvas_width) + "x" + std::to_string(s.canvas_height) +
                                                  " differs from the image " + std::to_string(s.width) + "x" + std::to_string(s.height));
            return s;
        }
        if (s.image_chunk == "VP8L" && ((fl & 0x10) != 0) != s.vp8l_alpha_hint)
            issue(s, code::alpha_hint_mismatch, "warning",
                  std::string("VP8X alpha flag is ") + ((fl & 0x10) ? "set" : "clear") + " but the VP8L alpha_is_used hint is " +
                      (s.vp8l_alpha_hint ? "set" : "clear"));
    }
    bool warned = false;
    for (const Issue& i : s.issues) warned = warned || i.severity == "warning";
    s.container_status = warned ? "warning" : "valid";
    return s;
}

}  // namespace iat
