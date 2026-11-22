//! 獨立的 TIFF／BigTIFF 容器掃描（移植 C++ 版 `src/engine/tiff_scan.*`，不使用解碼器）。
//! 檢查檔頭、第一個 IFD、每個 tag 的值範圍、每個 strip／tile 的 offset 加 byte count 是否在檔案內，
//! 以及 IFD 鏈。所有位移與長度都是 64 位元並做溢位檢查；檔案內容不可信，任何畸形大小都不會 panic。

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::input::Input;
use crate::limits::Limits;
use crate::report::{Count, IccInfo, Issue, code};
use crate::text::{file_text, hex, sha256_hex};
use crate::u64_to_usize;

/// 真實的 IFD 遠少於 100 個項目。
const MAX_ENTRIES: u64 = 4096;

/// 掃描小欄位時一次讀進來的檔案視窗大小（只影響系統呼叫次數，不影響 bytes_scanned）。
const CACHE_BYTES: usize = 64 << 10;

/// TIFF 欄位型別的大小；不認得的型別為 0。
pub fn type_size(t: u16) -> u32 {
    match t {
        1 | 2 | 6 | 7 => 1,         // BYTE ASCII SBYTE UNDEFINED
        3 | 8 => 2,                 // SHORT SSHORT
        4 | 9 | 11 | 13 => 4,       // LONG SLONG FLOAT IFD
        5 | 10 | 12 | 16..=18 => 8, // RATIONAL SRATIONAL DOUBLE LONG8 SLONG8 IFD8
        _ => 0,
    }
}

/// 一個 IFD 項目。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub tag: u16,
    pub typ: u16,
    pub count: u64,
    /// 值的檔案位移（值放在項目內時是項目內的位置）
    pub value_offset: u64,
    /// count × 型別大小
    pub bytes: u64,
    /// 值是否放在項目內（classic 4 bytes、BigTIFF 8 bytes 以內）
    pub inline: bool,
}

#[derive(Clone, Debug)]
pub struct TiffScan {
    /// unverified | valid | warning | corrupt
    pub container_status: &'static str,
    /// valid | parse_failed | over_limit
    pub metadata_status: &'static str,
    pub issues: Vec<Issue>,
    pub bytes_scanned: u64,

    pub header_ok: bool,
    /// BigTIFF（64 位元位移）
    pub big: bool,
    pub little_endian: bool,
    /// 第一個 IFD 可讀
    pub has_ifd: bool,
    pub ifd_offset: u64,
    /// 只有一張影像時為 0
    pub next_ifd: u64,
    /// 有 tag 330
    pub subifds: bool,

    pub width: u32,
    pub height: u32,
    /// 共同的 BitsPerSample；各聲道不同時為 0
    pub bits: u16,
    pub spp: u16,
    pub compression: u16,
    pub photometric: u16,
    pub planar: u16,
    pub sample_format: u16,
    pub predictor: u16,
    pub fill_order: u16,
    /// tag 274 的原始值
    pub orientation: Option<u16>,
    pub extra_samples: Vec<u16>,
    pub tiled: bool,
    pub tile_width: u32,
    pub tile_length: u32,
    pub rows_per_strip: u32,
    /// IFD 列出的 strip 或 tile 數
    pub blocks: u64,
    /// 影像資料用到的最高位元組
    pub data_end: u64,

    pub icc: IccInfo,
    /// 有出現的 WhitePoint／PrimaryChromaticities／TransferFunction
    pub colorimetry: Vec<String>,

    // metadata 摘要（只放雜湊、大小與小的值）
    pub exif: Option<String>,
    pub xmp: Option<String>,
    pub text: Option<String>,
    pub resolution: Option<String>,
    pub datetime: Option<String>,
    pub exif_status: &'static str,
    pub xmp_status: &'static str,
    pub text_status: &'static str,
    pub other_tags: Vec<String>,
    /// 依檔案順序的 tag 編號（報告用）
    pub tag_list: Vec<String>,

    /// 第一個 IFD 的項目個數
    pub entry_count: u64,
    /// 型別認得的項目，依檔案順序（含重複的 tag）
    pub entries: Vec<Entry>,
    /// 每個 tag 第一次出現的項目
    pub tags: BTreeMap<u16, Entry>,
}

