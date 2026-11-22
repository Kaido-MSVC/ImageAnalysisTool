//! 獨立的 PNG 容器掃描（移植 C++ 版 `src/engine/png_scan.cpp`，不經過任何 PNG 解碼器）。
//!
//! 除了移植的部分，這裡另外記錄兩件事，供 `png_side` 使用：
//! - **解碼器輸入**：只把 IHDR、PLTE、IDAT、IEND（與掃描停下處之後的原始位元組）交給 png crate，
//!   輔助 chunk 的語意全部由這裡決定。
//! - **libpng 相容檢查**：C++ 版的問題代碼有一部分來自 libpng 1.6.59 的警告與拒收
//!   （`metadata_parse_failed`、`decoder_warning`、`icc_rejected_by_decoder`），以及 libpng 實際採用的
//!   PLTE／tRNS。這裡依 libpng 的 chunk 規則（位置、重複、長度、iCCP／文字／eXIf／PLTE／tRNS 處理）
//!   重做一次同樣的檢查，見 docs/decisions.md D-R12。

use crate::inflate::{Inflated, ZInflater, ZState, inflate_capped};
use crate::input::Input;
use crate::limits::Limits;
use crate::report::{Count, IccInfo, Issue, code};
use crate::text::{hex, latin1_text, sha256_hex};

#[derive(Clone, Debug, Default)]
pub struct PngText {
    /// Latin-1 關鍵字轉成的 UTF-8（只供顯示）
    pub keyword: String,
    /// 檔案中的關鍵字位元組（雜湊用）
    pub keyword_raw: Vec<u8>,
    /// tEXt | zTXt | iTXt
    pub kind: &'static str,
    /// 解壓後文字內容的 SHA-256
    pub sha256: String,
    pub bytes: u64,
    /// present | parse_failed | over_limit
    pub status: &'static str,
}

/// libpng 讀到這個 chunk 時所處的階段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// `png_read_info`：第一個 IDAT 之前
    BeforeIdat,
    /// IDAT 串流之中
    InIdat,
    /// `png_read_end`：IDAT 串流之後
    AfterIdat,
}

/// libpng 會以錯誤停止的地方。
#[derive(Clone, Debug)]
pub struct LibFatal {
    pub phase: Phase,
    pub message: String,
    pub io: bool,
}

/// libpng 1.6.59（C++ 版 iat 的設定）對輔助 chunk 的處理結果。
#[derive(Clone, Debug, Default)]
pub struct Libpng {
    /// 採用的 PLTE（RGB 三元組）；調色盤影像截到 2^bit_depth 筆
    pub palette: Vec<u8>,
    /// 調色盤影像採用的 tRNS alpha
    pub trns_alpha: Vec<u8>,
    /// 灰階／RGB 採用的 tRNS 色鍵（灰階只用第一個值）
    pub trns_key: Option<[u16; 3]>,
    pub iccp_accepted: bool,
    /// 警告（`<chunk>: <訊息>`）與發生的階段
    pub warnings: Vec<(Phase, String)>,
    /// 第一個會讓 libpng 停止的錯誤
    pub fatal: Option<LibFatal>,
}

/// IDAT 串流的獨立檢查（解壓但不保留資料）。
#[derive(Clone, Copy, Debug)]
pub struct IdatStream {
    pub state: ZState,
    /// 解壓後的位元組數（含每列的濾波位元組）
    pub total_out: u64,
    /// 串流結束後，同一個 IDAT 裡還有剩下的壓縮資料
    pub extra_input: bool,
}

impl Default for IdatStream {
    fn default() -> Self {
        Self { state: ZState::Running, total_out: 0, extra_input: false }
    }
}

#[derive(Clone, Debug)]
pub struct PngScan {
    pub signature_ok: bool,
    /// valid | warning | corrupt | unverified
    pub container_status: &'static str,
    pub issues: Vec<Issue>,

    pub has_ihdr: bool,
    pub width: u32,
    pub height: u32,
    pub bit_depth: i32,
    pub color_type: i32,
    pub compression: i32,
    pub filter: i32,
    pub interlace: i32,
    pub has_plte: bool,
    pub has_trns: bool,
    pub animated: bool,

    /// 讀到 IEND 且 CRC 正確
    pub ended: bool,
    pub end_offset: u64,
    pub trailing_bytes: u64,
    pub idat_bytes: u64,
    pub idat_chunks: u32,
    pub bytes_scanned: u64,
    pub chunk_order: Vec<String>,

    pub cicp: Option<[u8; 4]>,
    pub srgb_intent: Option<u8>,
    pub gama: Option<u32>,
    pub chrm: Option<[u32; 8]>,
    pub sbit: Vec<u8>,
    pub icc: IccInfo,
    pub iccp_present: bool,
    pub mdcv_hex: Option<String>,
    pub clli_hex: Option<String>,

    pub exif: Option<Vec<u8>>,
    /// present | absent | over_limit
    pub exif_status: &'static str,
    pub phys: Option<String>,
    pub time: Option<String>,
    pub ornt: Option<i32>,
    pub texts: Vec<PngText>,
    pub other_ancillary: Vec<String>,
    /// valid | parse_failed | over_limit
    pub metadata_status: &'static str,

    /// 交給 png crate 的位元組範圍（檔案位移、長度），依序串接
    pub feed: Vec<(u64, u64)>,
    pub lib: Libpng,
    pub idat: IdatStream,
}

impl Default for PngScan {
    fn default() -> Self {
        Self {
            signature_ok: false,
            container_status: "unverified",
            issues: Vec::new(),
            has_ihdr: false,
            width: 0,
            height: 0,
            bit_depth: 0,
            color_type: -1,
            compression: -1,
            filter: -1,
            interlace: -1,
            has_plte: false,
            has_trns: false,
            animated: false,
            ended: false,
            end_offset: 0,
            trailing_bytes: 0,
            idat_bytes: 0,
            idat_chunks: 0,
            bytes_scanned: 0,
            chunk_order: Vec::new(),
            cicp: None,
            srgb_intent: None,
            gama: None,
            chrm: None,
            sbit: Vec::new(),
            icc: IccInfo::default(),
            iccp_present: false,
            mdcv_hex: None,
            clli_hex: None,
            exif: None,
            exif_status: "absent",
            phys: None,
            time: None,
            ornt: None,
            texts: Vec::new(),
            other_ancillary: Vec::new(),
            metadata_status: "valid",
            feed: Vec::new(),
            lib: Libpng::default(),
            idat: IdatStream::default(),
        }
    }
}

fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

fn valid_type(t: &[u8]) -> bool {
    t.iter().all(|c| c.is_ascii_alphabetic())
}

fn valid_ihdr_combo(depth: i32, ctype: i32) -> bool {
    match ctype {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    }
}

impl PngScan {
    fn issue(&mut self, c: &str, sev: &str, msg: impl Into<String>) {
        self.issues.push(Issue::new(c, sev, "", msg));
    }

    fn corrupt(&mut self, c: &str, msg: impl Into<String>) {
        self.container_status = "corrupt";
        self.issue(c, "error", msg);
    }
}

/// Exif 方向：1..8；沒有這個標籤時為 0；結構無法解析時為 -1（同 C++ 版）。
pub fn exif_orientation(e: &[u8]) -> i32 {
    let n = e.len();
    if n < 8 {
        return -1;
    }
    let le = match &e[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return -1,
    };
    let u16_at = |o: usize| -> Option<u32> {
        let b = e.get(o..o + 2)?;
        Some(u32::from(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }))
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = e.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    };
    if u16_at(2) != Some(42) {
        return -1;
    }
    let ifd = u64::from(u32_at(4).unwrap_or(0));
    if ifd + 2 > n as u64 {
        return -1;
    }
    let Some(count) = u16_at(ifd as usize) else { return -1 };
    if ifd + 2 + u64::from(count) * 12 > n as u64 {
        return -1;
    }
    for i in 0..u64::from(count) {
        let o = (ifd + 2 + i * 12) as usize;
        if u16_at(o) == Some(0x0112) {
            if u16_at(o + 2) != Some(3) || u32_at(o + 4) != Some(1) {
                return -1;
            }
            let v = u16_at(o + 8).unwrap_or(0);
            return if (1..=8).contains(&v) { v as i32 } else { -1 };
        }
    }
    0
}

// ------------------------------------------------------------ libpng 相容檢查

const MODE_IHDR: u8 = 1;
const MODE_PLTE: u8 = 2;
const MODE_IDAT: u8 = 4;
const MODE_AFTER_IDAT: u8 = 8;
/// libpng `LZ77Min`（2+5+4）
const LZ77_MIN: u32 = 11;
/// C++ 版 libpng 設定的文字 chunk 快取上限（PNG_USER_CHUNK_CACHE_MAX）
const CHUNK_CACHE_MAX: u32 = 1000;

#[derive(Clone, Copy)]
enum MaxLen {
    NoCheck,
    /// 不超過 png_chunk_max（C++ 版設為 metadata_item_bytes）
    Limit,
    Bytes(u32),
}

/// libpng 1.6.59 `read_chunks` 表：最大長度、最小長度、必須在哪些 chunk 之前、之後、可否重複。
fn chunk_rule(t: &[u8; 4]) -> Option<(MaxLen, u32, u8, u8, bool)> {
    use MaxLen::*;
    let col = MODE_PLTE | MODE_IDAT;
    let lkmin = 3 + LZ77_MIN;
    Some(match t {
        b"tRNS" => (Bytes(256), 0, MODE_IDAT, MODE_IHDR, false),
        b"cHRM" => (Bytes(32), 32, col, MODE_IHDR, false),
        b"gAMA" => (Bytes(4), 4, col, MODE_IHDR, false),
        b"iCCP" => (NoCheck, lkmin, col, MODE_IHDR, false),
        b"sBIT" => (Bytes(4), 1, col, MODE_IHDR, false),
        b"sRGB" => (Bytes(1), 1, col, MODE_IHDR, false),
        b"cICP" => (Bytes(4), 4, col, MODE_IHDR, false),
        b"mDCV" => (Bytes(24), 24, col, MODE_IHDR, false),
        b"eXIf" => (Limit, 4, 0, MODE_IHDR, false),
        b"cLLI" => (Bytes(8), 8, col, MODE_IHDR, false),
        b"tEXt" => (NoCheck, 2, 0, MODE_IHDR, true),
        b"zTXt" => (Limit, lkmin, 0, MODE_IHDR, true),
        b"iTXt" => (NoCheck, 6, 0, MODE_IHDR, true),
        b"bKGD" => (Bytes(6), 1, MODE_IDAT, MODE_IHDR, false),
        b"hIST" => (Bytes(1024), 0, MODE_IDAT, MODE_PLTE, false),
        b"pHYs" => (Bytes(9), 9, MODE_IDAT, MODE_IHDR, false),
        b"sPLT" => (NoCheck, 3, MODE_IDAT, MODE_IHDR, true),
        b"tIME" => (Bytes(7), 7, 0, MODE_IHDR, false),
        b"oFFs" => (Bytes(9), 9, MODE_IDAT, MODE_IHDR, false),
        b"pCAL" => (NoCheck, 14, MODE_IDAT, MODE_IHDR, false),
        b"sCAL" => (Limit, 4, MODE_IDAT, MODE_IHDR, false),
        _ => return None,
    })
}

/// zlib 結果對應的 libpng（zlib）訊息。
fn z_message(r: &Inflated) -> &'static str {
    match r {
        Inflated::Ok { .. } => "unexpected end of LZ stream",
        Inflated::Truncated => "truncated",
        Inflated::OverLimit => "insufficient memory",
        Inflated::Corrupt { adler: true } => "incorrect data check",
        Inflated::Corrupt { adler: false } => "damaged LZ stream",
    }
}

struct LibState {
    mode: u8,
    seen: Vec<[u8; 4]>,
    cache: u32,
    chunk_max: u64,
    color_type: i32,
    bit_depth: i32,
    num_palette: usize,
}

impl Libpng {
    fn warn(&mut self, phase: Phase, chunk: &[u8; 4], msg: &str) {
        self.warnings.push((phase, format!("{}: {msg}", String::from_utf8_lossy(chunk))));
    }

    fn fatal(&mut self, phase: Phase, msg: String, io: bool) {
        if self.fatal.is_none() {
            self.fatal = Some(LibFatal { phase, message: msg, io });
        }
    }
}

