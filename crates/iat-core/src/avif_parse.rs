//! AVIF 的 item 與 property 解析，照 libavif 1.4.2 `avifDecoderParse` 的行為（C++ 版 iat 1.0.0 的設定：
//! strict 全開、只解色彩與 alpha、預設的尺寸上限）。依據是 C++ 建置裡的 `src/read.c`、`stream.c`、
//! `obu.c`、`avif.c`、`exif.c`（docs/decisions.md D-R19）。
//!
//! 只做 primary item 這條路：`avis` 為主要 brand 的影像序列（moov／trak）不解析，回報為序列；
//! gain map（tmap）與 sample transform 不處理（C++ 版也不解碼它們，但 libavif 解析時會檢查，
//! 這些檢查沒有重做，見 D-R19 的已知風險）。
//!
//! 錯誤只分成 C++ 版會區分的幾類：截斷（TRUNCATED_DATA）、配置失敗（OUT_OF_MEMORY），其餘一律是
//! 結構錯誤。訊息文字盡量沿用 libavif 的 diag，但不保證逐字相同（允許差異）。

use crate::input::Input;

/// libavif 的錯誤種類（只保留 C++ 版會區分的）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFail {
    /// AVIF_RESULT_TRUNCATED_DATA
    Truncated,
    /// AVIF_RESULT_OUT_OF_MEMORY
    OutOfMemory,
    /// 其他（BMFF_PARSE_FAILED、INVALID_IMAGE_GRID、NOT_IMPLEMENTED、MISSING_IMAGE_ITEM、IO_ERROR…）
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseFail,
    /// libavif 的結果名稱（`avifResultToString`）
    pub result: &'static str,
    /// libavif 的 diag 訊息（可能為空）
    pub detail: String,
}

impl ParseError {
    fn new(kind: ParseFail, result: &'static str, detail: impl Into<String>) -> Self {
        Self { kind, result, detail: detail.into() }
    }
    fn bmff(detail: impl Into<String>) -> Self {
        Self::new(ParseFail::Other, "BMFF parsing failed", detail)
    }
    fn truncated(detail: impl Into<String>) -> Self {
        Self::new(ParseFail::Truncated, "Truncated data", detail)
    }
    fn grid(detail: impl Into<String>) -> Self {
        Self::new(ParseFail::Other, "Invalid image grid", detail)
    }
    fn not_implemented(detail: impl Into<String>) -> Self {
        Self::new(ParseFail::Other, "Not implemented", detail)
    }
    fn missing(detail: impl Into<String>) -> Self {
        Self::new(ParseFail::Other, "Missing image item", detail)
    }
    fn oom() -> Self {
        Self::new(ParseFail::OutOfMemory, "Out of memory", "")
    }
    /// C++ 版的訊息形式：`<結果>`，有 diag 時加上 ` (<diag>)`。
    pub fn message(&self) -> String {
        if self.detail.is_empty() { self.result.to_string() } else { format!("{} ({})", self.result, self.detail) }
    }
}

type R<T> = Result<T, ParseError>;

/// libavif 的預設上限（AVIF_DEFAULT_IMAGE_SIZE_LIMIT、AVIF_DEFAULT_IMAGE_DIMENSION_LIMIT）。
pub const IMAGE_SIZE_LIMIT: u32 = 16384 * 16384;
pub const IMAGE_DIMENSION_LIMIT: u32 = 32768;
const AUXTYPE_SIZE: usize = 64;
const CONTENTTYPE_SIZE: usize = 64;
const MAX_PIXI_PLANE_DEPTHS: u8 = 4;
const MAX_IPMA_VERSION_AND_FLAGS_SEEN: usize = 4;
const AVIF_MAX_AV1_LAYER_COUNT: u16 = 4;
const URN_ALPHA0: &str = "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha";
const URN_ALPHA1: &str = "urn:mpeg:hevc:2015:auxid:1";
const CONTENT_TYPE_XMP: &str = "application/rdf+xml";

/// `avifDimensionsTooLarge`
pub fn dimensions_too_large(w: u32, h: u32) -> bool {
    (h != 0 && w > IMAGE_SIZE_LIMIT / h) || w > IMAGE_DIMENSION_LIMIT || h > IMAGE_DIMENSION_LIMIT
}

// ---------------------------------------------------------------- avifROStream

struct Stream<'a> {
    d: &'a [u8],
    off: usize,
    bits_used: usize,
}

