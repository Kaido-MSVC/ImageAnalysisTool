//! PSD／PSB 前端（移植 C++ 版 `src/engine/psd_side.cpp`）：自己的結構掃描 + 自己的合併影像列解碼器。
//!
//! Raw 與 RLE（PackBits）逐列解碼：聲道以平面儲存，所以每個用到的聲道各有一個小的循序串流，
//! 不需要整張影像的緩衝或暫存檔。樣本是原生整數（8／16 位元 big-endian、1 位元 bitmap），從不轉換。
//! 支援範圍同 C++ 版 1.0.0：Bitmap／灰階／Indexed／RGB 的 8／16 位元，壓縮只有 Raw 與 RLE；
//! ZIP、CMYK、32 位元回報不支援（docs/decisions.md D-R16）。

use std::collections::BTreeMap;

use crate::engine::{Decoder, RowLayout, SideState};
use crate::input::Input;
use crate::limits::Limits;
use crate::png_scan::exif_orientation;
use crate::psd_scan::{PackBitsRow, PsdScan, be16, be32, scan_psd};
use crate::report::{Count, EffectiveColor, FileReport, Issue, MetaItem, Orientation, PsdInfo, code};
use crate::text::{sha256_hex, short_hash};
use crate::u64_to_usize;

fn mode_name(m: u16) -> &'static str {
    match m {
        0 => "bitmap",
        1 => "grayscale",
        2 => "indexed",
        3 => "rgb",
        4 => "cmyk",
        7 => "multichannel",
        8 => "duotone",
        9 => "lab",
        _ => "unknown",
    }
}

fn compression_name(c: u16) -> &'static str {
    match c {
        0 => "raw",
        1 => "rle",
        2 => "zip",
        3 => "zip_prediction",
        _ => "unknown",
    }
}

const STREAM_BUFFER: u64 = 1 << 20;

/// 一個聲道平面的循序讀取器。
#[derive(Default)]
struct Stream {
    pos: u64,
    end: u64,
    cap: usize,
    buf: Vec<u8>,
    at: usize,
}

impl Stream {
    fn new(pos: u64, end: u64, cap: usize) -> Self {
        Self { pos, end, cap, buf: Vec::new(), at: 0 }
    }

    /// 複製 dst.len() bytes；I/O 錯誤或平面提早結束時回傳 false。
    fn take(&mut self, input: &Input<'_>, mut dst: &mut [u8]) -> bool {
        while !dst.is_empty() {
            if self.at == self.buf.len() {
                if self.pos >= self.end {
                    return false;
                }
                let want = usize::try_from(self.end - self.pos).map_or(self.cap, |n| n.min(self.cap));
                self.buf.resize(want, 0);
                if !matches!(input.read_at(self.pos, &mut self.buf), Ok(n) if n == want) {
                    return false;
                }
                self.pos += want as u64;
                self.at = 0;
            }
            let k = dst.len().min(self.buf.len() - self.at);
            dst[..k].copy_from_slice(&self.buf[self.at..self.at + k]);
            self.at += k;
            dst = &mut dst[k..];
        }
        true
    }
}

struct PsdDecoder<'a> {
    input: &'a Input<'a>,
    sc: Box<PsdScan>,
    used: u32,
    out_ch: u32,
    /// 每個用到的聲道解出的一列
    raw: Vec<Vec<u8>>,
    streams: Vec<Stream>,
    packed: Vec<u8>,
    /// RLE 的列位元組數，只含用到的聲道（聲道為主）
    counts: Vec<u32>,
    first: Vec<u16>,
    uniform: bool,
    palette_oob: bool,
}

impl<'a> PsdDecoder<'a> {
    /// 一列最佳 PackBits 的大小（128 一組的字面 + 標頭位元組）：只決定讀取緩衝的大小；
    /// 更長的合法列分段解碼（D33）。
    fn max_packed(&self) -> u64 {
        self.sc.row_bytes + self.sc.row_bytes.div_ceil(128)
    }