/// 一個完整（CRC 正確）的 chunk 交給 libpng 時的處理。`payload` 為 None 表示資料超過可檢查的大小。
fn libpng_chunk(lib: &mut Libpng, st: &mut LibState, phase: Phase, t: &[u8; 4], len: u32, payload: Option<&[u8]>) {
    if lib.fatal.is_some() {
        return;
    }
    let critical = t[0] & 0x20 == 0;
    match t {
        b"IHDR" => {
            st.mode |= MODE_IHDR;
            return;
        }
        b"IDAT" | b"IEND" => return,
        b"PLTE" => {
            libpng_plte(lib, st, phase, len, payload);
            return;
        }
        _ => {}
    }
    let Some((max_len, min_len, before, after, multiple)) = chunk_rule(t) else {
        // 未知的 chunk（含這個建置沒有支援的 APNG chunk）：輔助的直接略過，關鍵的是錯誤。
        if critical {
            lib.fatal(phase, format!("{}: unknown critical chunk", String::from_utf8_lossy(t)), false);
        }
        return;
    };
    let err = if st.mode & before != 0 || st.mode & after != after {
        Some("out of place")
    } else if !multiple && st.seen.contains(t) {
        Some("duplicate")
    } else if len < min_len {
        Some("too short")
    } else {
        match max_len {
            MaxLen::Limit if u64::from(len) > st.chunk_max => Some("length exceeds libpng limit"),
            MaxLen::Bytes(m) if len > m => Some("too long"),
            _ => None,
        }
    };
    if let Some(e) = err {
        lib.warn(phase, t, e);
        return;
    }
    let ok = match t {
        b"iCCP" => libpng_iccp(lib, st, phase, len, payload),
        b"tEXt" | b"zTXt" | b"iTXt" | b"sPLT" => libpng_text(lib, st, phase, t, len, payload),
        b"eXIf" => match payload {
            Some(p) if be32(p) == 0x4949_2A00 || be32(p) == 0x4D4D_002A => true,
            Some(_) => {
                lib.warn(phase, t, "invalid");
                false
            }
            None => true,
        },
        b"tRNS" => libpng_trns(lib, st, phase, len, payload),
        // 其餘 chunk 的內容檢查（cHRM、gAMA、sRGB…）沒有重做，視為接受。
        _ => true,
    };
    if ok {
        st.seen.push(*t);
    }
}

fn libpng_plte(lib: &mut Libpng, st: &mut LibState, phase: Phase, len: u32, payload: Option<&[u8]>) {
    let err = if st.mode & MODE_PLTE != 0 {
        Some("duplicate")
    } else if st.mode & MODE_IDAT != 0 {
        Some("out of place")
    } else if st.color_type & 2 == 0 {
        Some("ignored in grayscale PNG")
    } else if len > 3 * 256 || !len.is_multiple_of(3) {
        Some("invalid")
    } else if st.color_type != 3 && (st.seen.contains(b"tRNS") || st.seen.contains(b"bKGD")) {
        Some("out of place")
    } else {
        None
    };
    if let Some(e) = err {
        if st.color_type == 3 {
            lib.fatal(phase, format!("PLTE: {e}"), false);
        } else {
            lib.warn(phase, b"PLTE", e);
        }
        return;
    }
    let max = if st.color_type == 3 { 1usize << st.bit_depth } else { 256 };
    let num = ((len / 3) as usize).min(max);
    if let Some(p) = payload {
        lib.palette = p[..num * 3].to_vec();
    }
    st.num_palette = num;
    st.mode |= MODE_PLTE;
}

fn libpng_trns(lib: &mut Libpng, st: &LibState, phase: Phase, len: u32, payload: Option<&[u8]>) -> bool {
    let Some(p) = payload else { return false };
    let u16_at = |o: usize| u16::from_be_bytes([p[o], p[o + 1]]);
    match st.color_type {
        0 if len == 2 => lib.trns_key = Some([u16_at(0), 0, 0]),
        2 if len == 6 => lib.trns_key = Some([u16_at(0), u16_at(2), u16_at(4)]),
        0 | 2 => {
            lib.warn(phase, b"tRNS", "invalid");
            return false;
        }
        3 => {
            if st.mode & MODE_PLTE == 0 {
                lib.warn(phase, b"tRNS", "out of place");
                return false;
            }
            if len as usize > st.num_palette || len == 0 {
                lib.warn(phase, b"tRNS", "invalid");
                return false;
            }
            lib.trns_alpha = p.to_vec();
        }
        _ => {
            lib.warn(phase, b"tRNS", "invalid with alpha channel");
            return false;
        }
    }
    true
}

fn libpng_text(
    lib: &mut Libpng,
    st: &mut LibState,
    phase: Phase,
    t: &[u8; 4],
    len: u32,
    payload: Option<&[u8]>,
) -> bool {
    if st.cache != 0 {
        if st.cache == 1 {
            return false;
        }
        st.cache -= 1;
        if st.cache == 1 {
            lib.warn(phase, t, "no space in chunk cache");
            return false;
        }
    }
    if t == b"sPLT" {
        return true;
    }
    // tEXt／iTXt 配置 length+1，zTXt 配置 length（超過 png_chunk_max 時配置失敗）。
    let need = u64::from(len) + u64::from(t != b"zTXt");
    let Some(p) = payload.filter(|_| need <= st.chunk_max) else {
        lib.warn(phase, t, "out of memory");
        return false;
    };
    let kw = p.iter().position(|&b| b == 0).unwrap_or(p.len());
    if t == b"tEXt" {
        return true;
    }
    if !(1..=79).contains(&kw) {
        lib.warn(phase, t, "bad keyword");
        return false;
    }
    let limit = |prefix: usize| st.chunk_max.saturating_sub(prefix as u64 + 1);
    let err: Option<&str> = if t == b"zTXt" {
        if kw + 3 > p.len() {
            Some("truncated")
        } else if p[kw + 1] != 0 {
            Some("unknown compression type")
        } else {
            let r = inflate_capped(&p[kw + 2..], limit(kw + 2));
            match &r {
                Inflated::Ok { extra, .. } => {
                    if *extra > 0 {
                        lib.warn(phase, t, "extra compressed data");
                    }
                    None
                }
                _ => Some(z_message(&r)),
            }
        }
    } else {
        // iTXt：關鍵字、壓縮旗標、壓縮方法、語言\0、翻譯關鍵字\0、文字
        if kw + 5 > p.len() {
            Some("truncated")
        } else if p[kw + 1] == 0 || (p[kw + 1] == 1 && p[kw + 2] == 0) {
            let compressed = p[kw + 1] != 0;
            let mut q = kw + 3;
            while q < p.len() && p[q] != 0 {
                q += 1;
            }
            q += 1;
            while q < p.len() && p[q] != 0 {
                q += 1;
            }
            q += 1;
            if !compressed && q <= p.len() {
                None
            } else if compressed && q < p.len() {
                let r = inflate_capped(&p[q..], limit(q));
                match &r {
                    Inflated::Ok { extra, .. } => {
                        if *extra > 0 {
                            lib.warn(phase, t, "extra compressed data");
                        }
                        None
                    }
                    _ => Some(z_message(&r)),
                }
            } else {
                Some("truncated")
            }
        } else {
            Some("bad compression info")
        }
    };
    match err {
        Some(e) => {
            lib.warn(phase, t, e);
            false
        }
        None => true,
    }
}