impl<'a> Stream<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, off: 0, bits_used: 0 }
    }
    fn remaining(&self) -> usize {
        self.d.len() - self.off
    }
    fn has(&self, n: usize) -> bool {
        n <= self.remaining()
    }
    fn current(&self) -> &'a [u8] {
        &self.d[self.off..]
    }
    fn skip(&mut self, n: usize) -> bool {
        if !self.has(n) {
            return false;
        }
        self.off += n;
        true
    }
    fn read(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.off..self.off.checked_add(n)?)?;
        self.off += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.read(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.read(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.read(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.read(8).map(|b| u64::from_be_bytes(b.try_into().expect("8 bytes")))
    }
    fn ux8(&mut self, factor: u8) -> Option<u64> {
        match factor {
            0 => Some(0),
            1 => self.u8().map(u64::from),
            2 => self.u16().map(u64::from),
            4 => self.u32().map(u64::from),
            8 => self.u64(),
            _ => None,
        }
    }
    /// `avifROStreamReadBitsU32`：MSB 先讀，跨位元組。
    fn bits(&mut self, mut n: usize) -> Option<u32> {
        let mut v = 0u32;
        while n > 0 {
            if self.bits_used == 0 && !self.skip(1) {
                return None;
            }
            let packed = self.d[self.off - 1];
            let take = n.min(8 - self.bits_used);
            self.bits_used += take;
            n -= take;
            let b = (u32::from(packed) >> (8 - self.bits_used)) & ((1u32 << take) - 1);
            v |= b << n;
            if self.bits_used == 8 {
                self.bits_used = 0;
            }
        }
        Some(v)
    }
    /// `avifROStreamReadVersionAndFlags`
    fn version_flags(&mut self) -> Option<(u8, u32)> {
        let b = self.read(4)?;
        Some((b[0], (u32::from(b[1]) << 16) | (u32::from(b[2]) << 8) | u32::from(b[3])))
    }
    /// `avifROStreamReadString`：要有 NUL；回傳截到 `max - 1` 個字元的字串。
    fn string(&mut self, max: usize) -> Option<String> {
        let rest = self.current();
        let len = rest.iter().position(|&c| c == 0)?;
        self.off += len + 1;
        let keep = len.min(max.saturating_sub(1));
        Some(String::from_utf8_lossy(&rest[..keep]).into_owned())
    }
}

struct BoxHeader {
    kind: [u8; 4],
    /// 內容長度（標頭之後）；size 為 0 時見 `size_zero`
    size: usize,
    size_zero: bool,
}

/// `avifROStreamReadBoxHeaderPartial`
fn box_header_partial(s: &mut Stream<'_>, top: bool) -> Option<BoxHeader> {
    let start = s.off;
    let small = s.u32()?;
    let kind: [u8; 4] = s.read(4)?.try_into().expect("4 bytes");
    let mut size = u64::from(small);
    if size == 1 {
        size = s.u64()?;
    }
    if &kind == b"uuid" {
        s.read(16)?;
    }
    let read = (s.off - start) as u64;
    if size == 0 {
        if !top {
            return None;
        }
        return Some(BoxHeader { kind, size: 0, size_zero: true });
    }
    if size < read {
        return None;
    }
    Some(BoxHeader { kind, size: usize::try_from(size - read).ok()?, size_zero: false })
}

/// `avifROStreamReadBoxHeader`（非頂層）：內容不能超出外層。
fn box_header(s: &mut Stream<'_>) -> Option<BoxHeader> {
    let h = box_header_partial(s, false)?;
    (h.size <= s.remaining()).then_some(h)
}

// ---------------------------------------------------------------- property 與 item

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Av1C {
    pub seq_profile: u8,
    pub seq_level_idx0: u8,
    pub seq_tier0: u8,
    pub high_bitdepth: u8,
    pub twelve_bit: u8,
    pub monochrome: u8,
    pub chroma_subsampling_x: u8,
    pub chroma_subsampling_y: u8,
    pub chroma_sample_position: u8,
}

impl Av1C {
    pub fn depth(&self) -> u32 {
        if self.twelve_bit != 0 {
            12
        } else if self.high_bitdepth != 0 {
            10
        } else {
            8
        }
    }
    fn format(&self) -> YuvFormat {
        if self.monochrome != 0 {
            YuvFormat::Yuv400
        } else if self.chroma_subsampling_x != 0 && self.chroma_subsampling_y != 0 {
            YuvFormat::Yuv420
        } else if self.chroma_subsampling_x != 0 {
            YuvFormat::Yuv422
        } else {
            YuvFormat::Yuv444
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clap {
    pub v: [u32; 8],
}

#[derive(Clone, Debug)]
enum Colr {
    Icc { offset: u64, size: usize },
    Nclx { primaries: u16, transfer: u16, matrix: u16, full: bool },
    Other,
}

#[derive(Clone, Debug)]
enum Prop {
    Ispe(u32, u32),
    AuxC(String),
    Colr(Colr),
    Av1C(Av1C),
    Pasp,
    Clap(Clap),
    Irot(u8),
    Imir(u8),
    Pixi(Vec<u8>),
    /// 運算點（rav1d 一律用 0，libavif 依 a1op 選；D-R19 已知風險）
    A1op,
    Lsel(u16),
    A1lx([u32; 3]),
    Clli(u16, u16),
    Opaque,
}

#[derive(Clone, Debug)]
struct Property {
    kind: [u8; 4],
    prop: Prop,
}

fn find<'a>(props: &'a [Property], kind: &[u8; 4]) -> Option<&'a Prop> {
    props.iter().find(|p| &p.kind == kind).map(|p| &p.prop)
}

#[derive(Clone, Debug, Default)]
struct Item {
    id: u32,
    kind: [u8; 4],
    content_type: String,
    extents: Vec<(u64, usize)>,
    size: usize,
    idat: bool,
    props: Vec<Property>,
    ipma_seen: bool,
    unsupported_essential: bool,
    thumbnail_for: u32,
    aux_for: u32,
    desc_for: u32,
    dimg_for: u32,
    dimg_idx: u32,
    has_dimg_from: bool,
    prem_by: u32,
    width: u32,
    height: u32,
}

impl Item {
    fn is_av1(&self) -> bool {
        &self.kind == b"av01"
    }
    fn is_grid(&self) -> bool {
        &self.kind == b"grid"
    }
    /// `avifDecoderItemShouldBeSkipped`
    fn skipped(&self) -> bool {
        self.size == 0 || self.unsupported_essential || (!self.is_av1() && !self.is_grid()) || self.thumbnail_for != 0
    }
    fn is_alpha_aux_of(&self, color: u32) -> bool {
        self.aux_for == color
            && matches!(find(&self.props, b"auxC"), Some(Prop::AuxC(u)) if u == URN_ALPHA0 || u == URN_ALPHA1)
    }
}

#[derive(Default)]
struct Meta {
    items: Vec<Item>,
    properties: Vec<Property>,
    primary: u32,
    /// idat 內容在檔案裡的位置與長度
    idat: Option<(u64, usize)>,
}

impl Meta {
    fn find_or_create(&mut self, id: u32) -> R<usize> {
        if let Some(i) = self.items.iter().position(|it| it.id == id) {
            return Ok(i);
        }
        self.items.try_reserve(1).map_err(|_| ParseError::oom())?;
        self.items.push(Item { id, ..Item::default() });
        Ok(self.items.len() - 1)
    }
}

fn check_item_id(boxname: &str, id: u32) -> R<()> {
    if id == 0 {
        return Err(ParseError::bmff(format!("Box[{boxname}] has an invalid item ID [{id}]")));
    }
    Ok(())
}

fn parse_hdlr(raw: &[u8]) -> Option<[u8; 4]> {
    let mut s = Stream::new(raw);
    let (v, _) = s.version_flags()?;
    if v != 0 || s.u32()? != 0 {
        return None;
    }
    let t: [u8; 4] = s.read(4)?.try_into().ok()?;
    for _ in 0..3 {
        s.u32()?;
    }
    s.string(0)?;
    Some(t)
}

fn parse_iloc(meta: &mut Meta, raw: &[u8]) -> R<()> {
    let e = || ParseError::bmff("Box[iloc] is truncated or invalid");
    let mut s = Stream::new(raw);
    let (version, _) = s.version_flags().ok_or_else(e)?;
    if version > 2 {
        return Err(ParseError::bmff(format!("Box[iloc] has an unsupported version [{version}]")));
    }
    let offset_size = s.bits(4).ok_or_else(e)? as u8;
    let length_size = s.bits(4).ok_or_else(e)? as u8;
    let base_size = s.bits(4).ok_or_else(e)? as u8;
    let index_size = if version == 1 || version == 2 {
        s.bits(4).ok_or_else(e)? as u8
    } else {
        s.bits(4).ok_or_else(e)?;
        0
    };
    let ok = |v: u8| matches!(v, 0 | 4 | 8);
    if !ok(offset_size) || !ok(length_size) || !ok(base_size) || !ok(index_size) {
        return Err(ParseError::bmff("Box[iloc] has an invalid size"));
    }
    let count = if version < 2 { u32::from(s.u16().ok_or_else(e)?) } else { s.u32().ok_or_else(e)? };
    for _ in 0..count {
        let id = if version < 2 { u32::from(s.u16().ok_or_else(e)?) } else { s.u32().ok_or_else(e)? };
        check_item_id("iloc", id)?;
        let i = meta.find_or_create(id)?;
        if !meta.items[i].extents.is_empty() {
            return Err(ParseError::bmff(format!("Item ID [{id}] contains duplicate sets of extents")));
        }
        if version == 1 || version == 2 {
            let reserved = s.bits(12).ok_or_else(e)?;
            if reserved != 0 {
                return Err(ParseError::bmff(format!("Box[iloc] has a non null reserved field [{reserved}]")));
            }
            let cm = s.bits(4).ok_or_else(e)?;
            if cm != 0 && cm != 1 {
                return Err(ParseError::bmff(format!("Box[iloc] has an unsupported construction method [{cm}]")));
            }
            if cm == 1 {
                meta.items[i].idat = true;
            }
        }
        s.u16().ok_or_else(e)?; // data_reference_index
        let base = s.ux8(base_size).ok_or_else(e)?;
        let extents = s.u16().ok_or_else(e)?;
        for _ in 0..extents {
            if (version == 1 || version == 2) && index_size > 0 {
                s.ux8(index_size).ok_or_else(e)?;
            }
            let off = s.ux8(offset_size).ok_or_else(e)?;
            let len = s.ux8(length_size).ok_or_else(e)?;
            let item = &mut meta.items[i];
            let Some(offset) = base.checked_add(off) else {
                return Err(ParseError::bmff(format!("Item ID [{id}] contains an extent offset which overflows")));
            };
            let len = usize::try_from(len).map_err(|_| ParseError::bmff("extent length overflows"))?;
            let Some(total) = item.size.checked_add(len) else {
                return Err(ParseError::bmff(format!(
                    "Item ID [{id}] contains an extent length which overflows the item size"
                )));
            };
            item.extents.try_reserve(1).map_err(|_| ParseError::oom())?;
            item.extents.push((offset, len));
            item.size = total;
        }
    }
    Ok(())
}

fn parse_pitm(meta: &mut Meta, raw: &[u8]) -> bool {
    if meta.primary > 0 {
        return false;
    }
    let mut s = Stream::new(raw);
    let Some((v, _)) = s.version_flags() else { return false };
    let id = if v == 0 { s.u16().map(u32::from) } else { s.u32() };
    match id {
        Some(id) => {
            meta.primary = id;
            true
        }
        None => false,
    }
}

fn parse_property(kind: [u8; 4], raw: &[u8], raw_offset: u64) -> R<Prop> {
    let fail = |name: &str| ParseError::bmff(format!("Box[{name}] is invalid"));
    let mut s = Stream::new(raw);
    let p = match &kind {
        b"ispe" => {
            let ok = (|| {
                let (v, _) = s.version_flags()?;
                (v == 0).then_some(())?;
                Some(Prop::Ispe(s.u32()?, s.u32()?))
            })();
            ok.ok_or_else(|| fail("ispe"))?
        }
        b"auxC" => {
            let ok = (|| {
                let (v, _) = s.version_flags()?;
                (v == 0).then_some(())?;
                Some(Prop::AuxC(s.string(AUXTYPE_SIZE)?))
            })();
            ok.ok_or_else(|| fail("auxC"))?
        }
        b"colr" => {
            let t = s.read(4).ok_or_else(|| fail("colr"))?;
            if t == b"rICC" || t == b"prof" {
                if s.remaining() == 0 {
                    return Err(ParseError::bmff("Box[colr] contains empty ICC_profile"));
                }
                Prop::Colr(Colr::Icc { offset: raw_offset + s.off as u64, size: s.remaining() })
            } else if t == b"nclx" {
                let ok = (|| {
                    let p = s.u16()?;
                    let tr = s.u16()?;
                    let m = s.u16()?;
                    let full = s.bits(1)? != 0;
                    let reserved = s.bits(7)?;
                    (reserved == 0).then_some(Prop::Colr(Colr::Nclx { primaries: p, transfer: tr, matrix: m, full }))
                })();
                ok.ok_or_else(|| fail("colr"))?
            } else {
                Prop::Colr(Colr::Other)
            }
        }
        b"av1C" => {
            let ok = (|| {
                let marker = s.bits(1)?;
                let version = s.bits(7)?;
                (marker == 1 && version == 1).then_some(())?;
                let c = Av1C {
                    seq_profile: s.bits(3)? as u8,
                    seq_level_idx0: s.bits(5)? as u8,
                    seq_tier0: s.bits(1)? as u8,
                    high_bitdepth: s.bits(1)? as u8,
                    twelve_bit: s.bits(1)? as u8,
                    monochrome: s.bits(1)? as u8,
                    chroma_subsampling_x: s.bits(1)? as u8,
                    chroma_subsampling_y: s.bits(1)? as u8,
                    chroma_sample_position: s.bits(2)? as u8,
                };
                s.skip(1).then_some(Prop::Av1C(c))
            })();
            ok.ok_or_else(|| fail("av1C"))?
        }
        b"pasp" => {
            s.u32().and(s.u32()).ok_or_else(|| fail("pasp"))?;
            Prop::Pasp
        }
        b"clap" => {
            let mut v = [0u32; 8];
            for x in &mut v {
                *x = s.u32().ok_or_else(|| fail("clap"))?;
            }
            Prop::Clap(Clap { v })
        }
        b"irot" => {
            let ok = (|| {
                (s.bits(6)? == 0).then_some(())?;
                Some(Prop::Irot(s.bits(2)? as u8))
            })();
            ok.ok_or_else(|| fail("irot"))?
        }
        b"imir" => {
            let ok = (|| {
                (s.bits(7)? == 0).then_some(())?;
                Some(Prop::Imir(s.bits(1)? as u8))
            })();
            ok.ok_or_else(|| fail("imir"))?
        }
        b"pixi" => {
            let (v, _) = s.version_flags().ok_or_else(|| fail("pixi"))?;
            if v != 0 {
                return Err(fail("pixi"));
            }
            let n = s.u8().ok_or_else(|| fail("pixi"))?;
            if !(1..=MAX_PIXI_PLANE_DEPTHS).contains(&n) {
                return Err(ParseError::not_implemented(format!("Box[pixi] contains unsupported plane count [{n}]")));
            }
            let mut depths = Vec::new();
            for i in 0..n {
                let d = s.u8().ok_or_else(|| fail("pixi"))?;
                if d == 0 {
                    return Err(ParseError::bmff(format!("Box[pixi] plane depth shall not be 0 for channel {i}")));
                }
                if d > 16 {
                    return Err(ParseError::not_implemented(format!("Box[pixi] plane depth {d} is not supported")));
                }
                if let Some(&first) = depths.first()
                    && d != first
                {
                    return Err(ParseError::not_implemented("Box[pixi] contains unsupported mismatched plane depths"));
                }
                depths.push(d);
            }
            Prop::Pixi(depths)
        }
        b"a1op" => {
            let op = s.u8().ok_or_else(|| fail("a1op"))?;
            if op > 31 {
                return Err(fail("a1op"));
            }
            Prop::A1op
        }
        b"lsel" => {
            let l = s.u16().ok_or_else(|| fail("lsel"))?;
            if l != 0xFFFF && l >= AVIF_MAX_AV1_LAYER_COUNT {
                return Err(fail("lsel"));
            }
            Prop::Lsel(l)
        }
        b"a1lx" => {
            let large = s.u8().ok_or_else(|| fail("a1lx"))?;
            if large & 0xFE != 0 {
                return Err(fail("a1lx"));
            }
            let mut sz = [0u32; 3];
            for x in &mut sz {
                *x = if large != 0 { s.u32() } else { s.u16().map(u32::from) }.ok_or_else(|| fail("a1lx"))?;
            }
            Prop::A1lx(sz)
        }
        b"clli" => {
            let ok = (|| Some(Prop::Clli(s.u16()?, s.u16()?)))();
            ok.ok_or_else(|| fail("clli"))?
        }
        _ => Prop::Opaque,
    };
    Ok(p)
}

fn parse_ipco(meta: &mut Meta, raw: &[u8], raw_offset: u64) -> R<()> {
    let mut s = Stream::new(raw);
    while s.has(1) {
        let h = box_header(&mut s).ok_or_else(|| ParseError::bmff("Box[ipco] child header is invalid"))?;
        let body = &s.current()[..h.size];
        let prop = parse_property(h.kind, body, raw_offset + s.off as u64)?;
        meta.properties.try_reserve(1).map_err(|_| ParseError::oom())?;
        meta.properties.push(Property { kind: h.kind, prop });
        s.skip(h.size);
    }
    Ok(())
}

fn parse_ipma(meta: &mut Meta, raw: &[u8]) -> R<u32> {
    let e = || ParseError::bmff("Box[ipma] is truncated");
    let mut s = Stream::new(raw);
    let (version, flags) = s.version_flags().ok_or_else(e)?;
    let u15 = flags & 1 != 0;
    let count = s.u32().ok_or_else(e)?;
    let mut prev = 0u32;
    for _ in 0..count {
        let id = if version < 1 { u32::from(s.u16().ok_or_else(e)?) } else { s.u32().ok_or_else(e)? };
        check_item_id("ipma", id)?;
        if id <= prev {
            return Err(ParseError::bmff("Box[ipma] item IDs are not ordered by increasing ID"));
        }
        prev = id;
        let i = meta.find_or_create(id)?;
        if meta.items[i].ipma_seen {
            return Err(ParseError::bmff(format!("Duplicate Box[ipma] for item ID [{id}]")));
        }
        meta.items[i].ipma_seen = true;
        let n = s.u8().ok_or_else(e)?;
        for _ in 0..n {
            let essential = s.bits(1).ok_or_else(e)? != 0;
            let mut idx = s.bits(if u15 { 15 } else { 7 }).ok_or_else(e)? as usize;
            if idx == 0 {
                if essential {
                    return Err(ParseError::bmff(format!(
                        "Box[ipma] for item ID [{id}] contains an illegal essential property index 0"
                    )));
                }
                continue;
            }
            idx -= 1;
            let Some(src) = meta.properties.get(idx).cloned() else {
                return Err(ParseError::bmff(format!(
                    "Box[ipma] for item ID [{id}] contains an illegal property index [{idx}]"
                )));
            };
            let supported = !matches!(src.prop, Prop::Opaque);
            let kind = src.kind;
            if supported {
                if essential && &kind == b"a1lx" {
                    return Err(ParseError::bmff(format!(
                        "Item ID [{id}] has a a1lx property association which must not be marked essential, but is"
                    )));
                }
                if !essential && [b"a1op", b"lsel", b"clap", b"irot", b"imir"].iter().any(|k| **k == kind) {
                    return Err(ParseError::bmff(format!(
                        "Item ID [{id}] has a {} property association which must be marked essential, but is not",
                        String::from_utf8_lossy(&kind)
                    )));
                }
            } else if essential {
                meta.items[i].unsupported_essential = true;
            }
            let item = &mut meta.items[i];
            item.props.try_reserve(1).map_err(|_| ParseError::oom())?;
            item.props.push(src);
        }
    }
    Ok((u32::from(version) << 24) | flags)
}

fn parse_iprp(meta: &mut Meta, raw: &[u8], raw_offset: u64) -> R<()> {
    let mut s = Stream::new(raw);
    let h = box_header(&mut s).ok_or_else(|| ParseError::bmff("Box[iprp] child header is invalid"))?;
    if &h.kind != b"ipco" {
        return Err(ParseError::bmff("Failed to find Box[ipco] as the first box in Box[iprp]"));
    }
    parse_ipco(meta, &s.current()[..h.size], raw_offset + s.off as u64)?;
    s.skip(h.size);
    let mut seen: Vec<u32> = Vec::new();
    while s.has(1) {
        let h = box_header(&mut s).ok_or_else(|| ParseError::bmff("Box[iprp] child header is invalid"))?;
        if &h.kind != b"ipma" {
            return Err(ParseError::bmff("Box[iprp] contains a box that isn't type 'ipma'"));
        }
        let vf = parse_ipma(meta, &s.current()[..h.size])?;
        if seen.contains(&vf) {
            return Err(ParseError::bmff("Multiple Box[ipma] with a given pair of values of version and flags"));
        }
        if seen.len() == MAX_IPMA_VERSION_AND_FLAGS_SEEN {
            return Err(ParseError::bmff("Exceeded possible count of unique ipma version and flags tuples"));
        }
        seen.push(vf);
        s.skip(h.size);
    }
    Ok(())
}

fn parse_infe(meta: &mut Meta, raw: &[u8]) -> R<()> {
    let e = || ParseError::bmff("Box[infe] is truncated");
    let mut s = Stream::new(raw);
    let (version, _) = s.version_flags().ok_or_else(e)?;
    if version != 2 && version != 3 {
        return Err(ParseError::bmff(format!("Box[infe]: Expecting box version 2 or 3, got version {version}")));
    }
    let id = if version == 2 { u32::from(s.u16().ok_or_else(e)?) } else { s.u32().ok_or_else(e)? };
    check_item_id("infe", id)?;
    s.u16().ok_or_else(e)?; // item_protection_index
    let kind: [u8; 4] = s.read(4).ok_or_else(e)?.try_into().expect("4 bytes");
    s.string(0).ok_or_else(e)?; // item_name
    let content_type = if &kind == b"mime" { s.string(CONTENTTYPE_SIZE).ok_or_else(e)? } else { String::new() };
    let i = meta.find_or_create(id)?;
    meta.items[i].kind = kind;
    meta.items[i].content_type = content_type;
    Ok(())
}

fn parse_iinf(meta: &mut Meta, raw: &[u8]) -> R<()> {
    let e = || ParseError::bmff("Box[iinf] is truncated");
    let mut s = Stream::new(raw);
    let (version, _) = s.version_flags().ok_or_else(e)?;
    let count = match version {
        0 => u32::from(s.u16().ok_or_else(e)?),
        1 => s.u32().ok_or_else(e)?,
        _ => return Err(ParseError::bmff(format!("Box[iinf] has an unsupported version {version}"))),
    };
    for _ in 0..count {
        let h = box_header(&mut s).ok_or_else(e)?;
        if &h.kind != b"infe" {
            return Err(ParseError::bmff("Box[iinf] contains a box that isn't type 'infe'"));
        }
        parse_infe(meta, &s.current()[..h.size])?;
        s.skip(h.size);
    }
    Ok(())
}

fn parse_iref(meta: &mut Meta, raw: &[u8]) -> R<()> {
    let e = || ParseError::bmff("Box[iref] is truncated");
    let mut s = Stream::new(raw);
    let (version, _) = s.version_flags().ok_or_else(e)?;
    if version > 1 {
        return Ok(());
    }
    while s.has(1) {
        let h = box_header(&mut s).ok_or_else(e)?;
        let from = if version == 0 { u32::from(s.u16().ok_or_else(e)?) } else { s.u32().ok_or_else(e)? };
        check_item_id("iref", from)?;
        let fi = meta.find_or_create(from)?;
        if &h.kind == b"dimg" {
            if meta.items[fi].has_dimg_from {
                return Err(ParseError::bmff(format!(
                    "Box[iinf] contains duplicate boxes of type 'dimg' with the same from_item_ID value {from}"
                )));
            }
            meta.items[fi].has_dimg_from = true;
        }
        let n = s.u16().ok_or_else(e)?;
        for idx in 0..n {
            let to = if version == 0 { u32::from(s.u16().ok_or_else(e)?) } else { s.u32().ok_or_else(e)? };
            check_item_id("iref", to)?;
            match &h.kind {
                b"thmb" => meta.items[fi].thumbnail_for = to,
                b"auxl" => meta.items[fi].aux_for = to,
                b"cdsc" => meta.items[fi].desc_for = to,
                b"dimg" => {
                    let ti = meta.find_or_create(to)?;
                    let dimg = &mut meta.items[ti];
                    if dimg.dimg_for == from {
                        return Err(ParseError::grid(""));
                    }
                    if dimg.dimg_for != 0 {
                        return Err(ParseError::not_implemented(""));
                    }
                    dimg.dimg_for = from;
                    dimg.dimg_idx = u32::from(idx);
                }
                b"prem" => meta.items[fi].prem_by = to,
                _ => {}
            }
        }
        // libavif 讀完 reference 就接著讀下一個 box 標頭，不依 box 大小跳過剩下的位元組
    }
    Ok(())
}

fn parse_grpl(raw: &[u8]) -> R<()> {
    let e = || ParseError::bmff("Box[grpl] is truncated");
    let mut s = Stream::new(raw);
    while s.has(1) {
        box_header(&mut s).ok_or_else(e)?;
        s.version_flags().ok_or_else(e)?;
        s.u32().ok_or_else(e)?; // group_id
        let n = s.u32().ok_or_else(e)?;
        for _ in 0..n {
            s.u32().ok_or_else(e)?;
        }
    }
    Ok(())
}

/// `avifParseMetaBox`。`raw_offset` 是 meta 內容在檔案裡的位置。
fn parse_meta(raw: &[u8], raw_offset: u64) -> R<Meta> {
    let mut meta = Meta::default();
    let mut s = Stream::new(raw);
    let (version, _) = s.version_flags().ok_or_else(|| ParseError::bmff("Box[meta] is truncated"))?;
    if version != 0 {
        return Err(ParseError::bmff("Box[meta] has an unsupported version"));
    }
    let mut first = true;
    let mut unique: Vec<[u8; 4]> = Vec::new();
    let mut seen_unique = |kind: &[u8; 4]| -> R<()> {
        if unique.contains(kind) {
            return Err(ParseError::bmff(format!(
                "Box[meta] contains a duplicate unique box of type '{}'",
                String::from_utf8_lossy(kind)
            )));
        }
        unique.push(*kind);
        Ok(())
    };
    while s.has(1) {
        let h = box_header(&mut s).ok_or_else(|| ParseError::bmff("Box[meta] child header is invalid"))?;
        let body = &s.current()[..h.size];
        let body_offset = raw_offset + s.off as u64;
        if first {
            if &h.kind != b"hdlr" {
                return Err(ParseError::bmff("Box[meta] does not have a Box[hdlr] as its first child box"));
            }
            let t = parse_hdlr(body).ok_or_else(|| ParseError::bmff("Box[hdlr] is invalid"))?;
            if &t != b"pict" {
                return Err(ParseError::bmff("Box[hdlr] handler_type is not 'pict'"));
            }
            first = false;
        } else {
            match &h.kind {
                b"hdlr" => return Err(ParseError::bmff("Box[meta] contains a duplicate unique box of type 'hdlr'")),
                b"iloc" => {
                    seen_unique(&h.kind)?;
                    parse_iloc(&mut meta, body)?;
                }
                b"pitm" => {
                    seen_unique(&h.kind)?;
                    if !parse_pitm(&mut meta, body) {
                        return Err(ParseError::bmff("Box[pitm] is invalid"));
                    }
                }
                b"idat" => {
                    seen_unique(&h.kind)?;
                    if meta.idat.is_some() || h.size == 0 {
                        return Err(ParseError::bmff("idat box has a length of 0"));
                    }
                    meta.idat = Some((body_offset, h.size));
                }
                b"iprp" => {
                    seen_unique(&h.kind)?;
                    parse_iprp(&mut meta, body, body_offset)?;
                }
                b"iinf" => {
                    seen_unique(&h.kind)?;
                    parse_iinf(&mut meta, body)?;
                }
                b"iref" => {
                    seen_unique(&h.kind)?;
                    parse_iref(&mut meta, body)?;
                }
                b"grpl" => {
                    seen_unique(&h.kind)?;
                    parse_grpl(body)?;
                }
                _ => {}
            }
        }
        s.skip(h.size);
    }
    if first {
        return Err(ParseError::bmff("Box[meta] has no child boxes"));
    }
    Ok(meta)
}

// ---------------------------------------------------------------- 讀取 item 資料

/// 檔案讀取（C++ 版 FileIO：位移超過檔尾是 I/O 錯誤，其餘讀到檔尾為止）。
fn io_read(input: &Input<'_>, off: u64, n: usize) -> R<Vec<u8>> {
    let size = input.size();
    if off > size {
        return Err(ParseError::new(ParseFail::Other, "IO error", ""));
    }
    let take = usize::try_from((size - off).min(n as u64)).unwrap_or(n);
    let mut buf = Vec::new();
    buf.try_reserve_exact(take).map_err(|_| ParseError::oom())?;
    buf.resize(take, 0);
    match input.read_at(off, &mut buf) {
        Ok(got) if got == take => Ok(buf),
        _ => Err(ParseError::new(ParseFail::Other, "IO error", "")),
    }
}

/// item 的資料放在哪裡（檔案或 meta 的 idat），供解碼時讀取。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemData {
    pub id: u32,
    pub extents: Vec<(u64, usize)>,
    pub idat: Option<(u64, usize)>,
    pub size: usize,
    pub idat_stored: bool,
}

/// `avifDecoderItemRead`（offset 0；`partial` 為 0 時讀整個 item）。
pub fn read_item(input: &Input<'_>, it: &ItemData, partial: usize) -> R<Vec<u8>> {
    if it.extents.is_empty() {
        return Err(ParseError::truncated(format!("Item ID {} has zero extents", it.id)));
    }
    let idat = if it.idat_stored {
        match it.idat {
            Some(d) => Some(d),
            None => {
                return Err(ParseError::new(
                    ParseFail::Other,
                    "No content",
                    format!("Item ID {} is stored in an idat, but no associated idat box was found", it.id),
                ));
            }
        }
    } else {
        None
    };
    let size_hint = input.size();
    if size_hint > 0 && it.size as u64 > size_hint {
        return Err(ParseError::truncated(format!(
            "Item ID {} reported size failed size hint sanity check. Truncated data?",
            it.id
        )));
    }
    if it.size == 0 {
        return Err(ParseError::truncated(format!("Item ID {} read has overflowing offset", it.id)));
    }
    let want = if partial > 0 && partial < it.size { partial } else { it.size };
    let mut out = Vec::new();
    out.try_reserve_exact(want).map_err(|_| ParseError::oom())?;
    let mut remaining = want;
    for &(off, len) in &it.extents {
        let n = len.min(remaining);
        if let Some((idat_off, idat_len)) = idat {
            if off > idat_len as u64 {
                return Err(ParseError::bmff(format!("Item ID {} has impossible extent offset in idat buffer", it.id)));
            }
            if len > idat_len - off as usize {
                return Err(ParseError::bmff(format!("Item ID {} has impossible extent size in idat buffer", it.id)));
            }
            out.extend(io_read(input, idat_off + off, n)?);
        } else {
            if size_hint > 0 && off > size_hint {
                return Err(ParseError::bmff(format!(
                    "Item ID {} extent offset failed size hint sanity check. Truncated data?",
                    it.id
                )));
            }
            let got = io_read(input, off, n)?;
            if got.len() != n {
                return Err(ParseError::truncated(format!(
                    "Item ID {} tried to read {n} bytes, but only received {} bytes",
                    it.id,
                    got.len()
                )));
            }
            out.extend(got);
        }
        remaining -= n;
        if remaining == 0 {
            break;
        }
    }
    if remaining != 0 {
        return Err(ParseError::truncated(format!("Item ID {} has {remaining} unexpected trailing bytes", it.id)));
    }
    Ok(out)
}

// ---------------------------------------------------------------- AV1 序列標頭（obu.c）

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    error: bool,
}

