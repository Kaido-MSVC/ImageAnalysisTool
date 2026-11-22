//! 獨立的 WebP RIFF 掃描（移植 C++ 版 `src/engine/webp_scan.*`，不使用解碼器）。
//!
//! 容器完整性與 VP8X 旗標一致性只依 RIFF 結構判定：解碼器會接受少了補齊位元組的檔案，
//! 也會讓 VP8L 標頭的提示蓋過 VP8X 的 alpha 旗標，所以這些判斷不能交給解碼器。

use sha2::{Digest, Sha256};

use crate::input::Input;
use crate::limits::Limits;
use crate::report::{Issue, code};
use crate::text::hex;

#[derive(Clone, Debug)]
pub struct WebpScan {
    /// unverified | valid | warning | corrupt
    pub container_status: &'static str,
    /// valid | over_limit
    pub metadata_status: &'static str,
    pub issues: Vec<Issue>,
    pub bytes_scanned: u64,
    /// RIFF 酬載之後的位元組數（RIFF 被截斷時為 None）
    pub trailing_bytes: Option<u64>,
    /// 依檔案順序的 FourCC
    pub chunks: Vec<String>,

    /// 有 VP8X
    pub extended: bool,
    pub vp8x_flags: u8,
    pub canvas_width: u32,
    pub canvas_height: u32,
    /// VP8X 動畫旗標，或有 ANIM／ANMF chunk
    pub animated: bool,
    /// "VP8L" | "VP8 " | ""（沒有影像 chunk）
    pub image_chunk: &'static str,
    pub has_alph: bool,
    /// 取自 VP8L／VP8 位元串流標頭
    pub width: u32,
    pub height: u32,
    /// VP8L 的 alpha_is_used 位元
    pub vp8l_alpha_hint: bool,
    /// 第一個 VP8L chunk 內容的位置與長度（檔案裡實際的位元組）
    pub vp8l_payload: Option<(u64, u64)>,

    /// embedded | absent | over_budget
    pub icc_status: &'static str,
    pub icc: Vec<u8>,
    /// present | absent | over_limit
    pub exif_status: &'static str,
    pub exif: Vec<u8>,
    pub xmp_bytes: Option<u64>,
    pub xmp_sha256: String,
    pub unknown_chunks: Vec<String>,
}

impl Default for WebpScan {
    fn default() -> Self {
        Self {
            container_status: "unverified",
            metadata_status: "valid",
            issues: Vec::new(),
            bytes_scanned: 0,
            trailing_bytes: None,
            chunks: Vec::new(),
            extended: false,
            vp8x_flags: 0,
            canvas_width: 0,
            canvas_height: 0,
            animated: false,
            image_chunk: "",
            has_alph: false,
            width: 0,
            height: 0,
            vp8l_alpha_hint: false,
            vp8l_payload: None,
            icc_status: "absent",
            icc: Vec::new(),
            exif_status: "absent",
            exif: Vec::new(),
            xmp_bytes: None,
            xmp_sha256: String::new(),
            unknown_chunks: Vec::new(),
        }
    }
}

impl WebpScan {
    fn issue(&mut self, code: &str, sev: &str, msg: impl Into<String>) {
        self.issues.push(Issue::new(code, sev, "", msg));
    }
    fn corrupt(&mut self, code: &str, msg: impl Into<String>) {
        self.container_status = "corrupt";
        self.issue(code, "error", msg);
    }
    fn warn(&mut self, msg: impl Into<String>) {
        self.issue("webp_container_warning", "warning", msg);
    }
}

fn le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}
fn le24(p: &[u8]) -> u32 {
    u32::from(p[0]) | (u32::from(p[1]) << 8) | (u32::from(p[2]) << 16)
}

fn fourcc_text(p: &[u8]) -> String {
    p[..4].iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' }).collect()
}

struct Reader<'a, 'b> {
    f: &'a Input<'b>,
    size: u64,
    scanned: u64,
}

impl Reader<'_, '_> {
    fn bytes(&mut self, off: u64, dst: &mut [u8]) -> bool {
        let Some(end) = off.checked_add(dst.len() as u64) else { return false };
        if end > self.size {
            return false;
        }
        match self.f.read_at(off, dst) {
            Ok(n) if n == dst.len() => {
                self.scanned += dst.len() as u64;
                true
            }
            _ => false,
        }
    }
    fn payload(&mut self, off: u64, n: u64) -> Option<Vec<u8>> {
        let mut out = vec![0; usize::try_from(n).ok()?];
        (n == 0 || self.bytes(off, &mut out)).then_some(out)
    }
    /// 以 SHA-256 串流讀過酬載（不保留內容）。
    fn hash(&mut self, mut off: u64, mut n: u64) -> Option<String> {
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 65536];
        while n > 0 {
            let take = usize::try_from(n).map_or(buf.len(), |n| n.min(buf.len()));
            if !self.bytes(off, &mut buf[..take]) {
                return None;
            }
            h.update(&buf[..take]);
            off += take as u64;
            n -= take as u64;
        }
        Some(hex(&h.finalize()))
    }
}