/// `png_icc_profile_error` 的訊息格式。
fn icc_msg(name: &[u8], value: u32, reason: &str, is_sig: bool) -> String {
    let name = String::from_utf8_lossy(&name[..name.len().min(79)]);
    if is_sig {
        let b = value.to_be_bytes();
        let tag: String = b.iter().map(|&c| if (32..127).contains(&c) { c as char } else { '?' }).collect();
        format!("profile '{name}': '{tag}': {reason}")
    } else {
        format!("profile '{name}': {value:X}h: {reason}")
    }
}

fn is_icc_signature(v: u32) -> bool {
    v.to_be_bytes().iter().all(|&c| c.is_ascii_alphanumeric() || c == b' ')
}

fn libpng_iccp(lib: &mut Libpng, st: &LibState, phase: Phase, len: u32, payload: Option<&[u8]>) -> bool {
    // 超過可檢查大小的 iCCP 不重做檢查，視為接受（D-R12）。
    let Some(p) = payload else {
        lib.iccp_accepted = true;
        return true;
    };
    let t = b"iCCP";
    let read_len = (len as usize).min(81);
    if (len as usize - read_len) < LZ77_MIN as usize {
        lib.warn(phase, t, "too short");
        return false;
    }
    let kw = p[..read_len.min(80)].iter().position(|&b| b == 0).unwrap_or(read_len.min(80));
    if !(1..=79).contains(&kw) {
        lib.warn(phase, t, "bad keyword");
        return false;
    }
    if kw + 1 >= read_len || p[kw + 1] != 0 {
        lib.warn(phase, t, "bad compression method");
        return false;
    }
    let name = &p[..kw];
    // 依 libpng 的順序：先解壓 132 bytes 標頭，再到標籤表結尾，最後到整個 profile。
    // libpng 解壓到 profile 長度就停，之後的資料（包括 Adler-32）不再檢查。
    let mut r = IccReader::new(&p[kw + 2..]);
    let result = (|| -> Result<u32, String> {
        r.fill(132)?;
        let h = &r.out[..132];
        let plen = be32(h);
        if plen < 132 {
            return Err(icc_msg(name, plen, "too short", false));
        }
        if u64::from(plen) > st.chunk_max {
            return Err(icc_msg(name, plen, "profile too long", false));
        }
        let h = h.to_vec();
        if let Some(m) = icc_header_error(lib, phase, name, plen, &h, st.color_type) {
            return Err(m);
        }
        let tags = u64::from(be32(&h[128..]));
        r.fill(132 + 12 * tags)?;
        if let Some(m) = icc_tag_error(lib, phase, name, plen, &r.out) {
            return Err(m);
        }
        r.fill(u64::from(plen))?;
        Ok(plen)
    })();
    match result {
        Err(m) => {
            lib.warn(phase, t, &m);
            false
        }
        Ok(_) => {
            // 讀完 profile 後 chunk 裡還有沒讀進來的壓縮資料：libpng 只警告，仍然採用。
            // libpng 先把前 81 bytes 讀成關鍵字緩衝，之後以 1024 bytes 為單位讀取。
            let in_first = 81usize.saturating_sub(kw + 2);
            let rest_total = len as usize - read_len;
            let from_rest = r.used.saturating_sub(in_first);
            let read_from_rest = from_rest.div_ceil(1024).saturating_mul(1024).min(rest_total);
            if read_from_rest < rest_total {
                lib.warn(phase, t, "extra compressed data");
            }
            lib.iccp_accepted = true;
            true
        }
    }
}

/// 逐步解壓 iCCP 的 profile，保留全部已解出的資料。
struct IccReader<'a> {
    z: ZInflater,
    input: &'a [u8],
    used: usize,
    out: Vec<u8>,
}

impl<'a> IccReader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { z: ZInflater::new(), input, used: 0, out: Vec::new() }
    }

    /// 解壓到至少 `n` bytes；做不到時回傳 libpng（zlib）的訊息。
    fn fill(&mut self, n: u64) -> Result<(), String> {
        while (self.out.len() as u64) < n {
            if self.z.state() != ZState::Running {
                return Err(match self.z.state() {
                    ZState::Done => "unexpected end of LZ stream",
                    ZState::Adler => "incorrect data check",
                    ZState::Corrupt => "damaged LZ stream",
                    _ => "truncated",
                }
                .into());
            }
            let out = &mut self.out;
            let k = self.z.feed(&self.input[self.used..], false, &mut |b| {
                out.extend_from_slice(b);
                (out.len() as u64) < n
            });
            self.used += k;
        }
        Ok(())
    }
}