impl Bits<'_> {
    /// `avifBitsRead`：讀過頭時補 0 並記錄錯誤（最後一個位元組之後才算錯）。
    fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.pos / 8;
            let bit = match self.d.get(byte) {
                Some(&b) => u32::from((b >> (7 - self.pos % 8)) & 1),
                None => {
                    self.error = true;
                    0
                }
            };
            v = (v << 1) | bit;
            self.pos += 1;
        }
        v
    }
    fn vlc(&mut self) -> u32 {
        let mut n = 0;
        while self.read(1) == 0 {
            n += 1;
            if n == 32 {
                return u32::MAX;
            }
        }
        if n > 0 { ((1u32 << n) - 1) + self.read(n) } else { 0 }
    }
    fn uleb128(&mut self) -> u32 {
        let mut val = 0u64;
        let mut i = 0;
        let mut more;
        loop {
            let v = self.read(8);
            more = v & 0x80 != 0;
            val |= u64::from(v & 0x7F) << i;
            i += 7;
            if !more || i >= 56 {
                break;
            }
        }
        if val > u64::from(u32::MAX) || more {
            self.error = true;
            return 0;
        }
        val as u32
    }
}

/// 序列標頭的色彩資訊（libavif 在沒有 nclx 時採用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqColor {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

fn parse_seq_header(b: &mut Bits<'_>) -> Option<SeqColor> {
    let profile = b.read(3);
    if profile > 2 || b.error {
        return None;
    }
    let still = b.read(1);
    let reduced = b.read(1);
    if reduced != 0 && still == 0 {
        return None;
    }
    if reduced != 0 {
        b.read(5);
    } else {
        let timing = b.read(1);
        let mut decoder_model = 0;
        let mut delay_len = 0;
        if timing != 0 {
            b.read(32);
            b.read(32);
            if b.read(1) != 0 && b.vlc() == u32::MAX {
                return None;
            }
            decoder_model = b.read(1);
            if decoder_model != 0 {
                delay_len = b.read(5) + 1;
                b.read(32);
                b.read(10);
            }
        }
        let initial_delay = b.read(1);
        let ops = b.read(5) + 1;
        for _ in 0..ops {
            b.read(12);
            let level = b.read(5);
            if level > 7 {
                b.read(1);
            }
            if decoder_model != 0 && b.read(1) != 0 {
                b.read(delay_len);
                b.read(delay_len);
                b.read(1);
            }
            if initial_delay != 0 && b.read(1) != 0 {
                b.read(4);
            }
        }
    }
    if b.error {
        return None;
    }
    let wb = b.read(4) + 1;
    let hb = b.read(4) + 1;
    b.read(wb);
    b.read(hb);
    if reduced == 0 && b.read(1) != 0 {
        b.read(7);
    }
    if b.error {
        return None;
    }
    b.read(1); // use_128x128_superblock
    b.read(2);
    if reduced == 0 {
        b.read(4);
        let order_hint = b.read(1);
        if order_hint != 0 {
            b.read(2);
        }
        let force_sct = if b.read(1) != 0 { 2 } else { b.read(1) };
        if force_sct > 0 && b.read(1) == 0 {
            b.read(1);
        }
        if order_hint != 0 {
            b.read(3);
        }
    }
    if b.error {
        return None;
    }
    b.read(3); // enable_superres, enable_cdef, enable_restoration
    // color_config
    let high = b.read(1);
    let bit_depth = if profile == 2 && high != 0 {
        if b.read(1) != 0 { 12 } else { 10 }
    } else if high != 0 {
        10
    } else {
        8
    };
    let mono = if profile != 1 { b.read(1) } else { 0 };
    let (p, t, m) = if b.read(1) != 0 { (b.read(8), b.read(8), b.read(8)) } else { (2, 2, 2) };
    let full;
    if mono != 0 {
        full = b.read(1) != 0;
    } else if p == 1 && t == 13 && m == 0 {
        full = true;
    } else {
        full = b.read(1) != 0;
        let (sx, sy) = match profile {
            0 => (1, 1),
            1 => (0, 0),
            _ => {
                if bit_depth == 12 {
                    let sx = b.read(1);
                    let sy = if sx != 0 { b.read(1) } else { 0 };
                    (sx, sy)
                } else {
                    (1, 0)
                }
            }
        };
        if sx != 0 && sy != 0 {
            b.read(2);
        }
    }
    if b.error {
        return None;
    }
    if mono == 0 {
        b.read(1);
    }
    b.read(1); // film_grain_params_present
    if b.error {
        return None;
    }
    Some(SeqColor { primaries: p as u16, transfer: t as u16, matrix: m as u16, full_range: full })
}

