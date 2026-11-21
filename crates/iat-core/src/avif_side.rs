//! AVIF 前端（移植 C++ 版 `src/engine/avif_side.cpp`）：自己的 ISOBMFF 掃描（[`crate::avif_scan`]）、
//! libavif 行為的 item 解析（[`crate::avif_parse`]），AV1 用 rav1d（[`iat_av1`]，D-R09、D-R19）。
//!
//! 無損的判定分兩步（C++ 版 D30）：
//! - metadata：4:4:4 且 matrix_coefficients 0（identity），或 4:0:0；全範圍；直通 alpha；單張靜止影像。
//!   這只是必要條件，AV1 是否真的無損編碼從容器看不出來。
//! - 像素：只在 compare 模式，由引擎在樣本比較之後判定。
//!
//! metadata 不符合無損的檔案不解碼：解碼要做色度升取樣或 YUV→RGB 矩陣，也就是色彩轉換。
//! 解碼時平面直接讀取（identity：R = V、G = Y、B = U；4:0:0：Y = 灰階），8-bit 為 uint8，
//! 10／12-bit 為原生數值；limited range 的 alpha 照 libavif 轉成全範圍。

use std::collections::BTreeMap;

use iat_av1::{DecodeError, DecodeOptions, Layout, Picture, Plane as AvPlane};

use crate::avif_parse::{self, ParseFail, Parsed, Plane, YuvFormat};
use crate::avif_scan::{AvifScan, avif_orientation, scan_avif};
use crate::engine::{Decoder, RowLayout, SideState, temp_failure};
use crate::limits::Limits;
use crate::png_scan::exif_orientation;
use crate::report::{AvifInfo, Count, EffectiveColor, FileReport, Issue, MetaItem, Orientation, code};
use crate::temp::{TempError, TempErrorKind};
use crate::text::{sha256_hex, short_hash};
use crate::worker::Av1Guard;

pub const DECODER_NAME: &str = "rav1d";
pub const DECODER_VERSION: &str = "1.1.0";
/// 部分列的區間清單：與影像大小無關的固定額度（同 C++ 版 kCoverageAllowance）
const COVERAGE_ALLOWANCE: u64 = 1 << 20;

/// libavif 的 `avifLimitedToFullY`（reformat.c）：limited range 的 alpha 轉成全範圍。
pub fn limited_to_full(depth: u8, v: u16) -> u16 {
    let (lo, hi, full): (i32, i32, i32) = match depth {
        8 => (16, 235, 255),
        10 => (64, 940, 1023),
        12 => (256, 3760, 4095),
        _ => return v,
    };
    let v = i32::from(v);
    (((v - lo) * full + (hi - lo) / 2) / (hi - lo)).clamp(0, full) as u16
}

/// 解碼前就確定的版面。
struct Geometry {
    width: u32,
    height: u32,
    /// 1（灰階）或 3（identity 的 Y、U、V）
    planes: u32,
    alpha: bool,
    depth: u32,
}

impl Geometry {
    fn channels(&self) -> u32 {
        self.planes + u32::from(self.alpha)
    }
    fn bytes_per_sample(&self) -> u64 {
        if self.depth <= 8 { 1 } else { 2 }
    }
    fn row_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.channels()) * self.bytes_per_sample()
    }
}

/// 解碼後的色彩或 alpha：grid 時每個 tile 一張 picture。
struct Decoded {
    grid: Option<avif_parse::Grid>,
    tiles: Vec<Picture>,
}

impl Decoded {
    /// (tile 寬, tile 高)：所有 tile 都相同（已檢查）。
    fn tile_size(&self) -> (u32, u32) {
        (self.tiles[0].width(), self.tiles[0].height())
    }
    fn image_size(&self) -> (u32, u32) {
        match self.grid {
            Some(g) => (g.output_width, g.output_height),
            None => self.tile_size(),
        }
    }
    fn depth(&self) -> u8 {
        self.tiles[0].bit_depth()
    }
    /// 影像第 `y` 列、`plane` 的樣本寫進 `out`（長度為影像寬）。
    fn read_row(&self, plane: AvPlane, y: u32, out: &mut [u16], scratch: &mut Vec<u16>) {
        let (tw, th) = self.tile_size();
        let Some(g) = self.grid else {
            self.tiles[0].read_row(plane, y, out);
            return;
        };
        let (r, ty) = (y / th, y % th);
        scratch.resize(tw as usize, 0);
        for c in 0..g.columns {
            let x0 = (c * tw) as usize;
            if x0 >= out.len() {
                break;
            }
            let n = (out.len() - x0).min(tw as usize);
            let pic = &self.tiles[(r * g.columns + c) as usize];
            pic.read_row(plane, ty, scratch);
            out[x0..x0 + n].copy_from_slice(&scratch[..n]);
        }
    }
}