impl Default for TiffScan {
    fn default() -> Self {
        Self {
            container_status: "unverified",
            metadata_status: "valid",
            issues: Vec::new(),
            bytes_scanned: 0,
            header_ok: false,
            big: false,
            little_endian: true,
            has_ifd: false,
            ifd_offset: 0,
            next_ifd: 0,
            subifds: false,
            width: 0,
            height: 0,
            bits: 0,
            spp: 1,
            compression: 1,
            photometric: 0xFFFF,
            planar: 1,
            sample_format: 1,
            predictor: 1,
            fill_order: 1,
            orientation: None,
            extra_samples: Vec::new(),
            tiled: false,
            tile_width: 0,
            tile_length: 0,
            rows_per_strip: 0,
            blocks: 0,
            data_end: 0,
            icc: IccInfo::default(),
            colorimetry: Vec::new(),
            exif: None,
            xmp: None,
            text: None,
            resolution: None,
            datetime: None,
            exif_status: "present",
            xmp_status: "present",
            text_status: "present",
            other_tags: Vec::new(),
            tag_list: Vec::new(),
            entry_count: 0,
            entries: Vec::new(),
            tags: BTreeMap::new(),
        }
    }
}

impl TiffScan {
    pub fn tag(&self, tag: u16) -> Option<&Entry> {
        self.tags.get(&tag)
    }
}

/// 依位元組順序讀出 n（≤ 8）個位元組的無號整數。
pub fn uint(p: &[u8], little_endian: bool) -> u64 {
    let it = p.iter().take(8);
    if little_endian {
        it.rev().fold(0, |v, &b| (v << 8) | u64::from(b))
    } else {
        it.fold(0, |v, &b| (v << 8) | u64::from(b))
    }
}

/// 檔案讀取器：只讀檔案內的範圍，bytes_scanned 依邏輯讀取量累計（同 C++ 版 Reader）。
pub struct Reader<'a, 'b> {
    f: &'a Input<'b>,
    size: u64,
    le: bool,
    scanned: u64,
    cache_at: u64,
    cache: Vec<u8>,
}

impl<'a, 'b> Reader<'a, 'b> {
    pub fn new(f: &'a Input<'b>, le: bool) -> Self {
        Self { f, size: f.size(), le, scanned: 0, cache_at: 0, cache: Vec::new() }
    }

    pub fn scanned(&self) -> u64 {
        self.scanned
    }

    /// 讀 dst.len() bytes；超出檔案或讀不到時回傳 false。
    pub fn bytes(&mut self, off: u64, dst: &mut [u8]) -> bool {
        let n = dst.len() as u64;
        let Some(end) = off.checked_add(n) else { return false };
        if end > self.size {
            return false;
        }
        if dst.len() <= 16 {
            let cache_end = self.cache_at + self.cache.len() as u64;
            if off < self.cache_at || end > cache_end {
                let want = usize::try_from(self.size - off).map_or(CACHE_BYTES, |n| n.min(CACHE_BYTES));
                self.cache.resize(want, 0);
                if !matches!(self.f.read_at(off, &mut self.cache), Ok(k) if k == want) {
                    self.cache.clear();
                    return false;
                }
                self.cache_at = off;
            }
            let start = u64_to_usize(off - self.cache_at);
            let Some(src) = self.cache.get(start..start + dst.len()) else { return false };
            dst.copy_from_slice(src);
        } else if !matches!(self.f.read_at(off, dst), Ok(k) if k == dst.len()) {
            return false;
        }
        self.scanned += n;
        true
    }

    /// 讀 `len` bytes 成新的 Vec；長度無法表示或超出檔案時回傳 None。
    pub fn read_vec(&mut self, off: u64, len: u64) -> Option<Vec<u8>> {
        // 先確認範圍，再配置記憶體
        off.checked_add(len).filter(|&end| end <= self.size)?;
        let mut v = vec![0u8; usize::try_from(len).ok()?];
        self.bytes(off, &mut v).then_some(v)
    }

    pub fn u16(&self, p: [u8; 2]) -> u16 {
        if self.le { u16::from_le_bytes(p) } else { u16::from_be_bytes(p) }
    }

    pub fn u(&self, p: &[u8]) -> u64 {
        uint(p, self.le)
    }

    /// 整數陣列值的一個元素（BYTE、SHORT、LONG、LONG8、IFD／IFD8）。
    pub fn element(&mut self, e: &Entry, i: u64) -> Option<u64> {
        let ts = type_size(e.typ);
        if i >= e.count || !matches!(e.typ, 1 | 3 | 4 | 13 | 16 | 18) {
            return None;
        }
        let mut b = [0u8; 8];
        let off = e.value_offset.checked_add(i.checked_mul(u64::from(ts))?)?;
        let dst = b.get_mut(..ts as usize)?;
        if !self.bytes(off, dst) {
            return None;
        }
        Some(self.u(dst))
    }