/// `av1SequenceHeaderParse`：在 OBU 裡找序列標頭。
pub fn sequence_header_color(obus: &[u8]) -> Option<SeqColor> {
    let mut d = obus;
    while !d.is_empty() {
        let mut b = Bits { d, pos: 0, error: false };
        if b.read(1) != 0 {
            return None;
        }
        let obu_type = b.read(4);
        let ext = b.read(1);
        let has_size = b.read(1);
        b.read(1);
        if ext != 0 {
            b.read(8);
        }
        let size = if has_size != 0 { b.uleb128() } else { (d.len() as u32).wrapping_sub(1 + ext) };
        if b.error {
            return None;
        }
        let start = b.pos / 8;
        if size as usize > d.len() - start {
            return None;
        }
        if obu_type == 1 {
            let mut sb = Bits { d: &d[start..start + size as usize], pos: 0, error: false };
            return parse_seq_header(&mut sb);
        }
        d = &d[start + size as usize..];
    }
    None
}

// ---------------------------------------------------------------- 結果

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvFormat {
    Yuv444,
    Yuv422,
    Yuv420,
    Yuv400,
}

impl YuvFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Yuv444 => "444",
            Self::Yuv422 => "422",
            Self::Yuv420 => "420",
            Self::Yuv400 => "400",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub rows: u32,
    pub columns: u32,
    pub output_width: u32,
    pub output_height: u32,
}

