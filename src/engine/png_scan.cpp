#include "png_scan.hpp"

#include "../common/checked.hpp"
#include "../common/text.hpp"

#include <zlib.h>

#include <cstdio>
#include <cstring>

namespace iat {

namespace {

uint32_t be32(const uint8_t* p) {
    return (static_cast<uint32_t>(p[0]) << 24) | (static_cast<uint32_t>(p[1]) << 16) |
           (static_cast<uint32_t>(p[2]) << 8) | p[3];
}

bool valid_type(const uint8_t* t) {
    for (int i = 0; i < 4; ++i)
        if (!((t[i] >= 'A' && t[i] <= 'Z') || (t[i] >= 'a' && t[i] <= 'z'))) return false;
    return true;
}

void issue(PngScan& s, const char* code, const char* sev, const std::string& msg) {
    s.issues.push_back(Issue{code, sev, "", msg});
}

void corrupt(PngScan& s, const char* code, const std::string& msg) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}

bool valid_ihdr_combo(int depth, int ctype) {
    switch (ctype) {
        case 0: return depth == 1 || depth == 2 || depth == 4 || depth == 8 || depth == 16;
        case 3: return depth == 1 || depth == 2 || depth == 4 || depth == 8;
        case 2: case 4: case 6: return depth == 8 || depth == 16;
        default: return false;
    }
}

}  // namespace

std::string inflate_capped(const uint8_t* data, size_t n, uint64_t cap, std::vector<uint8_t>& out) {
    out.clear();
    z_stream zs{};
    if (inflateInit(&zs) != Z_OK) return "corrupt";
    zs.next_in = const_cast<Bytef*>(data);
    zs.avail_in = static_cast<uInt>(n);
    uint8_t buf[64 * 1024];
    std::string result = "truncated";
    for (;;) {
        zs.next_out = buf;
        zs.avail_out = sizeof buf;
        const int rc = inflate(&zs, Z_NO_FLUSH);
        const size_t produced = sizeof buf - zs.avail_out;
        if (out.size() + produced > cap) { result = "over_limit"; break; }
        out.insert(out.end(), buf, buf + produced);
        if (rc == Z_STREAM_END) { result = "ok"; break; }
        if (rc == Z_BUF_ERROR && zs.avail_in == 0) { result = "truncated"; break; }
        if (rc != Z_OK) { result = "corrupt"; break; }
    }
    inflateEnd(&zs);
    if (result != "ok") out.clear();
    return result;
}

int exif_orientation(const std::vector<uint8_t>& e) {
    const size_t n = e.size();
    if (n < 8) return -1;
    bool le;
    if (e[0] == 'I' && e[1] == 'I') le = true;
    else if (e[0] == 'M' && e[1] == 'M') le = false;
    else return -1;
    auto u16 = [&](size_t o) -> uint32_t { return le ? (e[o] | (e[o + 1] << 8)) : ((e[o] << 8) | e[o + 1]); };
    auto u32 = [&](size_t o) -> uint32_t {
        return le ? (e[o] | (e[o + 1] << 8) | (e[o + 2] << 16) | (static_cast<uint32_t>(e[o + 3]) << 24))
                  : ((static_cast<uint32_t>(e[o]) << 24) | (e[o + 1] << 16) | (e[o + 2] << 8) | e[o + 3]);
    };
    if (u16(2) != 42) return -1;
    const uint64_t ifd = u32(4);
    if (ifd + 2 > n) return -1;
    const uint32_t count = u16(static_cast<size_t>(ifd));
    if (ifd + 2 + static_cast<uint64_t>(count) * 12 > n) return -1;
    for (uint32_t i = 0; i < count; ++i) {
        const size_t o = static_cast<size_t>(ifd + 2 + static_cast<uint64_t>(i) * 12);
        if (u16(o) == 0x0112) {
            if (u16(o + 2) != 3 || u32(o + 4) != 1) return -1;  // SHORT, count 1
            const uint32_t v = u16(o + 8);
            return (v >= 1 && v <= 8) ? static_cast<int>(v) : -1;
        }
    }
    return 0;
}