    pub fn scalar(&mut self, e: &Entry) -> Option<u64> {
        if e.count >= 1 { self.element(e, 0) } else { None }
    }
}

fn issue(s: &mut TiffScan, code: &str, sev: &str, msg: String) {
    s.issues.push(Issue::new(code, sev, "", msg));
}

fn corrupt(s: &mut TiffScan, code: &str, msg: String) {
    s.container_status = "corrupt";
    issue(s, code, "error", msg);
}

fn short_hex(h: &str) -> &str {
    &h[..h.len().min(16)]
}

pub fn compression_name(c: u16) -> String {
    match c {
        1 => "none",
        2 => "CCITT RLE",
        3 => "CCITT G3",
        4 => "CCITT G4",
        5 => "LZW",
        6 => "old-style JPEG",
        7 => "JPEG",
        8 => "Deflate",
        32946 => "Deflate (legacy code)",
        32773 => "PackBits",
        34925 => "LZMA",
        50000 => "ZSTD",
        50001 => "WebP",
        50002 => "JPEG XL",
        34887 => "LERC",
        _ => return format!("code {c}"),
    }
    .into()
}

pub fn scan_tiff(f: &Input<'_>, lim: &Limits) -> TiffScan {
    let mut s = TiffScan::default();
    let file_size = f.size();
    let mut h = [0u8; 16];
    // 位元組順序要先知道；先用小端讀檔頭，確定後再換。
    let mut rd = Reader::new(f, true);
    if !rd.bytes(0, &mut h[..8]) {
        corrupt(&mut s, code::TRUNCATED, "file shorter than a TIFF header".into());
        s.bytes_scanned = rd.scanned();
        return s;
    }
    s.little_endian = match &h[..2] {
        b"II" => true,
        b"MM" => false,
        _ => {
            corrupt(&mut s, code::STRUCTURE_ERROR, "bad byte-order mark".into());
            s.bytes_scanned = rd.scanned();
            return s;
        }
    };
    rd.le = s.little_endian;
    let magic = rd.u(&h[2..4]);
    if magic == 42 {
        s.ifd_offset = rd.u(&h[4..8]);
    } else if magic == 43 {
        s.big = true;
        let (head, tail) = h.split_at_mut(8);
        if !rd.bytes(8, tail) {
            corrupt(&mut s, code::TRUNCATED, "file shorter than a BigTIFF header".into());
            s.bytes_scanned = rd.scanned();
            return s;
        }
        if rd.u(&head[4..6]) != 8 || rd.u(&head[6..8]) != 0 {
            corrupt(&mut s, code::STRUCTURE_ERROR, "BigTIFF offset size must be 8".into());
            s.bytes_scanned = rd.scanned();
            return s;
        }
        s.ifd_offset = rd.u(&tail[..8]);
    } else {
        corrupt(&mut s, code::STRUCTURE_ERROR, format!("bad TIFF magic {magic}"));
        s.bytes_scanned = rd.scanned();
        return s;
    }
    s.header_ok = true;
    scan_ifd(&mut s, &mut rd, file_size, lim);
    s.bytes_scanned = rd.scanned();
    s
}