/// 一個 tile（或非 grid 影像本身）：item 的尺寸（ispe）與資料位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub data: ItemData,
}

/// 色彩或 alpha 的解碼計畫（grid 時 tile 依 dimg 順序，也就是由左而右、由上而下）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub grid: Option<Grid>,
    pub tiles: Vec<Tile>,
}

/// `avifDecoderParse` 之後 C++ 版讀到的內容。
#[derive(Clone, Debug)]
pub struct Parsed {
    /// 主要 brand 為 avis：libavif 走影像序列（tracks），這裡不解析
    pub tracks: bool,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub yuv_format: YuvFormat,
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
    pub icc: Option<Vec<u8>>,
    pub clli: (u16, u16),
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub irot: Option<u8>,
    pub imir: Option<u8>,
    pub clap: bool,
    pub alpha_present: bool,
    pub alpha_premultiplied: bool,
    pub image_count: u32,
    pub color: Plane,
    pub alpha: Option<Plane>,
}

impl Parsed {
    fn sequence() -> Self {
        Self {
            tracks: true,
            width: 0,
            height: 0,
            depth: 0,
            yuv_format: YuvFormat::Yuv420,
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: true,
            icc: None,
            clli: (0, 0),
            exif: None,
            xmp: None,
            irot: None,
            imir: None,
            clap: false,
            alpha_present: false,
            alpha_premultiplied: false,
            image_count: 0,
            color: Plane { grid: None, tiles: Vec::new() },
            alpha: None,
        }
    }
}