pub fn scan_webp(f: &Input<'_>, lim: &Limits) -> WebpScan {
    let file_size = f.size();
    let mut s = WebpScan::default();
    let mut rd = Reader { f, size: file_size, scanned: 0 };
    scan_into(&mut s, &mut rd, file_size, lim);
    s.bytes_scanned = rd.scanned;
    s
}

fn scan_into(s: &mut WebpScan, rd: &mut Reader<'_, '_>, file_size: u64, lim: &Limits) {
    let mut h = [0u8; 12];
    if !rd.bytes(0, &mut h) {
        s.corrupt(code::TRUNCATED, "file shorter than a RIFF/WEBP header");
        return;
    }
    if &h[..4] != b"RIFF" || &h[8..12] != b"WEBP" {
        s.corrupt(code::STRUCTURE_ERROR, "not a RIFF WEBP file");
        return;
    }
    let riff_size = u64::from(le32(&h[4..]));
    let riff_end = 8 + riff_size;
    if riff_size < 4 {
        s.corrupt(code::STRUCTURE_ERROR, format!("RIFF size {riff_size} is too small"));
        return;
    }
    if riff_size & 1 == 1 {
        s.warn(format!("RIFF size {riff_size} is odd"));
    }
    if riff_end > file_size {
        s.corrupt(code::TRUNCATED, format!("RIFF declares {riff_end} bytes, file has {file_size}"));
        return;
    }
    let trailing = file_size - riff_end;
    s.trailing_bytes = Some(trailing);
    if trailing > 0 {
        s.issue(code::TRAILING_DATA, "warning", format!("{trailing} bytes after the RIFF payload"));
    }

    let (mut pos, mut meta_total) = (12u64, 0u64);
    let (mut images, mut iccp, mut exifs, mut xmps, mut anims) = (0, 0, 0, 0, 0);
    let (mut first, mut after_image) = (true, false);
    while pos < riff_end {
        let mut ch = [0u8; 8];
        if riff_end - pos < 8 || !rd.bytes(pos, &mut ch) {
            s.corrupt(code::TRUNCATED, format!("chunk header at offset {pos} runs past the RIFF payload"));
            return;
        }
        let tag: [u8; 4] = [ch[0], ch[1], ch[2], ch[3]];
        let tag = &tag;
        let size = u64::from(le32(&ch[4..]));
        let (data, end) = (pos + 8, pos + 8 + size);
        let padded = end + (size & 1);
        let name = fourcc_text(&ch);
        s.chunks.push(name.clone());
        if end > riff_end {
            s.corrupt(
                code::TRUNCATED,
                format!("chunk '{name}' at offset {pos} ({size} bytes) runs past the RIFF payload"),
            );
            return;
        }
        if padded > riff_end {
            // S4：解碼器容許少了補齊位元組，容器規則不容許
            s.corrupt(code::TRUNCATED, format!("chunk '{name}' has an odd size but its padding byte is missing"));
            return;
        }
        if first {
            first = false;
            if !matches!(tag, b"VP8 " | b"VP8L" | b"VP8X") {
                s.corrupt(code::STRUCTURE_ERROR, format!("first chunk is '{name}', expected VP8, VP8L or VP8X"));
                return;
            }
        } else if !s.extended {
            s.warn(format!("chunk '{name}' after the image in a simple-format file"));
        }
        match tag {
            b"VP8X" => {
                let mut x = [0u8; 10];
                if s.extended || pos != 12 {
                    s.corrupt(code::STRUCTURE_ERROR, "VP8X is not the first chunk or appears twice");
                    return;
                }
                if size < 10 || !rd.bytes(data, &mut x) {
                    s.corrupt(code::STRUCTURE_ERROR, "VP8X chunk shorter than 10 bytes");
                    return;
                }
                s.extended = true;
                s.vp8x_flags = x[0];
                s.canvas_width = le24(&x[4..]) + 1;
                s.canvas_height = le24(&x[7..]) + 1;
                if u64::from(s.canvas_width) * u64::from(s.canvas_height) > 0xFFFF_FFFF {
                    s.corrupt(code::STRUCTURE_ERROR, "VP8X canvas width x height exceeds 2^32 - 1");
                }
            }
            b"VP8L" | b"VP8 " => {
                images += 1;
                after_image = true;
                if images == 1 {
                    let lossless = tag == b"VP8L";
                    s.image_chunk = if lossless { "VP8L" } else { "VP8 " };
                    let mut b = [0u8; 10];
                    let need = if lossless { 5 } else { 10 };
                    if size < need as u64 || !rd.bytes(data, &mut b[..need]) {
                        s.corrupt(code::TRUNCATED, format!("{} chunk too short for its header", s.image_chunk));
                        return;
                    }
                    if lossless {
                        if b[0] != 0x2F {
                            s.corrupt(code::STRUCTURE_ERROR, "VP8L signature is not 0x2F");
                            return;
                        }
                        let v = le32(&b[1..]);
                        s.width = (v & 0x3FFF) + 1;
                        s.height = ((v >> 14) & 0x3FFF) + 1;
                        s.vp8l_alpha_hint = (v >> 28) & 1 != 0;
                        if v >> 29 != 0 {
                            s.corrupt(code::STRUCTURE_ERROR, format!("VP8L version {} (must be 0)", v >> 29));
                            return;
                        }
                        s.vp8l_payload = Some((data, size));
                    } else {
                        if b[3] != 0x9D || b[4] != 0x01 || b[5] != 0x2A {
                            s.corrupt(code::STRUCTURE_ERROR, "VP8 start code missing");
                            return;
                        }
                        s.width = (u32::from(b[6]) | (u32::from(b[7]) << 8)) & 0x3FFF;
                        s.height = (u32::from(b[8]) | (u32::from(b[9]) << 8)) & 0x3FFF;
                    }
                }
            }
            b"ALPH" => {
                s.has_alph = true;
                if after_image {
                    s.warn("ALPH chunk after the image data");
                }
            }
            b"ANIM" | b"ANMF" => {
                anims += 1;
                s.animated = true;
            }
            b"ICCP" => {
                iccp += 1;
                if iccp > 1 {
                    s.warn("more than one ICCP chunk (the first is used)");
                } else {
                    if s.chunks.len() != 2 {
                        s.warn("ICCP is not directly after VP8X");
                    }
                    if size > lim.metadata_item_bytes() || meta_total + size > lim.metadata_file_bytes() {
                        s.icc_status = "over_budget";
                        s.metadata_status = "over_limit";
                        s.issue(
                            code::METADATA_OVER_LIMIT,
                            "warning",
                            format!("ICC profile of {size} bytes exceeds the metadata budget"),
                        );
                    } else if let Some(p) = rd.payload(data, size) {
                        s.icc = p;
                        s.icc_status = "embedded";
                        meta_total += size;
                    } else {
                        s.corrupt(code::TRUNCATED, "ICCP payload unreadable");
                        return;
                    }
                }
            }
            b"EXIF" => {
                exifs += 1;
                if exifs > 1 {
                    s.warn("more than one EXIF chunk (the first is used)");
                } else if size > lim.metadata_item_bytes() || meta_total + size > lim.metadata_file_bytes() {
                    s.exif_status = "over_limit";
                    s.metadata_status = "over_limit";
                } else if let Some(p) = rd.payload(data, size) {
                    s.exif = p;
                    s.exif_status = "present";
                    meta_total += size;
                } else {
                    s.corrupt(code::TRUNCATED, "EXIF payload unreadable");
                    return;
                }
            }
            b"XMP " => {
                xmps += 1;
                if xmps > 1 {
                    s.warn("more than one XMP chunk (the first is used)");
                } else if let Some(hx) = rd.hash(data, size) {
                    s.xmp_sha256 = hx;
                    s.xmp_bytes = Some(size);
                } else {
                    s.corrupt(code::TRUNCATED, "XMP payload unreadable");
                    return;
                }
            }
            _ => s.unknown_chunks.push(name),
        }
        pos = padded;
    }

    if s.container_status == "corrupt" {
        return;
    }
    if images == 0 && !s.animated {
        s.corrupt(code::STRUCTURE_ERROR, "no VP8 or VP8L image chunk");
        return;
    }
    if images > 1 && !s.animated {
        s.corrupt(code::STRUCTURE_ERROR, format!("{images} image chunks in a still image"));
        return;
    }
    if s.extended {
        let fl = s.vp8x_flags;
        if fl & 0x02 != 0 {
            s.animated = true;
        }
        if (fl & 0x20 != 0) != (iccp > 0) {
            s.warn("VP8X ICC flag does not match the presence of an ICCP chunk");
        }
        if (fl & 0x08 != 0) != (exifs > 0) {
            s.warn("VP8X EXIF flag does not match the presence of an EXIF chunk");
        }
        if (fl & 0x04 != 0) != (xmps > 0) {
            s.warn("VP8X XMP flag does not match the presence of an XMP chunk");
        }
        if (fl & 0x02 != 0) != (anims > 0) {
            s.warn("VP8X animation flag does not match the presence of ANIM/ANMF chunks");
        }
        if !s.animated && images == 1 && (s.canvas_width != s.width || s.canvas_height != s.height) {
            s.corrupt(
                code::STRUCTURE_ERROR,
                format!(
                    "VP8X canvas {}x{} differs from the image {}x{}",
                    s.canvas_width, s.canvas_height, s.width, s.height
                ),
            );
            return;
        }
        if s.image_chunk == "VP8L" && (fl & 0x10 != 0) != s.vp8l_alpha_hint {
            let msg = format!(
                "VP8X alpha flag is {} but the VP8L alpha_is_used hint is {}",
                if fl & 0x10 != 0 { "set" } else { "clear" },
                if s.vp8l_alpha_hint { "set" } else { "clear" }
            );
            s.issue(code::ALPHA_HINT_MISMATCH, "warning", msg);
        }
    }
    let warned = s.issues.iter().any(|i| i.severity == "warning");
    s.container_status = if warned { "warning" } else { "valid" };
}

