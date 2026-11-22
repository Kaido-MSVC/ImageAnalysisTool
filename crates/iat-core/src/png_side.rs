//! PNG 前端（移植 C++ 版 `src/engine/png_side.cpp`）：獨立的容器掃描 + png crate 解出未經轉換的列，
//! 再依明確規則展開成正規樣本。
//!
//! 和 C++ 版的差別（docs/decisions.md D-R11、D-R12）：
//! - 解碼器是 png crate 0.18.1（`Transformations::IDENTITY`），只餵 IHDR／PLTE／IDAT／IEND；
//!   PLTE 與 tRNS 的值、輔助 chunk 的處理都來自 png_scan 的 libpng 相容檢查。
//! - libpng 在最後一列才發現的 IDAT 錯誤（最後一個 IDAT 的 CRC、Adler-32）算成最後一列失敗；
//!   IDAT 之後的錯誤（缺 IEND、後面 chunk 的 CRC）算成 `after_image_data`。這兩者都依掃描結果判定。

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::rc::Rc;

use png::{Adam7Info, DecodeOptions, DecodingError, InterlaceInfo, Transformations};

use crate::engine::{Decoder, RowLayout, SideState, has_alpha, temp_failure};
use crate::inflate::ZState;
use crate::input::Input;
use crate::limits::Limits;
use crate::png_scan::{Phase, PngScan, PngText, exif_orientation, scan_png};
use crate::report::{Count, EffectiveColor, FileReport, Issue, MetaItem, Orientation, code};
use crate::temp::WRITE_BUFFER;
use crate::text::{hex, sha256_hex, short_hash};
use crate::u64_to_usize;

pub const DECODER_NAME: &str = "png";
pub const DECODER_VERSION: &str = "0.18.1";

fn layout_for(depth: i32, ctype: i32, has_trns: bool) -> RowLayout {
    let d = depth.cast_unsigned();
    match ctype {
        0 => RowLayout::new(if has_trns { "gray_alpha" } else { "gray" }, if has_trns { 2 } else { 1 }, d),
        2 => RowLayout::new(if has_trns { "rgba" } else { "rgb" }, if has_trns { 4 } else { 3 }, d),
        3 => RowLayout::new(if has_trns { "rgba" } else { "rgb" }, if has_trns { 4 } else { 3 }, 8),
        4 => RowLayout::new("gray_alpha", 2, d),
        6 => RowLayout::new("rgba", 4, d),
        _ => RowLayout::default(),
    }
}

fn png_channels(ctype: i32) -> u64 {
    match ctype {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => 0,
    }
}

// Adam7 各 pass 的幾何（PNG 規格 8.2），pass 0..6。
const XS: [u32; 7] = [0, 4, 0, 2, 0, 1, 0];
const XI: [u32; 7] = [8, 8, 4, 4, 2, 2, 1];
const YS: [u32; 7] = [0, 0, 4, 0, 2, 0, 1];
const YI: [u32; 7] = [8, 8, 8, 4, 4, 2, 2];
fn pass_cols(w: u32, p: usize) -> u32 {
    if w > XS[p] { (w - XS[p]).div_ceil(XI[p]) } else { 0 }
}
fn pass_rows(h: u32, p: usize) -> u32 {
    if h > YS[p] { (h - YS[p]).div_ceil(YI[p]) } else { 0 }
}

/// 一側 PNG 逐列解碼所需的工具緩衝（同 C++ 版 `png_row_budget`）。
pub fn png_row_budget(width: u32, bit_depth: i32, color_type: i32, has_trns: bool) -> Option<u64> {
    let l = layout_for(bit_depth, color_type, has_trns);
    let bits =
        u64::from(width).checked_mul(png_channels(color_type))?.checked_mul(u64::from(bit_depth.cast_unsigned()))?;
    if l.channels == 0 {
        return None;
    }
    let rb = bits.div_ceil(8);
    let canon = u64::from(width).checked_mul(u64::from(l.channels))?.checked_mul(2)?;
    let rows3 = (rb + 64).checked_mul(3)?;
    rows3.checked_add(canon)?.checked_add(256 << 10)
}

// ------------------------------------------------------------ 色彩