// ---------------------------------------------------------------- avifParse（頂層）

struct TopLevel {
    major: [u8; 4],
    brands: Vec<[u8; 4]>,
    meta: Option<Meta>,
}

fn has_brand(brands: &[[u8; 4]], b: &[u8; 4]) -> bool {
    brands.iter().any(|x| x == b)
}

fn parse_top(input: &Input<'_>) -> R<TopLevel> {
    let size = input.size();
    let mut pos = 0u64;
    let (mut ftyp_seen, mut meta_seen, mut moov_seen, mut meta_size_zero) = (false, false, false, false);
    let (mut needs_meta, mut needs_moov, mut needs_tmap, mut tmap_seen) = (false, false, false, false);
    let mut top = TopLevel { major: [0; 4], brands: Vec::new(), meta: None };
    loop {
        if pos > size {
            return Err(ParseError::bmff(""));
        }
        let head = io_read(input, pos, 32)?;
        if head.is_empty() {
            break;
        }
        let mut hs = Stream::new(&head);
        let h =
            box_header_partial(&mut hs, true).ok_or_else(|| ParseError::bmff("File-level box header is invalid"))?;
        pos += hs.off as u64;
        let is_ftyp = &h.kind == b"ftyp";
        let is_meta = &h.kind == b"meta";
        let is_moov = &h.kind == b"moov";
        let variable = is_ftyp || is_meta || is_moov;
        if !is_ftyp && (variable || [b"free", b"skip", b"mdat"].iter().any(|k| **k == h.kind)) && !ftyp_seen {
            return Err(ParseError::bmff(""));
        }
        let mut content = Vec::new();
        let body_offset = pos;
        let mut box_size = h.size as u64;
        if variable {
            let want = if h.size_zero { usize::try_from(size - pos).unwrap_or(usize::MAX) } else { h.size };
            content = io_read(input, pos, want)?;
            if h.size_zero {
                box_size = content.len() as u64;
            } else if content.len() != h.size {
                return Err(ParseError::truncated(""));
            }
        } else if h.size_zero {
            return Err(ParseError::bmff(""));
        }
        pos = pos.checked_add(box_size).ok_or_else(|| ParseError::bmff(""))?;
        if is_ftyp {
            if ftyp_seen {
                return Err(ParseError::bmff(""));
            }
            let mut s = Stream::new(&content);
            let (Some(major), Some(_minor)) = (s.read(4), s.read(4)) else { return Err(ParseError::bmff("")) };
            if !s.remaining().is_multiple_of(4) {
                return Err(ParseError::bmff(format!(
                    "Box[ftyp] contains a compatible brands section that isn't divisible by 4 [{}]",
                    s.remaining()
                )));
            }
            top.major = major.try_into().expect("4 bytes");
            top.brands = std::iter::once(top.major).chain(s.current().as_chunks::<4>().0.iter().copied()).collect();
            if !has_brand(&top.brands, b"avif") && !has_brand(&top.brands, b"avis") {
                return Err(ParseError::new(ParseFail::Other, "Invalid ftyp", ""));
            }
            ftyp_seen = true;
            needs_meta = has_brand(&top.brands, b"avif");
            needs_moov = has_brand(&top.brands, b"avis");
            needs_tmap = has_brand(&top.brands, b"tmap");
            if needs_tmap {
                needs_meta = true;
            }
        } else if is_meta {
            if meta_seen {
                return Err(ParseError::bmff(""));
            }
            meta_size_zero = h.size_zero;
            let meta = parse_meta(&content, body_offset)?;
            tmap_seen = meta.items.iter().any(|i| &i.kind == b"tmap");
            top.meta = Some(meta);
            meta_seen = true;
        } else if is_moov {
            if moov_seen {
                return Err(ParseError::bmff(""));
            }
            // moov（影像序列）不解析：C++ 版一律回報不支援的動畫（D-R19 已知風險）
            moov_seen = true;
        }
        if ftyp_seen && (!needs_meta || meta_seen) && (!needs_moov || moov_seen) && (!needs_tmap || tmap_seen) {
            return Ok(top);
        }
    }
    if !ftyp_seen {
        return Err(ParseError::new(ParseFail::Other, "Invalid ftyp", ""));
    }
    if (needs_meta && !meta_seen) || (needs_moov && !moov_seen) {
        return Err(ParseError::truncated(""));
    }
    if needs_tmap && !tmap_seen {
        return Err(if meta_size_zero { ParseError::truncated("") } else { ParseError::bmff("") });
    }
    Ok(top)
}

// ---------------------------------------------------------------- avifDecoderParse／Reset

const STRICT_PIXI: bool = true;
const STRICT_CLAP: bool = true;

fn item_data(meta: &Meta, it: &Item) -> ItemData {
    ItemData { id: it.id, extents: it.extents.clone(), idat: meta.idat, size: it.size, idat_stored: it.idat }
}

/// `avifCodecDecodeInputFillFromDecoderItem` 的檢查（allowProgressive 為 false：一個 sample）。
fn check_fill(input: &Input<'_>, it: &Item) -> R<()> {
    let size_hint = input.size();
    if size_hint > 0 && it.size as u64 > size_hint {
        return Err(ParseError::bmff("Exceeded avifIO's sizeHint, possibly truncated data"));
    }
    let mut layer_count = 0u16;
    if let Some(Prop::A1lx(sizes)) = find(&it.props, b"a1lx") {
        let mut remaining = it.size;
        for &l in sizes {
            layer_count += 1;
            let l = l as usize;
            if l > 0 {
                if l >= remaining {
                    return Err(ParseError::bmff("a1lx layer index does not fit in item size"));
                }
                remaining -= l;
            } else {
                remaining = 0;
                break;
            }
        }
        if remaining > 0 {
            layer_count += 1;
        }
    }
    if let Some(Prop::Lsel(l)) = find(&it.props, b"lsel")
        && *l != 0xFFFF
        && layer_count > 0
        && *l >= layer_count
    {
        return Err(ParseError::bmff("lsel property requests a layer index which isn't present in a1lx property"));
    }
    Ok(())
}

/// `avifCropRectFromCleanApertureBox` 的有效性（strict）。
fn clap_valid(c: &Clap, image_w: u32, image_h: u32) -> bool {
    let [wn, wd, hn, hd, hon, hod, von, vod] = c.v.map(|x| x as i32);
    if wd <= 0 || hd <= 0 || hod <= 0 || vod <= 0 || wn < 0 || hn < 0 {
        return false;
    }
    if wn % wd != 0 || hn % hd != 0 {
        return false;
    }
    let (cw, ch) = (wn / wd, hn / hd);
    if image_w > i32::MAX as u32 || image_h > i32::MAX as u32 {
        return false;
    }
    let center = |dim: i32| if dim % 2 != 0 { (i64::from(dim), 2i64) } else { (i64::from(dim >> 1), 1i64) };
    // avifFractionAdd／Sub：分母通分後檢查 int32 溢位
    let add = |a: (i64, i64), b: (i64, i64)| -> Option<(i64, i64)> {
        let (n, d) = if a.1 == b.1 { (a.0 + b.0, a.1) } else { (a.0 * b.1 + b.0 * a.1, a.1 * b.1) };
        let fits = |x: i64| (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&x);
        (fits(n) && fits(d)).then_some((n, d))
    };
    let neg = |a: (i64, i64)| (-a.0, a.1);
    let Some(ccx) = add(center(image_w as i32), (i64::from(hon), i64::from(hod))) else { return false };
    let Some(ccy) = add(center(image_h as i32), (i64::from(von), i64::from(vod))) else { return false };
    let Some(cx) = add(ccx, neg((i64::from(cw), 2))) else { return false };
    if cx.0 % cx.1 != 0 {
        return false;
    }
    let Some(cy) = add(ccy, neg((i64::from(ch), 2))) else { return false };
    if cy.0 % cy.1 != 0 {
        return false;
    }
    if cx.0 < 0 || cy.0 < 0 {
        return false;
    }
    let (x, y) = ((cx.0 / cx.1) as u64, (cy.0 / cy.1) as u64);
    cw != 0 && ch != 0 && x + cw as u64 <= u64::from(image_w) && y + ch as u64 <= u64::from(image_h)
}

