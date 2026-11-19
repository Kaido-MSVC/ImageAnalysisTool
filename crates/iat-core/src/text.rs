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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(hex(&[0x00, 0xab, 0x7f]), "00ab7f");
        assert_eq!(latin1_text(b"Caf\xe9\x01"), "Café\u{FFFD}");
        assert_eq!(short_hash("0123456789abcdef0123"), "0123456789abcdef");
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