/// `png_icc_check_header`：回傳錯誤訊息；只是警告的情況記成警告並繼續。
fn icc_header_error(
    lib: &mut Libpng,
    phase: Phase,
    name: &[u8],
    plen: u32,
    h: &[u8],
    color_type: i32,
) -> Option<String> {
    let t = b"iCCP";
    let temp = be32(h);
    if temp != plen {
        return Some(icc_msg(name, temp, "length does not match profile", is_icc_signature(temp)));
    }
    if h[8] > 3 && plen & 3 != 0 {
        return Some(icc_msg(name, plen, "invalid length", false));
    }
    let tags = be32(&h[128..]);
    if tags > 357_913_930 || u64::from(plen) < 132 + 12 * u64::from(tags) {
        return Some(icc_msg(name, tags, "tag count too large", is_icc_signature(tags)));
    }
    let intent = be32(&h[64..]);
    if intent >= 0xffff {
        return Some(icc_msg(name, intent, "invalid rendering intent", is_icc_signature(intent)));
    }
    if intent >= 4 {
        lib.warn(phase, t, &icc_msg(name, intent, "intent outside defined range", is_icc_signature(intent)));
    }
    let sig = be32(&h[36..]);
    if sig != 0x6163_7370 {
        return Some(icc_msg(name, sig, "invalid signature", is_icc_signature(sig)));
    }
    const D50: [u8; 12] = [0x00, 0x00, 0xf6, 0xd6, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xd3, 0x2d];
    if h[68..80] != D50 {
        lib.warn(phase, t, &icc_msg(name, 0, "PCS illuminant is not D50", false));
    }
    let cs = be32(&h[16..]);
    match cs {
        0x5247_4220 if color_type & 2 == 0 => {
            return Some(icc_msg(name, cs, "RGB color space not permitted on grayscale PNG", true));
        }
        0x4752_4159 if color_type & 2 != 0 => {
            return Some(icc_msg(name, cs, "Gray color space not permitted on RGB PNG", true));
        }
        0x5247_4220 | 0x4752_4159 => {}
        _ => return Some(icc_msg(name, cs, "invalid ICC profile color space", is_icc_signature(cs))),
    }
    let class = be32(&h[12..]);
    match class {
        0x7363_6e72 | 0x6d6e_7472 | 0x7072_7472 | 0x7370_6163 => {}
        0x6162_7374 => return Some(icc_msg(name, class, "invalid embedded Abstract ICC profile", true)),
        0x6c69_6e6b => return Some(icc_msg(name, class, "unexpected DeviceLink ICC profile class", true)),
        0x6e6d_636c => lib.warn(phase, t, &icc_msg(name, class, "unexpected NamedColor ICC profile class", true)),
        _ => lib.warn(phase, t, &icc_msg(name, class, "unrecognized ICC profile class", is_icc_signature(class))),
    }
    let pcs = be32(&h[20..]);
    if pcs != 0x5859_5a20 && pcs != 0x4c61_6220 {
        return Some(icc_msg(name, pcs, "unexpected ICC PCS encoding", is_icc_signature(pcs)));
    }
    None
}

/// `png_icc_check_tag_table`。
fn icc_tag_error(lib: &mut Libpng, phase: Phase, name: &[u8], plen: u32, prof: &[u8]) -> Option<String> {
    let tags = be32(&prof[128..]) as usize;
    for i in 0..tags {
        let tag = &prof[132 + 12 * i..];
        let (id, start, len) = (be32(tag), be32(&tag[4..]), be32(&tag[8..]));
        if start > plen || len > plen - start {
            return Some(icc_msg(name, id, "ICC profile tag outside profile", is_icc_signature(id)));
        }
        if start & 3 != 0 {
            lib.warn(
                phase,
                b"iCCP",
                &icc_msg(name, id, "ICC profile tag start not a multiple of 4", is_icc_signature(id)),
            );
        }
    }
    None
}

// ------------------------------------------------------------ 掃描

/// 每次讀取的區塊大小（同 C++ 版 1 MiB）。
const IO_BLOCK: usize = 1 << 20;
/// libpng 讀 IDAT 的區塊大小（PNG_IDAT_READ_SIZE）：被截斷的最後一塊不會被使用。
pub const LIBPNG_IDAT_READ: u64 = 8192;
/// 不在 metadata 預算內、但仍需要內容做 libpng 相容檢查的 chunk，最多讀這麼多。
const LIB_CHECK_MAX: u64 = 64 << 20;