/// 解碼失敗的原因：(decode_status_reason, 問題訊息, decoder_errors 的一筆)、資源上限或暫存錯誤。
enum Fail {
    Decode { reason: &'static str, message: String, detail: String },
    Resource(String),
    Temp(TempError),
}

fn libavif_style(result: &str, detail: &str) -> (String, String) {
    let msg = format!("rav1d: {result}");
    let full = if detail.is_empty() { msg.clone() } else { format!("{msg} ({detail})") };
    (msg, full)
}

fn parse_fail(e: &avif_parse::ParseError) -> Fail {
    let (message, detail) = libavif_style(e.result, &e.detail);
    match e.kind {
        ParseFail::OutOfMemory => Fail::Resource(message),
        ParseFail::Truncated => Fail::Decode { reason: "truncated", message, detail },
        ParseFail::Other => Fail::Decode { reason: "decode_error", message, detail },
    }
}

fn disagrees(what: String) -> Fail {
    Fail::Decode {
        reason: "decoder_disagrees_with_parse",
        message: "decoded planes do not match the parsed header".into(),
        detail: what,
    }
}

fn grid_error(detail: &str) -> Fail {
    let (message, detail) = libavif_style("Invalid image grid", detail);
    Fail::Decode { reason: "decode_error", message, detail }
}

struct AvifDecoder<'a> {
    input: &'a crate::input::Input<'a>,
    parsed: Parsed,
    geo: Geometry,
    codec_estimate: Option<u64>,
    row: Vec<u8>,
    decoded: bool,
}

