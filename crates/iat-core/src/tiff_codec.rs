//! TIFF 的 strip／tile 解碼，照 C++ 版 1.0.0 使用的 libtiff 4.7.2 的行為：
//!
//! - PackBits：逐行移植 `tif_packbits.c` 的 `PackBitsDecode`，警告與錯誤一起。
//! - Deflate（8 與 32946）：miniz_oxide 解壓，照 `tif_zip.c` 的 `ZIPDecode`：輸出滿了就停，
//!   但和 zlib 一樣會繼續處理不需要輸出的部分（區塊結尾與 Adler-32）。
//! - LZW：weezl 解壓，照 `tif_lzw.c`：輸出滿了就停、不要求 EOI；舊式（位元反轉）碼另外處理。
//! - 水平差分預測（Predictor 2）與 FillOrder 2 的位元反轉。
//! - `TIFFOpenOptionsSetMaxSingleMemAlloc`／`MaxCumulatedMemAlloc` 的配置上限（[`Mem`]）。
//!
//! 錯誤與警告的文字照 libtiff 的格式（`模組: 訊息`），只用於報告。

use weezl::decode::Configuration;
use weezl::{BitOrder, LzwError, LzwStatus};

use crate::inflate::{ZInflater, ZState};
use crate::u64_to_usize;

/// 解碼器的錯誤與警告，各最多 32 則（同 C++ 版的 TiffIo）。
#[derive(Clone, Debug, Default)]
pub struct Messages {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// 配置上限被觸發過（libtiff 的「byte limit defined in open options」）
    pub limit_hit: bool,
}

const MAX_MESSAGES: usize = 32;

impl Messages {
    pub fn error(&mut self, module: &str, msg: &str) {
        if msg.contains("byte limit defined in open options") {
            self.limit_hit = true;
        }
        if self.errors.len() < MAX_MESSAGES {
            self.errors.push(format!("{module}: {msg}"));
        }
    }
    pub fn warning(&mut self, module: &str, msg: &str) {
        if self.warnings.len() < MAX_MESSAGES {
            self.warnings.push(format!("{module}: {msg}"));
        }
    }
}

/// libtiff 的配置上限（單筆與累計），以位元組計。只記帳，不真的配置。
#[derive(Clone, Debug)]
pub struct Mem {
    single: u64,
    cumulated: u64,
    current: u64,
}

impl Mem {
    pub fn new(single: u64, cumulated: u64) -> Self {
        Self { single, cumulated, current: 0 }
    }

    /// `_TIFFmallocExt`：超過上限時記錄 libtiff 的錯誤並回傳 false。
    pub fn alloc(&mut self, msgs: &mut Messages, module: &str, size: u64) -> bool {
        self.realloc(msgs, module, 0, size)
    }

    /// `_TIFFreallocExt`：old 是原本的大小（新配置為 0）。
    pub fn realloc(&mut self, msgs: &mut Messages, module: &str, old: u64, size: u64) -> bool {
        if self.single > 0 && size > self.single {
            msgs.error(
                module,
                &format!(
                    "Memory allocation of {size} bytes is beyond the {} byte limit defined in open options",
                    self.single
                ),
            );
            return false;
        }
        let base = self.current.saturating_sub(old);
        if self.cumulated > 0 && size > old && size > self.cumulated.saturating_sub(base) {
            msgs.error(
                module,
                &format!(
                    "Cumulated memory allocation of {} + {} bytes is beyond the {} cumulated byte limit defined in open options",
                    self.current,
                    size - old,
                    self.cumulated
                ),
            );
            return false;
        }
        self.current = base + size;
        true
    }

    pub fn free(&mut self, size: u64) {
        self.current = self.current.saturating_sub(size);
    }
}

/// `TIFFReverseBits`：FillOrder 2 時每個位元組的位元反轉。
pub fn reverse_bits(buf: &mut [u8]) {
    for b in buf {
        *b = b.reverse_bits();
    }
}

/// 16 位元樣本從檔案的位元組順序換成主機順序（x86-64 為小端）。
pub fn swab16_to_native(buf: &mut [u8], file_little_endian: bool) {
    if file_little_endian == cfg!(target_endian = "little") {
        return;
    }
    for p in buf.as_chunks_mut::<2>().0 {
        p.swap(0, 1);
    }
}

