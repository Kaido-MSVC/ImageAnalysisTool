//! TIFF／BigTIFF 前端（移植 C++ 版 `src/engine/tiff_side.cpp`）：獨立的容器掃描 + 自己讀 strip／tile。
//!
//! C++ 版用 libtiff 4.7.2 的 `TIFFReadEncodedStrip`／`TIFFReadEncodedTile`；這裡照它的行為自己做
//! （docs/decisions.md D-R17）：IFD 沿用 [`crate::tiff_scan`] 的解析，目錄層級照 libtiff 處理
//! StripByteCounts 的修正、未壓縮單一 strip 的切分（strip chopping）與配置上限，區塊的讀取與解壓
//! 照 libtiff 的讀取路徑，解壓見 [`crate::tiff_codec`]。樣本是原生整數（主機位元組順序），不轉換；
//! 不支援的範圍（多頁、SubIFD、分平面、浮點等）與 C++ 版一樣在解碼前回報。

use std::collections::BTreeMap;

use crate::engine::{Decoder, RowLayout, SideState, temp_failure};
use crate::input::Input;
use crate::limits::Limits;
use crate::report::{Count, EffectiveColor, FileReport, Issue, MetaItem, Orientation, code};
use crate::temp::{TempError, TempErrorKind, WRITE_BUFFER};
use crate::text::{file_text, short_hash};
use crate::tiff_codec::{
    Mem, Messages, lzw_decode, packbits_decode, reverse_bits, swab16_to_native, undo_horizontal, zip_decode,
};
use crate::tiff_scan::{Entry, TiffScan, compression_name, scan_tiff, type_size, uint};

pub const DECODER_NAME: &str = "weezl";
pub const DECODER_VERSION: &str = "0.1.12";

/// libtiff 的 `STRIP_SIZE_DEFAULT`（C++ 版建置的 tif_config.h）：未壓縮單一 strip 切成約這麼大。
const STRIP_SIZE_DEFAULT: u64 = 8192;
/// libtiff 讀取大塊資料時第一次最多配置 1 MiB，之後每次乘 10（`TIFFReadAndRealloc`）。
const READ_THRESHOLD: u64 = 1 << 20;
/// `sizeof(TIFFDirEntry)`（x64）：`TIFFFetchDirectory` 為每個項目配置一個。
const DIR_ENTRY_SIZE: u64 = 32;
/// LZW 解碼表：`CSIZE`（4095 + 1024）× `sizeof(code_t)`（x64 為 16）。
const LZW_CODETAB_BYTES: u64 = 5119 * 16;
/// libtiff 的 `NOSTRIP`：讀 tile 時 td_curstrip 的值，只出現在 LZW 的訊息裡。
const NOSTRIP: u32 = u32::MAX;

fn compression_in_scope(c: u16) -> bool {
    matches!(c, 1 | 5 | 8 | 32946 | 32773)
}

/// `TIFFGetMaxCompressionRatio`（libtiff 4.7.2 的各 codec 值）。
fn max_compression_ratio(c: u16) -> u64 {
    match c {
        1 => 1,
        5 => 1400,
        8 | 32946 => 1032,
        32773 => 64,
        _ => 0,
    }
}

/// libtiff 已知、以 `TIFF_SETGET_ASCII` 讀取的 tag（會檢查結尾的 NUL）。
fn ascii_field_name(tag: u16) -> Option<&'static str> {
    Some(match tag {
        269 => "DocumentName",
        270 => "ImageDescription",
        271 => "Make",
        272 => "Model",
        285 => "PageName",
        305 => "Software",
        306 => "DateTime",
        315 => "Artist",
        316 => "HostComputer",
        337 => "TargetPrinter",
        33432 => "Copyright",
        _ => return None,
    })
}

fn fill_metadata(fr: &mut FileReport, sc: &TiffScan) {
    let mut item = |name: &str, v: Option<&String>, status: &str| {
        fr.metadata.insert(
            name.into(),
            MetaItem {
                present: v.is_some(),
                status: if v.is_some() { status } else { "absent" }.into(),
                summary: v.cloned().unwrap_or_default(),
            },
        );
    };
    item("exif", sc.exif.as_ref(), sc.exif_status);
    item("xmp", sc.xmp.as_ref(), sc.xmp_status);
    item("text", sc.text.as_ref(), sc.text_status);
    item("resolution", sc.resolution.as_ref(), "present");
    item("time", sc.datetime.as_ref(), "present");
    item("hdr_signals", None, "");
    item("sbit", None, "");
    item("ornt_private", None, "");
    let other = (!sc.other_tags.is_empty()).then(|| sc.other_tags.join(","));
    item("other_chunks", other.as_ref(), "present");
}

fn effective_color(sc: &TiffScan) -> EffectiveColor {
    let mut c = EffectiveColor::default();
    if sc.icc.status != "absent" {
        let what = if sc.icc.sha256.is_empty() { sc.icc.status.as_str() } else { short_hash(&sc.icc.sha256) };
        c.signals.push(format!("ICCProfile({what})"));
    }
    c.signals.extend(sc.colorimetry.iter().cloned());
    if sc.icc.status == "embedded" {
        c.kind = "icc".into();
        c.key = format!("icc:{}", sc.icc.sha256);
        c.description = format!("embedded ICC {} bytes", sc.icc.bytes.map_or(0, |b| b.0));
    } else if sc.icc.status != "absent" {
        c.kind = "icc_unreadable".into();
        c.description = format!("ICC profile tag present but not usable ({})", sc.icc.status);
    } else if !sc.colorimetry.is_empty() {
        c.kind = "tiff_colorimetry".into();
        c.description = "TIFF colorimetry tags without ICC (no reliable equivalence)".into();
    } else {
        c.kind = "none".into();
        c.description = "no ICC profile (TIFF has no specified default colour space)".into();
    }
    c
}