PngScan scan_png(const ReadOnlyFile& f, uint64_t file_size, const Limits& limits) {
    PngScan s;
    uint8_t sig[8];
    auto got = f.read_at(0, sig, 8);
    if (!got) { corrupt(s, code::structure_error, "I/O error reading signature"); return s; }
    static const uint8_t png_sig[8] = {0x89, 'P', 'N', 'G', 0x0D, 0x0A, 0x1A, 0x0A};
    if (*got < 8 || memcmp(sig, png_sig, 8) != 0) {
        corrupt(s, code::unknown_format, "PNG signature not found");
        return s;
    }
    s.signature_ok = true;

    uint64_t pos = 8;
    uint64_t kept_total = 0;
    bool seen_idat = false, idat_closed = false;
    std::string last_type;
    uint32_t run = 0;
    auto push_order = [&](const std::string& t) {
        if (t == last_type) { ++run; return; }
        if (!last_type.empty()) s.chunk_order.push_back(run > 1 ? last_type + " x" + std::to_string(run) : last_type);
        last_type = t;
        run = 1;
    };
    std::vector<uint8_t> io(1u << 20);

    while (true) {
        if (pos == file_size) { corrupt(s, code::missing_end_marker, "file ends before IEND"); break; }
        uint8_t hdr[8];
        got = f.read_at(pos, hdr, 8);
        if (!got) { corrupt(s, code::structure_error, "I/O error"); break; }
        if (*got < 8) { corrupt(s, code::truncated, "chunk header truncated at offset " + std::to_string(pos)); break; }
        const uint32_t len = be32(hdr);
        const std::string type(reinterpret_cast<const char*>(hdr + 4), 4);
        if (len > 0x7FFFFFFFu || !valid_type(hdr + 4)) {
            corrupt(s, code::structure_error, "invalid chunk header at offset " + std::to_string(pos));
            break;
        }
        const auto chunk_end = checked_sum({pos, 12, len});
        if (!chunk_end || *chunk_end > file_size) {
            corrupt(s, code::truncated, type + " chunk truncated at offset " + std::to_string(pos));
            break;
        }
        if (pos == 8 && type != "IHDR") { corrupt(s, code::structure_error, "first chunk is not IHDR"); break; }
        push_order(type);

        // Keep payloads of small known chunks; stream everything else through CRC.
        const bool is_idat = type == "IDAT";
        const bool wanted = !is_idat && type != "IEND";
        const bool keep = wanted && len <= limits.metadata_item_bytes() &&
                          kept_total + len <= limits.metadata_file_bytes();
        std::vector<uint8_t> payload;
        uLong crc = crc32(0, hdr + 4, 4);
        uint64_t off = pos + 8, left = len;
        if (keep) payload.resize(len);
        bool io_fail = false;
        while (left > 0) {
            const size_t want = static_cast<size_t>(std::min<uint64_t>(left, io.size()));
            auto r = f.read_at(off, io.data(), want);
            if (!r || *r != want) { io_fail = true; break; }
            crc = crc32(crc, io.data(), static_cast<uInt>(want));
            if (keep) memcpy(payload.data() + (len - left), io.data(), want);
            off += want;
            left -= want;
        }
        uint8_t crcb[4];
        if (io_fail || f.read_at(off, crcb, 4).value_or(0) != 4) {
            corrupt(s, code::structure_error, "I/O error inside " + type);
            break;
        }
        if (be32(crcb) != static_cast<uint32_t>(crc)) {
            corrupt(s, code::crc_error, type + " CRC mismatch at offset " + std::to_string(pos));
            break;
        }
        if (keep) kept_total += len;
        if (wanted && !keep) {
            s.metadata_status = "over_limit";
            issue(s, code::metadata_over_limit, "warning", type + " (" + std::to_string(len) + " bytes) exceeds metadata budget");
        }

        // IDAT must be consecutive.
        if (is_idat) {
            if (idat_closed) { corrupt(s, code::structure_error, "IDAT chunks are not consecutive"); break; }
            seen_idat = true;
            s.idat_bytes += len;
            ++s.idat_chunks;
        } else if (seen_idat) {
            idat_closed = true;
        }

        if (type == "IHDR") {
            if (s.has_ihdr || len != 13) { corrupt(s, code::structure_error, "bad or repeated IHDR"); break; }
            s.has_ihdr = true;
            s.width = be32(payload.data());
            s.height = be32(payload.data() + 4);
            s.bit_depth = payload[8];
            s.color_type = payload[9];
            s.compression = payload[10];
            s.filter = payload[11];
            s.interlace = payload[12];
            if (s.width == 0 || s.height == 0 || s.width > 0x7FFFFFFFu || s.height > 0x7FFFFFFFu ||
                !valid_ihdr_combo(s.bit_depth, s.color_type) || s.compression != 0 || s.filter != 0 ||
                s.interlace > 1) {
                corrupt(s, code::structure_error, "invalid IHDR field values");
                break;
            }
        } else if (type == "PLTE") {
            s.has_plte = true;
        } else if (type == "tRNS") {
            s.has_trns = true;
        } else if (type == "acTL" || type == "fcTL" || type == "fdAT") {
            s.animated = true;
        } else if (type == "cICP" && keep) {
            if (len == 4) s.cicp = std::array<uint8_t, 4>{payload[0], payload[1], payload[2], payload[3]};
            else issue(s, code::metadata_parse_failed, "warning", "cICP length invalid");
        } else if (type == "sRGB" && keep) {
            if (len == 1) s.srgb_intent = payload[0];
            else issue(s, code::metadata_parse_failed, "warning", "sRGB length invalid");
        } else if (type == "gAMA" && keep) {
            if (len == 4) s.gama = be32(payload.data());
            else issue(s, code::metadata_parse_failed, "warning", "gAMA length invalid");
        } else if (type == "cHRM" && keep) {
            if (len == 32) {
                std::array<uint32_t, 8> c{};
                for (int i = 0; i < 8; ++i) c[i] = be32(payload.data() + 4 * i);
                s.chrm = c;
            } else {
                issue(s, code::metadata_parse_failed, "warning", "cHRM length invalid");
            }
        } else if (type == "sBIT" && keep) {
            s.sbit = payload;
        } else if (type == "mDCV" && keep) {
            s.mdcv_hex = hex(payload);
        } else if (type == "cLLI" && keep) {
            s.clli_hex = hex(payload);
        } else if (type == "iCCP") {
            s.iccp_present = true;
            if (!keep) {
                s.icc.status = "over_budget";
            } else {
                const auto nul = std::find(payload.begin(), payload.end(), uint8_t{0});
                if (nul == payload.end() || nul + 2 > payload.end() || *(nul + 1) != 0) {
                    s.icc.status = "unreadable";
                    issue(s, code::metadata_parse_failed, "warning", "iCCP header invalid");
                } else {
                    s.icc.profile_name.assign(payload.begin(), nul);
                    std::vector<uint8_t> prof;
                    const size_t zoff = static_cast<size_t>(nul - payload.begin()) + 2;
                    const std::string st = inflate_capped(payload.data() + zoff, payload.size() - zoff,
                                                          limits.metadata_item_bytes(), prof);
                    if (st == "ok") {
                        s.icc.status = "embedded";
                        s.icc.bytes = Count{prof.size()};
                        s.icc.sha256 = sha256_hex(prof.data(), prof.size());
                    } else {
                        s.icc.status = st == "over_limit" ? "over_budget" : "unreadable";
                        issue(s, code::metadata_parse_failed, "warning", "iCCP profile could not be inflated: " + st);
                    }
                }
            }
        } else if (type == "eXIf") {
            if (keep) { s.exif = payload; s.exif_status = "present"; }
            else s.exif_status = "over_limit";
        } else if (type == "pHYs" && keep) {
            if (len == 9) s.phys = std::to_string(be32(payload.data())) + "/" + std::to_string(be32(payload.data() + 4)) +
                                   (payload[8] == 1 ? " per metre" : " (unit unknown)");
            else issue(s, code::metadata_parse_failed, "warning", "pHYs length invalid");
        } else if (type == "tIME" && keep) {
            if (len == 7) {
                char b[32];
                snprintf(b, sizeof b, "%04u-%02u-%02u %02u:%02u:%02u", (payload[0] << 8) | payload[1], payload[2],
                         payload[3], payload[4], payload[5], payload[6]);
                s.time = b;
            } else {
                issue(s, code::metadata_parse_failed, "warning", "tIME length invalid");
            }
        } else if (type == "orNT" && keep) {
            if (len == 1) s.ornt = payload[0];
        } else if ((type == "tEXt" || type == "zTXt" || type == "iTXt") && keep) {
            PngText t;
            t.kind = type;
            const auto nul = std::find(payload.begin(), payload.end(), uint8_t{0});
            if (nul == payload.end()) {
                t.status = "parse_failed";
            } else {
                t.keyword_raw.assign(payload.begin(), nul);
                t.keyword = latin1_text(payload.data(), t.keyword_raw.size());
                size_t p = static_cast<size_t>(nul - payload.begin()) + 1;
                std::vector<uint8_t> value;
                std::string st = "ok";
                if (type == "tEXt") {
                    value.assign(payload.begin() + p, payload.end());
                } else if (type == "zTXt") {
                    if (p + 1 > payload.size() || payload[p] != 0) st = "corrupt";
                    else st = inflate_capped(payload.data() + p + 1, payload.size() - p - 1, limits.metadata_item_bytes(), value);
                } else {  // iTXt: flag, method, language\0, translated\0, text
                    if (p + 2 > payload.size()) {
                        st = "corrupt";
                    } else {
                        const int flag = payload[p], method = payload[p + 1];
                        p += 2;
                        for (int skip = 0; skip < 2 && st == "ok"; ++skip) {
                            auto z = std::find(payload.begin() + p, payload.end(), uint8_t{0});
                            if (z == payload.end()) st = "corrupt";
                            else p = static_cast<size_t>(z - payload.begin()) + 1;
                        }
                        if (st == "ok") {
                            if (flag == 0) value.assign(payload.begin() + p, payload.end());
                            else if (flag == 1 && method == 0)
                                st = inflate_capped(payload.data() + p, payload.size() - p, limits.metadata_item_bytes(), value);
                            else st = "corrupt";
                        }
                    }
                }
                if (st == "ok") {
                    t.bytes = value.size();
                    t.sha256 = sha256_hex(value.data(), value.size());
                } else {
                    t.status = st == "over_limit" ? "over_limit" : "parse_failed";
                }
            }
            if (t.status == "parse_failed") {
                s.metadata_status = "parse_failed";
                issue(s, code::metadata_parse_failed, "warning", type + " '" + t.keyword + "' could not be decoded");
            }
            s.texts.push_back(t);
        } else if (!is_idat && type != "IEND" && type != "IHDR" && (hdr[4] & 0x20)) {
            if (std::find(s.other_ancillary.begin(), s.other_ancillary.end(), type) == s.other_ancillary.end())
                s.other_ancillary.push_back(type);
        }

        pos = *chunk_end;
        if (type == "IEND") {
            s.ended = true;
            s.end_offset = pos;
            break;
        }
    }
    if (!last_type.empty()) s.chunk_order.push_back(run > 1 ? last_type + " x" + std::to_string(run) : last_type);
    s.bytes_scanned = pos;

    if (s.ended) {
        s.trailing_bytes = file_size - s.end_offset;
        if (!s.has_ihdr || s.idat_chunks == 0) corrupt(s, code::structure_error, "missing IHDR or IDAT");
        else if (s.color_type == 3 && !s.has_plte) corrupt(s, code::structure_error, "palette image without PLTE");
        if (s.container_status != "corrupt") {
            s.container_status = "valid";
            if (s.trailing_bytes > 0) {
                s.container_status = "warning";
                issue(s, code::trailing_data, "warning",
                      std::to_string(s.trailing_bytes) + " bytes after IEND");
            }
        }
    }
    return s;
}

}  // namespace iat
