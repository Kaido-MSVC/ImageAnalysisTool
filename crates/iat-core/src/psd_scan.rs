//! 獨立的 PSD／PSB 結構掃描（移植 C++ 版 `src/engine/psd_scan.*`，依 Adobe Photoshop File Formats
//! Specification）。讀檔頭、色彩模式資料、影像資源、圖層與遮色片區段的長度，以及合併影像
//! （composite）資料區段；從不解讀圖層像素。所有長度與位移都是 64 位元並對檔案大小檢查。
//! PSB（版本 2）只差在欄位寬度：區段長度 64 位元、RLE 的列位元組數 32 位元。
//!
//! 圖層區段的走訪刻意保留 C++ 版的無號減法行為（`wrapping_sub`），結果才會和 C++ 版相同。

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::input::Input;
use crate::limits::Limits;
use crate::report::{Issue, code};
use crate::text::hex;
use crate::u64_to_usize;

#[derive(Clone, Debug)]
pub struct PsdScan {
    /// unverified | valid | warning | corrupt
    pub container_status: &'static str,
    /// valid | over_limit
    pub metadata_status: &'static str,
    pub issues: Vec<Issue>,
    pub bytes_scanned: u64,
    /// 影像資料之後的位元組數（資料被截斷時為 None）
    pub trailing_bytes: Option<u64>,
    /// 走到的區段（報告用）
    pub sections: Vec<String>,

    pub header_ok: bool,
    /// 1 = PSD、2 = PSB
    pub version: u16,
    pub channels: u16,
    pub width: u32,
    pub height: u32,
    /// 1 | 8 | 16 | 32
    pub depth: u16,
    /// 0 bitmap、1 gray、2 indexed、3 RGB、4 CMYK、7 multichannel、8 duotone、9 Lab
    pub mode: u16,

    /// indexed：768 bytes，R[256] G[256] B[256]
    pub palette: Vec<u8>,
    /// 資源 0x0416（顏色數）
    pub palette_count: Option<u16>,
    /// 資源 0x0417
    pub transparent_index: Option<u16>,

    /// 版本資訊（0x0421）的 hasRealMergedData；很多非 Adobe 的程式不寫這個資源。
    pub version_info: bool,
    pub has_real_merged: bool,
    /// 資源 0x0411 = 1
    pub icc_untagged: bool,
    /// embedded | absent | over_budget
    pub icc_status: &'static str,
    pub icc: Vec<u8>,
    /// present | absent | over_limit
    pub exif_status: &'static str,
    /// 原始 TIFF（資源 0x0422）
    pub exif: Vec<u8>,
    /// 資源 0x0424
    pub xmp_bytes: Option<u64>,
    pub xmp_sha256: String,
    /// 資源 0x0404
    pub iptc_bytes: Option<u64>,
    pub iptc_sha256: String,
    /// 資源 0x03ED 的摘要
    pub resolution: Option<String>,
    pub resource_count: u32,

    /// 圖層與遮色片資訊。無法走完標記區塊時 layers_known 為 false，合併影像就不算「已平面化」。
    pub layer_section_bytes: u64,
    /// 有號：< 0 表示第一個額外聲道是合併影像的透明度
    pub layer_count_field: i32,
    /// |欄位|（加上 Lr16／Lr32／Layr 區塊）
    pub layer_count: u32,
    pub layers_known: bool,

    /// 壓縮欄位的位置
    pub image_data_offset: u64,
    /// 0 raw、1 RLE（PackBits）、2 ZIP、3 ZIP+prediction
    pub compression: u16,
    /// 每個聲道一列的位元組數（未壓縮）
    pub row_bytes: u64,
    /// 壓縮欄位（與 RLE 表）之後的第一個位元組
    pub data_offset: u64,
    /// 只有 RLE
    pub rle_table_offset: u64,
    /// raw：channels × height × row_bytes；RLE：列位元組數的總和
    pub data_bytes: u64,
}