fn effective_color(sc: &PngScan, decoder_kept_iccp: bool) -> EffectiveColor {
    let mut c = EffectiveColor::default();
    if let Some(p) = sc.cicp {
        c.signals.push(format!("cICP({},{},{},{})", p[0], p[1], p[2], p[3]));
    }
    if sc.iccp_present {
        let v = if sc.icc.sha256.is_empty() { sc.icc.status.as_str() } else { short_hash(&sc.icc.sha256) };
        c.signals.push(format!("iCCP({v})"));
    }
    if let Some(i) = sc.srgb_intent {
        c.signals.push(format!("sRGB(intent {i})"));
    }
    if let Some(g) = sc.gama {
        c.signals.push(format!("gAMA({g})"));
    }
    let chrm_list = |ch: &[u32; 8]| ch.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
    if let Some(ch) = &sc.chrm {
        c.signals.push(format!("cHRM({})", chrm_list(ch)));
    }
    // PNG 優先順序（D4）：cICP > iCCP > sRGB > cHRM/gAMA。
    if let Some(p) = sc.cicp {
        let v = format!("{}/{}/{}/{}", p[0], p[1], p[2], p[3]);
        c.kind = "cicp".into();
        c.key = format!("cicp:{v}");
        c.description = format!("cICP primaries/transfer/matrix/range = {v}");
    } else if sc.iccp_present {
        if sc.icc.status == "embedded" && decoder_kept_iccp {
            c.kind = "icc".into();
            c.key = format!("icc:{}", sc.icc.sha256);
            c.description = format!("embedded ICC {} bytes", sc.icc.bytes.map_or(0, |b| b.0));
        } else {
            c.kind = "icc_unreadable".into();
            let why = if decoder_kept_iccp { "" } else { ", rejected by the libpng-compatible iCCP check" };
            c.description = format!("iCCP present but not usable ({}{why})", sc.icc.status);
        }
    } else if sc.srgb_intent.is_some() {
        c.kind = "srgb".into();
        c.key = "srgb".into();
        c.description = "sRGB chunk".into();
    } else if let (Some(g), Some(ch)) = (sc.gama, &sc.chrm) {
        c.kind = "chrm_gama".into();
        c.key = format!("gama:{g};chrm:{}", chrm_list(ch));
        c.description = "explicit gAMA + cHRM".into();
    } else if sc.gama.is_some() || sc.chrm.is_some() {
        c.kind = "partial".into();
        c.description = if sc.gama.is_some() {
            "gAMA without cHRM (primaries unknown)"
        } else {
            "cHRM without gAMA (transfer unknown)"
        }
        .into();
    } else {
        c.kind = "none".into();
        c.description = "no colour information (PNG has no specified default)".into();
    }
    c
}

// ------------------------------------------------------------ metadata

fn fill_metadata(r: &mut FileReport, sc: &PngScan) {
    let mut item = |name: &str, present: bool, status: &str, summary: String| {
        let status = if present { status } else { "absent" };
        r.metadata.insert(name.into(), MetaItem { present, status: status.into(), summary });
    };
    if sc.exif_status == "over_limit" {
        item("exif", true, "over_limit", "exceeds metadata budget".into());
    } else if let Some(e) = &sc.exif {
        let o = exif_orientation(e);
        let ori = if o > 0 { format!(" orientation={o}") } else { String::new() };
        let summary = format!("bytes={} sha256={}{ori}", e.len(), short_hash(&sha256_hex(e)));
        item("exif", true, if o < 0 { "parse_failed" } else { "present" }, summary);
    } else {
        item("exif", false, "", String::new());
    }

    let xmp = sc.texts.iter().find(|t| t.keyword_raw == b"XML:com.adobe.xmp");
    let others: Vec<&PngText> = sc.texts.iter().filter(|t| t.keyword_raw != b"XML:com.adobe.xmp").collect();
    // C++ 版找最後一個 XMP（迴圈覆寫），其餘都算一般文字。
    let xmp = sc.texts.iter().rev().find(|t| t.keyword_raw == b"XML:com.adobe.xmp").or(xmp);
    match xmp {
        Some(x) => {
            let summary = if x.status == "present" {
                format!("bytes={} sha256={}", x.bytes, short_hash(&x.sha256))
            } else {
                x.status.into()
            };
            item("xmp", true, x.status, summary);
        }
        None => item("xmp", false, "", String::new()),
    }
    if others.is_empty() {
        item("text", false, "", String::new());
    } else {
        use sha2::{Digest, Sha256};
        let mut kw = Vec::new();
        let mut status = "present";
        let mut h = Sha256::new();
        for t in &others {
            kw.push(format!("{}:{}", t.kind, t.keyword));
            h.update(&t.keyword_raw); // 原始位元組，不是顯示用的 UTF-8
            h.update(t.sha256.as_bytes());
            if t.status != "present" {
                status = t.status;
            }
        }
        let summary = format!(
            "entries={} keys=[{}] values_sha256={}",
            others.len(),
            kw.join(","),
            short_hash(&hex(&h.finalize()))
        );
        item("text", true, status, summary);
    }

    item("resolution", sc.phys.is_some(), "present", sc.phys.clone().unwrap_or_default());
    item("time", sc.time.is_some(), "present", sc.time.clone().unwrap_or_default());
    let mut hdr = String::new();
    if let Some(p) = sc.cicp {
        hdr += &format!("cICP={}/{}/{}/{} ", p[0], p[1], p[2], p[3]);
    }
    if let Some(m) = &sc.mdcv_hex {
        hdr += &format!("mDCV={m} ");
    }
    if let Some(c) = &sc.clli_hex {
        hdr += &format!("cLLI={c}");
    }
    item("hdr_signals", !hdr.is_empty(), "present", hdr);
    item("sbit", !sc.sbit.is_empty(), "present", hex(&sc.sbit));
    item("ornt_private", sc.ornt.is_some(), "present", sc.ornt.map(|o| format!("orNT={o}")).unwrap_or_default());
    item("other_chunks", !sc.other_ancillary.is_empty(), "present", sc.other_ancillary.join(","));
}

