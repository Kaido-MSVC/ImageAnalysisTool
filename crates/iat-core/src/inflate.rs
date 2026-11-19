//! zlib 串流解壓（miniz_oxide 核心），用在 PNG 的 iCCP／zTXt／iTXt 與 IDAT 串流檢查。
//!
//! 和 miniz_oxide 的 `stream::inflate` 不同的是這裡保留解壓器的原始狀態，
//! 能區分 Adler-32 不符、資料損壞與輸入不足（截斷）。

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::inflate_flags::{
    TINFL_FLAG_COMPUTE_ADLER32, TINFL_FLAG_HAS_MORE_INPUT, TINFL_FLAG_PARSE_ZLIB_HEADER,
};
use miniz_oxide::inflate::core::{DecompressorOxide, TINFL_LZ_DICT_SIZE, decompress};

/// 解壓器目前的狀態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZState {
    /// 還沒到串流結尾
    Running,
    /// 串流結束，Adler-32 相符
    Done,
    /// 輸入用完仍未到結尾
    Truncated,
    /// 解壓到結尾，但 Adler-32 不符
    Adler,
    /// 標頭或 deflate 資料損壞
    Corrupt,
}

pub struct ZInflater {
    d: Box<DecompressorOxide>,
    win: Box<[u8]>,
    pos: usize,
    state: ZState,
    total_out: u64,
}

impl Default for ZInflater {
    fn default() -> Self {
        Self::new()
    }
}

impl ZInflater {
    pub fn new() -> Self {
        Self {
            d: Box::default(),
            win: vec![0u8; TINFL_LZ_DICT_SIZE].into_boxed_slice(),
            pos: 0,
            state: ZState::Running,
            total_out: 0,
        }
    }

    pub fn state(&self) -> ZState {
        self.state
    }

    pub fn total_out(&self) -> u64 {
        self.total_out
    }

    /// 送入一段壓縮資料。`more` 表示之後還有資料；最後一段傳 false，
    /// 此時仍未到結尾就是截斷。輸出依序交給 `sink`，`sink` 回傳 false 時停止（例如超過上限）。
    /// 回傳這段輸入用掉的位元組數；串流結束後其餘的輸入不會被用到。
    pub fn feed(&mut self, input: &[u8], more: bool, sink: &mut dyn FnMut(&[u8]) -> bool) -> usize {
        let mut used = 0;
        while self.state == ZState::Running {
            let mut flags = TINFL_FLAG_PARSE_ZLIB_HEADER | TINFL_FLAG_COMPUTE_ADLER32;
            if more {
                flags |= TINFL_FLAG_HAS_MORE_INPUT;
            }
            let (st, n_in, n_out) = decompress(&mut self.d, &input[used..], &mut self.win, self.pos, flags);
            used += n_in;
            if n_out > 0 {
                self.total_out += n_out as u64;
                let keep_going = sink(&self.win[self.pos..self.pos + n_out]);
                self.pos = (self.pos + n_out) & (TINFL_LZ_DICT_SIZE - 1);
                if !keep_going {
                    return used;
                }
            }
            match st {
                TINFLStatus::Done => self.state = ZState::Done,
                TINFLStatus::HasMoreOutput => {}
                TINFLStatus::NeedsMoreInput => {
                    if !more {
                        self.state = ZState::Truncated;
                    }
                    return used;
                }
                TINFLStatus::FailedCannotMakeProgress => self.state = ZState::Truncated,
                TINFLStatus::Adler32Mismatch => self.state = ZState::Adler,
                _ => self.state = ZState::Corrupt,
            }
        }
        used
    }
}

/// 一次解壓整段資料，輸出超過 `cap` 時停止。
pub enum Inflated {
    /// 到達串流結尾；`extra` 為結尾之後剩下的輸入位元組數
    Ok {
        data: Vec<u8>,
        extra: usize,
    },
    OverLimit,
    Truncated,
    /// `adler`：其餘資料完好，只有 Adler-32 不符
    Corrupt {
        adler: bool,
    },
}

impl Inflated {
    /// C++ 版 `inflate_capped` 的狀態字串。
    pub fn status(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::OverLimit => "over_limit",
            Self::Truncated => "truncated",
            Self::Corrupt { .. } => "corrupt",
        }
    }
}

pub fn inflate_capped(data: &[u8], cap: u64) -> Inflated {
    let mut z = ZInflater::new();
    let mut out = Vec::new();
    let mut over = false;
    let used = z.feed(data, false, &mut |b| {
        if out.len() as u64 + b.len() as u64 > cap {
            over = true;
            return false;
        }
        out.extend_from_slice(b);
        true
    });
    if over {
        return Inflated::OverLimit;
    }
    match z.state() {
        ZState::Done => Inflated::Ok { data: out, extra: data.len() - used },
        ZState::Truncated | ZState::Running => Inflated::Truncated,
        ZState::Adler => Inflated::Corrupt { adler: true },
        ZState::Corrupt => Inflated::Corrupt { adler: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // zlib.compress(b"Hello, zlib!")
    const HELLO: [u8; 20] = [120, 156, 243, 72, 205, 201, 201, 215, 81, 168, 202, 201, 76, 82, 4, 0, 27, 101, 4, 19];

    #[test]
    fn statuses() {
        match inflate_capped(&HELLO, 100) {
            Inflated::Ok { data, extra } => {
                assert_eq!(data, b"Hello, zlib!");
                assert_eq!(extra, 0);
            }
            _ => panic!("expected ok"),
        }
        assert!(matches!(inflate_capped(&HELLO[..10], 100), Inflated::Truncated));
        assert!(matches!(inflate_capped(&HELLO, 5), Inflated::OverLimit));
        let mut bad = HELLO;
        bad[19] ^= 1;
        assert!(matches!(inflate_capped(&bad, 100), Inflated::Corrupt { adler: true }));
        let mut extra = HELLO.to_vec();
        extra.extend_from_slice(b"xyz");
        assert!(matches!(inflate_capped(&extra, 100), Inflated::Ok { extra: 3, .. }));
        assert!(matches!(inflate_capped(b"\x78\x9c\xff\xff\xff", 100), Inflated::Corrupt { adler: false }));
    }

    #[test]
    fn streaming_across_pieces() {
        let mut z = ZInflater::new();
        let mut out = Vec::new();
        for (i, piece) in HELLO.chunks(3).enumerate() {
            let more = (i + 1) * 3 < HELLO.len();
            z.feed(piece, more, &mut |b| {
                out.extend_from_slice(b);
                true
            });
        }
        assert_eq!(z.state(), ZState::Done);
        assert_eq!(out, b"Hello, zlib!");
    }
}