    fn stream_cap(&self) -> u64 {
        STREAM_BUFFER.min(self.sc.row_bytes.max(4096))
    }

    fn fail(
        &self,
        s: &mut SideState<'_>,
        issues: &mut Vec<Issue>,
        status: &str,
        reason: &str,
        issue_code: &str,
        msg: String,
    ) -> bool {
        s.failed = true;
        s.rep.decode_status = status.into();
        s.rep.decode_status_reason = reason.into();
        issues.push(Issue::new(issue_code, "error", s.scope, msg));
        false
    }

    fn sample(&self, c: usize, x: usize) -> u16 {
        let p = &self.raw[c];
        match self.sc.depth {
            16 => u16::from_be_bytes([p[2 * x], p[2 * x + 1]]),
            8 => u16::from(p[x]),
            _ => u16::from((p[x >> 3] >> (7 - (x & 7))) & 1), // bitmap
        }
    }

    fn expand(&mut self, canon: &mut [u16]) {
        let oc = self.out_ch as usize;
        for x in 0..self.sc.width as usize {
            let px = &mut canon[x * oc..(x + 1) * oc];
            match self.sc.mode {
                2 => {
                    let idx = self.sample(0, x) as usize;
                    if self.sc.palette_count.is_some_and(|n| idx >= usize::from(n)) {
                        self.palette_oob = true;
                    }
                    px[0] = u16::from(self.sc.palette[idx]);
                    px[1] = u16::from(self.sc.palette[256 + idx]);
                    px[2] = u16::from(self.sc.palette[512 + idx]);
                    if oc == 4 {
                        px[3] = if self.sc.transparent_index.is_some_and(|t| usize::from(t) == idx) { 0 } else { 255 };
                    }
                }
                // bitmap：1 = 黑 -> 灰階碼 0
                0 => px[0] = if self.sample(0, x) != 0 { 0 } else { 1 },
                _ => {
                    for (c, v) in px.iter_mut().enumerate() {
                        *v = self.sample(c, x);
                    }
                }
            }
        }
    }

    fn check_uniform(&mut self, canon: &[u16], row: u64) {
        let oc = self.out_ch as usize;
        if row == 0 {
            self.first = canon[..oc].to_vec();
        }
        self.uniform = canon.chunks_exact(oc).all(|p| p == self.first.as_slice());
    }
}