/// `avifDecoderItemValidateProperties`
fn validate_properties(meta: &Meta, it: &Item, props: &[Property], pixi_required: bool) -> R<()> {
    let Some(Prop::Av1C(config)) = find(props, b"av1C") else {
        return Err(ParseError::bmff(format!("Item ID {} is missing mandatory av1C property", it.id)));
    };
    if it.is_grid() {
        for tile in meta.items.iter().filter(|t| t.dimg_for == it.id) {
            let Some(Prop::Av1C(tc)) = find(&tile.props, b"av1C") else {
                return Err(ParseError::bmff(format!("Tile item ID {} is missing mandatory av1C property", tile.id)));
            };
            if tc != config {
                return Err(ParseError::bmff(format!(
                    "The fields of the av1C property of tile item ID {} differs from other tiles",
                    tile.id
                )));
            }
        }
    }
    match find(props, b"pixi") {
        None if pixi_required && STRICT_PIXI => {
            return Err(ParseError::bmff(format!("[Strict] Item ID {} is missing mandatory pixi property", it.id)));
        }
        Some(Prop::Pixi(depths)) if depths.iter().any(|&d| u32::from(d) != config.depth()) => {
            return Err(ParseError::bmff(format!(
                "Item ID {} depth specified by pixi property does not match av1C property depth",
                it.id
            )));
        }
        _ => {}
    }
    if STRICT_CLAP && let Some(Prop::Clap(c)) = find(props, b"clap") {
        let Some(Prop::Ispe(w, h)) = find(props, b"ispe") else {
            return Err(ParseError::bmff(format!(
                "[Strict] Item ID {} is missing an ispe property, so its clap property cannot be validated",
                it.id
            )));
        };
        if !clap_valid(c, *w, *h) {
            return Err(ParseError::bmff("[Strict] invalid clap"));
        }
    }
    Ok(())
}

/// grid item 的 tile，依 dimg 順序（`avifFillDimgIdxToItemIdxArray`）。
fn grid_tiles(meta: &Meta, grid_id: u32, expected: u32) -> R<Vec<usize>> {
    let mut idx = vec![usize::MAX; expected as usize];
    for (i, it) in meta.items.iter().enumerate() {
        if it.dimg_for == grid_id {
            let d = it.dimg_idx as usize;
            if d >= idx.len() || idx[d] != usize::MAX {
                return Err(ParseError::grid(""));
            }
            idx[d] = i;
        }
    }
    if idx.contains(&usize::MAX) {
        return Err(ParseError::grid(""));
    }
    Ok(idx)
}

/// `avifParseImageGridBox`
fn parse_grid(raw: &[u8]) -> R<Grid> {
    let mut s = Stream::new(raw);
    let e = || ParseError::bmff("Box[grid] is truncated");
    let version = s.u8().ok_or_else(e)?;
    if version != 0 {
        return Err(ParseError::not_implemented(format!("Box[grid] has unsupported version [{version}]")));
    }
    let flags = s.u8().ok_or_else(e)?;
    let rows = u32::from(s.u8().ok_or_else(e)?) + 1;
    let columns = u32::from(s.u8().ok_or_else(e)?) + 1;
    let (w, h) = if flags & 1 == 0 {
        (u32::from(s.u16().ok_or_else(e)?), u32::from(s.u16().ok_or_else(e)?))
    } else {
        (s.u32().ok_or_else(e)?, s.u32().ok_or_else(e)?)
    };
    if w == 0 || h == 0 {
        return Err(ParseError::grid(format!("Grid box contains illegal dimensions: [{w} x {h}]")));
    }
    if dimensions_too_large(w, h) {
        return Err(ParseError::not_implemented(format!("Grid box dimensions are too large: [{w} x {h}]")));
    }
    if s.remaining() != 0 {
        return Err(ParseError::bmff(""));
    }
    Ok(Grid { rows, columns, output_width: w, output_height: h })
}

/// `avifGetExifTiffHeaderOffset`
fn exif_tiff_offset(e: &[u8]) -> Option<usize> {
    let n = e.len().min(u32::MAX as usize);
    (0..n).take_while(|o| o + 4 < n).find(|&o| &e[o..o + 4] == b"MM\0\x2a" || &e[o..o + 4] == b"II\x2a\0")
}

/// 讀 item 並解析成 grid（`avifDecoderItemReadAndParse` 的 grid 部分）。
fn read_grid(input: &Input<'_>, meta: &Meta, it: &Item) -> R<Grid> {
    let raw = read_item(input, &item_data(meta, it), 0)?;
    let grid = parse_grid(&raw)?;
    let count = meta.items.iter().filter(|t| t.dimg_for == it.id).count() as u64;
    if count != u64::from(grid.rows) * u64::from(grid.columns) {
        return Err(ParseError::grid(""));
    }
    if !meta.items.iter().any(|t| t.dimg_for == it.id && t.is_av1()) {
        return Err(ParseError::grid(""));
    }
    Ok(grid)
}

/// `avifDecoderAdoptGridTileCodecType`：檢查 tile，回傳第一個 tile 的 av1C（加到 grid item 的 property）。
fn adopt_grid_tiles(meta: &Meta, order: &[usize]) -> R<Property> {
    let mut first: Option<&Item> = None;
    let mut config = None;
    for &i in order {
        let t = &meta.items[i];
        if !t.is_av1() {
            return Err(ParseError::grid(format!("Tile item ID {} has an unknown item type", t.id)));
        }
        if t.unsupported_essential {
            return Err(ParseError::grid("Grid image contains tile with an unsupported property marked as essential"));
        }
        match first {
            None => {
                first = Some(t);
                let Some(p) = t.props.iter().find(|p| &p.kind == b"av1C") else {
                    return Err(ParseError::grid("Grid image's first tile is missing an av1C property"));
                };
                config = Some(p.clone());
            }
            Some(f) if f.kind != t.kind => return Err(ParseError::grid("")),
            Some(_) => {}
        }
    }
    config.ok_or_else(|| ParseError::grid(""))
}

/// 色彩或 alpha 的 item 整理成解碼計畫，並做 libavif 在 Reset 裡對它的檢查。
fn plan(
    input: &Input<'_>,
    meta: &mut Meta,
    item_idx: usize,
    grid: Option<Grid>,
    in_input: bool,
    tiles_override: Option<Vec<usize>>,
) -> R<Plane> {
    let it = meta.items[item_idx].clone();
    let mut props = it.props.clone();
    let mut tiles = Vec::new();
    if let Some(g) = grid {
        let order = match tiles_override {
            Some(o) => o,
            None => grid_tiles(meta, it.id, g.rows * g.columns)?,
        };
        let cfg = adopt_grid_tiles(meta, &order)?;
        props.push(cfg);
        meta.items[item_idx].props = props.clone();
        for &i in &order {
            let t = &meta.items[i];
            check_fill(input, t)?;
            tiles.push(Tile { width: t.width, height: t.height, data: item_data(meta, t) });
        }
    } else {
        if it.size == 0 {
            return Err(ParseError::missing(""));
        }
        check_fill(input, &it)?;
        tiles.push(Tile { width: it.width, height: it.height, data: item_data(meta, &it) });
    }
    let probe = Item { props: props.clone(), ..it.clone() };
    validate_properties(meta, &probe, &props, in_input)?;
    Ok(Plane { grid, tiles })
}