impl Default for PsdScan {
    fn default() -> Self {
        Self {
            container_status: "unverified",
            metadata_status: "valid",
            issues: Vec::new(),
            bytes_scanned: 0,
            trailing_bytes: None,
            sections: Vec::new(),
            header_ok: false,
            version: 0,
            channels: 0,
            width: 0,
            height: 0,
            depth: 0,
            mode: 0,
            palette: Vec::new(),
            palette_count: None,
            transparent_index: None,
            version_info: false,
            has_real_merged: false,
            icc_untagged: false,
            icc_status: "absent",
            icc: Vec::new(),
            exif_status: "absent",
            exif: Vec::new(),
            xmp_bytes: None,
            xmp_sha256: String::new(),
            iptc_bytes: None,
            iptc_sha256: String::new(),
            resolution: None,
            resource_count: 0,
            layer_section_bytes: 0,
            layer_count_field: 0,
            layer_count: 0,
            layers_known: true,
            image_data_offset: 0,
            compression: 0xFFFF,
            row_bytes: 0,
            data_offset: 0,
            rle_table_offset: 0,
            data_bytes: 0,
        }
    }
}

impl PsdScan {
    fn issue(&mut self, code: &str, sev: &str, msg: impl Into<String>) {
        self.issues.push(Issue::new(code, sev, "", msg));
    }
    fn corrupt(&mut self, code: &str, msg: impl Into<String>) {
        self.container_status = "corrupt";
        self.issue(code, "error", msg);
    }
    fn warn(&mut self, msg: impl Into<String>) {
        if self.container_status == "valid" {
            self.container_status = "warning";
        }
        self.issue("psd_container_warning", "warning", msg);
    }
}

pub(crate) fn be16(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[0], p[1]])
}
pub(crate) fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}
fn be64(p: &[u8]) -> u64 {
    (u64::from(be32(p)) << 32) | u64::from(be32(&p[4..]))
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
    /// 檢查過邊界的長度欄位（32 或 64 位元）。
    fn length(&mut self, off: u64, wide: bool) -> Option<u64> {
        let mut b = [0u8; 8];
        let n = if wide { 8 } else { 4 };
        self.bytes(off, &mut b[..n]).then(|| if wide { be64(&b) } else { u64::from(be32(&b)) })
    }
}

/// 16.16 定點數，3 位小數。
fn fixed_text(v: u32) -> String {
    let milli = (u64::from(v) * 1000 + 32768) >> 16;
    format!("{}.{:03}", milli / 1000, milli % 1000)
}

fn depth_valid(mode: u16, depth: u16) -> bool {
    match mode {
        0 => depth == 1,
        2 => depth == 8,
        8 => depth == 8 || depth == 16,
        1 | 3 | 4 | 7 | 9 => matches!(depth, 8 | 16 | 32),
        _ => false,
    }
}

fn min_channels(mode: u16) -> u16 {
    match mode {
        3 | 9 => 3,
        4 => 4,
        _ => 1,
    }
}