/// 目錄讀完（含 libtiff 的修正與切分）之後的 strip／tile 表。
struct Directory {
    offsets: Vec<u64>,
    counts: Vec<u64>,
    /// strip：每條 strip 的列數（切分後）；tile 不用
    rows_per_strip: u32,
}

/// 讀一個整數陣列 tag 的全部值（scan 已確認值在檔案內）。
fn read_array(input: &Input<'_>, e: &Entry, le: bool) -> Option<Vec<u64>> {
    let ts = type_size(e.typ) as usize;
    if !matches!(e.typ, 1 | 3 | 4 | 13 | 16 | 18) {
        return None;
    }
    let n = usize::try_from(e.count).ok()?;
    let mut out = Vec::new();
    out.try_reserve_exact(n).ok()?;
    let mut buf = vec![0u8; ts * 8192];
    let mut off = e.value_offset;
    let mut left = n;
    while left > 0 {
        let k = left.min(8192);
        let chunk = buf.get_mut(..k * ts)?;
        if !matches!(input.read_at(off, chunk), Ok(got) if got == chunk.len()) {
            return None;
        }
        out.extend(chunk.chunks_exact(ts).map(|p| uint(p, le)));
        off = off.checked_add((k * ts) as u64)?;
        left -= k;
    }
    Some(out)
}

