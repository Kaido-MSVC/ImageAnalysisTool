//! 報告用的文字工具（對應 C++ 版 `common/text.hpp` 與 `win_util` 的 hex／sha256_hex）。

use sha2::{Digest, Sha256};

pub fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// 雜湊字串的前 16 個字元（顯示用）。
pub fn short_hash(h: &str) -> &str {
    &h[..h.len().min(16)]
}

fn is_control(cp: u32) -> bool {
    cp < 0x20 || (0x7F..0xA0).contains(&cp)
}

/// ISO 8859-1 位元組（PNG 文字 chunk 的關鍵字）轉成顯示用的 UTF-8；控制字元換成 U+FFFD。
pub fn latin1_text(raw: &[u8]) -> String {
    raw.iter().map(|&b| if is_control(b as u32) { '\u{FFFD}' } else { b as char }).collect()
}

/// 檔案裡編碼不明的字串（TIFF ASCII 欄位、解碼器訊息）轉成顯示用的 UTF-8（C++ 版 `file_text`）：
/// 有效的 UTF-8 保留；無效、過長編碼或代理區的序列每個起始位元組換成一個 U+FFFD 後重新同步；
/// 控制字元也換成 U+FFFD。和 `String::from_utf8_lossy` 的替換粒度不同，所以自己解碼。
pub fn file_text(raw: &[u8]) -> String {
    utf8_sanitize(raw, false)
}

/// C++ 版 `utf8_sanitize`：無效的 UTF-8 序列同 [`file_text`]；`keep_controls` 為 true 時保留控制字元
/// （worker 的 stderr 照原樣含換行）。
pub fn utf8_sanitize(raw: &[u8], keep_controls: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        let (mut cp, n) = match c {
            0x00..=0x7F => (u32::from(c), 1),
            0xC2..=0xDF => (u32::from(c & 0x1F), 2),
            0xE0..=0xEF => (u32::from(c & 0x0F), 3),
            0xF0..=0xF4 => (u32::from(c & 0x07), 4),
            _ => (0, 0),
        };
        let mut ok = n != 0 && i + n <= raw.len();
        for k in 1..n {
            match raw.get(i + k) {
                Some(&cc) if ok && cc & 0xC0 == 0x80 => cp = (cp << 6) | u32::from(cc & 0x3F),
                _ => ok = false,
            }
        }
        if ok
            && ((n == 3 && (cp < 0x800 || (0xD800..=0xDFFF).contains(&cp)))
                || (n == 4 && !(0x10000..=0x10FFFF).contains(&cp)))
        {
            ok = false;
        }
        if !ok {
            out.push('\u{FFFD}');
            i += 1;
            continue;
        }
        let ch = char::from_u32(cp).filter(|_| keep_controls || !is_control(cp)).unwrap_or('\u{FFFD}');
        out.push(ch);
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_text_replaces_like_cpp() {
        assert_eq!(file_text(b"2024:01:02 03:04:05"), "2024:01:02 03:04:05");
        assert_eq!(file_text("中文".as_bytes()), "中文");
        // 每個無效的起始位元組一個 U+FFFD；截斷的序列也是
        assert_eq!(file_text(b"a\xff\xfeb"), "a\u{FFFD}\u{FFFD}b");
        assert_eq!(file_text(b"\xe4\xb8"), "\u{FFFD}\u{FFFD}");
        // 過長編碼與代理區
        assert_eq!(file_text(b"\xe0\x80\x80"), "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(file_text(b"\xed\xa0\x80"), "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(file_text(b"x\ty\x7f"), "x\u{FFFD}y\u{FFFD}");
    }

    #[test]
    fn helpers() {
        assert_eq!(hex(&[0x00, 0xab, 0x7f]), "00ab7f");
        assert_eq!(latin1_text(b"Caf\xe9\x01"), "Café\u{FFFD}");
        assert_eq!(short_hash("0123456789abcdef0123"), "0123456789abcdef");
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
