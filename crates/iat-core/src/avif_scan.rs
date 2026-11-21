//! 獨立的 AVIF（ISOBMFF）掃描（移植 C++ 版 `src/engine/avif_scan.*`，不使用解析器）：頂層 box 的
//! 範圍與截斷、ftyp 的 brand，以及 meta/iprp/ipco 下的色彩 property（`colr` nclx／prof）。
//! 容器完整性由這裡自己判定；item 與 property 的解析交給 [`crate::avif_parse`]（libavif 的行為）。
//!
//! 每次讀取都計入 `bytes_scanned`，讀法與 C++ 版逐一相同（報告的 coverage 要一致）。

use crate::input::Input;
use crate::report::{Issue, code};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvifNclx {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

#[derive(Clone, Debug)]
pub struct AvifScan {
    /// valid | warning | corrupt
    pub container_status: &'static str,
    pub issues: Vec<Issue>,
    pub bytes_scanned: u64,
    /// 依檔案順序的頂層 box（相鄰重複只記一次）
    pub boxes: Vec<String>,
    pub major_brand: String,
    /// 主要 brand 加上相容 brand
    pub brands: Vec<String>,
    /// 有 `avif` 或 `avis` brand
    pub is_avif: bool,
    /// `avis` brand 或有 `moov` box
    pub sequence: bool,
    pub has_meta: bool,
    /// ipco 裡每個 `colr` nclx property（依順序）
    pub nclx: Vec<AvifNclx>,
    /// `colr` prof／rICC property 的個數
    pub colr_prof: u32,
}

impl Default for AvifScan {
    fn default() -> Self {
        Self {
            container_status: "unverified",
            issues: Vec::new(),
            bytes_scanned: 0,
            boxes: Vec::new(),
            major_brand: String::new(),
            brands: Vec::new(),
            is_avif: false,
            sequence: false,
            has_meta: false,
            nclx: Vec::new(),
            colr_prof: 0,
        }
    }
}

fn corrupt(s: &mut AvifScan, c: &str, msg: String) {
    s.container_status = "corrupt";
    s.issues.push(Issue::new(c, "error", "", msg));
}

/// 可列印的 ASCII 原樣保留，其餘換成 `?`（C++ 版 `fourcc`）。
pub(crate) fn fourcc(p: &[u8]) -> String {
    p.iter().take(4).map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' }).collect()
}

fn be16(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[0], p[1]])
}

fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

#[derive(Clone, Debug)]
struct BoxHdr {
    kind: String,
    payload: u64,
    end: u64,
}

struct Walker<'a, 'b> {
    input: &'a Input<'b>,
    size: u64,
}

impl Walker<'_, '_> {
    fn bytes(&self, s: &mut AvifScan, off: u64, dst: &mut [u8]) -> bool {
        let Some(end) = off.checked_add(dst.len() as u64) else { return false };
        if end > self.size {
            return false;
        }
        match self.input.read_at(off, dst) {
            Ok(n) if n == dst.len() => {
                s.bytes_scanned += dst.len() as u64;
                true
            }
            _ => false,
        }
    }

    /// `[pos, limit)` 裡 `pos` 的 box 標頭；`top` 時 `limit` 是檔尾。
    fn header(&self, s: &mut AvifScan, pos: u64, limit: u64, top: bool) -> Option<BoxHdr> {
        let short = if top { code::TRUNCATED } else { code::STRUCTURE_ERROR };
        let mut h = [0u8; 16];
        if limit - pos < 8 || !self.bytes(s, pos, &mut h[..8]) {
            corrupt(s, short, format!("box header at offset {pos} is incomplete"));
            return None;
        }
        let kind = fourcc(&h[4..8]);
        let mut size = u64::from(be32(&h));
        let mut hdr = 8;
        if size == 1 {
            if limit - pos < 16 || !self.bytes(s, pos + 8, &mut h[8..16]) {
                corrupt(s, short, format!("'{kind}' largesize field is incomplete"));
                return None;
            }
            size = (u64::from(be32(&h[8..])) << 32) | u64::from(be32(&h[12..]));
            hdr = 16;
        } else if size == 0 {
            size = limit - pos; // 延伸到外層 box／檔案結尾
        }
        if size < hdr {
            corrupt(s, code::STRUCTURE_ERROR, format!("'{kind}' box size {size} is smaller than its header"));
            return None;
        }
        if size > limit - pos {
            let past = if top { "the end of the file" } else { "its parent box" };
            corrupt(s, short, format!("'{kind}' box ({size} bytes at offset {pos}) runs past {past}"));
            return None;
        }
        Some(BoxHdr { kind, payload: pos + hdr, end: pos + size })
    }

    /// `[begin, end)` 的子 box，逐一交給 `f`；子 box 損壞或 `f` 回傳 false 時停止並回傳 false。
    fn children(
        &self,
        s: &mut AvifScan,
        begin: u64,
        end: u64,
        f: &mut dyn FnMut(&Self, &mut AvifScan, &BoxHdr) -> bool,
    ) -> bool {
        let mut p = begin;
        while p < end {
            let Some(b) = self.header(s, p, end, false) else { return false };
            if !f(self, s, &b) {
                return false;
            }
            p = b.end;
        }
        true
    }
}