// ------------------------------------------------------------ 餵給 png crate 的位元組

/// 把檔案中的幾段範圍串成一個連續的串流。
struct FeedReader<'a> {
    input: &'a Input<'a>,
    segs: Vec<(u64, u64)>,
    /// 每段在串流中的起點
    starts: Vec<u64>,
    total: u64,
    pos: u64,
    /// 讀取原始檔失敗（和截斷造成的 EOF 區分開）
    io_error: Rc<Cell<bool>>,
}

impl<'a> FeedReader<'a> {
    fn new(input: &'a Input<'a>, segs: Vec<(u64, u64)>, io_error: Rc<Cell<bool>>) -> Self {
        let mut starts = Vec::with_capacity(segs.len());
        let mut total = 0;
        for &(_, len) in &segs {
            starts.push(total);
            total += len;
        }
        Self { input, segs, starts, total, pos: 0, io_error }
    }
}

impl Read for FeedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total || buf.is_empty() {
            return Ok(0);
        }
        let i = self.starts.partition_point(|&s| s <= self.pos) - 1;
        let (off, len) = self.segs[i];
        let within = self.pos - self.starts[i];
        let n = usize::try_from(len - within).map_or(buf.len(), |r| r.min(buf.len()));
        match self.input.read_at(off + within, &mut buf[..n]) {
            Ok(k) if k == n => {
                self.pos += n as u64;
                Ok(n)
            }
            Ok(_) => {
                self.io_error.set(true);
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "input file changed while reading"))
            }
            Err(e) => {
                self.io_error.set(true);
                Err(e)
            }
        }
    }
}

impl Seek for FeedReader<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.total.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        let p = p.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        self.pos = p;
        Ok(p)
    }
}

// ------------------------------------------------------------ 解碼器

type PngReader<'a> = png::Reader<BufReader<FeedReader<'a>>>;

struct PngDecoder<'a> {
    scan: Box<PngScan>,
    input: &'a Input<'a>,
    io_error: Rc<Cell<bool>>,
    reader: Option<PngReader<'a>>,
    raw: Vec<u8>,
    /// 最後一列才會發現的 IDAT 錯誤（同 libpng 在最後一列失敗）
    idat_end_error: Option<String>,
    /// libpng 在 IDAT 收尾時的警告
    idat_warnings: Vec<String>,
    reached_idat_end: bool,
    palette_oob: bool,
    // Adam7
    bps: u32,
    pass_off: [u64; 7],
    temp_total: u64,
    passes_done: bool,
    store: Vec<u8>,
}

/// png crate 的錯誤分類。
enum Failure {
    Resource,
    Io,
    Decode,
}

impl<'a> PngDecoder<'a> {
    fn interlaced(&self) -> bool {
        self.scan.interlace == 1
    }

    fn classify(&self, e: &DecodingError) -> Failure {
        match e {
            DecodingError::LimitsExceeded => Failure::Resource,
            _ if self.io_error.get() => Failure::Io,
            _ => Failure::Decode,
        }
    }

    fn fail(
        &mut self,
        s: &mut SideState<'_>,
        issues: &mut Vec<Issue>,
        f: Failure,
        msg: String,
        place: String,
        some_data: bool,
    ) -> bool {
        s.failed = true;
        let rep = &mut s.rep;
        rep.decode_status = match f {
            Failure::Resource => "not_checked",
            _ if some_data => "partial",
            _ => "failed",
        }
        .into();
        rep.decode_status_reason = match f {
            Failure::Resource => "resource_limit",
            Failure::Io => "io_error",
            Failure::Decode => "decode_error",
        }
        .into();
        rep.decoder_errors.push(msg.clone());
        let c = if matches!(f, Failure::Resource) { code::RESOURCE_LIMIT } else { code::DECODE_FAILED };
        issues.push(Issue::new(c, "error", s.scope, format!("png decoder at {place}: {msg}")));
        false
    }

    /// png crate 的下一列（含 Adam7 位置），錯誤時回傳分類與訊息。
    fn next_raw(&mut self) -> Result<Option<InterlaceInfo>, (Failure, String)> {
        let rd = self.reader.as_mut().expect("open() created the reader");
        match rd.next_interlaced_row() {
            Ok(Some(row)) => {
                let info = *row.interlace();
                self.raw.clear();
                self.raw.extend_from_slice(row.data());
                Ok(Some(info))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                let f = self.classify(&e);
                Err((f, e.to_string()))
            }
        }
    }