impl AvifDecoder<'_> {
    /// 一個 plane（色彩或 alpha）的所有 tile：讀 item、rav1d 解碼、檢查尺寸（libavif 會把和 ispe
    /// 不同的畫格縮放；Rust 版判為 decoder_disagrees_with_parse，使用者決定 B，D-R19）。
    fn decode_plane(&self, s: &SideState<'_>, plane: &Plane, alpha: bool) -> Result<Decoded, Fail> {
        let mut tiles = Vec::with_capacity(plane.tiles.len());
        let opts = DecodeOptions { frame_size_limit: avif_parse::IMAGE_SIZE_LIMIT };
        let failed = if alpha { "Decoding of alpha planes failed" } else { "Decoding of color planes failed" };
        for t in &plane.tiles {
            let data = avif_parse::read_item(self.input, &t.data, 0).map_err(|e| parse_fail(&e))?;
            let pic = {
                let _guard = Av1Guard::new(s.scope);
                iat_av1::decode_still(&data, &opts)
            };
            drop(data);
            let pic = pic.map_err(|e: DecodeError| {
                let (message, detail) = libavif_style(failed, &e.to_string());
                Fail::Decode { reason: "decode_error", message, detail }
            })?;
            if pic.width() != t.width || pic.height() != t.height {
                return Err(disagrees(format!(
                    "AV1 frame {}x{} differs from the item's ispe {}x{} (libavif would rescale it)",
                    pic.width(),
                    pic.height(),
                    t.width,
                    t.height
                )));
            }
            tiles.push(pic);
        }
        let d = Decoded { grid: plane.grid, tiles };
        if let Some(g) = d.grid {
            // avifDecoderDataAllocateImagePlanes／CopyTileToImage 的檢查
            let first = &d.tiles[0];
            let (tw, th) = (u64::from(first.width()), u64::from(first.height()));
            if tw * u64::from(g.columns) < u64::from(g.output_width)
                || th * u64::from(g.rows) < u64::from(g.output_height)
            {
                return Err(grid_error("Grid image tiles do not completely cover the image"));
            }
            if tw * u64::from(g.columns - 1) >= u64::from(g.output_width)
                || th * u64::from(g.rows - 1) >= u64::from(g.output_height)
            {
                return Err(grid_error(
                    "Grid image tiles in the rightmost column and bottommost row do not overlap the reconstructed image grid canvas",
                ));
            }
            let fmt = first.layout();
            let sub_x = matches!(fmt, Layout::I420 | Layout::I422) && !alpha;
            let sub_y = matches!(fmt, Layout::I420) && !alpha;
            if tw < 64
                || th < 64
                || (sub_x && (g.output_width % 2 != 0 || tw % 2 != 0))
                || (sub_y && (g.output_height % 2 != 0 || th % 2 != 0))
            {
                return Err(grid_error("Grid image tile size is not valid"));
            }
            for p in &d.tiles[1..] {
                if p.width() != first.width()
                    || p.height() != first.height()
                    || p.bit_depth() != first.bit_depth()
                    || p.layout() != first.layout()
                    || p.full_range() != first.full_range()
                {
                    return Err(grid_error("Grid image contains mismatched tiles"));
                }
            }
        }
        Ok(d)
    }

    fn decode_all(&self, s: &mut SideState<'_>) -> Result<(), Fail> {
        if s.av1_failed {
            let (message, detail) = libavif_style("Decoding of color planes failed", "rav1d panicked");
            return Err(Fail::Decode { reason: "decode_error", message, detail });
        }
        let color = self.decode_plane(s, &self.parsed.color, false)?;
        let alpha = match &self.parsed.alpha {
            Some(a) => Some(self.decode_plane(s, a, true)?),
            None => None,
        };
        let (cw, ch) = color.image_size();
        if let Some(a) = &alpha
            && (a.image_size() != (cw, ch) || a.depth() != color.depth())
        {
            let (message, detail) = libavif_style(
                if a.grid.is_some() { "Invalid image grid" } else { "Decoding of alpha planes failed" },
                "The color image item does not match the alpha image item in width, height, or bit depth",
            );
            return Err(Fail::Decode { reason: "decode_error", message, detail });
        }
        // C++ 版：解碼後的影像和解析時的寬高、平面不符就停止
        let g = &self.geo;
        if (cw, ch) != (g.width, g.height) {
            return Err(disagrees(format!("decoded {cw}x{ch}, parsed {}x{}", g.width, g.height)));
        }
        let layout = color.tiles[0].layout();
        let layout_ok = if g.planes == 3 { layout == Layout::I444 } else { true };
        if u32::from(color.depth()) != g.depth || !layout_ok {
            return Err(disagrees(format!(
                "decoded {layout:?} {}-bit, parsed {} planes {}-bit",
                color.depth(),
                g.planes,
                g.depth
            )));
        }
        // 正規樣本依列寫進暫存檔：Gray 或 R、G、B，再加 A。identity：R = V、G = Y、B = U。
        let w = g.width as usize;
        let ch_n = g.channels() as usize;
        let (mut y_row, mut u_row, mut v_row, mut a_row) = (vec![0u16; w], vec![0u16; w], vec![0u16; w], vec![0u16; w]);
        let mut scratch = Vec::new();
        let mut out = Vec::with_capacity(self.geo.row_bytes() as usize);
        let alpha_full = alpha.as_ref().map(|a| a.tiles.iter().map(Picture::full_range).collect::<Vec<_>>());
        let temp = s.temp.as_mut().expect("open() reserved this temp file");
        for y in 0..g.height {
            color.read_row(AvPlane::Y, y, &mut y_row, &mut scratch);
            if g.planes == 3 {
                color.read_row(AvPlane::U, y, &mut u_row, &mut scratch);
                color.read_row(AvPlane::V, y, &mut v_row, &mut scratch);
            }
            if let (Some(a), Some(full)) = (&alpha, &alpha_full) {
                a.read_row(AvPlane::Y, y, &mut a_row, &mut scratch);
                let depth = a.depth();
                let (tw, th) = a.tile_size();
                let cols = a.grid.map_or(1, |gr| gr.columns);
                for (x, v) in a_row.iter_mut().enumerate() {
                    let t = ((y / th) * cols + x as u32 / tw) as usize;
                    if !full[t.min(full.len() - 1)] {
                        *v = limited_to_full(depth, *v);
                    }
                }
            }
            out.clear();
            for x in 0..w {
                let px: [u16; 4] =
                    if g.planes == 3 { [v_row[x], y_row[x], u_row[x], a_row[x]] } else { [y_row[x], a_row[x], 0, 0] };
                for &v in &px[..ch_n] {
                    if g.depth <= 8 {
                        out.push(v as u8);
                    } else {
                        out.extend_from_slice(&v.to_ne_bytes());
                    }
                }
            }
            temp.append(&out).map_err(Fail::Temp)?;
        }
        Ok(())
    }

    fn fail(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>, f: Fail) {
        s.failed = true;
        match f {
            Fail::Decode { reason, message, detail } => {
                // C++ 版：libavif 失敗時 decoder_errors 記完整訊息，問題只記結果名稱
                if reason != "decoder_disagrees_with_parse" {
                    s.rep.decoder_errors.push(detail);
                }
                s.rep.decode_status = "failed".into();
                s.rep.decode_status_reason = reason.into();
                issues.push(Issue::new(code::DECODE_FAILED, "error", s.scope, message));
            }
            Fail::Temp(e) => temp_failure(s, issues, &e),
            Fail::Resource(m) => {
                s.rep.decode_status = "not_checked".into();
                s.rep.decode_status_reason = "resource_limit".into();
                issues.push(Issue::new(code::RESOURCE_LIMIT, "error", s.scope, m));
            }
        }
    }
}

