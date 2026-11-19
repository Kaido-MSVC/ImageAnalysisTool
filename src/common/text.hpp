// Text taken from input files, made safe for the reports (JSON requires UTF-8,
// Markdown is read by people). Raw bytes stay where they are hashed; these
// helpers only produce the displayed form.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>

namespace iat {

namespace text_detail {
inline void put_cp(std::string& out, uint32_t cp) {
    if (cp < 0x80) {
        out += static_cast<char>(cp);
    } else if (cp < 0x800) {
        out += static_cast<char>(0xC0 | (cp >> 6));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    } else if (cp < 0x10000) {
        out += static_cast<char>(0xE0 | (cp >> 12));
        out += static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    } else {
        out += static_cast<char>(0xF0 | (cp >> 18));
        out += static_cast<char>(0x80 | ((cp >> 12) & 0x3F));
        out += static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    }
}
inline constexpr uint32_t kReplacement = 0xFFFD;
inline bool is_control(uint32_t cp) { return cp < 0x20 || (cp >= 0x7F && cp < 0xA0); }
}  // namespace text_detail

// Decodes UTF-8 and re-encodes it: every invalid, overlong or surrogate sequence
// becomes U+FFFD. With keep_controls false, C0/DEL/C1 controls become U+FFFD too
// (file strings must not break report lines); true keeps them (whole Markdown text).
inline std::string utf8_sanitize(std::string_view s, bool keep_controls) {
    using namespace text_detail;
    std::string out;
    out.reserve(s.size());
    size_t i = 0;
    while (i < s.size()) {
        const uint8_t c = static_cast<uint8_t>(s[i]);
        uint32_t cp = 0;
        size_t n = 0;
        if (c < 0x80) { cp = c; n = 1; }
        else if (c >= 0xC2 && c <= 0xDF) { cp = c & 0x1F; n = 2; }
        else if (c >= 0xE0 && c <= 0xEF) { cp = c & 0x0F; n = 3; }
        else if (c >= 0xF0 && c <= 0xF4) { cp = c & 0x07; n = 4; }
        bool ok = n != 0 && i + n <= s.size();
        for (size_t k = 1; ok && k < n; ++k) {
            const uint8_t cc = static_cast<uint8_t>(s[i + k]);
            if ((cc & 0xC0) != 0x80) ok = false;
            else cp = (cp << 6) | (cc & 0x3F);
        }
        if (ok && ((n == 3 && (cp < 0x800 || (cp >= 0xD800 && cp <= 0xDFFF))) || (n == 4 && (cp < 0x10000 || cp > 0x10FFFF))))
            ok = false;
        if (!ok) {             // one replacement per bad lead byte, then resync
            put_cp(out, kReplacement);
            ++i;
            continue;
        }
        put_cp(out, !keep_controls && is_control(cp) ? kReplacement : cp);
        i += n;
    }
    return out;
}

// A file string of unknown encoding for display (TIFF ASCII, libtiff messages):
// valid UTF-8 is kept, anything else and every control character becomes U+FFFD.
inline std::string file_text(std::string_view raw) { return utf8_sanitize(raw, false); }

// ISO 8859-1 bytes (PNG tEXt/zTXt/iTXt keywords, tEXt/zTXt text) as UTF-8 for display;
// control characters become U+FFFD.
inline std::string latin1_text(const uint8_t* p, size_t n) {
    using namespace text_detail;
    std::string out;
    out.reserve(n);
    for (size_t i = 0; i < n; ++i) put_cp(out, is_control(p[i]) ? kReplacement : p[i]);
    return out;
}

}  // namespace iat