/// `TIFFReadDirectory` 中和這個前端有關的部分：目錄與 tag 值的配置（上限）、tag 順序與 ASCII
/// 的警告、strip／tile 表的載入、StripByteCounts 的修正，以及未壓縮單一 strip 的切分。
/// 回傳 None 表示 libtiff 會開檔失敗（錯誤已記在 msgs）。
fn read_directory(input: &Input<'_>, sc: &TiffScan, mem: &mut Mem, msgs: &mut Messages) -> Option<Directory> {
    const MODULE: &str = "TIFFReadDirectory";
    let le = sc.little_endian;
    // TIFFFetchDirectory：原始項目 + TIFFDirEntry 陣列
    let raw_dir = sc.entry_count * if sc.big { 20 } else { 12 };
    if !mem.alloc(msgs, "_TIFFreallocExt", raw_dir) {
        return None;
    }
    if !mem.alloc(msgs, "_TIFFreallocExt", sc.entry_count * DIR_ENTRY_SIZE) {
        return None;
    }
    mem.free(raw_dir);
    // TIFFReadDirectoryCheckOrder
    let mut m = 0u32;
    for e in &sc.entries {
        if u32::from(e.tag) < m {
            msgs.warning(
                "TIFFReadDirectoryCheckOrder",
                "Invalid TIFF directory; tags are not sorted in ascending order",
            );
            break;
        }
        m = u32::from(e.tag) + 1;
    }
    // 一般 tag 的值：讀進來的暫存陣列 + 設進目錄的複本（配置失敗時 libtiff 略過該 tag）
    let (offs_tag, cnts_tag) = if sc.tiled { (324, 325) } else { (273, 279) };
    for e in &sc.entries {
        if matches!(e.tag, 273 | 279 | 324 | 325) {
            continue;
        }
        if !e.inline && mem.alloc(msgs, "_TIFFreallocExt", e.bytes) {
            // 設進目錄的複本留著；失敗時 libtiff 略過這個 tag，只留下錯誤訊息
            mem.alloc(msgs, "_TIFFmallocExt", e.bytes);
            mem.free(e.bytes);
        }
        if e.typ == 2
            && let Some(name) = ascii_field_name(e.tag)
            && sc.tags.get(&e.tag) == Some(e)
        {
            let mut data = vec![0u8; usize::try_from(e.bytes).unwrap_or(0).min(1 << 20)];
            let nul = if matches!(input.read_at(e.value_offset, &mut data), Ok(k) if k == data.len()) {
                data.iter().position(|&b| b == 0).map_or(e.count, |p| p as u64)
            } else {
                e.count
            };
            if nul + 1 < e.count {
                msgs.warning(
                    MODULE,
                    &format!(
                        "ASCII value for tag \"{name}\" contains null byte in value; value incorrectly truncated during reading due to implementation limitations"
                    ),
                );
            } else if nul + 1 > e.count {
                msgs.warning(
                    MODULE,
                    &format!("ASCII value for tag \"{name}\" does not end in null byte. Forcing it to be null"),
                );
            }
        }
    }
    // strip／tile 表（TIFFFetchStripThing）：原始陣列，非 LONG8 時再轉成 64 位元陣列
    let (offs_e, cnts_e) = (*sc.tag(offs_tag)?, *sc.tag(cnts_tag)?);
    for e in [offs_e, cnts_e] {
        let name = match e.tag {
            273 => "StripOffsets",
            279 => "StripByteCounts",
            324 => "TileOffsets",
            _ => "TileByteCounts",
        };
        let out_of_memory = |msgs: &mut Messages| {
            msgs.error("TIFFFetchStripThing", &format!("Out of memory reading of \"{name}\""));
        };
        let raw = e.bytes;
        if !mem.alloc(msgs, "_TIFFreallocExt", raw) {
            out_of_memory(msgs);
            return None;
        }
        if e.typ != 16 {
            let wide = e.count.checked_mul(8)?;
            if !mem.alloc(msgs, "_TIFFmallocExt", wide) {
                out_of_memory(msgs);
                return None;
            }
            mem.free(raw);
        }
    }
    let mut offsets = read_array(input, &offs_e, le)?;
    let mut counts = read_array(input, &cnts_e, le)?;
    let nstrips = offsets.len() as u64;

    // StripByteCounts 的修正（只有 strip，且未壓縮時才影響讀取）
    let scanline = u64::from(sc.width).checked_mul(u64::from(sc.spp))?.checked_mul(u64::from(sc.bits))?.div_ceil(8);
    // libtiff 的 td_rowsperstrip：沒有 tag 或值為 0 時是 UINT32_MAX
    let mut td_rps = u64::from(sc.rows_per_strip);
    let mut rows_per_strip = if sc.tiled { 0 } else { sc.rows_per_strip.min(sc.height) };
    let differing_counts = nstrips > 2
        && sc.compression == 1
        && counts.first() != counts.get(1)
        && counts.first() != Some(&0)
        && counts.get(1) != Some(&0);
    if sc.tiled && differing_counts {
        // tile 也套用這個檢查；未壓縮的 tile 讀取不看 byte count，所以只有警告
        msgs.warning(MODULE, "Wrong \"StripByteCounts\" field, ignoring and calculating from imagelength");
    }
    if !sc.tiled {
        let looks_bad = || {
            let (Some(&bc), Some(&off)) = (counts.first(), offsets.first()) else { return false };
            if off == 0 {
                return false;
            }
            if bc == 0 {
                return true;
            }
            if sc.compression != 1 {
                return false;
            }
            if off <= input.size() && bc > input.size() - off {
                return true;
            }
            scanline.checked_mul(u64::from(sc.height)).is_none_or(|need| bc < need)
        };
        if nstrips == 1 && looks_bad() {
            msgs.warning(MODULE, "Bogus \"StripByteCounts\" field, ignoring and calculating from imagelength");
            if let Some(c) = counts.first_mut() {
                *c = scanline.checked_mul(u64::from(sc.height))?;
            }
            if sc.tag(278).is_none() {
                td_rps = u64::from(sc.height);
            }
        } else if differing_counts {
            msgs.warning(MODULE, "Wrong \"StripByteCounts\" field, ignoring and calculating from imagelength");
            let rows = u64::from(sc.height) / nstrips;
            let each = scanline.checked_mul(rows)?;
            counts.iter_mut().for_each(|c| *c = each);
        }
        // ChopUpSingleUncompressedStrip
        if nstrips == 1 && sc.compression == 1 && scanline > 0 {
            let (stripbytes, rps) = if scanline > STRIP_SIZE_DEFAULT {
                (scanline, 1u64)
            } else {
                let per = STRIP_SIZE_DEFAULT / scanline;
                (per * scanline, per)
            };
            let n = u64::from(sc.height).div_ceil(rps);
            let file = input.size();
            let off0 = offsets.first().copied().unwrap_or(0);
            let too_big = n > 1_000_000 && (off0 >= file || stripbytes > (file - off0) / (n - 1));
            if rps < td_rps && rps != 0 && n != 0 && !too_big {
                let mut bytecount = counts.first().copied().unwrap_or(0);
                let alloc = n.checked_mul(8)?;
                let huge = alloc.checked_mul(2)? > 100 << 20 && alloc * 2 > file;
                if huge {
                    msgs.warning(
                        "allocChoppedUpStripArrays",
                        &format!(
                            "Requested memory size for StripByteCount and StripOffsets {} is greater than filesize {file}. Memory not allocated",
                            alloc * 2
                        ),
                    );
                } else if mem.alloc(msgs, "_TIFFreallocExt", alloc) && mem.alloc(msgs, "_TIFFreallocExt", alloc) {
                    let mut new_offs = Vec::new();
                    let mut new_counts = Vec::new();
                    let cap = usize::try_from(n).ok()?;
                    if new_offs.try_reserve_exact(cap).is_err() || new_counts.try_reserve_exact(cap).is_err() {
                        return None;
                    }
                    let mut offset = off0;
                    let mut sb = stripbytes;
                    for _ in 0..n {
                        sb = sb.min(bytecount);
                        new_counts.push(sb);
                        new_offs.push(if sb != 0 { offset } else { 0 });
                        offset = offset.checked_add(sb)?;
                        bytecount -= sb;
                    }
                    mem.free(16); // 原本的單一 strip 陣列
                    offsets = new_offs;
                    counts = new_counts;
                    rows_per_strip = rps as u32;
                }
            }
        }
    }
    Some(Directory { offsets, counts, rows_per_strip })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Strips,
    TileBand,
    TileTemp,
}

/// 區塊解碼失敗：libtiff 回傳 -1，錯誤訊息已記在 Messages。
struct BlockFailed;

struct TiffDecoder<'a> {
    input: &'a Input<'a>,
    sc: Box<TiffScan>,
    lim: Limits,
    msgs: Messages,
    mem: Mem,
    dir: Directory,
    mode: Mode,
    bps: u32,
    ch: u32,
    px: u64,
    row_bytes: u64,
    block_bytes: u64,
    band_bytes: u64,
    rps: u32,
    tw: u32,
    tl: u32,
    across: u32,
    down: u32,
    loaded: Option<u32>,
    blocks_read: u64,
    total_blocks: u64,
    tiles_in_temp: bool,
    buf: Vec<u8>,
    row: Vec<u8>,
    /// 壓縮資料的讀取緩衝（libtiff 的 tif_rawdata）
    raw: Vec<u8>,
    raw_size: u64,
    lzw_ready: bool,
    lzw_old_warned: bool,
}