impl<'a> Decoder<'a> for AvifDecoder<'a> {
    fn buffer_estimate(&self) -> Option<u64> {
        // 正規列 + 暫存列（讀回）+ 轉換緩衝 + 每列一個狀態位元組 + 部分列額度（同 C++ 版 WholeImageDecoder）
        let g = &self.geo;
        let canon = u64::from(g.width).checked_mul(u64::from(g.channels()))?.checked_mul(2)?;
        let temp_row = g.row_bytes();
        canon
            .checked_add(temp_row)?
            .checked_add(temp_row)?
            .checked_add(u64::from(g.height))?
            .checked_add(COVERAGE_ALLOWANCE)
    }

    fn codec_estimate(&self) -> Option<u64> {
        self.codec_estimate
    }

    fn open(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, _lim: &Limits) -> bool {
        let Some(total) = self.geo.row_bytes().checked_mul(u64::from(self.geo.height)) else {
            self.fail(s, issues, Fail::Resource("decoded image size overflows 64-bit arithmetic".into()));
            return false;
        };
        let res = match s.temp.as_mut() {
            Some(t) => t.reserve(&mut s.quota.borrow_mut(), total),
            None => Err(TempError::new(TempErrorKind::Unavailable, "no temporary file for this side")),
        };
        if let Err(e) = res {
            temp_failure(s, issues, &e);
            return false;
        }
        s.rep.decode_path += "+temp";
        if let Err(f) = self.decode_all(s) {
            self.fail(s, issues, f);
            return false;
        }
        if let Err(e) = s.temp.as_mut().expect("reserved above").flush() {
            temp_failure(s, issues, &e);
            return false;
        }
        self.row = vec![0; self.geo.row_bytes() as usize];
        self.decoded = true;
        true
    }

    fn read_row(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool {
        if s.failed || !self.decoded {
            return false;
        }
        let off = s.rows * self.geo.row_bytes();
        let temp = s.temp.as_mut().expect("open() wrote the temp file");
        if let Err(e) = temp.read_at(off, &mut self.row) {
            temp_failure(s, issues, &e);
            return false;
        }
        if self.geo.depth <= 8 {
            for (dst, &src) in s.canon.iter_mut().zip(&self.row) {
                *dst = u16::from(src);
            }
        } else {
            for (dst, src) in s.canon.iter_mut().zip(self.row.as_chunks::<2>().0) {
                *dst = u16::from_ne_bytes(*src);
            }
        }
        s.rows += 1;
        true
    }

    fn finish(&mut self, s: &mut SideState<'a>, _issues: &mut Vec<Issue>) {
        s.rep.coverage.rows_decoded = s.rows;
        if !s.failed && self.decoded && s.rows == u64::from(self.geo.height) {
            s.rep.decode_status = "complete".into();
            s.rep.decode_status_reason.clear();
        }
    }

    fn memory_metrics(&self, m: &mut BTreeMap<String, u64>) {
        *m.entry("tool_row_buffers".into()).or_default() += self.row.capacity() as u64;
        *m.entry("coverage_state".into()).or_default() += u64::from(self.geo.height);
        if let Some(c) = self.codec_estimate {
            *m.entry("codec_estimate".into()).or_default() += c;
        }
    }
}