/// 影像資源（第 2 區段）。結構損壞時回傳 false。
fn scan_resources(rd: &mut Reader<'_, '_>, s: &mut PsdScan, start: u64, len: u64, lim: &Limits) -> bool {
    let end = start + len;
    let (mut pos, mut meta_total) = (start, 0u64);
    let mut over_meta = |n: u64| match meta_total.checked_add(n) {
        Some(t) if n <= lim.metadata_item_bytes() && t <= lim.metadata_file_bytes() => {
            meta_total = t;
            false
        }
        _ => true,
    };
    while pos < end {
        let mut h = [0u8; 7];
        if end - pos < 12 || !rd.bytes(pos, &mut h) {
            s.corrupt(
                code::STRUCTURE_ERROR,
                format!("image resource header at offset {pos} runs past the resource section"),
            );
            return false;
        }
        if !matches!(&h[..4], b"8BIM" | b"MeSa" | b"AgHg" | b"PHUT" | b"DCSR") {
            s.corrupt(code::STRUCTURE_ERROR, format!("image resource at offset {pos} has an unknown signature"));
            return false;
        }
        let id = be16(&h[4..]);
        let name_field = (u64::from(h[6]) + 2) & !1; // 長度位元組 + 名稱，補齊到偶數
        let size_at = pos + 6 + name_field;
        let mut sz = [0u8; 4];
        if size_at + 4 > end || !rd.bytes(size_at, &mut sz) {
            // C++ 版在 "0x" 後面印的是十進位的 id；照抄
            s.corrupt(code::STRUCTURE_ERROR, format!("image resource 0x{id} name runs past the resource section"));
            return false;
        }
        let size = u64::from(be32(&sz));
        let data = size_at + 4;
        let next = data + size + (size & 1);
        if data + size > end {
            s.corrupt(
                code::STRUCTURE_ERROR,
                format!("image resource {id} ({size} bytes) runs past the resource section"),
            );
            return false;
        }
        s.resource_count += 1;
        let mut b = [0u8; 16];
        match id {
            0x0421 => {
                // 版本資訊：u32 version、u8 hasRealMergedData……
                if size >= 5 && rd.bytes(data, &mut b[..5]) {
                    s.version_info = true;
                    s.has_real_merged = b[4] != 0;
                } else {
                    s.warn("version info resource (0x0421) is shorter than 5 bytes");
                }
            }
            0x040F => {
                if over_meta(size) {
                    s.icc_status = "over_budget";
                } else if let Some(p) = rd.payload(data, size) {
                    s.icc = p;
                    s.icc_status = "embedded";
                }
            }
            0x0411 if size >= 1 && rd.bytes(data, &mut b[..1]) => s.icc_untagged = b[0] == 1,
            0x0416 if size >= 2 && rd.bytes(data, &mut b[..2]) => s.palette_count = Some(be16(&b)),
            0x0417 if size >= 2 && rd.bytes(data, &mut b[..2]) => s.transparent_index = Some(be16(&b)),
            0x0422 => {
                // EXIF data 1（原始 TIFF）
                if over_meta(size) {
                    s.exif_status = "over_limit";
                } else if let Some(p) = rd.payload(data, size) {
                    s.exif = p;
                    s.exif_status = "present";
                }
            }
            0x0424 => {
                if let Some(hx) = rd.hash(data, size) {
                    s.xmp_sha256 = hx;
                    s.xmp_bytes = Some(size);
                }
            }
            0x0404 => {
                if let Some(hx) = rd.hash(data, size) {
                    s.iptc_sha256 = hx;
                    s.iptc_bytes = Some(size);
                }
            }
            0x03ED if size >= 16 && rd.bytes(data, &mut b) => {
                s.resolution = Some(format!(
                    "h={} unit={} v={} unit={}",
                    fixed_text(be32(&b)),
                    be16(&b[4..]),
                    fixed_text(be32(&b[8..])),
                    be16(&b[12..])
                ));
            }
            _ => {}
        }
        pos = next;
    }
    if s.icc_status == "over_budget" || s.exif_status == "over_limit" {
        s.metadata_status = "over_limit";
        s.issue(
            code::METADATA_OVER_LIMIT,
            "warning",
            "an image resource exceeds the metadata budget (ICC or EXIF not read)",
        );
    }
    true
}

/// 圖層與遮色片資訊（第 4 區段）：只讀數量，略過圖層記錄。
fn scan_layers(rd: &mut Reader<'_, '_>, s: &mut PsdScan, start: u64, len: u64) {
    let wide = s.version == 2;
    if len == 0 {
        return;
    }
    let end = start + len;
    let mut pos = start;
    let w = if wide { 8 } else { 4 };
    let Some(li_len) = rd.length(pos, wide).filter(|&l| l <= end.wrapping_sub(pos).wrapping_sub(w)) else {
        s.layers_known = false;
        return;
    };
    pos += w;
    if li_len >= 2 {
        let mut b = [0u8; 2];
        if !rd.bytes(pos, &mut b) {
            s.layers_known = false;
            return;
        }
        s.layer_count_field = i32::from(be16(&b).cast_signed());
        s.layer_count = s.layer_count_field.unsigned_abs();
    }
    pos = pos.wrapping_add(li_len);
    // 全域圖層遮色片資訊：u32 長度 + 資料
    if pos == end {
        return;
    }
    let Some(gm) = rd.length(pos, false).filter(|&g| g <= end.wrapping_sub(pos).wrapping_sub(4)) else {
        s.layers_known = false;
        return;
    };
    pos = pos.wrapping_add(4 + gm);
    // 標記區塊：16／32 位元文件的圖層放在這裡的 Lr16／Lr32（與 Layr）
    let wide_keys: BTreeSet<&[u8]> = [
        &b"LMsk"[..],
        b"Lr16",
        b"Lr32",
        b"Layr",
        b"Mt16",
        b"Mt32",
        b"Mtrn",
        b"Alph",
        b"FMsk",
        b"lnk2",
        b"FEid",
        b"FXid",
        b"PxSD",
    ]
    .into_iter()
    .collect();
    while end.wrapping_sub(pos) >= 12 {
        let mut h = [0u8; 8];
        if !rd.bytes(pos, &mut h) {
            s.layers_known = false;
            return;
        }
        if &h[..4] != b"8BIM" && &h[..4] != b"8B64" {
            // 寫入程式會補齊這個區段，補齊的內容全是 0。
            if h[..4] == [0, 0, 0, 0] {
                return;
            }
            s.layers_known = false;
            return;
        }
        let key = &h[4..8];
        let w8 = wide && wide_keys.contains(key);
        let bl = rd.length(pos + 8, w8);
        let data = pos + 8 + if w8 { 8 } else { 4 };
        let Some(bl) = bl.filter(|&b| data <= end && b <= end - data) else {
            s.layers_known = false;
            return;
        };
        if matches!(key, b"Lr16" | b"Lr32" | b"Layr") && bl >= 2 {
            let mut b = [0u8; 2];
            if rd.bytes(data, &mut b) {
                let n = be16(&b).cast_signed();
                s.layer_count += u32::from(n.unsigned_abs());
                if n < 0 && s.layer_count_field == 0 {
                    s.layer_count_field = i32::from(n);
                }
            }
        }
        let mut next = data + bl;
        if next & 1 == 1 {
            next += 1;
        }
        pos = next;
    }
}