impl<'a> Decoder<'a> for PsdDecoder<'a> {
    fn buffer_estimate(&self) -> Option<u64> {
        let canon = u64::from(self.sc.width).checked_mul(u64::from(self.out_ch))?.checked_mul(2)?;
        let rows = self.sc.row_bytes.checked_mul(u64::from(self.used))?;
        let packed = if self.sc.compression == 1 { self.max_packed() } else { 0 };
        let streams = self.stream_cap().checked_mul(u64::from(self.used))?;
        let table = if self.sc.compression == 1 {
            u64::from(self.sc.height).checked_mul(u64::from(self.used))?.checked_mul(4)?
        } else {
            0
        };
        canon.checked_add(rows)?.checked_add(packed)?.checked_add(streams)?.checked_add(table)?.checked_add(64 << 10)
    }

    fn open(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, _lim: &Limits) -> bool {
        let used = self.used as usize;
        let row_bytes = u64_to_usize(self.sc.row_bytes);
        let h = self.sc.height as usize;
        let mut ok = true;
        let mut alloc = |v: &mut Vec<u8>, n: usize| {
            ok &= v.try_reserve_exact(n).is_ok();
            v.resize(if ok { n } else { 0 }, 0);
        };
        self.raw = vec![Vec::new(); used];
        for r in &mut self.raw {
            alloc(r, row_bytes);
        }
        if self.sc.compression == 1 {
            let mut p = Vec::new();
            alloc(&mut p, u64_to_usize(self.max_packed()));
            self.packed = p;
            ok &= self.counts.try_reserve_exact(h * used).is_ok();
            if ok {
                self.counts.resize(h * used, 0);
            }
        }
        if !ok {
            return self.fail(
                s,
                issues,
                "not_checked",
                "resource_limit",
                code::RESOURCE_LIMIT,
                "PSD row buffers could not be allocated".into(),
            );
        }
        let cap = u64_to_usize(self.stream_cap());
        let data_end = self.sc.data_offset + self.sc.data_bytes;
        if self.sc.compression == 0 {
            let plane = u64::from(self.sc.height) * self.sc.row_bytes;
            self.streams = (0..used as u64)
                .map(|c| Stream::new(self.sc.data_offset + c * plane, self.sc.data_offset + (c + 1) * plane, cap))
                .collect();
            return true;
        }
        // RLE：用到的聲道的列位元組數（聲道為主），以及每個平面的起點。
        let wide = self.sc.version == 2;
        let entry = if wide { 4 } else { 2 };
        let mut tb = vec![0u8; (h * entry).min(1 << 20)];
        let mut start = self.sc.data_offset;
        for c in 0..used {
            let mut off = self.sc.rle_table_offset + (c * h * entry) as u64;
            let mut plane = 0u64;
            let mut y = 0usize;
            while y < h {
                let n = (h - y).min(tb.len() / entry);
                let buf = &mut tb[..n * entry];
                if !matches!(self.input.read_at(off, buf), Ok(k) if k == n * entry) {
                    return self.fail(
                        s,
                        issues,
                        "failed",
                        "io_error",
                        code::DECODE_FAILED,
                        "reading the RLE row byte-count table failed".into(),
                    );
                }
                for (i, e) in buf.chunks_exact(entry).enumerate() {
                    let v = if wide { be32(e) } else { u32::from(be16(e)) };
                    self.counts[c * h + y + i] = v;
                    plane += u64::from(v);
                }
                off += (n * entry) as u64;
                y += n;
            }
            self.streams.push(Stream::new(start, (start + plane).min(data_end), cap));
            start += plane;
        }
        true
    }

    fn read_row(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool {
        if s.failed {
            return false;
        }
        let y = u64_to_usize(s.rows);
        let h = self.sc.height as usize;
        let partial = if s.rows > 0 { "partial" } else { "failed" };
        for c in 0..self.used as usize {
            let unreadable = || format!("channel {c} row {y} could not be read");
            if self.sc.compression == 0 {
                if !self.streams[c].take(self.input, &mut self.raw[c]) {
                    return self.fail(s, issues, partial, "io_error", code::DECODE_FAILED, unreadable());
                }
                continue;
            }
            // 表裡的列位元組數一定整個用掉，每段最多 packed.len()；合法但不是最佳的 PackBits
            // （0x80 空操作、拆開的 run）可能比最佳大小更長（D33），所以這裡不限制它。
            let mut left = u64::from(self.counts[c * h + y]);
            let mut row = PackBitsRow::new(&mut self.raw[c]);
            let mut why: Result<(), String> = Ok(());
            while left > 0 && why.is_ok() {
                let k = usize::try_from(left).map_or(self.packed.len(), |n| n.min(self.packed.len()));
                if !self.streams[c].take(self.input, &mut self.packed[..k]) {
                    return self.fail(s, issues, partial, "io_error", code::DECODE_FAILED, unreadable());
                }
                why = row.feed(&self.packed[..k]).map_err(String::from);
                left -= k as u64;
            }
            let why = why.and_then(|()| row.finish());
            if let Err(why) = why {
                return self.fail(
                    s,
                    issues,
                    partial,
                    "rle_row_invalid",
                    code::STRUCTURE_ERROR,
                    format!("channel {c} row {y}: {why}"),
                );
            }
        }
        let mut canon = std::mem::take(&mut s.canon);
        self.expand(&mut canon);
        if self.uniform {
            self.check_uniform(&canon, s.rows);
        }
        s.canon = canon;
        s.rows += 1;
        true
    }

    fn finish(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) {
        s.rep.coverage.rows_decoded = s.rows;
        if self.palette_oob {
            s.rep.decode_status = "failed".into();
            s.rep.decode_status_reason = "palette_index_out_of_range".into();
            issues.push(Issue::new(
                code::DECODE_FAILED,
                "error",
                s.scope,
                "palette index outside the declared number of colours",
            ));
            return;
        }
        if s.failed || s.rows != u64::from(self.sc.height) {
            return;
        }
        s.rep.decode_status = "complete".into();
        s.rep.decode_status_reason.clear();
        if self.uniform
            && let Some(p) = s.rep.psd.as_mut()
        {
            p.composite_uniform = true;
            issues.push(Issue::new(
                code::PSD_COMPOSITE_UNIFORM,
                "warning",
                s.scope,
                "every pixel of the merged image has the same value; the composite may not be a real merge of the layers",
            ));
        }
    }

    fn memory_metrics(&self, m: &mut BTreeMap<String, u64>) {
        let mut b = self.packed.capacity() as u64 + self.counts.capacity() as u64 * 4;
        b += self.raw.iter().map(|v| v.capacity() as u64).sum::<u64>();
        b += self.streams.iter().map(|st| st.buf.capacity() as u64).sum::<u64>();
        *m.entry("tool_row_buffers".into()).or_default() += b;
    }
}

