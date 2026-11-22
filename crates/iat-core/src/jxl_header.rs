//! JPEG XL codestream 標頭（SizeHeader 與 ImageMetadata，ISO/IEC 18181-1）的最小解析器，讀到色彩空間為止。
//!
//! 只用來取得 jxl crate 0.8.0 沒有公開的資訊：extra channel 的位元深度（C++ 版用 libjxl 的
//! `alpha_bits`／`alpha_exponent_bits` 判斷「alpha 深度與色彩深度不同」）與色彩聲道數（D-R18）。
//! 其餘標頭資訊仍以解碼器為準。輸入不可信：所有讀取都檢查剩餘位元，不會 panic。

/// 讀到結尾（標頭不完整）或欄位值不合法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderError;

/// LSB 優先的位元讀取器（JPEG XL 的位元順序）。
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Result<u64, HeaderError> {
        let mut v = 0u64;
        for i in 0..n {
            let byte = *self.data.get(self.pos / 8).ok_or(HeaderError)?;
            v |= u64::from((byte >> (self.pos % 8)) & 1) << i;
            self.pos += 1;
        }
        Ok(v)
    }
    /// 讀 `n` ≤ 32 位元並回傳 u32。
    #[expect(clippy::cast_possible_truncation, reason = "n ≤ 32 位元，值放得進 u32")]
    fn read_u32(&mut self, n: usize) -> Result<u32, HeaderError> {
        debug_assert!(n <= 32, "read_u32 reads at most 32 bits, got {n}");
        self.read(n).map(|v| v as u32)
    }
    fn flag(&mut self) -> Result<bool, HeaderError> {
        Ok(self.read(1)? == 1)
    }
}

/// U32 的一個分支。
#[derive(Clone, Copy)]
enum D {
    Val(u32),
    Bits(usize),
    Off(usize, u32),
}

fn u32c(b: &mut Bits<'_>, d: [D; 4]) -> Result<u32, HeaderError> {
    let sel = b.read_u32(2)? as usize;
    match d.get(sel).copied().ok_or(HeaderError)? {
        D::Val(v) => Ok(v),
        D::Bits(n) => b.read_u32(n),
        D::Off(n, off) => b.read_u32(n)?.checked_add(off).ok_or(HeaderError),
    }
}

/// 列舉值的編碼：U32(0, 1, 2 + 4 bits, 18 + 6 bits)。
fn enum_value(b: &mut Bits<'_>) -> Result<u32, HeaderError> {
    u32c(b, [D::Val(0), D::Val(1), D::Off(4, 2), D::Off(6, 18)])
}

/// 樣本深度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BitDepth {
    pub bits: u32,
    /// 整數樣本為 0
    pub exponent_bits: u32,
}

fn bit_depth(b: &mut Bits<'_>) -> Result<BitDepth, HeaderError> {
    let float = b.flag()?;
    if float {
        let bits = u32c(b, [D::Val(32), D::Val(16), D::Val(24), D::Off(6, 1)])?;
        let exponent_bits = b.read_u32(4)? + 1;
        Ok(BitDepth { bits, exponent_bits })
    } else {
        Ok(BitDepth { bits: u32c(b, [D::Val(8), D::Val(10), D::Val(12), D::Off(6, 1)])?, exponent_bits: 0 })
    }
}

/// extra channel 的類型代碼：0 alpha、1 depth、2 spot color …（ExtraChannelInfo.type）。
pub const EC_ALPHA: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtraChannel {
    pub kind: u32,
    pub depth: BitDepth,
    pub alpha_associated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub orientation: u32,
    pub have_animation: bool,
    pub depth: BitDepth,
    pub extra: Vec<ExtraChannel>,
    pub xyb_encoded: bool,
    pub want_icc: bool,
    /// 0 RGB、1 灰階、2 XYB、3 未知
    pub color_space: u32,
}

impl Header {
    /// libjxl 的 `num_color_channels`：灰階 1，其餘 3。
    pub fn color_channels(&self) -> u32 {
        if self.color_space == 1 { 1 } else { 3 }
    }
    /// 第一個 alpha extra channel（libjxl 的 `alpha_bits` 等取自它）。
    pub fn alpha(&self) -> Option<&ExtraChannel> {
        self.extra.iter().find(|e| e.kind == EC_ALPHA)
    }
}