impl<'a> TiffDecoder<'a> {
    fn new(input: &'a Input<'a>, sc: TiffScan, lim: &Limits, msgs: Messages, mem: Mem, dir: Directory) -> Self {
        Self {
            input,
            sc: Box::new(sc),
            lim: *lim,
            msgs,
            mem,
            dir,
            mode: Mode::Strips,
            bps: 1,
            ch: 1,
            px: 1,
            row_bytes: 0,
            block_bytes: 0,
            band_bytes: 0,
            rps: 0,
            tw: 0,
            tl: 0,
            across: 0,
            down: 0,
            loaded: None,
            blocks_read: 0,
            total_blocks: 0,
            tiles_in_temp: false,
            buf: Vec::new(),
            row: Vec::new(),
            raw: Vec::new(),
            raw_size: 0,
            lzw_ready: false,
            lzw_old_warned: false,
        }
    }

    /// 區塊幾何（libtiff 開檔後的 strip 大小，考慮了 strip chopping）。
    fn start(&mut self, s: &mut SideState<'_>, issues: &mut Vec<Issue>) -> bool {
        let (w, h) = (self.sc.width, self.sc.height);
        self.bps = u32::from(self.sc.bits) / 8;
        self.ch = u32::from(self.sc.spp);
        self.px = u64::from(self.ch) * u64::from(self.bps);
        let Some(rb) = u64::from(w).checked_mul(self.px) else { return self.overflow(s, issues) };
        self.row_bytes = rb;
        if self.sc.tiled {
            self.tw = self.sc.tile_width;
            self.tl = self.sc.tile_length;
            self.across = w.div_ceil(self.tw);
            self.down = h.div_ceil(self.tl);
            let Some(tb) = u64::from(self.tw).checked_mul(u64::from(self.tl)).and_then(|v| v.checked_mul(self.px))
            else {
                return self.overflow(s, issues);
            };
            self.block_bytes = tb;
            let Some(band) = tb.checked_mul(u64::from(self.across)) else { return self.overflow(s, issues) };
            // 一整排 tile 放得進預算的一半就放記憶體，否則逐 tile 寫進暫存再依列重組。
            self.mode = if band <= self.lim.buffer_bytes() / 2 { Mode::TileBand } else { Mode::TileTemp };
            self.band_bytes = band;
            self.total_blocks = u64::from(self.across) * u64::from(self.down);
        } else {
            self.rps = self.dir.rows_per_strip.min(h).max(1);
            let Some(sb) = self.row_bytes.checked_mul(u64::from(self.rps)) else { return self.overflow(s, issues) };
            self.block_bytes = sb;
            self.mode = Mode::Strips;
            self.total_blocks = u64::from(h.div_ceil(self.rps));
        }
        if self.dir.offsets.len() as u64 != self.total_blocks || self.dir.counts.len() as u64 != self.total_blocks {
            s.rep.decode_status = "failed".into();
            s.rep.decode_status_reason = "decoder_disagrees_with_scan".into();
            issues.push(Issue::new(
                code::DECODE_FAILED,
                "error",
                s.scope,
                "tiff decoder block count disagrees with the expected layout",
            ));
            return false;
        }
        s.rep.coverage.rows_total = u64::from(h);
        true
    }

    fn fail(&mut self, s: &mut SideState<'_>, issues: &mut Vec<Issue>, what: &str, some_data: bool) -> bool {
        s.failed = true;
        s.rep.decoder_errors.extend(self.msgs.errors.iter().map(|e| file_text(e.as_bytes())));
        let last = self.msgs.errors.last().map(|e| file_text(e.as_bytes()));
        if self.msgs.limit_hit {
            s.rep.decode_status = "not_checked".into();
            s.rep.decode_status_reason = "resource_limit".into();
            issues.push(Issue::new(
                code::RESOURCE_LIMIT,
                "error",
                s.scope,
                format!("tiff decoder allocation limit: {}", last.unwrap_or_else(|| what.into())),
            ));
            return false;
        }
        s.rep.decode_status = if some_data { "partial" } else { "failed" }.into();
        s.rep.decode_status_reason = "decode_error".into();
        let tail = last.map(|e| format!(" ({e})")).unwrap_or_default();
        issues.push(Issue::new(code::DECODE_FAILED, "error", s.scope, format!("tiff decoder: {what}{tail}")));
        false
    }

    fn block_failed(&mut self, s: &mut SideState<'_>, issues: &mut Vec<Issue>, block: &str) -> bool {
        let some = s.rows > 0 || self.blocks_read > 0;
        self.fail(s, issues, &format!("{block} decoded -1 bytes, expected a complete block"), some)
    }

    fn overflow(&self, s: &mut SideState<'_>, issues: &mut Vec<Issue>) -> bool {
        s.rep.decode_status = "not_checked".into();
        s.rep.decode_status_reason = "resource_limit".into();
        issues.push(Issue::new(code::RESOURCE_LIMIT, "error", s.scope, "TIFF buffer size overflows 64-bit arithmetic"));
        false
    }

    /// libtiff 訊息用的 td_row。
    fn td_row(&self, index: u32) -> u64 {
        if self.sc.tiled {
            u64::from(index % self.across.max(1)) * u64::from(self.tl)
        } else {
            u64::from(index) * u64::from(self.rps)
        }
    }