/// `avifDecoderParse` 到 `avifDecoderReset` 結束（item 這條路）。
///
/// # Errors
///
/// libavif 會失敗的情況，見 [`ParseError`]。
pub fn parse(input: &Input<'_>) -> R<Parsed> {
    let top = parse_top(input)?;
    let mut meta = top.meta.unwrap_or_default();
    // ispe（avifDecoderParse）
    for it in meta.items.iter_mut() {
        if it.skipped() {
            continue;
        }
        match find(&it.props, b"ispe") {
            Some(Prop::Ispe(w, h)) => {
                let (w, h) = (*w, *h);
                it.width = w;
                it.height = h;
                if w == 0 || h == 0 {
                    return Err(ParseError::bmff(format!("Item ID [{}] has an invalid size [{w}x{h}]", it.id)));
                }
                if dimensions_too_large(w, h) {
                    return Err(ParseError::bmff(format!("Item ID [{}] dimensions are too large [{w}x{h}]", it.id)));
                }
            }
            _ => {
                let alpha =
                    matches!(find(&it.props, b"auxC"), Some(Prop::AuxC(u)) if u == URN_ALPHA0 || u == URN_ALPHA1);
                return Err(ParseError::bmff(if alpha {
                    format!("[Strict] Alpha auxiliary image item ID [{}] is missing a mandatory ispe property", it.id)
                } else {
                    format!("Item ID [{}] is missing a mandatory ispe property", it.id)
                }));
            }
        }
    }
    if &top.major == b"avis" {
        return Ok(Parsed::sequence());
    }
    // ---- avifDecoderReset（primary item）
    if meta.primary == 0 {
        return Err(ParseError::missing("Primary item not specified"));
    }
    let Some(ci) = meta.items.iter().position(|it| !it.skipped() && it.id == meta.primary) else {
        return Err(ParseError::missing("Primary item not found"));
    };
    let color_grid = if meta.items[ci].is_grid() { Some(read_grid(input, &meta, &meta.items[ci])?) } else { None };
    let color_id = meta.items[ci].id;
    // alpha（avifMetaFindAlphaItem）
    let mut alpha_idx = meta.items.iter().position(|it| !it.skipped() && it.is_alpha_aux_of(color_id));
    let mut alpha_tiles: Option<Vec<usize>> = None;
    let mut alpha_in_input = alpha_idx.is_some();
    if alpha_idx.is_none()
        && let Some(g) = color_grid
    {
        let n = (g.rows * g.columns) as usize;
        let mut map = vec![usize::MAX; n];
        let mut count = 0usize;
        let mut none = false;
        for item in meta.items.iter().filter(|t| t.dimg_for == color_id) {
            let mut seen = false;
            for (j, aux) in meta.items.iter().enumerate() {
                if aux.is_alpha_aux_of(item.id) {
                    let d = item.dimg_idx as usize;
                    if seen || aux.dimg_for != 0 || d >= n || map[d] != usize::MAX {
                        return Err(ParseError::grid(""));
                    }
                    map[d] = j;
                    count += 1;
                    seen = true;
                }
            }
            if !seen {
                none = true;
                break;
            }
        }
        if !none {
            if count != n {
                return Err(ParseError::grid(""));
            }
            // libavif 建一個假的 grid item 來放 alpha tile（不在輸入裡，不檢查 pixi）
            let new_id = (1..).find(|id| !meta.items.iter().any(|t| t.id == *id)).expect("an unused id");
            let (w, h) = (meta.items[ci].width, meta.items[ci].height);
            for (d, &j) in map.iter().enumerate() {
                meta.items[j].dimg_for = new_id;
                meta.items[j].dimg_idx = d as u32;
            }
            meta.items.push(Item { id: new_id, kind: *b"grid", width: w, height: h, ..Item::default() });
            alpha_idx = Some(meta.items.len() - 1);
            alpha_tiles = Some(map);
            alpha_in_input = false;
        }
    }
    let alpha_grid = match alpha_idx {
        Some(ai) if meta.items[ai].is_grid() => {
            if alpha_in_input {
                Some(read_grid(input, &meta, &meta.items[ai])?)
            } else {
                color_grid
            }
        }
        _ => None,
    };
    // metadata（avifDecoderFindMetadata）
    let mut exif = None;
    let mut xmp = None;
    for it in &meta.items {
        if it.size == 0 || it.unsupported_essential || it.desc_for != color_id {
            continue;
        }
        if &it.kind == b"Exif" {
            let raw = read_item(input, &item_data(&meta, it), 0)?;
            let invalid = || ParseError::new(ParseFail::Other, "Invalid Exif payload", "");
            if raw.len() < 4 {
                return Err(invalid());
            }
            let declared = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
            let rest = &raw[4..];
            if exif_tiff_offset(rest) != Some(declared) {
                return Err(invalid());
            }
            exif = Some(rest.to_vec());
        } else if &it.kind == b"mime" && it.content_type == CONTENT_TYPE_XMP {
            xmp = Some(read_item(input, &item_data(&meta, it), 0)?);
        }
    }
    // 色彩與 alpha 的 tile（avifDecoderGenerateImageTiles + ValidateProperties）
    let color = plan(input, &mut meta, ci, color_grid, true, None)?;
    let alpha = match alpha_idx {
        Some(ai) => {
            if meta.items[ai].width == 0 && meta.items[ai].height == 0 {
                meta.items[ai].width = meta.items[ci].width;
                meta.items[ai].height = meta.items[ci].height;
            }
            Some(plan(input, &mut meta, ai, alpha_grid, alpha_in_input, alpha_tiles)?)
        }
        None => None,
    };
    let color_item = &meta.items[ci];
    let (width, height) = (color_item.width, color_item.height);
    let alpha_premultiplied = alpha_idx.is_some_and(|ai| color_item.prem_by == meta.items[ai].id);
    for t in color.tiles.iter().chain(alpha.iter().flat_map(|a| a.tiles.iter())) {
        if t.data.size == 0 {
            return Err(ParseError::bmff(""));
        }
    }
    let color_props = color_item.props.clone();
    // 色彩 property（avifReadColorProperties）
    let mut icc = None;
    let mut nclx = None;
    for p in &color_props {
        match &p.prop {
            Prop::Colr(Colr::Icc { offset, size }) => {
                if icc.is_some() {
                    return Err(ParseError::bmff(""));
                }
                icc = Some(io_read(input, *offset, *size)?);
            }
            Prop::Colr(Colr::Nclx { primaries, transfer, matrix, full }) => {
                if nclx.is_some() {
                    return Err(ParseError::bmff(""));
                }
                nclx =
                    Some(SeqColor { primaries: *primaries, transfer: *transfer, matrix: *matrix, full_range: *full });
            }
            _ => {}
        }
    }
    let clli = match find(&color_props, b"clli") {
        Some(Prop::Clli(a, b)) => (*a, *b),
        _ => (0, 0),
    };
    let clap = find(&color_props, b"clap").cloned();
    let irot = match find(&color_props, b"irot") {
        Some(Prop::Irot(a)) => Some(*a),
        _ => None,
    };
    let imir = match find(&color_props, b"imir") {
        Some(Prop::Imir(a)) => Some(*a),
        _ => None,
    };
    if let Some(ai) = alpha_idx {
        let ap = &meta.items[ai].props;
        let (ac, ar, am) = (find(ap, b"clap"), find(ap, b"irot"), find(ap, b"imir"));
        if ac.is_some() || ar.is_some() || am.is_some() {
            let same_clap = match (ac, &clap) {
                (None, None) => true,
                (Some(Prop::Clap(a)), Some(Prop::Clap(b))) => a == b,
                _ => false,
            };
            let ar = ar.and_then(|p| if let Prop::Irot(a) = p { Some(*a) } else { None });
            let am = am.and_then(|p| if let Prop::Imir(a) = p { Some(*a) } else { None });
            if !same_clap || ar != irot || am != imir {
                return Err(ParseError::not_implemented(
                    "Transformative property mismatch between alpha auxiliary image item and color item",
                ));
            }
        }
    }
    // 沒有 nclx 時讀第一個 tile 的序列標頭（每次多讀 64 bytes，最多 4096 bytes）
    let mut cicp = nclx;
    if cicp.is_none()
        && let Some(first) = color.tiles.first()
    {
        let mut n = 0usize;
        loop {
            n = (n + 64).min(first.data.size);
            let sample = read_item(input, &first.data, n)?;
            if let Some(c) = sequence_header_color(&sample) {
                cicp = Some(c);
                break;
            }
            if n == first.data.size || n >= 4096 {
                break;
            }
        }
    }
    let cicp = cicp.unwrap_or(SeqColor { primaries: 2, transfer: 2, matrix: 2, full_range: true });
    // av1C（avifReadCodecConfigProperty）
    let Some(Prop::Av1C(config)) = find(&color_props, b"av1C") else {
        return Err(ParseError::bmff(""));
    };
    Ok(Parsed {
        tracks: false,
        width,
        height,
        depth: config.depth(),
        yuv_format: config.format(),
        primaries: cicp.primaries,
        transfer: cicp.transfer,
        matrix: cicp.matrix,
        full_range: cicp.full_range,
        icc,
        clli,
        exif,
        xmp,
        irot,
        imir,
        clap: clap.is_some(),
        alpha_present: alpha.is_some(),
        alpha_premultiplied,
        image_count: 1,
        color,
        alpha,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_matches_libavif() {
        let d = [0b1011_0011u8, 0b0101_0000];
        let mut s = Stream::new(&d);
        assert_eq!(s.bits(4), Some(0b1011));
        assert_eq!(s.bits(4), Some(0b0011));
        assert_eq!(s.bits(1), Some(0));
        assert_eq!(s.bits(7), Some(0b101_0000));
        assert_eq!(s.bits(1), None);
    }

    #[test]
    fn exif_offset_search() {
        assert_eq!(exif_tiff_offset(b"Exif\0\0MM\0\x2a\0\0\0\x08"), Some(6));
        assert_eq!(exif_tiff_offset(b"II\x2a\0\x08"), Some(0));
        // libavif 要求 offset + 4 < 長度
        assert_eq!(exif_tiff_offset(b"II\x2a\0"), None);
    }

    #[test]
    fn clap_validation() {
        // 中央裁成一半
        let c = Clap { v: [50, 1, 50, 1, 0, 1, 0, 1] };
        assert!(clap_valid(&c, 100, 100));
        // 寬度不是整數
        assert!(!clap_valid(&Clap { v: [51, 2, 50, 1, 0, 1, 0, 1] }, 100, 100));
        // 超出影像
        assert!(!clap_valid(&Clap { v: [50, 1, 50, 1, 40, 1, 0, 1] }, 100, 100));
    }
}