    /// 一列原始資料（未轉換）展開成正規樣本（同 C++ 版 `expand_row`）。
    fn expand_row(&mut self, canon: &mut [u16], layout: &RowLayout, width: u32) {
        let row = &self.raw;
        let depth = self.scan.bit_depth.cast_unsigned();
        let ct = self.scan.color_type;
        let in_ch = u64_to_usize(png_channels(ct));
        let out_ch = layout.channels as usize;
        let packed = |i: usize| -> u32 {
            match depth {
                16 => u32::from(row[2 * i]) << 8 | u32::from(row[2 * i + 1]),
                8 => u32::from(row[i]),
                d => {
                    let bit = i * d as usize;
                    let shift = 8 - d as usize - (bit & 7);
                    (u32::from(row[bit >> 3]) >> shift) & ((1 << d) - 1)
                }
            }
        };
        let lib = &self.scan.lib;
        let npal = lib.palette.len() / 3;
        #[expect(clippy::cast_possible_truncation, reason = "layout.bits ≤ 16")]
        let maxv = ((1u32 << layout.bits) - 1) as u16;
        #[expect(clippy::cast_possible_truncation, reason = "PNG 樣本最多 16 位元")]
        let sample = |i: usize| packed(i) as u16;
        for x in 0..width as usize {
            let px = &mut canon[x * out_ch..(x + 1) * out_ch];
            if ct == 3 {
                let idx = packed(x) as usize;
                if idx >= npal {
                    self.palette_oob = true; // 判定為解碼失敗，不替代
                }
                let k = if idx < npal { idx } else { 0 };
                let pal = |c: usize| u16::from(lib.palette.get(k * 3 + c).copied().unwrap_or(0));
                px[0] = pal(0);
                px[1] = pal(1);
                px[2] = pal(2);
                if out_ch == 4 {
                    px[3] = lib.trns_alpha.get(idx).map_or(255, |&a| u16::from(a));
                }
            } else {
                for (c, v) in px[..in_ch].iter_mut().enumerate() {
                    *v = sample(x * in_ch + c);
                }
                if in_ch < out_ch {
                    // tRNS 色鍵 -> alpha（libpng 拒收的 tRNS 不算色鍵，全部不透明）
                    let key = lib.trns_key.is_some_and(|k| {
                        if ct == 0 { px[0] == k[0] } else { px[0] == k[0] && px[1] == k[1] && px[2] == k[2] }
                    });
                    px[in_ch] = if key { 0 } else { maxv };
                }
            }
        }
    }