pub fn scan_png(f: &Input<'_>, limits: &Limits) -> PngScan {
    let mut s = PngScan::default();
    let file_size = f.size();
    let mut sig = [0u8; 8];
    match f.read_at(0, &mut sig) {
        Err(_) => {
            s.corrupt(code::STRUCTURE_ERROR, "I/O error reading signature");
            return s;
        }
        Ok(n) if n < 8 || sig != *b"\x89PNG\r\n\x1a\n" => {
            s.corrupt(code::UNKNOWN_FORMAT, "PNG signature not found");
            return s;
        }
        Ok(_) => {}
    }
    s.signature_ok = true;
    s.feed.push((0, 8));

    let mut st = LibState {
        mode: 0,
        seen: Vec::new(),
        cache: CHUNK_CACHE_MAX,
        chunk_max: limits.metadata_item_bytes(),
        color_type: -1,
        bit_depth: 0,
        num_palette: 0,
    };
    let mut idat_z = ZInflater::new();
    let mut pos: u64 = 8;
    let mut kept_total: u64 = 0;
    let (mut seen_idat, mut idat_closed) = (false, false);
    let mut last_type = String::new();
    let mut run = 0u32;
    let mut io = vec![0u8; IO_BLOCK];
    let push_order = |order: &mut Vec<String>, last: &mut String, run: &mut u32, t: &str| {
        if t == last.as_str() {
            *run += 1;
            return;
        }
        if !last.is_empty() {
            order.push(if *run > 1 { format!("{last} x{run}") } else { last.clone() });
        }
        *last = t.to_string();
        *run = 1;
    };
    // 依目前位置決定 libpng 所在的階段。
    let phase_of = |seen_idat: bool, is_idat: bool| {
        if is_idat {
            Phase::InIdat
        } else if seen_idat {
            Phase::AfterIdat
        } else {
            Phase::BeforeIdat
        }
    };

    loop {
        if pos == file_size {
            s.corrupt(code::MISSING_END_MARKER, "file ends before IEND");
            s.lib.fatal(phase_of(seen_idat, false), "unexpected end of file".into(), false);
            break;
        }
        let mut hdr = [0u8; 8];
        let got = match f.read_at(pos, &mut hdr) {
            Ok(n) => n,
            Err(_) => {
                s.corrupt(code::STRUCTURE_ERROR, "I/O error");
                s.lib.fatal(phase_of(seen_idat, false), "I/O error while reading input".into(), true);
                break;
            }
        };
        if got < 8 {
            s.corrupt(code::TRUNCATED, format!("chunk header truncated at offset {pos}"));
            s.lib.fatal(phase_of(seen_idat, false), "unexpected end of file".into(), false);
            s.feed.push((pos, got as u64));
            break;
        }
        let len = be32(&hdr);
        let t: [u8; 4] = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let ty = String::from_utf8_lossy(&t).into_owned();
        let is_idat = &t == b"IDAT";
        if len > 0x7FFF_FFFF || !valid_type(&t) {
            s.corrupt(code::STRUCTURE_ERROR, format!("invalid chunk header at offset {pos}"));
            s.lib.fatal(phase_of(seen_idat, false), "bad header (invalid chunk)".into(), false);
            break;
        }
        let chunk_end = pos + 12 + u64::from(len);
        // 串流在前面的 IDAT 就已經結束：這個 IDAT 的錯誤由 png_read_end 發現（after_image_data）。
        let idat_phase = if idat_z.state() == ZState::Running { Phase::InIdat } else { Phase::AfterIdat };
        if chunk_end > file_size {
            s.corrupt(code::TRUNCATED, format!("{ty} chunk truncated at offset {pos}"));
            let phase = if is_idat { idat_phase } else { phase_of(seen_idat, false) };
            s.lib.fatal(phase, "unexpected end of file".into(), false);
            if is_idat {
                // libpng 以 8192 bytes 為單位讀 IDAT；讀不滿的那一塊不會被使用。
                let avail = file_size - pos - 8;
                s.feed.push((pos, 8 + avail / LIBPNG_IDAT_READ * LIBPNG_IDAT_READ));
            }
            break;
        }
        if pos == 8 && &t != b"IHDR" {
            s.corrupt(code::STRUCTURE_ERROR, "first chunk is not IHDR");
            break;
        }
        push_order(&mut s.chunk_order, &mut last_type, &mut run, &ty);

        // 小的已知 chunk 保留內容；其他的只串流計算 CRC。
        let wanted = !is_idat && &t != b"IEND";
        let keep = wanted
            && u64::from(len) <= limits.metadata_item_bytes()
            && kept_total + u64::from(len) <= limits.metadata_file_bytes();
        // libpng 相容檢查需要內容的 chunk：預算外的也暫時讀進來（有上限）。
        let lib_needs = matches!(&t, b"iCCP" | b"zTXt" | b"iTXt" | b"tEXt" | b"eXIf" | b"tRNS" | b"PLTE");
        let capture = keep || (lib_needs && u64::from(len) <= LIB_CHECK_MAX);
        let mut payload: Vec<u8> = Vec::new();
        if capture {
            payload.reserve_exact(len as usize);
        }
        let mut crc = crc32fast::Hasher::new();
        crc.update(&t);
        let (mut off, mut left) = (pos + 8, u64::from(len));
        let mut io_fail = false;
        while left > 0 {
            let want = left.min(io.len() as u64) as usize;
            match f.read_at(off, &mut io[..want]) {
                Ok(n) if n == want => {}
                _ => {
                    io_fail = true;
                    break;
                }
            }
            crc.update(&io[..want]);
            if capture {
                payload.extend_from_slice(&io[..want]);
            }
            // libpng 只對「串流結束時，同一個 IDAT 還剩下資料」發出 Extra compressed data。
            if is_idat && idat_z.state() == ZState::Running {
                let used = idat_z.feed(&io[..want], true, &mut |_| true);
                if idat_z.state() == ZState::Done && (used < want || left > want as u64) {
                    s.idat.extra_input = true;
                }
            }
            off += want as u64;
            left -= want as u64;
        }
        let mut crcb = [0u8; 4];
        if io_fail || !matches!(f.read_at(off, &mut crcb), Ok(4)) {
            s.corrupt(code::STRUCTURE_ERROR, format!("I/O error inside {ty}"));
            s.lib.fatal(phase_of(seen_idat, is_idat), "I/O error while reading input".into(), true);
            break;
        }
        if be32(&crcb) != crc.finalize() {
            s.corrupt(code::CRC_ERROR, format!("{ty} CRC mismatch at offset {pos}"));
            let phase = if is_idat { idat_phase } else { phase_of(seen_idat, false) };
            s.lib.fatal(phase, format!("{ty}: CRC error"), false);
            if is_idat {
                // CRC 在整個 chunk 讀完後才檢查：資料照樣交給解碼器。
                s.feed.push((pos, 12 + u64::from(len)));
                s.idat_bytes += u64::from(len);
                s.idat_chunks += 1;
            }
            break;
        }
        if keep {
            kept_total += u64::from(len);
        }
        if wanted && !keep {
            s.metadata_status = "over_limit";
            s.issue(code::METADATA_OVER_LIMIT, "warning", format!("{ty} ({len} bytes) exceeds metadata budget"));
        }

        // IDAT 必須連續。
        if is_idat {
            if idat_closed {
                s.corrupt(code::STRUCTURE_ERROR, "IDAT chunks are not consecutive");
                // libpng 在 png_read_end 只當成良性錯誤（警告）。
                s.lib.warn(Phase::AfterIdat, b"IDAT", "Too many IDATs found");
                break;
            }
            if !seen_idat {
                st.mode |= MODE_IDAT;
            }
            seen_idat = true;
            s.idat_bytes += u64::from(len);
            s.idat_chunks += 1;
            s.feed.push((pos, 12 + u64::from(len)));
        } else if seen_idat && !idat_closed {
            idat_closed = true;
            st.mode |= MODE_AFTER_IDAT;
        }
        // 非調色盤影像不需要 PLTE，也不交給 png crate（libpng 對它只是警告）。
        if &t == b"IHDR" || &t == b"IEND" || (&t == b"PLTE" && s.color_type == 3) {
            s.feed.push((pos, 12 + u64::from(len)));
        }

        let p = &payload[..];
        let phase = phase_of(seen_idat, is_idat);
        match &t {
            b"IHDR" => {
                if s.has_ihdr || len != 13 {
                    s.corrupt(code::STRUCTURE_ERROR, "bad or repeated IHDR");
                    s.lib.fatal(Phase::BeforeIdat, "IHDR: invalid".into(), false);
                    break;
                }
                s.has_ihdr = true;
                s.width = be32(p);
                s.height = be32(&p[4..]);
                s.bit_depth = i32::from(p[8]);
                s.color_type = i32::from(p[9]);
                s.compression = i32::from(p[10]);
                s.filter = i32::from(p[11]);
                s.interlace = i32::from(p[12]);
                st.color_type = s.color_type;
                st.bit_depth = s.bit_depth;
                if s.width == 0
                    || s.height == 0
                    || s.width > 0x7FFF_FFFF
                    || s.height > 0x7FFF_FFFF
                    || !valid_ihdr_combo(s.bit_depth, s.color_type)
                    || s.compression != 0
                    || s.filter != 0
                    || s.interlace > 1
                {
                    s.corrupt(code::STRUCTURE_ERROR, "invalid IHDR field values");
                    s.lib.fatal(Phase::BeforeIdat, "IHDR: invalid".into(), false);
                    break;
                }
            }
            b"PLTE" => s.has_plte = true,
            b"tRNS" => s.has_trns = true,
            b"acTL" | b"fcTL" | b"fdAT" => s.animated = true,
            b"cICP" if keep => {
                if len == 4 {
                    s.cicp = Some([p[0], p[1], p[2], p[3]]);
                } else {
                    s.issue(code::METADATA_PARSE_FAILED, "warning", "cICP length invalid");
                }
            }
            b"sRGB" if keep => {
                if len == 1 {
                    s.srgb_intent = Some(p[0]);
                } else {
                    s.issue(code::METADATA_PARSE_FAILED, "warning", "sRGB length invalid");
                }
            }
            b"gAMA" if keep => {
                if len == 4 {
                    s.gama = Some(be32(p));
                } else {
                    s.issue(code::METADATA_PARSE_FAILED, "warning", "gAMA length invalid");
                }
            }
            b"cHRM" if keep => {
                if len == 32 {
                    let mut c = [0u32; 8];
                    for (i, v) in c.iter_mut().enumerate() {
                        *v = be32(&p[4 * i..]);
                    }
                    s.chrm = Some(c);
                } else {
                    s.issue(code::METADATA_PARSE_FAILED, "warning", "cHRM length invalid");
                }
            }
            b"sBIT" if keep => s.sbit = p.to_vec(),
            b"mDCV" if keep => s.mdcv_hex = Some(hex(p)),
            b"cLLI" if keep => s.clli_hex = Some(hex(p)),
            b"iCCP" => {
                s.iccp_present = true;
                if !keep {
                    s.icc.status = "over_budget".into();
                } else {
                    scan_iccp(&mut s, p, limits);
                }
            }
            b"eXIf" => {
                if keep {
                    s.exif = Some(p.to_vec());
                    s.exif_status = "present";
                } else {
                    s.exif_status = "over_limit";
                }
            }
            b"pHYs" if keep => {
                if len == 9 {
                    let unit = if p[8] == 1 { " per metre" } else { " (unit unknown)" };
                    s.phys = Some(format!("{}/{}{unit}", be32(p), be32(&p[4..])));
                } else {
                    s.issue(code::METADATA_PARSE_FAILED, "warning", "pHYs length invalid");
                }
            }
            b"tIME" if keep => {
                if len == 7 {
                    s.time = Some(format!(
                        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                        u32::from(p[0]) << 8 | u32::from(p[1]),
                        p[2],
                        p[3],
                        p[4],
                        p[5],
                        p[6]
                    ));
                } else {
                    s.issue(code::METADATA_PARSE_FAILED, "warning", "tIME length invalid");
                }
            }
            b"orNT" if keep => {
                if len == 1 {
                    s.ornt = Some(i32::from(p[0]));
                }
            }
            b"tEXt" | b"zTXt" | b"iTXt" if keep => scan_text(&mut s, &t, p, limits),
            _ if !is_idat
                && &t != b"IEND"
                && &t != b"IHDR"
                && hdr[4] & 0x20 != 0
                && !s.other_ancillary.contains(&ty) =>
            {
                s.other_ancillary.push(ty.clone());
            }
            _ => {}
        }
        let lib_payload = capture.then_some(p);
        libpng_chunk(&mut s.lib, &mut st, phase, &t, len, lib_payload);

        pos = chunk_end;
        if &t == b"IEND" {
            s.ended = true;
            s.end_offset = pos;
            break;
        }
    }
    if !last_type.is_empty() {
        s.chunk_order.push(if run > 1 { format!("{last_type} x{run}") } else { last_type });
    }
    s.bytes_scanned = pos;
    if seen_idat {
        // 串流在 IDAT 之後就沒有輸入了：最後再呼叫一次，確定是截斷還是已結束。
        if idat_z.state() == ZState::Running {
            idat_z.feed(&[], false, &mut |_| true);
        }
        s.idat.state = idat_z.state();
        s.idat.total_out = idat_z.total_out();
    }

    if s.ended {
        s.trailing_bytes = file_size - s.end_offset;
        if !s.has_ihdr || s.idat_chunks == 0 {
            s.corrupt(code::STRUCTURE_ERROR, "missing IHDR or IDAT");
        } else if s.color_type == 3 && !s.has_plte {
            s.corrupt(code::STRUCTURE_ERROR, "palette image without PLTE");
            s.lib.fatal(Phase::BeforeIdat, "Missing PLTE before IDAT".into(), false);
        }
        if s.container_status != "corrupt" {
            s.container_status = "valid";
            if s.trailing_bytes > 0 {
                s.container_status = "warning";
                let n = s.trailing_bytes;
                s.issue(code::TRAILING_DATA, "warning", format!("{n} bytes after IEND"));
            }
        }
    }
    s
}

