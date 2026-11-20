//! 資源上限（對應 C++ 版 `src/common/limits.hpp`）。每個上限都是有限值，沒有「無上限」。

use std::fmt;

use crate::report::LimitSettings;

/// `--limits name=value` 的解析錯誤；Display 的文字與 C++ 版相同。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitError {
    /// 沒有 `=`
    Syntax(String),
    /// 值不是 1..12 位數的十進位整數
    NotPositiveInteger(String),
    /// 不認得的上限名稱
    UnknownName(String),
    /// 超出允許範圍
    OutOfRange { name: String, min: u64, max: u64 },
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(a) => write!(f, "expected name=value: {a}"),
            Self::NotPositiveInteger(a) => write!(f, "value must be a positive integer: {a}"),
            Self::UnknownName(n) => write!(f, "unknown limit: {n}"),
            Self::OutOfRange { name, min, max } => write!(f, "{name} must be between {min} and {max}"),
        }
    }
}

impl std::error::Error for LimitError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// worker 的 Job commit 上限（Rust 版目前只記錄，見 docs/decisions.md）
    pub job_commit_mib: u64,
    /// 工具自管的列／帶緩衝（兩側合計）
    pub buffer_mib: u64,
    /// 單一 metadata 項目（例如解壓後的 ICC）
    pub metadata_item_mib: u64,
    /// 單檔保留的 metadata 總量
    pub metadata_file_mib: u64,
    /// 暫存空間
    pub temp_gib: u64,
    /// 整次執行的牆鐘期限
    pub deadline_s: u64,
    /// 僅供測試：整圖型解碼器預估值的百分比，100 為正常。只能由隱藏旗標修改。
    pub test_codec_estimate_pct: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            job_commit_mib: 1024,
            buffer_mib: 64,
            metadata_item_mib: 8,
            metadata_file_mib: 32,
            temp_gib: 8,
            deadline_s: 600,
            test_codec_estimate_pct: 100,
        }
    }
}

/// 整圖型解碼器的硬上限保留量（D10）。
pub const CODEC_RESERVE: u64 = 64 << 20;

impl Limits {
    /// 解析 `name=value`。
    ///
    /// # Errors
    ///
    /// 格式、數值或名稱不對，或超出範圍時回傳 [`LimitError`]。
    pub fn set(&mut self, assignment: &str) -> Result<(), LimitError> {
        let Some((name, val)) = assignment.split_once('=') else {
            return Err(LimitError::Syntax(assignment.into()));
        };
        if val.is_empty() || val.len() > 12 || !val.bytes().all(|b| b.is_ascii_digit()) {
            return Err(LimitError::NotPositiveInteger(assignment.into()));
        }
        let v: u64 = val.parse().map_err(|_| LimitError::NotPositiveInteger(assignment.into()))?;
        let (field, lo, hi): (&mut u64, u64, u64) = match name {
            "job_commit_mib" => (&mut self.job_commit_mib, 16, 1 << 20),
            "buffer_mib" => (&mut self.buffer_mib, 1, 1 << 16),
            "metadata_item_mib" => (&mut self.metadata_item_mib, 1, 1024),
            "metadata_file_mib" => (&mut self.metadata_file_mib, 1, 4096),
            "temp_gib" => (&mut self.temp_gib, 1, 1 << 16),
            "deadline_s" => (&mut self.deadline_s, 1, 7 * 24 * 3600),
            _ => return Err(LimitError::UnknownName(name.into())),
        };
        if v < lo || v > hi {
            return Err(LimitError::OutOfRange { name: name.into(), min: lo, max: hi });
        }
        *field = v;
        Ok(())
    }

    pub fn buffer_bytes(&self) -> u64 {
        self.buffer_mib << 20
    }
    pub fn metadata_item_bytes(&self) -> u64 {
        self.metadata_item_mib << 20
    }
    pub fn metadata_file_bytes(&self) -> u64 {
        self.metadata_file_mib << 20
    }
    /// Job commit 上限減去固定保留量（D10）。
    pub fn codec_cap_bytes(&self) -> u64 {
        (self.job_commit_mib << 20).saturating_sub(CODEC_RESERVE)
    }

    pub fn settings(&self) -> LimitSettings {
        LimitSettings {
            job_commit_mib: self.job_commit_mib,
            buffer_mib: self.buffer_mib,
            metadata_item_mib: self.metadata_item_mib,
            metadata_file_mib: self.metadata_file_mib,
            temp_gib: self.temp_gib,
            deadline_s: self.deadline_s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_accepts_range_and_rejects_with_cpp_messages() {
        let mut l = Limits::default();
        assert_eq!(l.set("buffer_mib=8"), Ok(()));
        assert_eq!(l.buffer_mib, 8);
        assert_eq!(l.set("deadline_s=0").unwrap_err().to_string(), "deadline_s must be between 1 and 604800");
        assert_eq!(l.set("buffer_mib").unwrap_err().to_string(), "expected name=value: buffer_mib");
        assert_eq!(l.set("buffer_mib=-1").unwrap_err().to_string(), "value must be a positive integer: buffer_mib=-1");
        assert_eq!(
            l.set("buffer_mib=1234567890123").unwrap_err().to_string(),
            "value must be a positive integer: buffer_mib=1234567890123"
        );
        assert_eq!(l.set("foo=1").unwrap_err().to_string(), "unknown limit: foo");
        assert_eq!(
            l.set("job_commit_mib=15").unwrap_err().to_string(),
            "job_commit_mib must be between 16 and 1048576"
        );
    }

    #[test]
    fn codec_cap_never_underflows() {
        let l = Limits { job_commit_mib: 16, ..Limits::default() };
        assert_eq!(l.codec_cap_bytes(), 0);
        assert_eq!(Limits::default().codec_cap_bytes(), (1024 - 64) << 20);
    }
}
