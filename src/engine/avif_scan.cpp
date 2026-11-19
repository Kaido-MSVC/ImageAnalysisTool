#include "avif_scan.hpp"

#include "../common/checked.hpp"

#include <cstring>

namespace iat {

namespace {

void issue(AvifScan& s, const std::string& code, const std::string& sev, const std::string& msg) {
    s.issues.push_back(Issue{code, sev, "", msg});
}
void corrupt(AvifScan& s, const std::string& code, const std::string& msg) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}

uint32_t be32(const uint8_t* p) {
    return (static_cast<uint32_t>(p[0]) << 24) | (static_cast<uint32_t>(p[1]) << 16) | (static_cast<uint32_t>(p[2]) << 8) | p[3];
}
uint16_t be16(const uint8_t* p) { return static_cast<uint16_t>((p[0] << 8) | p[1]); }

std::string fourcc(const uint8_t* p) {
    std::string t;
    for (int i = 0; i < 4; ++i) t += (p[i] >= 0x20 && p[i] < 0x7F) ? static_cast<char>(p[i]) : '?';
    return t;
}

struct Box {
    std::string type;
    uint64_t start = 0, payload = 0, end = 0;
};

class Walker {
public:
    Walker(const ReadOnlyFile& f, uint64_t size, AvifScan& s) : f_(f), size_(size), s_(s) {}
    bool bytes(uint64_t off, void* dst, size_t n) {
        const auto end = checked_add(off, n);
        if (!end || *end > size_) return false;
        auto got = f_.read_at(off, dst, n);
        if (!got || *got != n) return false;
        s_.bytes_scanned += n;
        return true;
    }
    // Reads the box header at pos inside [pos, limit). top: limit is the file end.
    std::optional<Box> header(uint64_t pos, uint64_t limit, bool top) {
        uint8_t h[16];
        if (limit - pos < 8 || !bytes(pos, h, 8)) {
            corrupt(s_, top ? code::truncated : code::structure_error, "box header at offset " + std::to_string(pos) + " is incomplete");
            return std::nullopt;
        }
        Box b;
        b.type = fourcc(h + 4);
        b.start = pos;
        uint64_t size = be32(h);
        uint64_t hdr = 8;
        if (size == 1) {
            if (limit - pos < 16 || !bytes(pos + 8, h + 8, 8)) {
                corrupt(s_, top ? code::truncated : code::structure_error, "'" + b.type + "' largesize field is incomplete");
                return std::nullopt;
            }
            size = (static_cast<uint64_t>(be32(h + 8)) << 32) | be32(h + 12);
            hdr = 16;
        } else if (size == 0) {
            size = limit - pos;   // extends to the end of the enclosing box / file
        }
        if (size < hdr) {
            corrupt(s_, code::structure_error, "'" + b.type + "' box size " + std::to_string(size) + " is smaller than its header");
            return std::nullopt;
        }
        if (size > limit - pos) {
            corrupt(s_, top ? code::truncated : code::structure_error,
                    "'" + b.type + "' box (" + std::to_string(size) + " bytes at offset " + std::to_string(pos) + ") runs past " +
                        (top ? "the end of the file" : "its parent box"));
            return std::nullopt;
        }
        b.payload = pos + hdr;
        b.end = pos + size;
        return b;
    }
    // Walks the children of [begin, end); calls fn(box) for each. False on a broken child.
    template <class Fn>
    bool children(uint64_t begin, uint64_t end, Fn fn) {
        for (uint64_t p = begin; p < end;) {
            auto b = header(p, end, false);
            if (!b) return false;
            if (!fn(*b)) return false;
            p = b->end;
        }
        return true;
    }

private:
    const ReadOnlyFile& f_;
    uint64_t size_;
    AvifScan& s_;
};

void scan_meta(Walker& w, AvifScan& s, const Box& meta) {
    // meta is a FullBox: 4 bytes of version/flags before the children.
    if (meta.end - meta.payload < 4) { corrupt(s, code::structure_error, "'meta' box is too short"); return; }
    w.children(meta.payload + 4, meta.end, [&](const Box& b) {
        if (b.type != "iprp") return true;
        return w.children(b.payload, b.end, [&](const Box& c) {
            if (c.type != "ipco") return true;
            return w.children(c.payload, c.end, [&](const Box& p) {
                if (p.type != "colr" || p.end - p.payload < 4) return true;
                uint8_t t[11];
                if (!w.bytes(p.payload, t, 4)) return false;
                if (!memcmp(t, "nclx", 4)) {
                    if (p.end - p.payload < 11 || !w.bytes(p.payload, t, 11)) {
                        corrupt(s, code::structure_error, "'colr' nclx property is shorter than 11 bytes");
                        return false;
                    }
                    s.nclx.push_back(AvifNclx{be16(t + 4), be16(t + 6), be16(t + 8), (t[10] & 0x80) != 0});
                } else if (!memcmp(t, "prof", 4) || !memcmp(t, "rICC", 4)) {
                    ++s.colr_prof;
                }
                return true;
            });
        });
    });
}

}  // namespace

uint32_t avif_orientation(uint32_t a, int axis) {
    // Derived in docs/decisions.md (D30): displayed = imir(irot(stored)).
    static const uint32_t table[4][3] = {
        // no imir, axis 0, axis 1
        {1, 4, 2},
        {8, 5, 7},
        {3, 2, 4},
        {6, 7, 5},
    };
    return table[a & 3][axis < 0 ? 0 : (axis == 0 ? 1 : 2)];
}

AvifScan scan_avif(const ReadOnlyFile& f, uint64_t file_size) {
    AvifScan s;
    Walker w(f, file_size, s);
    s.container_status = "valid";
    bool first = true;
    for (uint64_t pos = 0; pos < file_size;) {
        auto b = w.header(pos, file_size, true);
        if (!b) return s;
        if (s.boxes.empty() || s.boxes.back() != b->type) s.boxes.push_back(b->type);
        if (first) {
            if (b->type != "ftyp" || b->end - b->payload < 8) { corrupt(s, code::structure_error, "the file does not start with an 'ftyp' box"); return s; }
            std::vector<uint8_t> ft(static_cast<size_t>(std::min<uint64_t>(b->end - b->payload, 4096)));
            if (!w.bytes(b->payload, ft.data(), ft.size())) { corrupt(s, code::truncated, "'ftyp' box could not be read"); return s; }
            s.major_brand = fourcc(ft.data());
            s.brands.push_back(s.major_brand);
            for (size_t i = 8; i + 4 <= ft.size(); i += 4) s.brands.push_back(fourcc(ft.data() + i));
            for (const auto& br : s.brands) {
                if (br == "avif" || br == "avis") s.is_avif = true;
                if (br == "avis") s.sequence = true;
            }
            first = false;
        } else if (b->type == "meta") {
            s.has_meta = true;
            scan_meta(w, s, *b);
            if (s.container_status == "corrupt") return s;
        } else if (b->type == "moov") {
            s.sequence = true;
        }
        pos = b->end;
    }
    if (first) { corrupt(s, code::truncated, "empty file"); return s; }
    if (!s.is_avif) corrupt(s, code::structure_error, "ftyp has neither the 'avif' nor the 'avis' brand");
    else if (!s.has_meta) corrupt(s, code::structure_error, "no 'meta' box (an AVIF image needs one)");
    return s;
}

}  // namespace iat