/// 解碼器的輸入：只含 VP8L chunk 的簡單格式 RIFF，內容是檔案裡實際的位元組，不補資料、不改長度。
///
/// 唯一的修改是把 VP8L 標頭的 alpha_is_used 位元設為 1。image-webp 解碼位元串流時不使用這個位元，
/// 只拿它決定要不要丟掉 alpha；設為 1 才能一律拿到完整的 RGBA（D-R15）。內容不到 5 bytes
/// （放不下標頭）時照原樣包裝，由解碼器回報錯誤。
pub fn vp8l_decoder_input(mut payload: Vec<u8>) -> Vec<u8> {
    if payload.len() >= 5 {
        payload[4] |= 0x10;
    }
    let pad = payload.len() & 1;
    let mut out = Vec::with_capacity(20 + payload.len() + pad);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((4 + 8 + payload.len() + pad) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBPVP8L");
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    if pad == 1 {
        out.push(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_input_keeps_payload_and_sets_only_the_hint_bit() {
        // 偶數長度：RIFF 大小 = 4 + 8 + 內容
        let p = vec![0x2F, 0, 0, 0, 0x00, 0xAA];
        let w = vp8l_decoder_input(p.clone());
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(le32(&w[4..]) as usize, 4 + 8 + p.len());
        assert_eq!(w.len(), 8 + le32(&w[4..]) as usize);
        assert_eq!(&w[8..16], b"WEBPVP8L");
        assert_eq!(le32(&w[16..]) as usize, p.len(), "chunk size is the real payload length");
        assert_eq!(w[20 + 4], 0x10, "alpha_is_used set");
        let mut back = w[20..].to_vec();
        back[4] &= !0x10;
        assert_eq!(back, p, "every other byte unchanged");
    }

    #[test]
    fn decoder_input_pads_odd_length() {
        let p = vec![0x2F, 1, 2, 3, 0x10, 5, 6];
        let w = vp8l_decoder_input(p.clone());
        assert_eq!(le32(&w[16..]) as usize, 7, "chunk size stays odd (real length)");
        assert_eq!(le32(&w[4..]) as usize, 4 + 8 + 7 + 1, "RIFF size includes the padding byte");
        assert_eq!(w.len(), 20 + 7 + 1);
        assert_eq!(*w.last().unwrap(), 0, "padding byte is zero");
        assert_eq!(&w[20..27], &p[..], "hint already set: payload unchanged");
    }

    #[test]
    fn decoder_input_truncated_chunk_is_reported_by_the_decoder() {
        let f = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/generated/s3_webp_rgba8_icc.webp"),
        )
        .expect("fixture");
        let (mut pos, mut payload) = (12usize, Vec::new());
        while pos + 8 <= f.len() {
            let size = le32(&f[pos + 4..]) as usize;
            if &f[pos..pos + 4] == b"VP8L" {
                payload = f[pos + 8..pos + 8 + size].to_vec();
                break;
            }
            pos += 8 + size + (size & 1);
        }
        assert!(payload.len() > 20, "fixture has a VP8L chunk");
        let decode = |p: Vec<u8>| -> Result<(), image_webp::DecodingError> {
            let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(vp8l_decoder_input(p)))?;
            let mut buf = vec![0; d.output_buffer_size().expect("small image")];
            d.read_image(&mut buf)
        };
        assert!(decode(payload.clone()).is_ok(), "the whole chunk decodes");
        let half = payload[..payload.len() / 2].to_vec();
        let w = vp8l_decoder_input(half.clone());
        assert_eq!(le32(&w[16..]) as usize, half.len(), "the cut length is passed on unchanged");
        assert!(decode(half).is_err(), "a cut bitstream is an error, never padded into an image");
        // 放不下標頭的內容照原樣包裝，由解碼器回報錯誤
        assert!(decode(vec![0x2F, 0, 0]).is_err());
    }
}