fn scan_iccp(s: &mut PngScan, p: &[u8], limits: &Limits) {
    let nul = p.iter().position(|&b| b == 0);
    match nul {
        Some(n) if n + 2 <= p.len() && p[n + 1] == 0 => {
            s.icc.profile_name = match std::str::from_utf8(&p[..n]) {
                Ok(v) => v.to_string(),
                Err(_) => latin1_text(&p[..n]),
            };
            match inflate_capped(&p[n + 2..], limits.metadata_item_bytes()) {
                Inflated::Ok { data, .. } => {
                    s.icc.status = "embedded".into();
                    s.icc.bytes = Some(Count(data.len() as u64));
                    s.icc.sha256 = sha256_hex(&data);
                }
                r => {
                    s.icc.status = if matches!(r, Inflated::OverLimit) { "over_budget" } else { "unreadable" }.into();
                    let st = r.status();
                    s.issue(
                        code::METADATA_PARSE_FAILED,
                        "warning",
                        format!("iCCP profile could not be inflated: {st}"),
                    );
                }
            }
        }
        _ => {
            s.icc.status = "unreadable".into();
            s.issue(code::METADATA_PARSE_FAILED, "warning", "iCCP header invalid");
        }
    }
}

fn scan_text(s: &mut PngScan, t: &[u8; 4], p: &[u8], limits: &Limits) {
    let kind: &'static str = match t {
        b"tEXt" => "tEXt",
        b"zTXt" => "zTXt",
        _ => "iTXt",
    };
    let mut txt = PngText { kind, status: "present", ..PngText::default() };
    let cap = limits.metadata_item_bytes();
    match p.iter().position(|&b| b == 0) {
        None => txt.status = "parse_failed",
        Some(nul) => {
            txt.keyword_raw = p[..nul].to_vec();
            txt.keyword = latin1_text(&txt.keyword_raw);
            let mut q = nul + 1;
            let mut value: Vec<u8> = Vec::new();
            let mut st: &'static str = "ok";
            if kind == "tEXt" {
                value = p[q..].to_vec();
            } else if kind == "zTXt" {
                if q + 1 > p.len() || p[q] != 0 {
                    st = "corrupt";
                } else {
                    match inflate_capped(&p[q + 1..], cap) {
                        Inflated::Ok { data, .. } => value = data,
                        r => st = r.status(),
                    }
                }
            } else if q + 2 > p.len() {
                st = "corrupt";
            } else {
                // iTXt：旗標、方法、語言\0、翻譯關鍵字\0、文字
                let (flag, method) = (p[q], p[q + 1]);
                q += 2;
                for _ in 0..2 {
                    match p[q..].iter().position(|&b| b == 0) {
                        Some(z) => q += z + 1,
                        None => {
                            st = "corrupt";
                            break;
                        }
                    }
                }
                if st == "ok" {
                    if flag == 0 {
                        value = p[q..].to_vec();
                    } else if flag == 1 && method == 0 {
                        match inflate_capped(&p[q..], cap) {
                            Inflated::Ok { data, .. } => value = data,
                            r => st = r.status(),
                        }
                    } else {
                        st = "corrupt";
                    }
                }
            }
            if st == "ok" {
                txt.bytes = value.len() as u64;
                txt.sha256 = sha256_hex(&value);
            } else {
                txt.status = if st == "over_limit" { "over_limit" } else { "parse_failed" };
            }
        }
    }
    if txt.status == "parse_failed" {
        s.metadata_status = "parse_failed";
        let kw = txt.keyword.clone();
        s.issue(code::METADATA_PARSE_FAILED, "warning", format!("{kind} '{kw}' could not be decoded"));
    }
    s.texts.push(txt);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_orientation_values() {
        // MM, IFD at 8, one entry: Orientation SHORT count 1 value 6
        let mut e = b"MM\x00\x2a\x00\x00\x00\x08\x00\x01".to_vec();
        e.extend_from_slice(&[0x01, 0x12, 0x00, 0x03, 0, 0, 0, 1, 0x00, 0x06, 0, 0]);
        assert_eq!(exif_orientation(&e), 6);
        assert_eq!(exif_orientation(b"XX\x00\x2a\x00\x00\x00\x08"), -1);
        let mut none = b"II\x2a\x00\x08\x00\x00\x00\x00\x00".to_vec();
        none.extend_from_slice(&[0; 4]);
        assert_eq!(exif_orientation(&none), 0);
    }

    /// 最小的 ICC 標頭（無標籤）：色彩空間 `cs`，PCS XYZ，D50。
    fn iccp_payload(cs: &[u8; 4]) -> Vec<u8> {
        let mut h = vec![0u8; 132];
        h[..4].copy_from_slice(&132u32.to_be_bytes());
        h[12..16].copy_from_slice(b"mntr");
        h[16..20].copy_from_slice(cs);
        h[20..24].copy_from_slice(b"XYZ ");
        h[36..40].copy_from_slice(b"acsp");
        h[68..80].copy_from_slice(&[0x00, 0x00, 0xf6, 0xd6, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xd3, 0x2d]);
        let mut p = b"prof\0\0".to_vec();
        p.extend(miniz_oxide::deflate::compress_to_vec_zlib(&h, 6));
        p
    }

    fn check_iccp(cs: &[u8; 4], color_type: i32) -> Libpng {
        let mut lib = Libpng::default();
        let st = LibState {
            mode: MODE_IHDR,
            seen: Vec::new(),
            cache: CHUNK_CACHE_MAX,
            chunk_max: 8 << 20,
            color_type,
            bit_depth: 8,
            num_palette: 0,
        };
        let p = iccp_payload(cs);
        // libpng 要求 iCCP 至少 81 + LZ77Min bytes；短的測試資料補到夠長（補在壓縮資料之後）
        let mut padded = p.clone();
        padded.resize(p.len().max(100), 0);
        libpng_iccp(&mut lib, &st, Phase::BeforeIdat, padded.len() as u32, Some(&padded));
        lib
    }

    #[test]
    fn iccp_colour_space_must_match_colour_type() {
        let ok = check_iccp(b"RGB ", 2);
        assert!(ok.iccp_accepted);
        let bad = check_iccp(b"GRAY", 2);
        assert!(!bad.iccp_accepted);
        assert_eq!(bad.warnings[0].1, "iCCP: profile 'prof': 'GRAY': Gray color space not permitted on RGB PNG");
        assert!(!check_iccp(b"RGB ", 0).iccp_accepted);
        assert!(check_iccp(b"GRAY", 4).iccp_accepted);
    }

    #[test]
    fn icc_message_format() {
        assert_eq!(icc_msg(b"x", 0x6162_7374, "bad", true), "profile 'x': 'abst': bad");
        assert_eq!(icc_msg(b"x", 100, "too short", false), "profile 'x': 64h: too short");
    }
}