    /// `TIFFReadEncodedStrip`／`TIFFReadEncodedTile`：把第 index 塊解進 out（out 的長度是該塊大小）。
    fn read_block(&mut self, index: u32, out: &mut [u8]) -> Result<(), BlockFailed> {
        let tiled = self.sc.tiled;
        let row = self.td_row(index);
        let i = index as usize;
        let (Some(&offset), Some(&count)) = (self.dir.offsets.get(i), self.dir.counts.get(i)) else {
            self.msgs.error(
                if tiled { "TIFFReadEncodedTile" } else { "TIFFReadEncodedStrip" },
                &format!("{index}: {} out of range", if tiled { "Tile" } else { "Strip" }),
            );
            return Err(BlockFailed);
        };
        let le = self.sc.little_endian;
        let reverse = self.sc.fill_order == 2;
        if self.sc.compression == 1 {
            // 未壓縮的捷徑：從 offset 直接讀整塊大小，不看 byte count
            let got = read_full(self.input, offset, out);
            if got != out.len() {
                let module = if tiled { "TIFFReadEncodedTile" } else { "TIFFReadEncodedStrip" };
                let msg = if tiled {
                    format!("Read error at row {row}, col 0; got {got} bytes, expected {}", out.len())
                } else {
                    format!("Read error at scanline {row}; got {got} bytes, expected {}", out.len())
                };
                self.msgs.error(module, &msg);
                return Err(BlockFailed);
            }
            if reverse {
                reverse_bits(out);
            }
            if self.sc.bits == 16 {
                swab16_to_native(out, le);
            }
            return Ok(());
        }

        // TIFFFillStrip／TIFFFillTile
        let module = if tiled { "TIFFFillTile" } else { "TIFFFillStrip" };
        let full_block = if tiled { self.block_bytes } else { self.row_bytes * u64::from(self.rps) };
        let mut bytecount = count;
        if bytecount == 0 || bytecount > i64::MAX as u64 {
            let msg = if tiled {
                format!("{bytecount}: Invalid tile byte count, tile {index}")
            } else {
                format!("Invalid strip byte count {bytecount}, strip {index}")
            };
            self.msgs.error(module, &msg);
            return Err(BlockFailed);
        }
        if full_block > 0 {
            let what = if tiled { "tile" } else { "strip" };
            if bytecount > 1 << 20 && (bytecount - 4096) / 10 > full_block {
                let limited = full_block * 10 + 4096;
                self.msgs.warning(
                    module,
                    &format!("Too large {what} byte count {bytecount}, {what} {index}. Limiting to {limited}"),
                );
                bytecount = limited;
            } else if full_block > 100 << 20 {
                let ratio = max_compression_ratio(self.sc.compression);
                if ratio > 0 && bytecount < full_block / ratio {
                    let cap = if tiled { "tile" } else { "strip" };
                    self.msgs.error(
                        module,
                        &format!(
                            "Likely invalid {cap} byte count for {cap} {index}. Uncompressed {cap} size is {full_block}, compressed one is {bytecount}"
                        ),
                    );
                    return Err(BlockFailed);
                }
            }
        }
        // TIFFReadAndRealloc：讀取緩衝依門檻逐步放大
        let mut threshold = READ_THRESHOLD;
        let mut done = 0u64;
        while done < bytecount {
            let mut to_read = bytecount - done;
            if to_read >= threshold && threshold < READ_THRESHOLD * 1000 && done + to_read > self.raw_size {
                to_read = threshold;
                threshold *= 10;
            }
            if done + to_read > self.raw_size {
                let new_size = (done + to_read).div_ceil(1024) * 1024;
                if !self.mem.realloc(&mut self.msgs, "_TIFFreallocExt", self.raw_size, new_size) {
                    self.msgs.error(module, &format!("No space for data buffer at scanline {row}"));
                    return Err(BlockFailed);
                }
                let Ok(n) = usize::try_from(new_size) else { return Err(BlockFailed) };
                if self.raw.try_reserve_exact(n.saturating_sub(self.raw.len())).is_err() {
                    self.msgs.error(module, &format!("No space for data buffer at scanline {row}"));
                    return Err(BlockFailed);
                }
                self.raw.resize(n, 0);
                self.raw_size = new_size;
            }
            let (Ok(from), Ok(to)) = (usize::try_from(done), usize::try_from(done + to_read)) else {
                return Err(BlockFailed);
            };
            let Some(dst) = self.raw.get_mut(from..to) else { return Err(BlockFailed) };
            let got = read_full(self.input, offset.saturating_add(done), dst);
            done += got as u64;
            if got as u64 != to_read {
                let msg = if tiled {
                    format!("Read error at row {row}, col 0, tile {index}; got {done} bytes, expected {bytecount}")
                } else {
                    format!("Read error at scanline {row}; got {done} bytes, expected {bytecount}")
                };
                self.msgs.error(module, &msg);
                return Err(BlockFailed);
            }
        }
        let Some(raw) = usize::try_from(bytecount).ok().and_then(|n| self.raw.get_mut(..n)) else {
            return Err(BlockFailed);
        };
        if reverse {
            reverse_bits(raw);
        }
        let raw: &[u8] = raw;
        let ok = match self.sc.compression {
            32773 => packbits_decode(raw, out, row, &mut self.msgs),
            8 | 32946 => zip_decode(raw, out, row, &mut self.msgs),
            _ => {
                if !self.lzw_ready {
                    if !self.mem.alloc(&mut self.msgs, "_TIFFmallocExt", LZW_CODETAB_BYTES) {
                        self.msgs.error("LZWSetupDecode", "No space for LZW code table");
                        return Err(BlockFailed);
                    }
                    self.lzw_ready = true;
                }
                if crate::tiff_codec::lzw_is_old_style(raw) && !self.lzw_old_warned {
                    self.msgs.warning("LZWPreDecode", "Old-style LZW codes, convert file");
                    self.lzw_old_warned = true;
                }
                let strip = if tiled { NOSTRIP } else { index };
                lzw_decode(raw, out, strip, row, &mut self.msgs)
            }
        };
        if !ok {
            return Err(BlockFailed);
        }
        if self.sc.bits == 16 {
            swab16_to_native(out, le);
        }
        // 預測只在 LZW 與 Deflate 時有作用（libtiff 的 None 與 PackBits 不設定預測器）
        if self.sc.predictor == 2 && matches!(self.sc.compression, 5 | 8 | 32946) {
            let row_size = if tiled { u64::from(self.tw) * self.px } else { self.row_bytes };
            undo_horizontal(out, row_size as usize, self.ch as usize, self.sc.bits);
        }
        Ok(())
    }