    fn pack(&mut self, src: &[u16]) {
        if self.bps == 1 {
            for (d, &v) in self.store.iter_mut().zip(src) {
                *d = v.to_le_bytes()[0]; // 8 位元樣本，只取低位元組
            }
        } else {
            for (i, &v) in src.iter().enumerate() {
                self.store[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }

    /// Adam7：依檔案順序解出每個 pass 的縮小列，展開後依序寫進暫存檔。
    fn decode_passes(&mut self, s: &mut SideState<'_>, issues: &mut Vec<Issue>) -> bool {
        let (w, h) = (self.scan.width, self.scan.height);
        let ch = s.layout.channels as usize;
        let total_rows: u64 = (0..7).filter(|&p| pass_cols(w, p) > 0).map(|p| u64::from(pass_rows(h, p))).sum();
        let mut read = 0u64;
        for (p, pass) in (1u8..=7).enumerate() {
            let (pw, ph) = (pass_cols(w, p), pass_rows(h, p));
            if pw == 0 || ph == 0 {
                continue; // png crate 也跳過空的 pass
            }
            for ry in 0..ph {
                let place = format!("Adam7 pass {} row {ry}", p + 1);
                if read + 1 == total_rows
                    && let Some(m) = self.idat_end_error.clone()
                {
                    return self.fail(s, issues, Failure::Decode, m, place, read > 0);
                }
                match self.next_raw() {
                    Ok(Some(InterlaceInfo::Adam7(a))) if a == Adam7Info::new(pass, ry, w) => {}
                    Ok(other) => {
                        s.failed = true;
                        s.rep.decode_status = if read > 0 { "partial" } else { "failed" }.into();
                        s.rep.decode_status_reason = "decoder_disagrees_with_scan".into();
                        issues.push(Issue::new(
                            code::DECODE_FAILED,
                            "error",
                            s.scope,
                            format!("png Adam7 row {other:?} differs from expected pass {} row {ry}", p + 1),
                        ));
                        return false;
                    }
                    Err((f, m)) => return self.fail(s, issues, f, m, place, read > 0),
                }
                read += 1;
                let mut canon = std::mem::take(&mut s.canon);
                let layout = s.layout.clone();
                self.expand_row(&mut canon, &layout, pw);
                self.pack(&canon[..pw as usize * ch]);
                s.canon = canon;
                let n = pw as usize * ch * self.bps as usize;
                let temp = s.temp.as_mut().expect("open() reserved temp");
                if let Err(e) = temp.append(&self.store[..n]) {
                    temp_failure(s, issues, &e);
                    return false;
                }
            }
        }
        self.reached_idat_end = true;
        let temp = s.temp.as_mut().expect("open() reserved temp");
        if let Err(e) = temp.flush() {
            temp_failure(s, issues, &e);
            return false;
        }
        if temp.written() != self.temp_total {
            let e = crate::temp::TempError::new(
                crate::temp::TempErrorKind::Internal,
                format!("Adam7 passes wrote {} of {} bytes", temp.written(), self.temp_total),
            );
            temp_failure(s, issues, &e);
            return false;
        }
        self.passes_done = true;
        true
    }

    /// 從包含這一列的各個 pass 重建影像第 s.rows 列；每一欄必須剛好填一次。
    fn reconstruct_row(&mut self, s: &mut SideState<'_>, issues: &mut Vec<Issue>) -> bool {
        let ch = s.layout.channels as usize;
        let y = s.next_row();
        let w = self.scan.width;
        let mut filled = 0u64;
        for p in 0..7 {
            let pw = pass_cols(w, p);
            if pw == 0 || y < YS[p] || !(y - YS[p]).is_multiple_of(YI[p]) {
                continue;
            }
            let ry = u64::from((y - YS[p]) / YI[p]);
            let row_bytes = pw as usize * ch * self.bps as usize;
            let temp = s.temp.as_mut().expect("open() reserved temp");
            if let Err(e) = temp.read_at(self.pass_off[p] + ry * row_bytes as u64, &mut self.store[..row_bytes]) {
                temp_failure(s, issues, &e);
                return false;
            }
            for k in 0..pw as usize {
                let dst = (XS[p] as usize + k * XI[p] as usize) * ch;
                for c in 0..ch {
                    let i = (k * ch + c) * self.bps as usize;
                    s.canon[dst + c] = if self.bps == 2 {
                        u16::from_le_bytes([self.store[i], self.store[i + 1]])
                    } else {
                        u16::from(self.store[i])
                    };
                }
            }
            filled += u64::from(pw);
        }
        if filled != u64::from(w) {
            let e = crate::temp::TempError::new(
                crate::temp::TempErrorKind::Internal,
                format!("Adam7 row {y} covered {filled} of {w} columns"),
            );
            temp_failure(s, issues, &e);
            return false;
        }
        true
    }

    /// libpng 的警告轉成問題（同 C++ 版 `record_decoder_messages`）。
    fn record_warnings(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>, after_idat_read: bool) {
        let lib = &self.scan.lib;
        let mut list: Vec<&str> = Vec::new();
        list.extend(lib.warnings.iter().filter(|w| w.0 == Phase::BeforeIdat).map(|w| w.1.as_str()));
        if self.reached_idat_end {
            list.extend(self.idat_warnings.iter().map(String::as_str));
        }
        if after_idat_read {
            list.extend(lib.warnings.iter().filter(|w| w.0 == Phase::AfterIdat).map(|w| w.1.as_str()));
        }
        // C++ 版 png_shim 只記錄前 32 個警告（PNGS_MAX_WARN），每個最多 159 字元。
        for w in list.into_iter().take(32) {
            let w: String = w.chars().take(159).collect();
            if ["zTXt", "iTXt", "iCCP", "tEXt"].iter().any(|k| w.contains(k)) {
                if s.rep.metadata_status == "valid" {
                    s.rep.metadata_status = "parse_failed".into();
                }
                issues.push(Issue::new(
                    code::METADATA_PARSE_FAILED,
                    "warning",
                    s.scope,
                    format!("libpng-compatible check: {w}"),
                ));
            } else {
                issues.push(Issue::new(
                    code::DECODER_WARNING,
                    "warning",
                    s.scope,
                    format!("libpng-compatible check: {w}"),
                ));
            }
        }
    }
}

impl<'a> Decoder<'a> for PngDecoder<'a> {
    fn buffer_estimate(&self) -> Option<u64> {
        let sc = &self.scan;
        let rows = png_row_budget(sc.width, sc.bit_depth, sc.color_type, sc.has_trns);
        if !self.interlaced() {
            return rows;
        }
        // Adam7 另外需要一列儲存（重建用）與暫存的寫入緩衝。
        let l = layout_for(sc.bit_depth, sc.color_type, sc.has_trns);
        let stored =
            u64::from(sc.width).checked_mul(u64::from(l.channels))?.checked_mul(if l.bits > 8 { 2 } else { 1 })?;
        rows?.checked_add(stored)?.checked_add(WRITE_BUFFER as u64)
    }

    fn open(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, _lim: &Limits) -> bool {
        // libpng 在 png_read_info 就會停下的情況（第一個 IDAT 之前的錯誤）。
        if let Some(f) = self.scan.lib.fatal.as_ref().filter(|f| f.phase == Phase::BeforeIdat) {
            s.failed = true;
            s.rep.decode_status = "failed".into();
            s.rep.decode_status_reason = if f.io { "io_error" } else { "header" }.into();
            s.rep.decoder_errors.push(f.message.clone());
            issues.push(Issue::new(code::DECODE_FAILED, "error", s.scope, format!("png decoder: {}", f.message)));
            return false;
        }
        let sc = &self.scan;
        let rb =
            (u64::from(sc.width) * png_channels(sc.color_type) * u64::from(sc.bit_depth.cast_unsigned())).div_ceil(8);
        let mut opts = DecodeOptions::default();
        // Adler-32 由 png_scan 獨立檢查（D-R11）：png crate 若自己檢查，小圖會在第一列之前就失敗。
        opts.set_ignore_adler32(true);
        opts.set_ignore_crc(false);
        opts.set_skip_ancillary_crc_failures(false);
        let feed = FeedReader::new(self.input, sc.feed.clone(), Rc::clone(&self.io_error));
        let mut dec = png::Decoder::new_with_options(BufReader::with_capacity(64 << 10, feed), opts);
        // png crate 自己的配置上限：幾列緩衝加解壓狀態，超過時回報資源上限（同 C++ 版限制 libpng 的配置）。
        let bytes = usize::try_from((rb + 64).saturating_mul(4).saturating_add(4 << 20)).unwrap_or(usize::MAX);
        dec.set_limits(png::Limits { bytes });
        dec.set_transformations(Transformations::IDENTITY);
        let rd = match dec.read_info() {
            Ok(r) => r,
            Err(e) => {
                s.failed = true;
                let f = self.classify(&e);
                s.rep.decode_status = if matches!(f, Failure::Resource) { "not_checked" } else { "failed" }.into();
                s.rep.decode_status_reason = match f {
                    Failure::Resource => "resource_limit",
                    Failure::Io => "io_error",
                    Failure::Decode => "header",
                }
                .into();
                s.rep.decoder_errors.push(e.to_string());
                let c = if matches!(f, Failure::Resource) { code::RESOURCE_LIMIT } else { code::DECODE_FAILED };
                issues.push(Issue::new(c, "error", s.scope, format!("png decoder: {e}")));
                return false;
            }
        };
        // 與獨立掃描交叉核對。
        let info = rd.info();
        let (oct, obd) = rd.output_color_type();
        let line = rd.output_line_size(info.width);
        if info.width != sc.width
            || info.height != sc.height
            || info.bit_depth as i32 != sc.bit_depth
            || info.color_type as i32 != sc.color_type
            || info.interlaced != (sc.interlace == 1)
            || oct != info.color_type
            || obd != info.bit_depth
            || line != Some(u64_to_usize(rb))
        {
            s.failed = true;
            s.rep.decode_status = "failed".into();
            s.rep.decode_status_reason = "decoder_disagrees_with_scan".into();
            issues.push(Issue::new(
                code::DECODE_FAILED,
                "error",
                s.scope,
                "png decoder header/row size disagrees with the container scan",
            ));
            return false;
        }
        self.reader = Some(rd);
        let accepted = sc.lib.iccp_accepted;
        s.rep.icc.decoder_accepted = accepted;
        s.rep.color = effective_color(sc, accepted);
        if sc.iccp_present && !accepted {
            issues.push(Issue::new(
                code::ICC_REJECTED_BY_DECODER,
                "warning",
                s.scope,
                "the embedded ICC profile fails the libpng-compatible iCCP check",
            ));
        }

        // IDAT 串流收尾（libpng 在最後一列讀取時處理）。
        let expected = expected_idat_bytes(sc);
        let st = sc.idat;
        let fatal_in_idat = sc.lib.fatal.as_ref().filter(|f| f.phase == Phase::InIdat).map(|f| f.message.clone());
        if let Some(m) = fatal_in_idat {
            self.idat_end_error = Some(m);
        } else if st.state == ZState::Adler && st.total_out == expected {
            self.idat_end_error = Some("IDAT: incorrect data check".into());
        }
        if st.extra_input {
            self.idat_warnings.push("IDAT: Extra compressed data".into());
        }
        if st.total_out > expected && matches!(st.state, ZState::Done | ZState::Adler) {
            if st.state == ZState::Adler {
                self.idat_warnings.push("IDAT: incorrect data check".into());
            }
            self.idat_warnings.push("IDAT: Too much image data".into());
        }

        self.raw.reserve(u64_to_usize(rb));
        if self.interlaced() {
            // 解碼開始前先在暫存預留整張正規影像。
            self.bps = if s.layout.bits > 8 { 2 } else { 1 };
            let (w, h) = (sc.width, sc.height);
            let mut total: u64 = 0;
            for p in 0..7 {
                self.pass_off[p] = total;
                let bytes = u64::from(pass_cols(w, p))
                    .checked_mul(u64::from(pass_rows(h, p)))
                    .and_then(|v| v.checked_mul(u64::from(s.layout.channels)))
                    .and_then(|v| v.checked_mul(u64::from(self.bps)));
                let Some(sum) = bytes.and_then(|b| total.checked_add(b)) else {
                    s.failed = true;
                    s.rep.decode_status = "not_checked".into();
                    s.rep.decode_status_reason = "resource_limit".into();
                    issues.push(Issue::new(
                        code::RESOURCE_LIMIT,
                        "error",
                        s.scope,
                        "Adam7 temporary size overflows 64-bit arithmetic",
                    ));
                    return false;
                };
                total = sum;
            }
            self.temp_total = total;
            let res = match s.temp.as_mut() {
                Some(t) => t.reserve(&mut s.quota.borrow_mut(), total),
                None => Err(crate::temp::TempError::new(
                    crate::temp::TempErrorKind::Unavailable,
                    "no temporary file for this side",
                )),
            };
            if let Err(e) = res {
                temp_failure(s, issues, &e);
                return false;
            }
            self.store = vec![0; w as usize * s.layout.channels as usize * self.bps as usize];
        }
        true
    }

    fn read_row(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool {
        if s.failed {
            return false;
        }
        if self.interlaced() {
            if !self.passes_done && !self.decode_passes(s, issues) {
                return false;
            }
            if !self.reconstruct_row(s, issues) {
                return false;
            }
            s.rows += 1;
            return true;
        }
        let y = s.rows;
        if y + 1 == u64::from(self.scan.height)
            && let Some(m) = self.idat_end_error.clone()
        {
            return self.fail(s, issues, Failure::Decode, m, format!("row {y}"), y > 0);
        }
        match self.next_raw() {
            Ok(Some(InterlaceInfo::Null(_))) => {}
            Ok(Some(_)) => {
                s.failed = true;
                s.rep.decode_status = if y > 0 { "partial" } else { "failed" }.into();
                s.rep.decode_status_reason = "decoder_disagrees_with_scan".into();
                issues.push(Issue::new(code::DECODE_FAILED, "error", s.scope, "png decoder returned an Adam7 row"));
                return false;
            }
            Ok(None) => {
                return self.fail(
                    s,
                    issues,
                    Failure::Decode,
                    "Not enough image data".into(),
                    format!("row {y}"),
                    y > 0,
                );
            }
            Err((f, m)) => return self.fail(s, issues, f, m, format!("row {y}"), y > 0),
        }
        let mut canon = std::mem::take(&mut s.canon);
        let layout = s.layout.clone();
        self.expand_row(&mut canon, &layout, self.scan.width);
        s.canon = canon;
        s.rows += 1;
        if s.rows == u64::from(self.scan.height) {
            self.reached_idat_end = true;
        }
        true
    }

    fn finish(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) {
        s.rep.coverage.rows_decoded = s.rows;
        let mut after_idat_read = false;
        if !s.failed && s.rows == u64::from(self.scan.height) {
            after_idat_read = true;
            // png_read_end：IDAT 之後的 chunk（缺 IEND、CRC 錯誤、截斷）。
            if let Some(f) = self.scan.lib.fatal.as_ref().filter(|f| f.phase == Phase::AfterIdat) {
                s.failed = true;
                s.rep.decode_status = "partial".into();
                s.rep.decode_status_reason = "after_image_data".into();
                s.rep.decoder_errors.push(f.message.clone());
                issues.push(Issue::new(
                    code::DECODE_FAILED,
                    "error",
                    s.scope,
                    format!("png decoder after image data: {}", f.message),
                ));
            } else {
                s.rep.decode_status = "complete".into();
                s.rep.decode_status_reason.clear();
            }
        }
        self.record_warnings(s, issues, after_idat_read);
        if self.palette_oob {
            s.rep.decode_status = "failed".into();
            s.rep.decode_status_reason = "palette_index_out_of_range".into();
            issues.push(Issue::new(code::DECODE_FAILED, "error", s.scope, "palette index outside PLTE"));
        }
        // 解碼完整但容器損壞（例如後面 chunk 的 CRC）：不算完整。
        if s.rep.decode_status == "complete" && s.rep.container_status == "corrupt" {
            s.rep.decode_status = "partial".into();
            s.rep.decode_status_reason = "container_corrupt".into();
        }
    }

    fn memory_metrics(&self, m: &mut BTreeMap<String, u64>) {
        *m.entry("tool_row_buffers".into()).or_default() += (self.raw.capacity() + self.store.capacity()) as u64;
    }
}

/// IDAT 解壓後應有的位元組數（每列加一個濾波位元組）。
fn expected_idat_bytes(sc: &PngScan) -> u64 {
    let depth = u64::from(sc.bit_depth.cast_unsigned());
    let row = |w: u32| (u64::from(w) * png_channels(sc.color_type) * depth).div_ceil(8) + 1;
    if sc.interlace == 1 {
        (0..7)
            .filter(|&p| pass_cols(sc.width, p) > 0)
            .map(|p| row(pass_cols(sc.width, p)) * u64::from(pass_rows(sc.height, p)))
            .sum()
    } else {
        row(sc.width) * u64::from(sc.height)
    }
}

/// 掃描並填好 FileReport；可以開始解碼時回傳解碼器（同 C++ 版 `prepare_png`）。
pub fn prepare_png<'a>(
    s: &mut SideState<'a>,
    issues: &mut Vec<Issue>,
    lim: &Limits,
) -> Option<Box<dyn Decoder<'a> + 'a>> {
    let t0 = std::time::Instant::now();
    let sc = scan_png(s.input, lim);
    s.scan_ms = crate::ms_since(t0);
    for mut i in sc.issues.iter().cloned() {
        i.scope = s.scope.into();
        issues.push(i);
    }
    let fr = &mut s.rep;
    fr.container_status = sc.container_status.into();
    fr.metadata_status = sc.metadata_status.into();
    fr.chunks = sc.chunk_order.clone();
    fr.coverage.bytes_scanned = Count(sc.bytes_scanned);
    if sc.ended {
        fr.coverage.trailing_bytes = Some(Count(sc.trailing_bytes));
    }
    fr.icc = sc.icc.clone();
    fill_metadata(fr, &sc);
    if !sc.has_ihdr {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "no_valid_ihdr".into();
        return None;
    }
    fr.width = sc.width;
    fr.height = sc.height;
    s.width = sc.width;
    s.height = sc.height;
    fr.interlace = sc.interlace; // 取自 IHDR，從不推測
    s.layout = layout_for(sc.bit_depth, sc.color_type, sc.has_trns);
    let fr = &mut s.rep;
    fr.coverage.rows_total = u64::from(sc.height);
    let sm = &mut fr.sample_model;
    sm.model = s.layout.model.into();
    sm.container_bits = sc.bit_depth.cast_unsigned();
    sm.channel_bits = vec![s.layout.bits; s.layout.channels as usize];
    sm.compare_type = match s.layout.bits {
        16 => "uint16",
        8 => "uint8",
        _ => "native_codes",
    }
    .into();
    sm.alpha = if has_alpha(s.layout.model) { "straight" } else { "none" }.into();
    if sc.color_type == 3 {
        sm.expansion = format!("palette{} -> {}8", if sc.has_trns { "+tRNS" } else { "" }, s.layout.model);
    } else if sc.has_trns {
        sm.expansion = "tRNS colour key -> alpha (key=0, else max)".into();
    }
    fr.decode_path =
        if sc.interlace == 1 { "png-adam7-passes-no-transform+temp" } else { "png-rows-no-transform" }.into();

    // 方向：eXIf 中的 Exif，否則是 PNG 的預設（不旋轉）。
    let scope = s.scope;
    if sc.exif_status == "over_limit" {
        // 沒讀到就是不知道：不假設 PNG 預設（D21）。
        fr.orientation = Orientation { source: "unknown".into(), value: 0 };
        issues.push(Issue::new(
            code::ORIENTATION_UNKNOWN,
            "warning",
            scope,
            "eXIf exceeds the metadata budget; orientation not read",
        ));
    } else if let Some(e) = &sc.exif {
        let o = exif_orientation(e);
        if o < 0 {
            fr.orientation = Orientation { source: "unparseable".into(), value: 0 };
            issues.push(Issue::new(
                code::ORIENTATION_UNPARSEABLE,
                "warning",
                scope,
                "eXIf orientation could not be parsed",
            ));
        } else if o == 0 {
            fr.orientation = Orientation { source: "default".into(), value: 1 };
        } else {
            fr.orientation = Orientation { source: "exif".into(), value: o.cast_unsigned() };
        }
    } else {
        fr.orientation = Orientation { source: "default".into(), value: 1 };
    }

    if sc.animated {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "animation".into();
        issues.push(Issue::new(
            code::ANIMATION_NOT_SUPPORTED,
            "error",
            scope,
            "APNG (acTL/fcTL/fdAT) is not supported",
        ));
        return None;
    }
    if sc.interlace != 0 && sc.interlace != 1 {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "invalid_interlace_method".into();
        return None;
    }
    Some(Box::new(PngDecoder {
        scan: Box::new(sc),
        input: s.input,
        io_error: Rc::new(Cell::new(false)),
        reader: None,
        raw: Vec::new(),
        idat_end_error: None,
        idat_warnings: Vec::new(),
        reached_idat_end: false,
        palette_oob: false,
        bps: 1,
        pass_off: [0; 7],
        temp_total: 0,
        passes_done: false,
        store: Vec::new(),
    }))
}