/// 一列的漸進式 PackBits 解碼（C++ 版 0.5.1，D33）。一列的壓縮位元組可以分成任意多段餵入；
/// finish() 要求最後一段 run 完整，且正好解出 `out.len()` bytes。所有合法的 PackBits 都接受：
/// 任何位置的 -128（0x80）空操作標頭，以及不是最佳的 run（拆開的字面、2 bytes 的重複），所以
/// 一列的壓縮大小除了它的位元組數之外沒有上限。會寫過列尾的 run 立刻是錯誤。
pub struct PackBitsRow<'a> {
    out: &'a mut [u8],
    o: usize,
    /// 還要複製的字面位元組
    literal_left: usize,
    /// > 0：下一個位元組是要重複的值
    repeat_count: usize,
}

impl<'a> PackBitsRow<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Self { out, o: 0, literal_left: 0, repeat_count: 0 }
    }

    pub fn feed(&mut self, input: &[u8]) -> Result<(), &'static str> {
        let size = self.out.len();
        let mut i = 0;
        while i < input.len() {
            if self.literal_left > 0 {
                let k = self.literal_left.min(input.len() - i);
                self.out[self.o..self.o + k].copy_from_slice(&input[i..i + k]);
                i += k;
                self.o += k;
                self.literal_left -= k;
                continue;
            }
            if self.repeat_count > 0 {
                let v = input[i];
                i += 1;
                self.out[self.o..self.o + self.repeat_count].fill(v);
                self.o += self.repeat_count;
                self.repeat_count = 0;
                continue;
            }
            let h = input[i].cast_signed();
            i += 1;
            if h >= 0 {
                let k = usize::from(h.unsigned_abs()) + 1;
                if k > size - self.o {
                    return Err("literal run overflows the row");
                }
                self.literal_left = k;
            } else if h != -128 {
                // -128 是空操作標頭
                let k = 1 + usize::from(h.unsigned_abs()); // h < 0：1 - h
                if k > size - self.o {
                    return Err("repeat run overflows the row");
                }
                self.repeat_count = k;
            }
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<(), String> {
        if self.literal_left > 0 {
            return Err("literal run passes the end of the row data".into());
        }
        if self.repeat_count > 0 {
            return Err("repeat run without its byte".into());
        }
        if self.o != self.out.len() {
            return Err(format!("row decodes to {} of {} bytes", self.o, self.out.len()));
        }
        Ok(())
    }
}

/// 正好一列的 PackBits 解碼：`input` 必須用掉每個位元組並正好解出 `out.len()` bytes。
pub fn packbits_row(input: &[u8], out: &mut [u8]) -> Result<(), String> {
    let mut row = PackBitsRow::new(out);
    row.feed(input).map_err(String::from)?;
    row.finish()
}

pub fn scan_psd(f: &Input<'_>, lim: &Limits) -> PsdScan {
    let file_size = f.size();
    let mut s = PsdScan::default();
    let mut rd = Reader { f, size: file_size, scanned: 0 };
    scan_into(&mut s, &mut rd, file_size, lim);
    s.bytes_scanned = rd.scanned;
    s
}