fn fill_metadata(fr: &mut FileReport, sc: &PsdScan) {
    let mut item = |name: &str, present: bool, status: &str, summary: String| {
        let status = if present { status } else { "absent" };
        fr.metadata.insert(name.into(), MetaItem { present, status: status.into(), summary });
    };
    if sc.exif_status == "over_limit" {
        item("exif", true, "over_limit", "exceeds metadata budget".into());
    } else if sc.exif_status == "present" {
        let o = exif_orientation(&sc.exif);
        let summary = format!(
            "bytes={} sha256={}{}",
            sc.exif.len(),
            short_hash(&sha256_hex(&sc.exif)),
            if o > 0 { format!(" orientation={o}") } else { String::new() }
        );
        item("exif", true, if o < 0 { "parse_failed" } else { "present" }, summary);
    } else {
        item("exif", false, "", String::new());
    }
    match sc.xmp_bytes {
        Some(n) => item("xmp", true, "present", format!("bytes={n} sha256={}", short_hash(&sc.xmp_sha256))),
        None => item("xmp", false, "", String::new()),
    }
    match sc.iptc_bytes {
        Some(n) => item("text", true, "present", format!("IPTC bytes={n} sha256={}", short_hash(&sc.iptc_sha256))),
        None => item("text", false, "", String::new()),
    }
    item("resolution", sc.resolution.is_some(), "present", sc.resolution.clone().unwrap_or_default());
    item("other_chunks", sc.resource_count > 0, "present", format!("image_resources={}", sc.resource_count));
}

