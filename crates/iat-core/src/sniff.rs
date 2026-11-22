//! 依內容辨識格式（對照 C++ 版 `analyze.cpp` 的 `sniff_format`），從不看副檔名。

/// 回傳報告用的格式名稱；`head` 是檔案開頭最多 64 bytes。
pub fn sniff_format(head: &[u8]) -> &'static str {
    let b = &head[..head.len().min(64)];
    let n = b.len();
    let at = |off: usize, sig: &[u8]| b.len() >= off + sig.len() && &b[off..off + sig.len()] == sig;
    if n >= 6 && at(0, b"8BPS\0\x01") {
        return "psd";
    }
    if n >= 6 && at(0, b"8BPS\0\x02") {
        return "psb";
    }
    if n >= 6 && at(0, b"8BPS") {
        return "psd"; // 版本不明：交給掃描器回報
    }
    if n >= 16 && at(4, b"ftyp") {
        // ISOBMFF：主要或相容 brand（在讀到的範圍內）為 avif／avis 時判為 AVIF。
        let bx = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        let end = n.min(if bx >= 16 { bx } else { 16 });
        let mut i = 8;
        while i + 4 <= end {
            if i != 12 && (at(i, b"avif") || at(i, b"avis")) {
                return "avif";
            }
            i += 4;
        }
    }
    if n >= 8 && at(0, b"\x89PNG\r\n\x1a\n") {
        return "png";
    }
    if n >= 4 && (at(0, b"II*\0") || at(0, b"MM\0*")) {
        return "tiff";
    }
    if n >= 4 && (at(0, b"II+\0") || at(0, b"MM\0+")) {
        return "bigtiff";
    }
    if n >= 2 && b[0] == 0xFF && b[1] == 0x0A {
        return "jxl_codestream";
    }
    if n >= 12 && at(0, b"\0\0\0\x0CJXL \r\n\x87\n") {
        return "jxl_container";
    }
    if n >= 12 && at(0, b"RIFF") && at(8, b"WEBP") {
        return "webp";
    }
    if n >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF {
        return "jpeg";
    }
    "unknown"
}

/// 副檔名對應的格式（比對 extension_mismatch 用）。
pub fn ext_format(name: &str) -> &'static str {
    let e = name.rfind('.').map_or(String::new(), |d| name[d + 1..].to_lowercase());
    match e.as_str() {
        "png" => "png",
        "tif" | "tiff" => "tiff",
        "jxl" => "jxl",
        "webp" => "webp",
        "psd" => "psd",
        "psb" => "psb",
        "avif" => "avif",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        assert_eq!(sniff_format(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"), "png");
        assert_eq!(sniff_format(b"II*\0\x08\0\0\0"), "tiff");
        assert_eq!(sniff_format(b"MM\0+\0\x08\0\0"), "bigtiff");
        assert_eq!(sniff_format(b"RIFF\0\0\0\0WEBPVP8L"), "webp");
        assert_eq!(sniff_format(b"\xff\x0a\0"), "jxl_codestream");
        assert_eq!(sniff_format(b"8BPS\0\x02\0\0"), "psb");
        assert_eq!(sniff_format(b"\0\0\0\x1cftypmif1\0\0\0\0mif1avif"), "avif");
        assert_eq!(sniff_format(b"\0\0\0\x18ftypavif\0\0\0\0mif1"), "avif");
        assert_eq!(sniff_format(b"\xff\xd8\xff\xe0"), "jpeg");
        assert_eq!(sniff_format(b"hello"), "unknown");
        assert_eq!(sniff_format(b""), "unknown");
    }

    #[test]
    fn extensions() {
        assert_eq!(ext_format("a.TIF"), "tiff");
        assert_eq!(ext_format("noext"), "other");
    }
}