/// 第一個 IFD 之後的掃描；提早結束時 container_status 已設為 corrupt。
fn scan_ifd(s: &mut TiffScan, rd: &mut Reader<'_, '_>, file_size: u64, lim: &Limits) {
    let big = s.big;
    let (cnt_size, ent_size, off_size): (usize, u64, usize) = if big { (8, 20, 8) } else { (2, 12, 4) };
    let mut cb = [0u8; 8];
    if s.ifd_offset < if big { 16 } else { 8 } || !rd.bytes(s.ifd_offset, &mut cb[..cnt_size]) {
        corrupt(s, code::STRUCTURE_ERROR, format!("first IFD offset {} is outside the file", s.ifd_offset));
        return;
    }
    let n = rd.u(&cb[..cnt_size]);
    if n == 0 || n > MAX_ENTRIES {
        corrupt(s, code::STRUCTURE_ERROR, format!("IFD entry count {n} is not plausible"));
        return;
    }
    s.entry_count = n;
    // n ≤ 4096，下面的乘法不會溢位
    let mut raw = vec![0u8; u64_to_usize(n * ent_size) + off_size];
    let body = s.ifd_offset.checked_add(cnt_size as u64);
    if !body.is_some_and(|b| rd.bytes(b, &mut raw)) {
        corrupt(s, code::TRUNCATED, "first IFD runs past the end of the file".into());
        return;
    }
    let body = body.unwrap_or_default();
    let mut prev = 0u16;
    for i in 0..n {
        let p = &raw[u64_to_usize(i * ent_size)..u64_to_usize((i + 1) * ent_size)];
        let tag = rd.u16([p[0], p[1]]);
        let typ = rd.u16([p[2], p[3]]);
        let count = if big { rd.u(&p[4..12]) } else { rd.u(&p[4..8]) };
        let ts = type_size(typ);
        s.tag_list.push(tag.to_string());
        if i > 0 && tag <= prev {
            issue(s, "tiff_tag_order", "warning", format!("tag {tag} is out of ascending order or repeated"));
        }
        prev = tag;
        if ts == 0 {
            issue(s, "tiff_unknown_type", "warning", format!("tag {tag} has unknown type {typ}"));
            continue;
        }
        let Some(bytes) = count.checked_mul(u64::from(ts)) else {
            corrupt(s, code::STRUCTURE_ERROR, format!("tag {tag} value size overflows"));
            return;
        };
        let field = if big { &p[12..20] } else { &p[8..12] };
        let inline_pos = body + i * ent_size + if big { 12 } else { 8 };
        let inline = bytes <= off_size as u64;
        let value_offset = if inline {
            inline_pos
        } else {
            let o = rd.u(field);
            if o.checked_add(bytes).is_none_or(|end| end > file_size) {
                corrupt(s, code::STRUCTURE_ERROR, format!("tag {tag} value lies outside the file"));
                return;
            }
            o
        };
        let e = Entry { tag, typ, count, value_offset, bytes, inline };
        s.entries.push(e);
        s.tags.entry(tag).or_insert(e);
    }
    s.next_ifd = rd.u(&raw[u64_to_usize(n * ent_size)..]);
    s.has_ifd = true;

    let get = |s: &TiffScan, tag: u16| s.tags.get(&tag).copied();
    let scalar = |s: &TiffScan, rd: &mut Reader<'_, '_>, tag: u16, dflt: u64| -> u64 {
        get(s, tag).and_then(|e| rd.scalar(&e)).unwrap_or(dflt)
    };

    // C++ 版以 static_cast<uint16_t>／<uint32_t> 截斷
    #[expect(clippy::cast_possible_truncation, reason = "對照 C++ static_cast 截斷")]
    let trunc16 = |v: u64| v as u16;
    #[expect(clippy::cast_possible_truncation, reason = "對照 C++ static_cast 截斷")]
    let trunc32 = |v: u64| v as u32;
    let clamp32 = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);

    s.width = clamp32(scalar(s, rd, 256, 0));
    s.height = clamp32(scalar(s, rd, 257, 0));
    s.spp = trunc16(scalar(s, rd, 277, 1));
    s.compression = trunc16(scalar(s, rd, 259, 1));
    s.photometric = trunc16(scalar(s, rd, 262, 0xFFFF));
    s.planar = trunc16(scalar(s, rd, 284, 1));
    s.sample_format = trunc16(scalar(s, rd, 339, 1));
    s.predictor = trunc16(scalar(s, rd, 317, 1));
    s.fill_order = trunc16(scalar(s, rd, 266, 1));
    if let Some(e) = get(s, 274) {
        s.orientation = Some(trunc16(rd.scalar(&e).unwrap_or(0)));
    }
    if let Some(e) = get(s, 258) {
        s.bits = trunc16(rd.element(&e, 0).unwrap_or(0));
        for i in 1..e.count.min(64) {
            if rd.element(&e, i).unwrap_or(0) != u64::from(s.bits) {
                s.bits = 0; // 各聲道不同
            }
        }
    } else {
        s.bits = 1; // TIFF 預設值
    }
    if let Some(e) = get(s, 338) {
        for i in 0..e.count.min(64) {
            s.extra_samples.push(trunc16(rd.element(&e, i).unwrap_or(0)));
        }
    }
    s.subifds = get(s, 330).is_some();
    if s.width == 0 || s.height == 0 {
        corrupt(s, code::STRUCTURE_ERROR, "ImageWidth/ImageLength missing or zero".into());
        return;
    }

    // ---- strip／tile：個數要符合幾何，每一塊都要在檔案內
    s.tiled = get(s, 322).is_some() || get(s, 323).is_some() || get(s, 324).is_some();
    let (offs, cnts);
    let mut expected: u64;
    if s.tiled {
        s.tile_width = trunc32(scalar(s, rd, 322, 0));
        s.tile_length = trunc32(scalar(s, rd, 323, 0));
        offs = get(s, 324);
        cnts = get(s, 325);
        if s.tile_width == 0
            || s.tile_length == 0
            || !s.tile_width.is_multiple_of(16)
            || !s.tile_length.is_multiple_of(16)
        {
            corrupt(
                s,
                code::STRUCTURE_ERROR,
                format!("tile size {}x{} is invalid (must be a non-zero multiple of 16)", s.tile_width, s.tile_length),
            );
            return;
        }
        let across = u64::from(s.width).div_ceil(u64::from(s.tile_width));
        let down = u64::from(s.height).div_ceil(u64::from(s.tile_length));
        expected = across.wrapping_mul(down);
    } else {
        let rps = scalar(s, rd, 278, u64::from(u32::MAX));
        s.rows_per_strip = if rps == 0 { u32::MAX } else { clamp32(rps) };
        offs = get(s, 273);
        cnts = get(s, 279);
        let eff = s.rows_per_strip.min(s.height);
        expected = u64::from(s.height).div_ceil(u64::from(eff));
    }
    if s.planar == 2 {
        // C++ 版的 uint64_t 乘法在溢位時迴繞
        expected = expected.wrapping_mul(u64::from(s.spp));
    }
    let (Some(offs), Some(cnts)) = (offs, cnts) else {
        corrupt(s, code::STRUCTURE_ERROR, "strip/tile offsets or byte counts are missing".into());
        return;
    };
    if offs.count != expected || cnts.count != expected {
        corrupt(
            s,
            code::STRUCTURE_ERROR,
            format!("IFD lists {} offsets and {} byte counts, geometry needs {expected}", offs.count, cnts.count),
        );
        return;
    }
    s.blocks = expected;
    for i in 0..expected {
        let o = rd.element(&offs, i);
        let c = rd.element(&cnts, i);
        let (Some(o), Some(c)) = (o, c) else {
            corrupt(s, code::STRUCTURE_ERROR, format!("strip/tile table entry {i} is unreadable"));
            return;
        };
        if c == 0 {
            corrupt(s, code::STRUCTURE_ERROR, format!("block {i} has byte count 0 (missing image data)"));
            return;
        }
        match o.checked_add(c) {
            Some(end) if end <= file_size => s.data_end = s.data_end.max(end),
            _ => {
                let kind = if o < file_size { code::TRUNCATED } else { code::STRUCTURE_ERROR };
                corrupt(
                    s,
                    kind,
                    format!("block {i} at offset {o} (+{c} bytes) lies outside the file of {file_size} bytes"),
                );
                return;
            }
        }
    }

    // ---- 其他影像
    if s.next_ifd != 0 {
        let mut nb = [0u8; 8];
        if !rd.bytes(s.next_ifd, &mut nb[..cnt_size]) {
            corrupt(s, code::STRUCTURE_ERROR, format!("next IFD offset {} is outside the file", s.next_ifd));
            return;
        }
    }

    // ---- 色彩：ICC（34675）與色度 tag
    if let Some(e) = get(s, 34675) {
        if e.bytes > lim.metadata_item_bytes() {
            s.icc.status = "over_budget".into();
            s.icc.bytes = Some(Count(e.bytes));
            s.metadata_status = "over_limit";
            issue(
                s,
                code::METADATA_OVER_LIMIT,
                "warning",
                format!("ICC profile of {} bytes exceeds metadata_item_mib", e.bytes),
            );
        } else {
            if let Some(icc) = rd.read_vec(e.value_offset, e.bytes) {
                s.icc.status = "embedded".into();
                s.icc.bytes = Some(Count(e.bytes));
                s.icc.sha256 = sha256_hex(&icc);
                s.icc.decoder_accepted = true;
            } else {
                s.icc.status = "unreadable".into();
            }
        }
    }
    for (tag, name) in [(318, "WhitePoint"), (319, "PrimaryChromaticities"), (301, "TransferFunction")] {
        if get(s, tag).is_some() {
            s.colorimetry.push(name.into());
        }
    }

    // ---- metadata（只放雜湊與大小，值不寫進報告）
    let max_item = lim.metadata_item_bytes();
    let blob_hash = |rd: &mut Reader<'_, '_>, e: &Entry, status: &mut &'static str| -> String {
        if e.bytes > max_item {
            *status = "over_limit";
            return format!("bytes={} (over limit)", e.bytes);
        }
        let Some(b) = rd.read_vec(e.value_offset, e.bytes) else {
            *status = "parse_failed";
            return "unreadable".into();
        };
        format!("bytes={} sha256={}", e.bytes, short_hex(&sha256_hex(&b)))
    };
    if let Some(e) = get(s, 700) {
        let mut st = s.xmp_status;
        s.xmp = Some(blob_hash(rd, &e, &mut st));
        s.xmp_status = st;
    }
    if let Some(e) = get(s, 34665) {
        let off = rd.scalar(&e);
        let mut eb = [0u8; 8];
        match off {
            Some(off) if rd.bytes(off, &mut eb[..cnt_size]) => {
                let en = rd.u(&eb[..cnt_size]);
                let len = en.checked_mul(ent_size).filter(|_| en <= MAX_ENTRIES).unwrap_or(0);
                let body = if len == 0 { None } else { rd.read_vec(off + cnt_size as u64, len) };
                if let Some(body) = body {
                    s.exif = Some(format!("exif_ifd entries={en} sha256={}", short_hex(&sha256_hex(&body))));
                } else {
                    s.exif = Some("exif_ifd unreadable".into());
                    s.exif_status = "parse_failed";
                }
            }
            _ => {
                s.exif = Some("exif_ifd offset outside the file".into());
                s.exif_status = "parse_failed";
            }
        }
    }
    {
        const TEXT_TAGS: [(u16, &str); 6] = [
            (270, "ImageDescription"),
            (271, "Make"),
            (272, "Model"),
            (305, "Software"),
            (315, "Artist"),
            (33432, "Copyright"),
        ];
        let mut keys = Vec::new();
        let mut hs = Sha256::new();
        for (tag, name) in TEXT_TAGS {
            let Some(e) = get(s, tag) else { continue };
            keys.push(name);
            let mut st = "present";
            let hsum = blob_hash(rd, &e, &mut st);
            if st != "present" {
                s.text_status = st;
            }
            hs.update(name.as_bytes());
            hs.update(hsum.as_bytes());
        }
        if !keys.is_empty() {
            s.text = Some(format!("keys=[{}] values_sha256={}", keys.join(","), short_hex(&hex(&hs.finalize()))));
        }
    }
    if let (Some(xr), Some(yr)) = (get(s, 282), get(s, 283))
        && xr.typ == 5
        && yr.typ == 5
    {
        let (mut xb, mut yb) = ([0u8; 8], [0u8; 8]);
        if rd.bytes(xr.value_offset, &mut xb) && rd.bytes(yr.value_offset, &mut yb) {
            let unit = scalar(s, rd, 296, 2);
            s.resolution = Some(format!(
                "{}/{} x {}/{} unit={unit}",
                rd.u(&xb[..4]),
                rd.u(&xb[4..]),
                rd.u(&yb[..4]),
                rd.u(&yb[4..])
            ));
        }
    }
    if let Some(e) = get(s, 306)
        && e.typ == 2
        && e.bytes <= 64
    {
        if let Some(t) = rd.read_vec(e.value_offset, e.bytes) {
            let end = t.iter().position(|&b| b == 0).unwrap_or(t.len());
            // 規格上是 ASCII；其他位元組顯示為 U+FFFD
            s.datetime = Some(file_text(&t[..end]));
        }
    }
    for (tag, name) in
        [(33723, "IPTC"), (34377, "Photoshop"), (34853, "GPS IFD"), (37724, "ImageSourceData"), (50706, "DNGVersion")]
    {
        if get(s, tag).is_some() {
            s.other_tags.push(name.into());
        }
    }
    for st in [s.exif_status, s.xmp_status, s.text_status] {
        if st == "parse_failed" {
            s.metadata_status = "parse_failed";
        } else if st == "over_limit" && s.metadata_status == "valid" {
            s.metadata_status = "over_limit";
        }
    }

    if s.container_status == "unverified" {
        s.container_status = if s.issues.iter().any(|i| i.severity == "warning") { "warning" } else { "valid" };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_sizes_and_uint() {
        assert_eq!(type_size(3), 2);
        assert_eq!(type_size(16), 8);
        assert_eq!(type_size(14), 0);
        assert_eq!(uint(&[0x34, 0x12], true), 0x1234);
        assert_eq!(uint(&[0x12, 0x34], false), 0x1234);
        assert_eq!(compression_name(32946), "Deflate (legacy code)");
        assert_eq!(compression_name(9), "code 9");
    }
}