pub fn prepare_psd<'a>(
    s: &mut SideState<'a>,
    issues: &mut Vec<Issue>,
    lim: &Limits,
) -> Option<Box<dyn Decoder<'a> + 'a>> {
    let t0 = std::time::Instant::now();
    let sc = scan_psd(s.input, lim);
    s.scan_ms = crate::ms_since(t0);
    let scope = s.scope;
    for mut i in sc.issues.iter().cloned() {
        i.scope = scope.into();
        issues.push(i);
    }
    let fr = &mut s.rep;
    fr.container_status = sc.container_status.into();
    fr.metadata_status = sc.metadata_status.into();
    fr.chunks = sc.sections.clone();
    fr.coverage.bytes_scanned = Count(sc.bytes_scanned);
    fr.coverage.trailing_bytes = sc.trailing_bytes.map(Count);
    fill_metadata(fr, &sc);
    if !sc.header_ok {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "structure_error".into();
        return None;
    }
    fr.width = sc.width;
    fr.height = sc.height;
    fr.coverage.rows_total = u64::from(sc.height);
    s.width = sc.width;
    s.height = sc.height;

    let composite = if sc.version_info {
        if sc.has_real_merged { "saved" } else { "not_saved" }
    } else if sc.layers_known && sc.layer_count == 0 {
        "flattened"
    } else {
        "flag_absent"
    };
    let fr = &mut s.rep;
    fr.psd = Some(PsdInfo {
        version: u32::from(sc.version),
        color_mode: mode_name(sc.mode).into(),
        depth: u32::from(sc.depth),
        channels: u32::from(sc.channels),
        compression: compression_name(sc.compression).into(),
        composite: composite.into(),
        composite_uniform: false,
        layer_count_field: sc.layer_count_field,
        layer_count: sc.layer_count,
        layers_known: sc.layers_known,
        alpha: String::new(),
    });

    // 色彩：只有內嵌的 ICC。PSD 沒有規定的預設色彩空間（同 TIFF 的 D7）。
    let mut signals = Vec::new();
    if sc.icc_untagged {
        signals.push("ICC untagged flag (0x0411)".to_string());
    }
    if sc.icc_status == "embedded" {
        let sha = sha256_hex(&sc.icc);
        fr.icc.status = "embedded".into();
        fr.icc.bytes = Some(Count(sc.icc.len() as u64));
        fr.icc.sha256 = sha.clone();
        signals.push(format!("ICC({})", short_hash(&sha)));
        fr.color = EffectiveColor {
            kind: "icc".into(),
            key: format!("icc:{sha}"),
            description: format!("embedded ICC {} bytes (resource 0x040F)", sc.icc.len()),
            signals,
        };
    } else if sc.icc_status == "over_budget" {
        fr.icc.status = "over_budget".into();
        fr.color = EffectiveColor {
            kind: "icc_unreadable".into(),
            key: String::new(),
            description: "ICC resource exceeds the metadata budget".into(),
            signals,
        };
    } else {
        fr.color = EffectiveColor {
            kind: "none".into(),
            key: String::new(),
            description: "no ICC profile (PSD has no specified default colour space)".into(),
            signals,
        };
    }
    // 方向：Photoshop 的檔頭沒有方向；有 EXIF（0x0422）時使用。
    if sc.exif_status == "over_limit" {
        fr.orientation = Orientation { source: "unknown".into(), value: 0 };
        issues.push(Issue::new(
            code::ORIENTATION_UNKNOWN,
            "warning",
            scope,
            "EXIF exceeds the metadata budget; orientation not read",
        ));
    } else if sc.exif_status == "present" {
        let o = exif_orientation(&sc.exif);
        if o < 0 {
            fr.orientation = Orientation { source: "unparseable".into(), value: 0 };
            issues.push(Issue::new(
                code::ORIENTATION_UNPARSEABLE,
                "warning",
                scope,
                "EXIF orientation could not be parsed",
            ));
        } else if o == 0 {
            fr.orientation = Orientation { source: "default".into(), value: 1 };
        } else {
            fr.orientation = Orientation { source: "exif".into(), value: o.cast_unsigned() };
        }
    } else {
        fr.orientation = Orientation { source: "default".into(), value: 1 };
    }

    if sc.container_status == "corrupt" {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "structure_error".into();
        return None;
    }
    if composite == "not_saved" {
        fr.decode_status = "not_checked".into();
        fr.decode_status_reason = "composite_not_saved".into();
        issues.push(Issue::new(
            code::PSD_COMPOSITE_NOT_SAVED,
            "error",
            scope,
            "version info (0x0421) says the file has no real merged image (saved without Maximize Compatibility); \
             the composite data is not compared",
        ));
        return None;
    }
    if composite == "flag_absent" {
        issues.push(Issue::new(
            code::PSD_COMPOSITE_FLAG_ABSENT,
            "warning",
            scope,
            "the file has layers but no version info resource (0x0421), so it cannot tell whether the merged image is \
             a real composite; the stored merged image is compared as is",
        ));
    }

    // 樣本模型
    let (base, model): (u32, &'static str) = match sc.mode {
        0 | 1 => (1, "gray"),
        2 => (1, "rgb"),
        3 => (3, "rgb"),
        _ => (0, ""),
    };
    if base == 0 || sc.depth == 32 {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "unsupported_sample_model".into();
        issues.push(Issue::new(
            code::UNSUPPORTED_SAMPLE_MODEL,
            "error",
            scope,
            format!(
                "PSD colour mode {}, {}-bit is outside the strict scope (Bitmap, Grayscale, Indexed, RGB at 8/16-bit)",
                mode_name(sc.mode),
                sc.depth
            ),
        ));
        return None;
    }
    if sc.compression == 2 || sc.compression == 3 {
        fr.decode_status = "not_checked".into();
        fr.decode_status_reason = "compression_not_supported".into();
        issues.push(Issue::new(
            code::COMPRESSION_NOT_SUPPORTED,
            "error",
            scope,
            format!(
                "merged image data compression {} is not supported (raw and RLE only)",
                compression_name(sc.compression)
            ),
        ));
        return None;
    }
    let channels = u32::from(sc.channels);
    let merged_alpha = sc.layer_count_field < 0 && sc.mode != 2 && sc.mode != 0 && channels > base;
    if sc.layer_count_field < 0 && !merged_alpha && sc.mode != 2 && sc.mode != 0 {
        issues.push(Issue::new(
            "psd_container_warning",
            "warning",
            scope,
            "negative layer count announces merged transparency, but there is no extra channel",
        ));
    }
    let used = base + u32::from(merged_alpha);
    let ignored = channels - used;
    if ignored > 0 {
        issues.push(Issue::new(
            code::EXTRA_CHANNEL_NOT_SUPPORTED,
            "warning",
            scope,
            format!(
                "{ignored} extra channel(s) (saved selections / spot colours) are not part of the image and are not compared"
            ),
        ));
    }

    let depth = u32::from(sc.depth);
    let sm = &mut fr.sample_model;
    sm.container_bits = depth;
    if sc.mode == 2 {
        let trans = sc.transparent_index.is_some();
        s.layout = RowLayout::new(if trans { "rgba" } else { "rgb" }, if trans { 4 } else { 3 }, 8);
        sm.expansion = format!("palette{} -> {}8", if trans { "+transparent index" } else { "" }, s.layout.model);
        sm.alpha = if trans { "straight" } else { "none" }.into();
    } else if sc.mode == 0 {
        s.layout = RowLayout::new("gray", 1, 1);
        sm.expansion = "bitmap (1 = black) inverted to gray 1-bit (0 = black)".into();
        sm.alpha = "none".into();
    } else {
        let m = if merged_alpha { if base == 1 { "gray_alpha" } else { "rgba" } } else { model };
        s.layout = RowLayout::new(m, used, depth);
        // Photoshop 把半透明的合併像素和白色混合；樣本照儲存的值。
        sm.alpha = if merged_alpha { "matted_white" } else { "none" }.into();
    }
    sm.model = s.layout.model.into();
    sm.channel_bits = vec![s.layout.bits; s.layout.channels as usize];
    sm.compare_type = match s.layout.bits {
        16 => "uint16",
        8 => "uint8",
        _ => "native_codes",
    }
    .into();
    let alpha = sm.alpha.clone();
    if let Some(p) = fr.psd.as_mut() {
        p.alpha = alpha;
    }
    fr.decode_path = format!("psd-composite-{}-rows", compression_name(sc.compression));
    let out_ch = s.layout.channels;
    Some(Box::new(PsdDecoder {
        input: s.input,
        sc: Box::new(sc),
        used,
        out_ch,
        raw: Vec::new(),
        streams: Vec::new(),
        packed: Vec::new(),
        counts: Vec::new(),
        first: Vec::new(),
        uniform: true,
        palette_oob: false,
    }))
}