    /// 原生樣本 -> 正規列的 x0 起 n 個像素。
    fn convert(&self, canon: &mut [u16], src: &[u8], x0: u32, n: u32) {
        let ch = self.ch as usize;
        let start = x0 as usize * ch;
        let count = n as usize * ch;
        let Some(dst) = canon.get_mut(start..start + count) else { return };
        if self.bps == 2 {
            for (d, p) in dst.iter_mut().zip(src.as_chunks::<2>().0) {
                *d = u16::from_ne_bytes(*p);
            }
        } else {
            for (d, &b) in dst.iter_mut().zip(src) {
                *d = u16::from(b);
            }
        }
        if self.sc.photometric == 0 {
            // MinIsWhite -> MinIsBlack，同位深（alpha 不動）
            let maxv: u16 = if self.bps == 2 { 0xFFFF } else { 0xFF };
            for px in dst.chunks_exact_mut(ch) {
                px[0] = maxv - px[0];
            }
        }
    }

    /// 大 tile：每個 tile 的有效範圍依檔案順序附加到暫存檔。
    fn tiles_to_temp(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool {
        let mut buf = std::mem::take(&mut self.buf);
        for ty in 0..self.down {
            for tx in 0..self.across {
                let idx = ty * self.across + tx;
                if self.read_block(idx, &mut buf).is_err() {
                    self.buf = buf;
                    return self.block_failed(s, issues, &format!("tile {idx}"));
                }
                self.blocks_read += 1;
                let vw = self.tw.min(self.sc.width - tx * self.tw);
                let vh = self.tl.min(self.sc.height - ty * self.tl);
                let line = u64::from(self.tw) * self.px;
                let valid = (u64::from(vw) * self.px) as usize;
                let temp = s.temp.as_mut().expect("open() reserved the temp file");
                for yy in 0..u64::from(vh) {
                    let at = (yy * line) as usize;
                    if let Err(e) = temp.append(&buf[at..at + valid]) {
                        self.buf = buf;
                        temp_failure(s, issues, &e);
                        return false;
                    }
                }
            }
        }
        self.buf = buf;
        let temp = s.temp.as_mut().expect("open() reserved the temp file");
        if let Err(e) = temp.flush() {
            temp_failure(s, issues, &e);
            return false;
        }
        let total = self.row_bytes * u64::from(self.sc.height);
        if temp.written() != total {
            let e = TempError::new(TempErrorKind::Internal, format!("tiles wrote {} of {total} bytes", temp.written()));
            temp_failure(s, issues, &e);
            return false;
        }
        self.tiles_in_temp = true;
        true
    }

    /// 暫存的版面：第 ty 排從 ty×tl×row_bytes 開始；排內第 tx 個 tile 從 vh×x0×px 開始
    /// （同一排前面的 tile 都是滿寬）。
    fn row_from_temp(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, y: u32) -> bool {
        let ty = y / self.tl;
        let yy = y - ty * self.tl;
        let vh = self.tl.min(self.sc.height - ty * self.tl);
        let band = u64::from(ty) * u64::from(self.tl) * self.row_bytes;
        let temp = s.temp.as_mut().expect("open() reserved the temp file");
        for tx in 0..self.across {
            let x0 = tx * self.tw;
            let vw = self.tw.min(self.sc.width - x0);
            let off = band + u64::from(vh) * u64::from(x0) * self.px + u64::from(yy) * u64::from(vw) * self.px;
            let at = (u64::from(x0) * self.px) as usize;
            let len = (u64::from(vw) * self.px) as usize;
            if let Err(e) = temp.read_at(off, &mut self.row[at..at + len]) {
                temp_failure(s, issues, &e);
                return false;
            }
        }
        let mut canon = std::mem::take(&mut s.canon);
        self.convert(&mut canon, &self.row, 0, self.sc.width);
        s.canon = canon;
        true
    }
}

/// 讀到 dst 填滿、檔尾或錯誤為止，回傳讀到的位元組數。
fn read_full(input: &Input<'_>, off: u64, dst: &mut [u8]) -> usize {
    let mut done = 0;
    while done < dst.len() {
        match input.read_at(off.saturating_add(done as u64), &mut dst[done..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => done += n,
        }
    }
    done
}

impl<'a> Decoder<'a> for TiffDecoder<'a> {
    fn buffer_estimate(&self) -> Option<u64> {
        let canon_row = u64::from(self.sc.width).checked_mul(u64::from(self.ch))?.checked_mul(2)?;
        let extra = 256u64 << 10;
        match self.mode {
            Mode::Strips => self.block_bytes.checked_add(canon_row)?.checked_add(extra),
            Mode::TileBand => self.band_bytes.checked_add(canon_row)?.checked_add(extra),
            Mode::TileTemp => self
                .block_bytes
                .checked_add(self.row_bytes)?
                .checked_add(canon_row)?
                .checked_add(WRITE_BUFFER as u64)?
                .checked_add(extra),
        }
    }

    fn open(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>, _lim: &Limits) -> bool {
        if self.mode == Mode::TileTemp {
            let Some(total) = self.row_bytes.checked_mul(u64::from(self.sc.height)) else {
                return self.overflow(s, issues);
            };
            let res = match s.temp.as_mut() {
                Some(t) => t.reserve(&mut s.quota.borrow_mut(), total),
                None => Err(TempError::new(TempErrorKind::Unavailable, "no temporary file for this side")),
            };
            if let Err(e) = res {
                temp_failure(s, issues, &e);
                return false;
            }
            let Ok(n) = usize::try_from(self.row_bytes) else { return self.overflow(s, issues) };
            if self.row.try_reserve_exact(n).is_err() {
                s.rep.decode_status = "not_checked".into();
                s.rep.decode_status_reason = "resource_limit".into();
                issues.push(Issue::new(
                    code::RESOURCE_LIMIT,
                    "error",
                    s.scope,
                    "TIFF row buffer could not be allocated",
                ));
                return false;
            }
            self.row.resize(n, 0);
            s.rep.decode_path += "+temp";
        }
        let n = if self.mode == Mode::TileBand { self.band_bytes } else { self.block_bytes };
        let Ok(n) = usize::try_from(n) else { return self.overflow(s, issues) };
        if self.buf.try_reserve_exact(n).is_err() {
            s.rep.decode_status = "not_checked".into();
            s.rep.decode_status_reason = "resource_limit".into();
            issues.push(Issue::new(code::RESOURCE_LIMIT, "error", s.scope, "TIFF block buffer could not be allocated"));
            return false;
        }
        self.buf.resize(n, 0);
        true
    }

    fn read_row(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) -> bool {
        if s.failed {
            return false;
        }
        let y = s.rows as u32;
        match self.mode {
            Mode::Strips => {
                let strip = y / self.rps;
                if self.loaded != Some(strip) {
                    let rows = self.rps.min(self.sc.height - strip * self.rps);
                    let want = (self.row_bytes * u64::from(rows)) as usize;
                    let mut buf = std::mem::take(&mut self.buf);
                    let r = self.read_block(strip, &mut buf[..want]);
                    self.buf = buf;
                    if r.is_err() {
                        return self.block_failed(s, issues, &format!("strip {strip}"));
                    }
                    self.loaded = Some(strip);
                    self.blocks_read += 1;
                }
                let at = ((y - strip * self.rps) as u64 * self.row_bytes) as usize;
                let len = self.row_bytes as usize;
                let mut canon = std::mem::take(&mut s.canon);
                self.convert(&mut canon, &self.buf[at..at + len], 0, self.sc.width);
                s.canon = canon;
            }
            Mode::TileBand => {
                let ty = y / self.tl;
                let bb = self.block_bytes as usize;
                if self.loaded != Some(ty) {
                    let mut buf = std::mem::take(&mut self.buf);
                    for tx in 0..self.across {
                        let idx = ty * self.across + tx;
                        let at = tx as usize * bb;
                        if self.read_block(idx, &mut buf[at..at + bb]).is_err() {
                            self.buf = buf;
                            return self.block_failed(s, issues, &format!("tile {idx}"));
                        }
                        self.blocks_read += 1;
                    }
                    self.buf = buf;
                    self.loaded = Some(ty);
                }
                let yy = (y - ty * self.tl) as u64;
                let line = u64::from(self.tw) * self.px;
                let mut canon = std::mem::take(&mut s.canon);
                for tx in 0..self.across {
                    let x0 = tx * self.tw;
                    let vw = self.tw.min(self.sc.width - x0); // 右邊的填補欄不算
                    let at = tx as usize * bb + (yy * line) as usize;
                    let len = (u64::from(vw) * self.px) as usize;
                    self.convert(&mut canon, &self.buf[at..at + len], x0, vw);
                }
                s.canon = canon;
            }
            Mode::TileTemp => {
                if !self.tiles_in_temp && !self.tiles_to_temp(s, issues) {
                    return false;
                }
                if !self.row_from_temp(s, issues, y) {
                    return false;
                }
            }
        }
        s.rows += 1;
        true
    }

    fn finish(&mut self, s: &mut SideState<'a>, issues: &mut Vec<Issue>) {
        s.rep.coverage.rows_decoded = s.rows;
        for w in &self.msgs.warnings {
            let w = file_text(w.as_bytes());
            issues.push(Issue::new(code::DECODER_WARNING, "warning", s.scope, format!("tiff decoder: {w}")));
            s.rep.decoder_warnings.push(w);
        }
        if !s.failed && s.rows == u64::from(self.sc.height) {
            if self.blocks_read != self.total_blocks {
                s.rep.decode_status = "partial".into();
                s.rep.decode_status_reason = "blocks_not_covered".into();
                issues.push(Issue::new(
                    code::DECODE_FAILED,
                    "error",
                    s.scope,
                    format!("read {} of {} strips/tiles", self.blocks_read, self.total_blocks),
                ));
                return;
            }
            s.rep.decode_status = "complete".into();
            s.rep.decode_status_reason.clear();
        }
    }

    fn memory_metrics(&self, m: &mut BTreeMap<String, u64>) {
        *m.entry("tool_row_buffers".into()).or_default() +=
            (self.buf.capacity() + self.row.capacity() + self.raw.capacity()) as u64;
    }
}

fn unsupported(s: &mut SideState<'_>, issues: &mut Vec<Issue>, reason: &str, issue_code: &str, msg: String) {
    s.rep.decode_status = "unsupported".into();
    s.rep.decode_status_reason = reason.into();
    issues.push(Issue::new(issue_code, "error", s.scope, msg));
}

pub fn prepare_tiff<'a>(
    s: &mut SideState<'a>,
    issues: &mut Vec<Issue>,
    lim: &Limits,
) -> Option<Box<dyn Decoder<'a> + 'a>> {
    let t0 = std::time::Instant::now();
    let sc = scan_tiff(s.input, lim);
    s.scan_ms = crate::ms_since(t0);
    let scope = s.scope;
    for mut i in sc.issues.iter().cloned() {
        i.scope = scope.into();
        issues.push(i);
    }
    let fr = &mut s.rep;
    fr.container_status = sc.container_status.into();
    fr.metadata_status = sc.metadata_status.into();
    fr.chunks = sc.tag_list.clone();
    fr.coverage.bytes_scanned = Count(sc.bytes_scanned);
    fr.icc = sc.icc.clone();
    fill_metadata(fr, &sc);
    if !sc.has_ifd || sc.container_status == "corrupt" || sc.width == 0 {
        fr.decode_status = "failed".into();
        fr.decode_status_reason = "structure_error".into();
        return None;
    }
    fr.width = sc.width;
    fr.height = sc.height;
    s.width = sc.width;
    s.height = sc.height;
    fr.coverage.rows_total = u64::from(sc.height);
    fr.color = effective_color(&sc);
    fr.decode_path = format!("tiff-{}-encoded", if sc.tiled { "tiles" } else { "strips" });
    match sc.orientation {
        None => fr.orientation = Orientation { source: "default".into(), value: 1 },
        Some(o @ 1..=8) => fr.orientation = Orientation { source: "tiff_tag".into(), value: u32::from(o) },
        Some(o) => {
            fr.orientation = Orientation { source: "unparseable".into(), value: 0 };
            issues.push(Issue::new(
                code::ORIENTATION_UNPARSEABLE,
                "warning",
                scope,
                format!("Orientation tag value {o} is invalid"),
            ));
        }
    }

    // ---- 支援範圍，在任何解碼之前檢查（同 C++ 版）
    if sc.next_ifd != 0 {
        unsupported(
            s,
            issues,
            "multi_image",
            code::MULTI_IMAGE_NOT_SUPPORTED,
            "TIFF has more than one image (next IFD present)".into(),
        );
        return None;
    }
    if sc.subifds {
        unsupported(
            s,
            issues,
            "subifd",
            code::MULTI_IMAGE_NOT_SUPPORTED,
            "TIFF has SubIFDs (tag 330); not supported".into(),
        );
        return None;
    }
    if !compression_in_scope(sc.compression) {
        // 完全不解碼：檔案「未檢查」，並說明原因
        s.rep.decode_status = "not_checked".into();
        s.rep.decode_status_reason = "compression_not_supported".into();
        issues.push(Issue::new(
            code::COMPRESSION_NOT_SUPPORTED,
            "error",
            scope,
            format!(
                "TIFF compression {} ({}) is outside the supported set (none, LZW, Deflate, PackBits); image data was not checked",
                compression_name(sc.compression),
                sc.compression
            ),
        ));
        return None;
    }
    let base: u16 = if sc.photometric == 2 { 3 } else { 1 };
    let why = if sc.planar != 1 {
        Some("separate planes (PlanarConfiguration=2)".to_string())
    } else if sc.sample_format != 1 {
        Some(format!("SampleFormat {} (only unsigned integer)", sc.sample_format))
    } else if sc.bits != 8 && sc.bits != 16 {
        Some(format!("BitsPerSample {} (only 8 or 16, equal per channel)", sc.bits))
    } else if sc.photometric > 2 {
        Some(format!("Photometric {} (only MinIsWhite, MinIsBlack, RGB)", sc.photometric))
    } else if sc.spp < base || sc.spp > base + 1 || sc.extra_samples.len() != usize::from(sc.spp - base) {
        Some(format!("SamplesPerPixel {} with {} extra samples", sc.spp, sc.extra_samples.len()))
    } else if sc.predictor != 1 && sc.predictor != 2 {
        Some(format!("Predictor {}", sc.predictor))
    } else {
        None
    };
    if let Some(why) = why {
        unsupported(s, issues, "sample_model", code::UNSUPPORTED_SAMPLE_MODEL, format!("TIFF {why} is not supported"));
        return None;
    }
    let alpha = sc.spp == base + 1;
    let model = match (base, alpha) {
        (3, true) => "rgba",
        (3, false) => "rgb",
        (_, true) => "gray_alpha",
        (_, false) => "gray",
    };
    s.layout = RowLayout::new(model, u32::from(sc.spp), u32::from(sc.bits));
    let sm = &mut s.rep.sample_model;
    sm.model = model.into();
    sm.container_bits = u32::from(sc.bits);
    sm.channel_bits = vec![u32::from(sc.bits); usize::from(sc.spp)];
    sm.compare_type = if sc.bits == 16 { "uint16" } else { "uint8" }.into();
    sm.alpha = match sc.extra_samples.first() {
        _ if !alpha => "none",
        Some(2) => "straight",
        Some(1) => "associated",
        _ => "unspecified",
    }
    .into();
    if sc.photometric == 0 {
        sm.expansion = "MinIsWhite -> MinIsBlack at the same depth (max - v)".into();
    }

    // ---- 「開檔」：libtiff 讀目錄（配置上限、警告、strip 表的修正與切分）
    let single = lim.buffer_bytes().max(lim.metadata_item_bytes());
    let total = lim.buffer_bytes().saturating_add(lim.metadata_file_bytes());
    let mut mem = Mem::new(single, total);
    let mut msgs = Messages::default();
    let Some(dir) = read_directory(s.input, &sc, &mut mem, &mut msgs) else {
        let mut dec = TiffDecoder::new(
            s.input,
            sc,
            lim,
            msgs,
            mem,
            Directory { offsets: Vec::new(), counts: Vec::new(), rows_per_strip: 0 },
        );
        dec.fail(s, issues, "could not read the first directory", false);
        return None;
    };
    let mut dec = TiffDecoder::new(s.input, sc, lim, msgs, mem, dir);
    if !dec.start(s, issues) {
        return None;
    }
    Some(Box::new(dec))
}
