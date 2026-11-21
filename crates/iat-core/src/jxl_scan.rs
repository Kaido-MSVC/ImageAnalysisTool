//! 獨立的 JPEG XL 容器掃描（移植 C++ 版 `src/engine/jxl_scan.*`，ISO/IEC 18181-2 的 box 結構，不使用解碼器）。
//! 裸 codestream（FF 0A）沒有 box，結尾由解碼器決定（見 [`crate::jxl_side`]）。
//! 所有位移與長度都是 64 位元並做溢位檢查；檔案內容不可信，畸形大小不會 panic。

use sha2::{Digest, Sha256};

use crate::input::Input;
use crate::limits::Limits;
use crate::report::{Issue, code};
use crate::text::hex;

#[derive(Clone, Debug)]
pub struct JxlScan {
    /// unverified | valid | warning | corrupt
    pub container_status: &'static str,
    /// valid | over_limit
    pub metadata_status: &'static str,
    pub issues: Vec<Issue>,
    pub bytes_scanned: u64,
    /// false：裸 codestream（FF 0A）
    pub container: bool,
    /// 依檔案順序的 box 類型（壓縮的 box 記為 "brob:Exif"）
    pub boxes: Vec<String>,
    /// jxlc 的內容或所有 jxlp 內容的總長（只有容器）
    pub codestream_bytes: u64,
    pub jxlp_parts: u32,
    /// codestream 各段在檔案中的範圍（位移, 長度）；裸 codestream 為整個檔案
    pub codestream_ranges: Vec<(u64, u64)>,
    /// 有 jxll box
    pub level_box: bool,
    /// jxll 的值，沒有時為 5
    pub level: u8,
    /// present | absent | over_limit | compressed
    pub exif_status: &'static str,
    /// TIFF 資料（已去掉 4 bytes 的位移前綴）
    pub exif: Vec<u8>,
    pub xmp_bytes: Option<u64>,
    pub xmp_sha256: String,
    /// 有 JPEG 重建資料
    pub jbrd: bool,
    pub other_boxes: Vec<String>,
}

impl Default for JxlScan {
    fn default() -> Self {
        Self {
            container_status: "unverified",
            metadata_status: "valid",
            issues: Vec::new(),
            bytes_scanned: 0,
            container: false,
            boxes: Vec::new(),
            codestream_bytes: 0,
            jxlp_parts: 0,
            codestream_ranges: Vec::new(),
            level_box: false,
            level: 5,
            exif_status: "absent",
            exif: Vec::new(),
            xmp_bytes: None,
            xmp_sha256: String::new(),
            jbrd: false,
            other_boxes: Vec::new(),
        }
    }
}

const SIGNATURE: &[u8; 12] = b"\0\0\0\x0CJXL \r\n\x87\n";

fn be(p: &[u8]) -> u64 {
    p.iter().fold(0, |v, &b| (v << 8) | u64::from(b))
}

fn type_text(p: &[u8]) -> String {
    p.iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '?' }).collect()
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

    fn hash(&mut self, mut off: u64, mut n: u64) -> Option<String> {
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 65536];
        while n > 0 {
            let take = n.min(buf.len() as u64) as usize;
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

fn issue(s: &mut JxlScan, code: &str, sev: &str, msg: String) {
    s.issues.push(Issue::new(code, sev, "", msg));
}

fn corrupt(s: &mut JxlScan, code: &str, msg: String) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}

fn warn(s: &mut JxlScan, msg: &str) {
    issue(s, "jxl_container_warning", "warning", msg.into());
}

pub fn scan_jxl(f: &Input<'_>, lim: &Limits) -> JxlScan {
    let mut s = JxlScan::default();
    let mut rd = Reader { f, size: f.size(), scanned: 0 };
    walk(&mut s, &mut rd, lim);
    s.bytes_scanned = rd.scanned;
    s
}