/// 水平差分的還原（`horAcc8`／`horAcc16`），樣本已是主機順序。`row_bytes` 是一列的位元組數
/// （tile 含右邊的填補），`stride` 是每個像素的樣本數。
pub fn undo_horizontal(buf: &mut [u8], row_bytes: usize, stride: usize, bits: u16) {
    if row_bytes == 0 {
        return;
    }
    for row in buf.chunks_exact_mut(row_bytes) {
        if bits == 16 {
            let mut vals: Vec<u16> = row.as_chunks::<2>().0.iter().map(|p| u16::from_ne_bytes(*p)).collect();
            for i in stride..vals.len() {
                vals[i] = vals[i].wrapping_add(vals[i - stride]);
            }
            for (p, v) in row.as_chunks_mut::<2>().0.iter_mut().zip(vals) {
                p.copy_from_slice(&v.to_ne_bytes());
            }
        } else {
            for i in stride..row.len() {
                row[i] = row[i].wrapping_add(row[i - stride]);
            }
        }
    }
}

/// `PackBitsDecode`：把 raw 解進 out（out 填滿為止）。`row` 是 libtiff 的 td_row（訊息用）。
pub fn packbits_decode(raw: &[u8], out: &mut [u8], row: u64, msgs: &mut Messages) -> bool {
    const MODULE: &str = "PackBitsDecode";
    let mut bp = 0usize;
    let mut op = 0usize;
    while bp < raw.len() && op < out.len() {
        let n = i64::from(raw[bp].cast_signed());
        bp += 1;
        // libtiff 的 tmsize_t 是有號數；slice 長度 ≤ isize::MAX，轉換不會失敗
        let occ = i64::try_from(out.len() - op).unwrap_or(i64::MAX);
        let cc = i64::try_from(raw.len() - bp).unwrap_or(i64::MAX);
        if n < 0 {
            if n == -128 {
                continue; // nop
            }
            let mut n = -n + 1;
            if occ < n {
                msgs.warning(MODULE, &format!("Discarding {} bytes to avoid buffer overrun", n - occ));
                n = occ;
            }
            if cc == 0 {
                msgs.warning(MODULE, "Terminating PackBitsDecode due to lack of data.");
                break;
            }
            let b = raw[bp];
            bp += 1;
            let n = u64_to_usize(n.cast_unsigned()); // 2 ≤ n 或 n = occ ≥ 1
            out[op..op + n].fill(b);
            op += n;
        } else {
            let mut n = n;
            if occ < n + 1 {
                msgs.warning(MODULE, &format!("Discarding {} bytes to avoid buffer overrun", n - occ + 1));
                n = occ - 1;
            }
            if cc < n + 1 {
                msgs.warning(MODULE, "Terminating PackBitsDecode due to lack of data.");
                break;
            }
            let k = u64_to_usize((n + 1).cast_unsigned()); // n ≥ 0
            out[op..op + k].copy_from_slice(&raw[bp..bp + k]);
            op += k;
            bp += k;
        }
    }
    if op < out.len() {
        out[op..].fill(0);
        msgs.error(MODULE, &format!("Not enough data for scanline {row}"));
        return false;
    }
    true
}

/// zlib 檔頭的檢查（同 zlib `inflate()` 的 HEAD 狀態），錯誤時回傳 zlib 的訊息。
/// `Ok(true)` 表示有預設字典（FDICT），zlib 會回傳 Z_NEED_DICT。
fn zlib_header(raw: &[u8]) -> Result<bool, &'static str> {
    let (Some(&cmf), Some(&flg)) = (raw.first(), raw.get(1)) else { return Err("") };
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err("incorrect header check");
    }
    if cmf & 0x0F != 8 {
        return Err("unknown compression method");
    }
    if (cmf >> 4) + 8 > 15 {
        return Err("invalid window size");
    }
    Ok(flg & 0x20 != 0)
}

/// `ZIPDecode`。`row` 是 libtiff 的 td_row（訊息用）。
pub fn zip_decode(raw: &[u8], out: &mut [u8], row: u64, msgs: &mut Messages) -> bool {
    const MODULE: &str = "ZIPDecode";
    let data_error = |msgs: &mut Messages, why: &str| {
        msgs.error(MODULE, &format!("Decoding error at scanline {row}, {why}"));
        false
    };
    match zlib_header(raw) {
        Err("") => {
            // 不到兩個位元組：zlib 沒有進展（Z_BUF_ERROR），訊息為空
            msgs.error(MODULE, "ZLib error: ");
            return false;
        }
        Err(why) => return data_error(msgs, why),
        Ok(true) => {
            msgs.error(MODULE, "ZLib error: ");
            return false;
        }
        Ok(false) => {}
    }
    let mut z = ZInflater::new();
    let mut filled = 0usize;
    z.feed(raw, false, &mut |b| {
        let room = out.len() - filled;
        let take = b.len().min(room);
        out[filled..filled + take].copy_from_slice(&b[..take]);
        filled += take;
        // 還有輸出空間時 zlib 會繼續；滿了之後再有輸出，zlib 早就停了
        b.len() <= room
    });
    let full = filled == out.len();
    match z.state() {
        ZState::Adler => data_error(msgs, "incorrect data check"),
        ZState::Corrupt => data_error(msgs, "invalid deflate data"),
        _ if full => true,
        ZState::Done => {
            out[filled..].fill(0);
            msgs.error(MODULE, &format!("Not enough data at scanline {row} (short {} bytes)", out.len() - filled));
            false
        }
        _ => {
            out[filled..].fill(0);
            msgs.error(MODULE, "ZLib error: ");
            false
        }
    }
}