fn scan_meta(w: &Walker<'_, '_>, s: &mut AvifScan, meta: &BoxHdr) {
    // meta 是 FullBox：子 box 前面有 4 bytes 的版本與旗標
    if meta.end - meta.payload < 4 {
        corrupt(s, code::STRUCTURE_ERROR, "'meta' box is too short".into());
        return;
    }
    w.children(s, meta.payload + 4, meta.end, &mut |w, s, b| {
        if b.kind != "iprp" {
            return true;
        }
        w.children(s, b.payload, b.end, &mut |w, s, c| {
            if c.kind != "ipco" {
                return true;
            }
            w.children(s, c.payload, c.end, &mut |w, s, p| {
                if p.kind != "colr" || p.end - p.payload < 4 {
                    return true;
                }
                let mut t = [0u8; 11];
                if !w.bytes(s, p.payload, &mut t[..4]) {
                    return false;
                }
                if &t[..4] == b"nclx" {
                    if p.end - p.payload < 11 || !w.bytes(s, p.payload, &mut t) {
                        corrupt(s, code::STRUCTURE_ERROR, "'colr' nclx property is shorter than 11 bytes".into());
                        return false;
                    }
                    s.nclx.push(AvifNclx {
                        primaries: be16(&t[4..]),
                        transfer: be16(&t[6..]),
                        matrix: be16(&t[8..]),
                        full_range: t[10] & 0x80 != 0,
                    });
                } else if &t[..4] == b"prof" || &t[..4] == b"rICC" {
                    s.colr_prof += 1;
                }
                true
            })
        })
    });
}

/// AVIF irot（逆時針 90° 的次數，先套用）再 imir（axis 0 上下翻轉＝EXIF 4、axis 1 左右翻轉＝EXIF 2）
/// 對應的 EXIF 方向值；`imir_axis` 為 None 表示沒有 imir（C++ 版 D30 的表）。
pub fn avif_orientation(irot_angle: u8, imir_axis: Option<u8>) -> u32 {
    const TABLE: [[u32; 3]; 4] = [[1, 4, 2], [8, 5, 7], [3, 2, 4], [6, 7, 5]];
    let col = match imir_axis {
        None => 0,
        Some(0) => 1,
        Some(_) => 2,
    };
    TABLE[usize::from(irot_angle & 3)][col]
}

pub fn scan_avif(input: &Input<'_>) -> AvifScan {
    let size = input.size();
    let mut s = AvifScan { container_status: "valid", ..AvifScan::default() };
    let w = Walker { input, size };
    let mut first = true;
    let mut pos = 0u64;
    while pos < size {
        let Some(b) = w.header(&mut s, pos, size, true) else { return s };
        if s.boxes.last() != Some(&b.kind) {
            s.boxes.push(b.kind.clone());
        }
        if first {
            if b.kind != "ftyp" || b.end - b.payload < 8 {
                corrupt(&mut s, code::STRUCTURE_ERROR, "the file does not start with an 'ftyp' box".into());
                return s;
            }
            let mut ft = vec![0u8; (b.end - b.payload).min(4096) as usize];
            if !w.bytes(&mut s, b.payload, &mut ft) {
                corrupt(&mut s, code::TRUNCATED, "'ftyp' box could not be read".into());
                return s;
            }
            s.major_brand = fourcc(&ft);
            s.brands.push(s.major_brand.clone());
            let mut i = 8;
            while i + 4 <= ft.len() {
                s.brands.push(fourcc(&ft[i..]));
                i += 4;
            }
            for br in &s.brands {
                if br == "avif" || br == "avis" {
                    s.is_avif = true;
                }
                if br == "avis" {
                    s.sequence = true;
                }
            }
            first = false;
        } else if b.kind == "meta" {
            s.has_meta = true;
            scan_meta(&w, &mut s, &b);
            if s.container_status == "corrupt" {
                return s;
            }
        } else if b.kind == "moov" {
            s.sequence = true;
        }
        pos = b.end;
    }
    if first {
        corrupt(&mut s, code::TRUNCATED, "empty file".into());
    } else if !s.is_avif {
        corrupt(&mut s, code::STRUCTURE_ERROR, "ftyp has neither the 'avif' nor the 'avis' brand".into());
    } else if !s.has_meta {
        corrupt(&mut s, code::STRUCTURE_ERROR, "no 'meta' box (an AVIF image needs one)".into());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_table_matches_cpp() {
        assert_eq!(avif_orientation(0, None), 1);
        assert_eq!(avif_orientation(1, None), 8);
        assert_eq!(avif_orientation(0, Some(0)), 4);
        assert_eq!(avif_orientation(0, Some(1)), 2);
        assert_eq!(avif_orientation(3, Some(1)), 5);
    }

    #[test]
    fn fourcc_masks_unprintable() {
        assert_eq!(fourcc(b"ft\x01p"), "ft?p");
    }
}