fn fill_metadata(fr: &mut FileReport, p: &Parsed, sc: &AvifScan) {
    let mut item = |name: &str, present: bool, status: &str, summary: String| {
        let status = if present { status } else { "absent" };
        fr.metadata.insert(name.into(), MetaItem { present, status: status.into(), summary });
    };
    match &p.exif {
        Some(e) if !e.is_empty() => {
            let o = exif_orientation(e);
            let summary = format!(
                "bytes={} sha256={}{}",
                e.len(),
                short_hash(&sha256_hex(e)),
                if o > 0 { format!(" orientation={o}") } else { String::new() }
            );
            item("exif", true, if o < 0 { "parse_failed" } else { "present" }, summary);
        }
        _ => item("exif", false, "", String::new()),
    }
    match &p.xmp {
        Some(x) if !x.is_empty() => {
            item("xmp", true, "present", format!("bytes={} sha256={}", x.len(), short_hash(&sha256_hex(x))))
        }
        _ => item("xmp", false, "", String::new()),
    }
    let hdr = if p.clli.0 != 0 || p.clli.1 != 0 { format!("clli={}/{}", p.clli.0, p.clli.1) } else { String::new() };
    item("hdr_signals", !hdr.is_empty(), "present", hdr);
    let boxes = sc.boxes.join(",");
    item("other_chunks", !boxes.is_empty(), "present", format!("boxes={boxes}"));
}