fn scan_into(s: &mut PsdScan, rd: &mut Reader<'_, '_>, file_size: u64, lim: &Limits) {
    let mut h = [0u8; 26];
    if !rd.bytes(0, &mut h) {
        s.corrupt(code::TRUNCATED, "file shorter than the 26-byte PSD header");
        return;
    }
    if &h[..4] != b"8BPS" {
        s.corrupt(code::STRUCTURE_ERROR, "missing 8BPS signature");
        return;
    }
    s.version = be16(&h[4..]);
    if s.version != 1 && s.version != 2 {
        s.corrupt(code::STRUCTURE_ERROR, format!("PSD version {} (expected 1 = PSD or 2 = PSB)", s.version));
        return;
    }
    s.container_status = "valid";
    s.sections.push(if s.version == 2 { "header(PSB)" } else { "header(PSD)" }.into());
    if h[6..12].iter().any(|&b| b != 0) {
        s.warn("reserved header bytes are not zero");
    }
    s.channels = be16(&h[12..]);
    s.height = be32(&h[14..]);
    s.width = be32(&h[18..]);
    s.depth = be16(&h[22..]);
    s.mode = be16(&h[24..]);
    let max_side: u32 = if s.version == 2 { 300_000 } else { 30_000 };
    if s.width == 0 || s.height == 0 || s.width > max_side || s.height > max_side {
        s.corrupt(
            code::STRUCTURE_ERROR,
            format!(
                "{}x{} is outside 1..{max_side} ({} limit)",
                s.width,
                s.height,
                if s.version == 2 { "PSB" } else { "PSD" }
            ),
        );
        return;
    }
    if s.channels < 1 || s.channels > 56 {
        s.corrupt(code::STRUCTURE_ERROR, format!("{} channels (1..56 allowed)", s.channels));
        return;
    }
    if !depth_valid(s.mode, s.depth) {
        s.corrupt(code::STRUCTURE_ERROR, format!("colour mode {} with depth {} is not defined", s.mode, s.depth));
        return;
    }
    if s.channels < min_channels(s.mode) {
        s.corrupt(
            code::STRUCTURE_ERROR,
            format!("colour mode {} needs at least {} channels", s.mode, min_channels(s.mode)),
        );
        return;
    }
    s.header_ok = true;
    s.row_bytes = (u64::from(s.width) * u64::from(s.depth)).div_ceil(8);
    let wide = s.version == 2;

    // ---- 色彩模式資料
    let mut pos = 26u64;
    let Some(cm) = rd.length(pos, false) else {
        s.corrupt(code::TRUNCATED, "colour mode data length missing");
        return;
    };
    pos += 4;
    if cm > file_size - pos {
        s.corrupt(code::TRUNCATED, format!("colour mode data ({cm} bytes) runs past the end of the file"));
        return;
    }
    if s.mode == 2 {
        if cm != 768 {
            s.corrupt(code::STRUCTURE_ERROR, format!("indexed colour needs 768 bytes of palette, found {cm}"));
            return;
        }
        match rd.payload(pos, 768) {
            Some(p) => s.palette = p,
            None => {
                s.corrupt(code::TRUNCATED, "palette could not be read");
                return;
            }
        }
    }
    pos += cm;
    s.sections.push("color_mode_data".into());

    // ---- 影像資源
    let Some(rl) = rd.length(pos, false) else {
        s.corrupt(code::TRUNCATED, "image resource section length missing");
        return;
    };
    pos += 4;
    if rl > file_size - pos {
        s.corrupt(code::TRUNCATED, format!("image resource section ({rl} bytes) runs past the end of the file"));
        return;
    }
    if !scan_resources(rd, s, pos, rl, lim) {
        return;
    }
    pos += rl;
    s.sections.push("image_resources".into());

    // ---- 圖層與遮色片資訊（只讀長度與數量）
    let Some(lm) = rd.length(pos, wide) else {
        s.corrupt(code::TRUNCATED, "layer and mask section length missing");
        return;
    };
    pos += if wide { 8 } else { 4 };
    if lm > file_size - pos {
        s.corrupt(code::TRUNCATED, format!("layer and mask section ({lm} bytes) runs past the end of the file"));
        return;
    }
    s.layer_section_bytes = lm;
    scan_layers(rd, s, pos, lm);
    pos += lm;
    s.sections.push("layer_and_mask".into());

    // ---- 影像資料
    s.image_data_offset = pos;
    let mut c = [0u8; 2];
    if !rd.bytes(pos, &mut c) {
        s.corrupt(code::TRUNCATED, "image data section is missing (file ends after the layer section)");
        return;
    }
    s.compression = be16(&c);
    pos += 2;
    s.sections.push("image_data".into());
    let plane_rows = u64::from(s.channels) * u64::from(s.height);
    match s.compression {
        0 => {
            let Some(total) = plane_rows.checked_mul(s.row_bytes) else {
                s.corrupt(code::STRUCTURE_ERROR, "raw image data size overflows 64-bit arithmetic");
                return;
            };
            s.data_offset = pos;
            s.data_bytes = total;
        }
        1 => {
            let entry: u64 = if wide { 4 } else { 2 };
            let table = plane_rows * entry;
            if table > file_size - pos {
                s.corrupt(code::TRUNCATED, "RLE row byte-count table runs past the end of the file");
                return;
            }
            s.rle_table_offset = pos;
            // 以 1 MiB 為單位加總（大的 PSB 表可能有好幾十 MB）。
            let mut buf = vec![0u8; 1 << 20];
            let (mut sum, mut off, mut left) = (0u64, pos, table);
            while left > 0 {
                let take = usize::try_from(left).map_or(buf.len(), |n| n.min(buf.len()));
                if !rd.bytes(off, &mut buf[..take]) {
                    s.corrupt(code::TRUNCATED, "RLE row byte-count table could not be read");
                    return;
                }
                for e in buf[..take].chunks_exact(u64_to_usize(entry)) {
                    sum += if wide { u64::from(be32(e)) } else { u64::from(be16(e)) };
                }
                off += take as u64;
                left -= take as u64;
            }
            s.data_offset = pos + table;
            s.data_bytes = sum;
        }
        2 | 3 => {
            // ZIP 串流不解壓就無法界定長度；影像資料不檢查。
            s.data_offset = pos;
            return;
        }
        other => {
            s.corrupt(code::STRUCTURE_ERROR, format!("image data compression {other} is not defined"));
            return;
        }
    }
    let data_end = s.data_offset.checked_add(s.data_bytes);
    let Some(data_end) = data_end.filter(|&e| e <= file_size) else {
        s.corrupt(
            code::TRUNCATED,
            format!("image data needs {} bytes from offset {}, file has {file_size}", s.data_bytes, s.data_offset),
        );
        return;
    };
    let trailing = file_size - data_end;
    s.trailing_bytes = Some(trailing);
    if trailing > 0 {
        if s.container_status == "valid" {
            s.container_status = "warning";
        }
        s.issue(code::TRAILING_DATA, "warning", format!("{trailing} bytes after the image data"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unpack(input: &[u8], n: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0; n];
        packbits_row(input, &mut out).map(|()| out)
    }

    #[test]
    fn packbits_accepts_any_valid_encoding() {
        assert_eq!(unpack(&[2, 1, 2, 3], 3).unwrap(), vec![1, 2, 3]);
        assert_eq!(unpack(&[0xFE, 7], 3).unwrap(), vec![7, 7, 7]);
        // 0x80 空操作、拆開的字面、2 bytes 的重複
        assert_eq!(unpack(&[0x80, 0, 1, 0x80, 0, 2, 0xFF, 9], 4).unwrap(), vec![1, 2, 9, 9]);
    }

    #[test]
    fn packbits_rejects_overflow_and_short_rows() {
        assert_eq!(unpack(&[3, 1, 2, 3, 4], 3).unwrap_err(), "literal run overflows the row");
        assert_eq!(unpack(&[0xFD, 1], 3).unwrap_err(), "repeat run overflows the row");
        assert_eq!(unpack(&[2, 1, 2], 3).unwrap_err(), "literal run passes the end of the row data");
        assert_eq!(unpack(&[0xFE], 3).unwrap_err(), "repeat run without its byte");
        assert_eq!(unpack(&[1, 1, 2], 3).unwrap_err(), "row decodes to 2 of 3 bytes");
    }

    #[test]
    fn packbits_pieces_equal_whole() {
        let input = [0x80, 2, 5, 6, 7, 0xFC, 1, 0, 9];
        let whole = unpack(&input, 9).unwrap();
        let mut out = vec![0; 9];
        let mut row = PackBitsRow::new(&mut out);
        for b in input {
            row.feed(&[b]).unwrap();
        }
        row.finish().unwrap();
        assert_eq!(out, whole);
    }

    #[test]
    fn fixed_point_text() {
        assert_eq!(fixed_text(72 << 16), "72.000");
        assert_eq!(fixed_text((300 << 16) + 32768), "300.500");
    }
}