/// 是否為舊式（位元反轉）LZW 碼：libtiff `LZWPreDecode` 的判斷。
pub fn lzw_is_old_style(raw: &[u8]) -> bool {
    raw.len() >= 2 && raw[0] == 0 && raw[1] & 1 != 0
}

/// `LZWDecode`／`LZWDecodeCompat`。`strip` 是 libtiff 的 td_curstrip（tile 時為 NOSTRIP），
/// `row` 是 td_row；兩者只用於訊息。
pub fn lzw_decode(raw: &[u8], out: &mut [u8], strip: u32, row: u64, msgs: &mut Messages) -> bool {
    let old = lzw_is_old_style(raw);
    let cfg =
        if old { Configuration::new(BitOrder::Lsb, 8) } else { Configuration::with_tiff_size_switch(BitOrder::Msb, 8) };
    let mut d = cfg.with_yield_on_full_buffer(true).build();
    let module = if old { "LZWDecodeCompat" } else { "LZWDecode" };
    let (mut pos, mut filled) = (0usize, 0usize);
    let not_enough = |msgs: &mut Messages, out: &mut [u8], filled: usize| {
        out[filled..].fill(0);
        msgs.error(module, &format!("Not enough data at scanline {row} (short {} bytes)", out.len() - filled));
        false
    };
    while filled < out.len() {
        let r = d.decode_bytes(&raw[pos..], &mut out[filled..]);
        pos += r.consumed_in;
        filled += r.consumed_out;
        if filled == out.len() {
            break;
        }
        match r.status {
            Err(LzwError::InvalidCode) => {
                out[filled..].fill(0);
                msgs.error("input", "Using code not yet in table");
                return false;
            }
            Ok(LzwStatus::Done) => return not_enough(msgs, out, filled),
            Ok(LzwStatus::NoProgress) => {
                if old {
                    // 舊式解碼器把用完的輸入當成 EOI（只發警告），之後回報資料不足
                    msgs.warning(module, &format!("LZWDecode: Strip {strip} not terminated with EOI code"));
                    return not_enough(msgs, out, filled);
                }
                out[filled..].fill(0);
                msgs.error(module, &format!("LZWDecode: Strip {strip} not terminated with EOI code"));
                return false;
            }
            Ok(LzwStatus::Ok) => {
                if r.consumed_in == 0 && r.consumed_out == 0 && pos >= raw.len() {
                    // 輸入用完卻沒有進展：同 NoProgress
                    if old {
                        msgs.warning(module, &format!("LZWDecode: Strip {strip} not terminated with EOI code"));
                        return not_enough(msgs, out, filled);
                    }
                    out[filled..].fill(0);
                    msgs.error(module, &format!("LZWDecode: Strip {strip} not terminated with EOI code"));
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_matches_libtiff() {
        let mut m = Messages::default();
        let mut out = [0u8; 6];
        // 字面 3 bytes + 重複 3 次
        assert!(packbits_decode(&[2, 1, 2, 3, 0xFE, 9], &mut out, 0, &mut m));
        assert_eq!(out, [1, 2, 3, 9, 9, 9]);
        assert!(m.warnings.is_empty());
        // 資料不足：警告 + 錯誤，剩下補 0
        let mut out = [7u8; 6];
        assert!(!packbits_decode(&[2, 1, 2], &mut out, 15, &mut m));
        assert_eq!(m.warnings, ["PackBitsDecode: Terminating PackBitsDecode due to lack of data."]);
        assert_eq!(m.errors, ["PackBitsDecode: Not enough data for scanline 15"]);
        assert_eq!(out, [0; 6]);
        // 溢出：丟掉多的部分，只發警告
        let mut m = Messages::default();
        let mut out = [0u8; 2];
        assert!(packbits_decode(&[0xFD, 5], &mut out, 0, &mut m));
        assert_eq!(out, [5, 5]);
        assert_eq!(m.warnings, ["PackBitsDecode: Discarding 2 bytes to avoid buffer overrun"]);
        // -128 是空操作
        let mut out = [0u8; 1];
        assert!(packbits_decode(&[0x80, 0, 4], &mut out, 0, &mut m));
        assert_eq!(out, [4]);
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
    }

    #[test]
    fn zip_like_libtiff() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let z = zlib(&data);
        let mut m = Messages::default();
        let mut out = vec![0u8; data.len()];
        assert!(zip_decode(&z, &mut out, 0, &mut m));
        assert_eq!(out, data);
        // 沒有 Adler-32：輸出滿了就算成功
        assert!(zip_decode(&z[..z.len() - 4], &mut out, 0, &mut m));
        // Adler-32 錯誤：在同一份輸入裡讀到結尾就會發現
        let mut bad = z.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(!zip_decode(&bad, &mut out, 25, &mut m));
        assert_eq!(
            m.errors.last().map(String::as_str),
            Some("ZIPDecode: Decoding error at scanline 25, incorrect data check")
        );
        // 資料不足（輸入用完）
        assert!(!zip_decode(&z[..z.len() / 2], &mut out, 0, &mut m));
        assert_eq!(m.errors.last().map(String::as_str), Some("ZIPDecode: ZLib error: "));
        // 串流比需要的短
        let mut big = vec![0u8; data.len() + 10];
        assert!(!zip_decode(&z, &mut big, 3, &mut m));
        assert_eq!(
            m.errors.last().map(String::as_str),
            Some("ZIPDecode: Not enough data at scanline 3 (short 10 bytes)")
        );
        // 檔頭錯誤
        assert!(!zip_decode(&[0x12, 0x34, 0, 0], &mut out, 0, &mut m));
        assert_eq!(
            m.errors.last().map(String::as_str),
            Some("ZIPDecode: Decoding error at scanline 0, incorrect header check")
        );
        assert!(!zip_decode(&[0x78], &mut out, 0, &mut m));
        // 需要的比串流短：多的輸出被丟掉
        let mut small = vec![0u8; 10];
        assert!(zip_decode(&z, &mut small, 0, &mut m));
        assert_eq!(small, data[..10]);
    }

    #[test]
    fn lzw_like_libtiff() {
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 97) as u8).collect();
        let enc = weezl::encode::Encoder::with_tiff_size_switch(BitOrder::Msb, 8).encode(&data).expect("encode");
        let mut m = Messages::default();
        let mut out = vec![0u8; data.len()];
        assert!(lzw_decode(&enc, &mut out, 0, 0, &mut m));
        assert_eq!(out, data);
        // 只要前面一段：不要求 EOI
        let mut part = vec![0u8; 100];
        assert!(lzw_decode(&enc, &mut part, 0, 0, &mut m));
        assert_eq!(part, data[..100]);
        // 輸入不足
        assert!(!lzw_decode(&enc[..enc.len() / 2], &mut out, 3, 12, &mut m));
        assert_eq!(
            m.errors.last().map(String::as_str),
            Some("LZWDecode: LZWDecode: Strip 3 not terminated with EOI code")
        );
        // EOI 太早
        let mut big = vec![0u8; data.len() + 5];
        assert!(!lzw_decode(&enc, &mut big, 0, 4, &mut m));
        assert_eq!(
            m.errors.last().map(String::as_str),
            Some("LZWDecode: Not enough data at scanline 4 (short 5 bytes)")
        );
    }

    #[test]
    fn predictor_and_bits() {
        let mut b = vec![1u8, 2, 1, 1, 1, 1];
        undo_horizontal(&mut b, 6, 2, 8);
        assert_eq!(b, [1, 2, 2, 3, 3, 4]);
        let mut w: Vec<u8> = [1u16, 0xFFFF, 2].iter().flat_map(|v| v.to_ne_bytes()).collect();
        undo_horizontal(&mut w, 6, 1, 16);
        let v: Vec<u16> = w.as_chunks::<2>().0.iter().map(|p| u16::from_ne_bytes(*p)).collect();
        assert_eq!(v, [1, 0, 2]);
        let mut r = [0b1000_0001u8, 0b0000_0010];
        reverse_bits(&mut r);
        assert_eq!(r, [0b1000_0001, 0b0100_0000]);
    }

    #[test]
    fn mem_limits() {
        let mut m = Messages::default();
        let mut mem = Mem::new(100, 150);
        assert!(mem.alloc(&mut m, "x", 100));
        assert!(!mem.alloc(&mut m, "x", 101));
        assert!(m.limit_hit);
        assert!(!mem.alloc(&mut m, "x", 60));
        assert!(mem.realloc(&mut m, "x", 100, 90));
        assert!(mem.alloc(&mut m, "x", 60));
        mem.free(u64::MAX);
        assert!(mem.alloc(&mut m, "x", 100));
    }
}