fn walk(s: &mut JxlScan, rd: &mut Reader<'_, '_>, lim: &Limits) {
    let file_size = rd.size;
    let mut h = [0u8; 12];
    if !rd.bytes(0, &mut h[..2]) {
        corrupt(s, code::TRUNCATED, "file shorter than a JPEG XL signature".into());
        return;
    }
    if h[..2] == [0xFF, 0x0A] {
        s.container_status = "valid"; // 裸 codestream 的結尾由解碼器決定
        s.codestream_ranges.push((0, file_size));
        return;
    }
    if !rd.bytes(0, &mut h) || &h != SIGNATURE {
        corrupt(s, code::STRUCTURE_ERROR, "not a JPEG XL codestream or container".into());
        return;
    }
    s.container = true;
    let mut pos = 0u64;
    let mut meta_total = 0u64;
    let (mut have_jxlc, mut last_part_seen, mut ftyp_ok) = (false, false, false);
    let mut expected_part = 0u32;
    let (mut exifs, mut xmps) = (0u32, 0u32);
    while pos < file_size {
        let mut bh = [0u8; 16];
        if file_size - pos < 8 || !rd.bytes(pos, &mut bh[..8]) {
            corrupt(
                s,
                code::TRUNCATED,
                format!("box header at offset {pos} is incomplete ({} bytes left)", file_size - pos),
            );
            return;
        }
        let mut size = be(&bh[..4]);
        let ty: [u8; 4] = [bh[4], bh[5], bh[6], bh[7]];
        let mut header = 8u64;
        if size == 1 {
            let (head, tail) = bh.split_at_mut(8);
            if file_size - pos < 16 || !rd.bytes(pos + 8, tail) {
                corrupt(s, code::TRUNCATED, "box largesize field is incomplete".into());
                return;
            }
            size = be(&tail[..8]);
            header = 16;
            if size < 16 {
                corrupt(
                    s,
                    code::STRUCTURE_ERROR,
                    format!("box '{}' largesize {size} is too small", type_text(&head[4..8])),
                );
                return;
            }
        } else if size == 0 {
            size = file_size - pos; // 最後一個 box 延伸到檔尾
        } else if size < 8 {
            corrupt(s, code::STRUCTURE_ERROR, format!("box '{}' size {size} is too small", type_text(&ty)));
            return;
        }
        let end = match pos.checked_add(size) {
            Some(e) if e <= file_size => e,
            _ => {
                corrupt(
                    s,
                    code::TRUNCATED,
                    format!("box '{}' at offset {pos} ({size} bytes) runs past the end of the file", type_text(&ty)),
                );
                return;
            }
        };
        let data = pos + header;
        let len = size - header;
        let mut shown = type_text(&ty);
        if pos == 0 {
            // 簽章 box，前面已檢查
        } else if &ty == b"ftyp" {
            let mut b = [0u8; 4];
            if s.boxes.len() != 1 {
                warn(s, "ftyp is not the second box");
            }
            if len < 4 || !rd.bytes(data, &mut b) || &b != b"jxl " {
                corrupt(s, code::STRUCTURE_ERROR, "ftyp major brand is not 'jxl '".into());
                return;
            }
            ftyp_ok = true;
        } else if &ty == b"jxll" {
            let mut b = [0u8; 1];
            if len != 1 || !rd.bytes(data, &mut b) {
                warn(s, "jxll box does not hold exactly one byte");
            } else {
                s.level_box = true;
                s.level = b[0];
            }
        } else if &ty == b"jxlc" {
            if have_jxlc || s.jxlp_parts > 0 {
                corrupt(s, code::STRUCTURE_ERROR, "more than one codestream box (jxlc/jxlp)".into());
                return;
            }
            have_jxlc = true;
            s.codestream_bytes = len;
            s.codestream_ranges.push((data, len));
        } else if &ty == b"jxlp" {
            let mut b = [0u8; 4];
            if have_jxlc {
                corrupt(s, code::STRUCTURE_ERROR, "jxlp together with jxlc".into());
                return;
            }
            if last_part_seen {
                corrupt(s, code::STRUCTURE_ERROR, "jxlp after the part marked last".into());
                return;
            }
            if len < 4 || !rd.bytes(data, &mut b) {
                corrupt(s, code::TRUNCATED, "jxlp index is missing".into());
                return;
            }
            let idx = be(&b) as u32;
            if idx & 0x7FFF_FFFF != expected_part {
                corrupt(
                    s,
                    code::STRUCTURE_ERROR,
                    format!("jxlp index {}, expected {expected_part}", idx & 0x7FFF_FFFF),
                );
                return;
            }
            expected_part += 1;
            s.jxlp_parts += 1;
            last_part_seen = idx & 0x8000_0000 != 0;
            s.codestream_bytes += len - 4;
            s.codestream_ranges.push((data + 4, len - 4));
        } else if &ty == b"brob" {
            let mut b = [0u8; 4];
            if len < 4 || !rd.bytes(data, &mut b) {
                corrupt(s, code::TRUNCATED, "brob inner type is missing".into());
                return;
            }
            shown = format!("brob:{}", type_text(&b));
            if matches!(&b, b"jxlc" | b"jxlp" | b"brob" | b"JXL " | b"ftyp" | b"jxll") {
                corrupt(s, code::STRUCTURE_ERROR, format!("brob may not compress a '{}' box", type_text(&b)));
                return;
            }
            if &b == b"Exif" {
                exifs += 1;
                if s.exif_status == "absent" {
                    s.exif_status = "compressed";
                }
            } else if &b == b"xml " {
                xmps += 1;
            }
        } else if &ty == b"Exif" {
            exifs += 1;
            if exifs > 1 {
                warn(s, "more than one Exif box (the first is used)");
            } else if len < 4 {
                s.exif_status = "present";
                warn(s, "Exif box shorter than its 4-byte offset");
            } else if len > lim.metadata_item_bytes() || meta_total.saturating_add(len) > lim.metadata_file_bytes() {
                s.exif_status = "over_limit";
                s.metadata_status = "over_limit";
            } else {
                let mut e = vec![0u8; len as usize];
                if !rd.bytes(data, &mut e) {
                    corrupt(s, code::TRUNCATED, "Exif payload unreadable".into());
                    return;
                }
                // TIFF 檔頭位於 4 bytes 位移欄位之後
                let off = be(&e[..4]);
                if off > len - 4 {
                    s.exif_status = "present";
                    warn(s, "Exif TIFF header offset lies outside the box");
                } else {
                    s.exif = e.get(4 + off as usize..).map(<[u8]>::to_vec).unwrap_or_default();
                    s.exif_status = "present";
                }
                meta_total += len;
            }
        } else if &ty == b"xml " {
            xmps += 1;
            if xmps > 1 {
                warn(s, "more than one xml box (the first is used)");
            } else {
                match rd.hash(data, len) {
                    Some(h) => {
                        s.xmp_sha256 = h;
                        s.xmp_bytes = Some(len);
                    }
                    None => {
                        corrupt(s, code::TRUNCATED, "xml payload unreadable".into());
                        return;
                    }
                }
            }
        } else if &ty == b"jbrd" {
            s.jbrd = true;
        } else {
            s.other_boxes.push(shown.clone());
        }
        s.boxes.push(shown);
        if end == pos {
            break;
        }
        pos = end;
    }
    if !ftyp_ok {
        corrupt(s, code::STRUCTURE_ERROR, "ftyp box missing".into());
        return;
    }
    if !have_jxlc && s.jxlp_parts == 0 {
        corrupt(s, code::STRUCTURE_ERROR, "no codestream box (jxlc or jxlp)".into());
        return;
    }
    if s.jxlp_parts > 0 && !last_part_seen {
        corrupt(s, code::TRUNCATED, "the last jxlp part (index with the high bit set) is missing".into());
        return;
    }
    s.container_status = if s.issues.iter().any(|i| i.severity == "warning") { "warning" } else { "valid" };
}