pub fn prepare_avif<'a>(
    s: &mut SideState<'a>,
    issues: &mut Vec<Issue>,
    lim: &Limits,
) -> Option<Box<dyn Decoder<'a> + 'a>> {
    let t0 = std::time::Instant::now();
    let sc = scan_avif(s.input);
    let scope = s.scope;
    for mut i in sc.issues.iter().cloned() {
        i.scope = scope.into();
        issues.push(i);
    }
    let fr = &mut s.rep;
    fr.container_status = sc.container_status.into();
    fr.chunks = sc.boxes.clone();
    fr.coverage.bytes_scanned = Count(sc.bytes_scanned);
    if sc.container_status != "corrupt" {
        fr.coverage.trailing_bytes = Some(Count(0)); // box 一直延伸到檔尾
    }
    fr.metadata_status = "valid".into();
    if sc.container_status == "corrupt" {
        s.scan_ms = crate::ms_since(t0);
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "structure_error".into();
        return None;
    }
    // 只解析標頭（不解像素），供 metadata 的無損判定
    let parsed = avif_parse::parse(s.input);
    s.scan_ms = crate::ms_since(t0);
    let fr = &mut s.rep;
    let p = match parsed {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("libavif-compatible parser: {}", e.message());
            fr.decoder_errors.push(msg.clone());
            if e.kind == ParseFail::OutOfMemory {
                fr.decode_status = "not_checked".into();
                fr.decode_status_reason = "resource_limit".into();
                issues.push(Issue::new(code::RESOURCE_LIMIT, "error", scope, msg));
            } else {
                let truncated = e.kind == ParseFail::Truncated;
                fr.container_status = "corrupt".into();
                fr.decode_status = "failed".into();
                fr.decode_status_reason = if truncated { "truncated" } else { "structure_error" }.into();
                issues.push(Issue::new(
                    if truncated { code::TRUNCATED } else { code::STRUCTURE_ERROR },
                    "error",
                    scope,
                    msg,
                ));
            }
            return None;
        }
    };
    if p.tracks {
        // 主要 brand 為 avis 的影像序列：libavif 走 tracks，這裡不解析（D-R19 已知差異）
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "animation".into();
        issues.push(Issue::new(
            code::ANIMATION_NOT_SUPPORTED,
            "error",
            scope,
            "AVIF image sequence (avis) is not supported",
        ));
        return None;
    }
    fr.width = p.width;
    fr.height = p.height;
    s.width = p.width;
    s.height = p.height;
    fr.coverage.rows_total = u64::from(p.height);
    fill_metadata(fr, &p, &sc);

    let mut ai = AvifInfo {
        yuv_format: p.yuv_format.name().into(),
        depth: p.depth,
        color_primaries: u32::from(p.primaries),
        transfer_characteristics: u32::from(p.transfer),
        matrix_coefficients: u32::from(p.matrix),
        full_range: p.full_range,
        cicp_source: if sc.nclx.is_empty() { "av1_sequence_header_or_default" } else { "colr_nclx" }.into(),
        alpha: p.alpha_present,
        alpha_premultiplied: p.alpha_premultiplied,
        image_count: p.image_count,
        ..AvifInfo::default()
    };

    // 色彩：ICC 優先於 CICP（AVIF／HEIF：colr prof 取代 nclx）
    let cicp = format!(
        "{}/{}/{}/{}",
        ai.color_primaries,
        ai.transfer_characteristics,
        ai.matrix_coefficients,
        u8::from(ai.full_range)
    );
    let mut signals = vec![format!("CICP({cicp}, {})", ai.cicp_source)];
    if let Some(icc) = &p.icc {
        if icc.len() as u64 > lim.metadata_item_bytes() {
            fr.icc.status = "over_budget".into();
            fr.color = EffectiveColor {
                kind: "icc_unreadable".into(),
                key: String::new(),
                description: "ICC exceeds the metadata budget".into(),
                signals,
            };
        } else {
            let sha = sha256_hex(icc);
            fr.icc.status = "embedded".into();
            fr.icc.bytes = Some(Count(icc.len() as u64));
            fr.icc.sha256 = sha.clone();
            signals.push(format!("ICC({})", short_hash(&sha)));
            fr.color = EffectiveColor {
                kind: "icc".into(),
                key: format!("icc:{sha}"),
                description: format!("embedded ICC {} bytes ('colr' prof)", icc.len()),
                signals,
            };
        }
    } else if !sc.nclx.is_empty() {
        // nclx 明確宣告：以 CICP key 比較，matrix 固定為 0、全範圍（PNG cICP 只描述 RGB）；實際值在 avif.*
        fr.color = EffectiveColor {
            kind: "cicp".into(),
            key: format!("cicp:{}/{}/0/1", ai.color_primaries, ai.transfer_characteristics),
            description: format!(
                "AVIF nclx primaries/transfer = {}/{} (RGB after identity decode)",
                ai.color_primaries, ai.transfer_characteristics
            ),
            signals,
        };
    } else {
        fr.color = EffectiveColor {
            kind: "none".into(),
            key: String::new(),
            description: "no 'colr' box (AV1 sequence header values only; not an explicit declaration)".into(),
            signals,
        };
    }

    // 方向：AVIF 以 irot／imir 為準，EXIF 的方向只是 metadata
    fr.orientation = if p.irot.is_some() || p.imir.is_some() {
        Orientation { source: "avif_irot_imir".into(), value: avif_orientation(p.irot.unwrap_or(0), p.imir) }
    } else {
        Orientation { source: "default".into(), value: 1 }
    };

    // metadata 的無損判定（只是必要條件）
    let why = &mut ai.lossless_metadata_reasons;
    if matches!(p.yuv_format, YuvFormat::Yuv420 | YuvFormat::Yuv422) {
        why.push("chroma_subsampled".into());
    }
    if p.yuv_format == YuvFormat::Yuv444 && p.matrix != 0 {
        // AVIF_MATRIX_COEFFICIENTS_YCGCO_RE 16、YCGCO_RO 17
        why.push(
            if p.matrix == 16 || p.matrix == 17 { "matrix_ycgco_r_not_supported" } else { "matrix_not_identity" }
                .into(),
        );
    }
    if !p.full_range {
        why.push("limited_range".into());
    }
    if ai.alpha_premultiplied {
        why.push("alpha_premultiplied".into());
    }
    ai.lossless_metadata = if why.is_empty() { "consistent" } else { "inconsistent" }.into();
    ai.pixel_verified_basis = "needs_reference".into(); // compare 模式由引擎判定
    let reasons = ai.lossless_metadata_reasons.clone();
    let yuv = ai.yuv_format.clone();
    let (matrix, full) = (ai.matrix_coefficients, ai.full_range);
    let alpha = ai.alpha;
    let image_count = ai.image_count;
    fr.avif = Some(ai);

    if image_count > 1 || sc.sequence {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "animation".into();
        issues.push(Issue::new(
            code::ANIMATION_NOT_SUPPORTED,
            "error",
            scope,
            "AVIF image sequence (avis) is not supported",
        ));
        return None;
    }
    if p.clap {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "clap_not_supported".into();
        issues.push(Issue::new(
            code::AVIF_CLAP_NOT_SUPPORTED,
            "error",
            scope,
            "clean aperture ('clap') crop is not supported; stored and displayed sizes differ",
        ));
        return None;
    }
    if !reasons.is_empty() {
        fr.decode_status = "unsupported".into();
        fr.decode_status_reason = "not_lossless_by_metadata".into();
        issues.push(Issue::new(
            code::AVIF_NOT_LOSSLESS_BY_METADATA,
            "error",
            scope,
            format!(
                "metadata is not consistent with lossless ({}; {yuv}, matrix {matrix}, {} range); not decoded, decoding would need a colour conversion",
                reasons.join(", "),
                if full { "full" } else { "limited" }
            ),
        ));
        return None;
    }
    let gray = p.yuv_format == YuvFormat::Yuv400;
    let planes = if gray { 1 } else { 3 };
    let channels = planes + u32::from(alpha);
    let model = match (gray, alpha) {
        (true, true) => "gray_alpha",
        (true, false) => "gray",
        (false, true) => "rgba",
        (false, false) => "rgb",
    };
    s.layout = RowLayout::new(model, channels, p.depth);
    let sm = &mut fr.sample_model;
    sm.model = model.into();
    sm.container_bits = p.depth;
    sm.channel_bits = vec![p.depth; channels as usize];
    sm.compare_type = if p.depth > 8 { "native_codes" } else { "uint8" }.into();
    sm.alpha = if alpha { "straight" } else { "none" }.into();
    sm.expansion =
        if gray { String::new() } else { "identity matrix: G = Y, B = U, R = V (planes read directly)".into() };
    fr.decode_path = if gray { "rav1d-planes-400" } else { "rav1d-planes-identity" }.into();
    issues.push(Issue::new(
        code::AVIF_LOSSLESS_METADATA_ONLY,
        "info",
        scope,
        "metadata is consistent with lossless (necessary conditions only; AV1 lossless coding is not visible). Pixel verification needs a compare against the original.",
    ));
    // libavif 保留 item 資料（不超過檔案大小）、AV1 解碼器的畫格緩衝與 libavif 自己的平面複本。
    // 每個樣本 6 bytes（C++ 版的預估，D31），讓 job_commit_mib 的判定與建議值和 C++ 版一致。
    let geo = Geometry { width: p.width, height: p.height, planes, alpha, depth: p.depth };
    let px = u64::from(p.width).checked_mul(u64::from(p.height));
    let body = px.and_then(|px| px.checked_mul(u64::from(channels))).and_then(|v| v.checked_mul(6));
    let codec_estimate = body.and_then(|b| b.checked_add(s.input.size())).and_then(|v| v.checked_add(48 << 20));
    Some(Box::new(AvifDecoder { input: s.input, parsed: p, geo, codec_estimate, row: Vec::new(), decoded: false }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_alpha_converts_like_libavif() {
        // 端點、夾值與捨入（(v - lo) * full + (hi - lo) / 2) / (hi - lo)
        assert_eq!(limited_to_full(8, 16), 0);
        assert_eq!(limited_to_full(8, 235), 255);
        assert_eq!(limited_to_full(8, 0), 0);
        assert_eq!(limited_to_full(8, 255), 255);
        assert_eq!(limited_to_full(8, 126), 128); // (110*255 + 109) / 219 = 128.5… → 128
        assert_eq!(limited_to_full(10, 64), 0);
        assert_eq!(limited_to_full(10, 940), 1023);
        assert_eq!(limited_to_full(10, 502), 512); // (438*1023 + 438) / 876 = 512.0
        assert_eq!(limited_to_full(12, 256), 0);
        assert_eq!(limited_to_full(12, 3760), 4095);
        assert_eq!(limited_to_full(12, 2008), 2048); // (1752*4095 + 1752) / 3504 = 2048
        // 每個值都單調不減
        for d in [8u8, 10, 12] {
            let max = (1u16 << d) - 1;
            for v in 1..=max {
                assert!(limited_to_full(d, v) >= limited_to_full(d, v - 1), "depth {d} v {v}");
            }
        }
    }
}