fn size_dim(b: &mut Bits<'_>, small: bool) -> Result<u32, HeaderError> {
    if small {
        Ok((b.read_u32(5)? + 1) * 8)
    } else {
        u32c(b, [D::Bits(9), D::Bits(13), D::Bits(18), D::Bits(30)])?.checked_add(1).ok_or(HeaderError)
    }
}

fn aspect(ratio: u64, y: u32) -> Option<u32> {
    let y = u64::from(y);
    let x = match ratio {
        1 => y,
        2 => y * 12 / 10,
        3 => y * 4 / 3,
        4 => y * 3 / 2,
        5 => y * 16 / 9,
        6 => y * 5 / 4,
        7 => y * 2,
        _ => return None,
    };
    u32::try_from(x).ok()
}

/// SizeHeader（或 intrinsic size）：回傳 (寬, 高)。
fn size_header(b: &mut Bits<'_>) -> Result<(u32, u32), HeaderError> {
    let small = b.flag()?;
    let y = size_dim(b, small)?;
    let ratio = b.read(3)?;
    let x = if ratio == 0 { size_dim(b, small)? } else { aspect(ratio, y).ok_or(HeaderError)? };
    Ok((x, y))
}

fn preview_header(b: &mut Bits<'_>) -> Result<(), HeaderError> {
    let div8 = b.flag()?;
    let dim = |b: &mut Bits<'_>| -> Result<u32, HeaderError> {
        if div8 {
            u32c(b, [D::Val(16), D::Val(32), D::Off(5, 1), D::Off(9, 33)])
        } else {
            u32c(b, [D::Bits(6), D::Off(8, 64), D::Off(10, 320), D::Off(12, 1344)])
        }
    };
    dim(b)?;
    if b.read(3)? == 0 {
        dim(b)?;
    }
    Ok(())
}

fn extra_channel(b: &mut Bits<'_>) -> Result<ExtraChannel, HeaderError> {
    if b.flag()? {
        // all_default：8-bit 的 straight alpha
        return Ok(ExtraChannel {
            kind: EC_ALPHA,
            depth: BitDepth { bits: 8, exponent_bits: 0 },
            alpha_associated: false,
        });
    }
    let kind = enum_value(b)?;
    let depth = bit_depth(b)?;
    u32c(b, [D::Val(0), D::Val(3), D::Val(4), D::Off(3, 1)])?; // dim_shift
    let name_len = u32c(b, [D::Val(0), D::Bits(4), D::Off(5, 16), D::Off(10, 48)])?;
    for _ in 0..name_len {
        b.read(8)?;
    }
    let mut alpha_associated = false;
    match kind {
        0 => alpha_associated = b.flag()?,
        2 => {
            for _ in 0..4 {
                b.read(16)?; // spot colour, f16
            }
        }
        5 => {
            u32c(b, [D::Val(1), D::Bits(2), D::Off(4, 3), D::Off(8, 19)])?; // CFA channel
        }
        _ => {}
    }
    Ok(ExtraChannel { kind, depth, alpha_associated })
}

/// 從 codestream 開頭（FF 0A 之後的位元組也一起給）解析到色彩空間。
///
/// # Errors
///
/// 位元組不足或欄位不合法時回傳 [`HeaderError`]。
pub fn parse(codestream: &[u8]) -> Result<Header, HeaderError> {
    if codestream.get(..2) != Some(&[0xFF, 0x0A]) {
        return Err(HeaderError);
    }
    let mut b = Bits { data: codestream, pos: 16 };
    let (width, height) = size_header(&mut b)?;
    let mut h = Header {
        width,
        height,
        orientation: 1,
        have_animation: false,
        depth: BitDepth { bits: 8, exponent_bits: 0 },
        extra: Vec::new(),
        xyb_encoded: true,
        want_icc: false,
        color_space: 0,
    };
    if b.flag()? {
        return Ok(h); // ImageMetadata.all_default
    }
    if b.flag()? {
        // extra_fields
        h.orientation = b.read_u32(3)? + 1;
        if b.flag()? {
            size_header(&mut b)?; // intrinsic size
        }
        if b.flag()? {
            preview_header(&mut b)?;
        }
        if b.flag()? {
            h.have_animation = true;
            u32c(&mut b, [D::Val(100), D::Val(1000), D::Off(10, 1), D::Off(30, 1)])?;
            u32c(&mut b, [D::Val(1), D::Val(1001), D::Off(8, 1), D::Off(10, 1)])?;
            u32c(&mut b, [D::Val(0), D::Bits(3), D::Bits(16), D::Bits(32)])?;
            b.flag()?; // have_timecodes
        }
    }
    h.depth = bit_depth(&mut b)?;
    b.flag()?; // modular_16_bit_buffer_sufficient
    let n = u32c(&mut b, [D::Val(0), D::Val(1), D::Off(4, 2), D::Off(12, 1)])?;
    for _ in 0..n {
        h.extra.push(extra_channel(&mut b)?);
    }
    h.xyb_encoded = b.flag()?;
    if !b.flag()? {
        // ColorEncoding 不是全預設
        h.want_icc = b.flag()?;
        h.color_space = enum_value(&mut b)?;
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LSB 優先寫入位元，組出測試用的 codestream 開頭。
    #[derive(Default)]
    struct W {
        bytes: Vec<u8>,
        bit: usize,
    }
    impl W {
        fn put(&mut self, v: u64, n: usize) -> &mut Self {
            for i in 0..n {
                if self.bit.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= (((v >> i) & 1) as u8) << (self.bit % 8);
                self.bit += 1;
            }
            self
        }
    }

    fn start() -> W {
        let mut w = W::default();
        w.put(0x0AFF, 16);
        // SizeHeader：small，高 2×8，比例 1:1
        w.put(1, 1).put(1, 5).put(1, 3);
        w
    }

    #[test]
    fn all_default_metadata() {
        let mut w = start();
        w.put(1, 1);
        let h = parse(&w.bytes).expect("header");
        assert_eq!((h.width, h.height, h.depth.bits, h.color_channels()), (16, 16, 8, 3));
        assert!(h.alpha().is_none());
    }

    #[test]
    fn alpha_depth_and_gray() {
        let mut w = start();
        w.put(0, 1).put(1, 1); // 不是全預設；extra_fields
        w.put(5, 3).put(0, 1).put(0, 1).put(0, 1); // orientation 6，沒有 intrinsic、preview、動畫
        w.put(0, 1).put(2, 2); // 整數 12-bit
        w.put(1, 1); // modular_16_bit
        w.put(1, 2); // 一個 extra channel
        w.put(0, 1).put(0, 2); // 不是全預設；類型 alpha
        w.put(0, 1).put(0, 2); // 8-bit
        w.put(0, 2).put(0, 2); // dim_shift 0、名稱長度 0
        w.put(1, 1); // associated
        w.put(0, 1); // 不是 XYB
        w.put(0, 1).put(0, 1).put(1, 2); // ColorEncoding：不是全預設、不要 ICC、灰階
        let h = parse(&w.bytes).expect("header");
        assert_eq!(h.orientation, 6);
        assert_eq!(h.depth, BitDepth { bits: 12, exponent_bits: 0 });
        let a = h.alpha().expect("alpha");
        assert_eq!((a.depth.bits, a.alpha_associated), (8, true));
        assert_eq!(h.color_channels(), 1);
        assert!(!h.xyb_encoded);
    }

    #[test]
    fn truncated_or_garbage_is_an_error() {
        assert_eq!(parse(&[]), Err(HeaderError));
        assert_eq!(parse(&[0xFF, 0x0A]), Err(HeaderError));
        assert_eq!(parse(&[0x00, 0x0A, 0, 0]), Err(HeaderError));
        // extra channel 數最大（4096），名稱長度最大：位元不足時回報錯誤而不是配置大量記憶體
        let mut w = start();
        w.put(0, 1).put(0, 1).put(0, 1).put(0, 2).put(1, 1).put(3, 2).put(4095, 12);
        assert_eq!(parse(&w.bytes), Err(HeaderError));
    }
}
