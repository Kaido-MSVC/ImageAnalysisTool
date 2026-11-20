//! 暫存空間（對應 C++ 版 `src/engine/temp_store.*` 與 `src/core/run.cpp` 的暫存檔部分）。
//!
//! 每次分析在暫存目錄建立兩個檔案 `iat-tmp-<run id>-a.bin`／`-b.bin`（每側一個）：
//! CREATE_NEW、不共用、FILE_FLAG_DELETE_ON_CLOSE，最後一個 handle 關閉時由檔案系統刪除，
//! 程序被終止時也一樣。規則與 C++ 版相同：
//! - 開始解碼前先向配額與剩餘空間預留全部大小並預先配置，空間不足一開始就回報；
//! - 附加模式：依序寫入，只能讀回已寫入的範圍，不會把沒寫過的洞讀成 0。
//!
//! 預先配置用 `File::set_len`（C++ 版用 FileAllocationInfo，不改檔案長度）；
//! 讀取範圍一律以已寫入的長度檢查，所以檔尾的 0 不會被讀到（docs/decisions.md D-R04）。

use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::win;

const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
const FILE_ATTRIBUTE_NOT_CONTENT_INDEXED: u32 = 0x2000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
const ERROR_HANDLE_DISK_FULL: u32 = 39;
const ERROR_DISK_FULL: u32 = 112;
const ERROR_DISK_QUOTA_EXCEEDED: u32 = 1295;
const TEMP_PREFIX: &str = "iat-tmp-";

/// 永遠保留的剩餘空間。
pub const DISK_MARGIN: u64 = 64 << 20;
/// 附加模式的寫入緩衝。
pub const WRITE_BUFFER: usize = 1 << 20;

fn is_space_error(e: u32) -> bool {
    matches!(e, ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL | ERROR_DISK_QUOTA_EXCEEDED)
}

fn ms(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}

pub struct TempQuota {
    limit: u64,
    free: u64,
    reserved: u64,
}

impl TempQuota {
    pub fn new(limit_bytes: u64, free_disk_bytes: u64) -> Self {
        Self { limit: limit_bytes, free: free_disk_bytes, reserved: 0 }
    }

    /// 成功時為 None，否則為 [`TempErrorKind::Quota`] 或 [`TempErrorKind::Disk`]。
    fn take(&mut self, n: u64) -> Option<TempErrorKind> {
        let Some(total) = self.reserved.checked_add(n).filter(|&t| t <= self.limit) else {
            return Some(TempErrorKind::Quota);
        };
        if total.checked_add(DISK_MARGIN).is_none_or(|t| t > self.free) {
            return Some(TempErrorKind::Disk);
        }
        self.reserved = total;
        None
    }

    pub fn reserved(&self) -> u64 {
        self.reserved
    }
}

/// 暫存失敗的種類；Display 是報告裡 `temp_<kind>` 原因的字尾。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempErrorKind {
    /// 超過 temp_gib 配額
    Quota,
    /// 暫存磁碟區空間不足
    Disk,
    /// 讀寫暫存檔失敗
    Io,
    /// 沒有提供暫存檔
    Unavailable,
    /// 工具內部的不一致（超出預留、讀未寫入的範圍等）
    Internal,
}

impl TempErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Disk => "disk",
            Self::Io => "io",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for TempErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct TempError {
    pub kind: TempErrorKind,
    pub message: String,
}

impl TempError {
    pub fn new(kind: TempErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl fmt::Display for TempError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TempError {}

/// 一側的暫存檔（附加模式）。
pub struct TempFile<'a> {
    file: Option<&'a File>,
    reserved: u64,
    written: u64,
    on_disk: u64,
    read: u64,
    io_ms: f64,
    buf: Vec<u8>,
}

impl<'a> TempFile<'a> {
    pub fn new(file: Option<&'a File>) -> Self {
        Self { file, reserved: 0, written: 0, on_disk: 0, read: 0, io_ms: 0.0, buf: Vec::new() }
    }

    pub fn written(&self) -> u64 {
        self.written
    }
    pub fn bytes_read(&self) -> u64 {
        self.read
    }
    pub fn io_ms(&self) -> f64 {
        self.io_ms
    }

    /// 向配額與剩餘空間預留，再預先配置。
    pub fn reserve(&mut self, quota: &mut TempQuota, bytes: u64) -> Result<(), TempError> {
        let Some(f) = self.file else {
            return Err(TempError::new(TempErrorKind::Unavailable, "no temporary file was provided to the worker"));
        };
        if self.reserved != 0 {
            return Err(TempError::new(TempErrorKind::Internal, "temporary file reserved twice"));
        }
        if let Some(k) = quota.take(bytes) {
            let msg = if k == TempErrorKind::Quota {
                format!(
                    "temporary storage needs {bytes} bytes, temp_gib allows {} (already reserved {}; raise with --limits temp_gib=)",
                    quota.limit, quota.reserved
                )
            } else {
                format!(
                    "temporary storage needs {bytes} bytes but the temp volume has {} bytes free (margin {DISK_MARGIN}; choose another volume with --temp-dir)",
                    quota.free
                )
            };
            return Err(TempError::new(k, msg));
        }
        let t0 = Instant::now();
        if bytes > 0
            && let Err(e) = f.set_len(bytes)
        {
            let c = win::os_code(&e);
            self.io_ms += ms(t0);
            let kind = if is_space_error(c) { TempErrorKind::Disk } else { TempErrorKind::Io };
            return Err(TempError::new(
                kind,
                format!("preallocating temporary storage failed: {}", win::error_text(c)),
            ));
        }
        self.io_ms += ms(t0);
        self.reserved = bytes;
        self.buf.reserve(WRITE_BUFFER);
        Ok(())
    }

    fn write_raw(&mut self, off: u64, data: &[u8]) -> Result<(), TempError> {
        let f = self.file.expect("reserve() checked the file");
        let t0 = Instant::now();
        let mut done = 0;
        while done < data.len() {
            match f.seek_write(&data[done..], off + done as u64) {
                Ok(0) => {
                    return Err(TempError::new(TempErrorKind::Io, "writing temporary storage failed: 0 bytes written"));
                }
                Ok(n) => done += n,
                Err(e) => {
                    let c = win::os_code(&e);
                    self.io_ms += ms(t0);
                    let kind = if is_space_error(c) { TempErrorKind::Disk } else { TempErrorKind::Io };
                    return Err(TempError::new(
                        kind,
                        format!("writing temporary storage failed: {}", win::error_text(c)),
                    ));
                }
            }
        }
        self.io_ms += ms(t0);
        Ok(())
    }

    pub fn append(&mut self, data: &[u8]) -> Result<(), TempError> {
        if self.written.checked_add(data.len() as u64).is_none_or(|e| e > self.reserved) {
            return Err(TempError::new(TempErrorKind::Internal, "temporary write beyond the reserved size"));
        }
        let mut p = data;
        while !p.is_empty() {
            let take = p.len().min(WRITE_BUFFER - self.buf.len());
            self.buf.extend_from_slice(&p[..take]);
            p = &p[take..];
            self.written += take as u64;
            if self.buf.len() == WRITE_BUFFER {
                self.flush()?;
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), TempError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buf);
        let r = self.write_raw(self.on_disk, &buf);
        if r.is_ok() {
            self.on_disk += buf.len() as u64;
        }
        self.buf = buf;
        self.buf.clear();
        r
    }

    /// 只能讀已經寫到磁碟上的範圍。
    pub fn read_at(&mut self, off: u64, data: &mut [u8]) -> Result<(), TempError> {
        if off.checked_add(data.len() as u64).is_none_or(|e| e > self.on_disk) {
            return Err(TempError::new(TempErrorKind::Internal, "temporary read outside the written range"));
        }
        let f = self.file.expect("data was written, so the file exists");
        let t0 = Instant::now();
        let mut done = 0;
        while done < data.len() {
            match f.seek_read(&mut data[done..], off + done as u64) {
                Ok(0) => {
                    self.io_ms += ms(t0);
                    return Err(TempError::new(
                        TempErrorKind::Io,
                        "reading temporary storage failed: unexpected end of file",
                    ));
                }
                Ok(n) => done += n,
                Err(e) => {
                    self.io_ms += ms(t0);
                    return Err(TempError::new(
                        TempErrorKind::Io,
                        format!("reading temporary storage failed: {}", win::error_text(win::os_code(&e))),
                    ));
                }
            }
        }
        self.read += data.len() as u64;
        self.io_ms += ms(t0);
        Ok(())
    }
}

// ------------------------------------------------------------ 每次執行的暫存檔

/// 本工具自己建立的檔名：`iat-tmp-<UUID>-a.bin`／`-b.bin`（UUID 為 8-4-4-4-12 十六進位）。
fn is_own_temp_name(n: &str) -> bool {
    let Some(rest) = n.strip_prefix(TEMP_PREFIX) else { return false };
    if rest.len() != 36 + 6 || !rest.is_ascii() {
        return false;
    }
    let (id, tail) = rest.split_at(36);
    (tail == "-a.bin" || tail == "-b.bin")
        && id.bytes().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// 刪除先前執行留下的暫存檔（只在作業系統當機或斷電時才會留下）。
/// 仍被執行中的另一個實例開著的檔案不共用，刪除會失敗而保留。
pub fn remove_stale(dir: &Path) -> u32 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_own_temp_name(name) || e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// 這次執行的兩個暫存檔。
pub struct RunTemp {
    pub paths: [PathBuf; 2],
    pub files: [Option<File>; 2],
}

impl RunTemp {
    pub fn create(dir: &Path, run_id: &str) -> Result<Self, String> {
        let mut t = Self { paths: [PathBuf::new(), PathBuf::new()], files: [None, None] };
        for (i, side) in ["a", "b"].into_iter().enumerate() {
            let p = dir.join(format!("{TEMP_PREFIX}{run_id}-{side}.bin"));
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .share_mode(0)
                .attributes(FILE_ATTRIBUTE_TEMPORARY | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
                .open(&p)
                .map_err(|e| format!("cannot create a temporary file: {}", win::error_text(win::os_code(&e))))?;
            t.paths[i] = p;
            t.files[i] = Some(f);
        }
        Ok(t)
    }

    /// 關閉 handle（檔案隨之刪除）後仍然存在的路徑。
    pub fn close(&mut self) -> Vec<PathBuf> {
        for f in &mut self.files {
            *f = None;
        }
        self.paths.iter().filter(|p| !p.as_os_str().is_empty() && win::path_exists(p)).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_names_only() {
        assert!(is_own_temp_name("iat-tmp-0123abcd-0123-4abc-8def-0123456789ab-a.bin"));
        assert!(is_own_temp_name("iat-tmp-0123ABCD-0123-4abc-8def-0123456789ab-b.bin"));
        assert!(!is_own_temp_name("iat-tmp-0123abcd-0123-4abc-8def-0123456789ab-c.bin"));
        assert!(!is_own_temp_name("iat-tmp-0123abcd_0123-4abc-8def-0123456789ab-a.bin"));
        assert!(!is_own_temp_name("iat-tmp-notes.bin"));
    }

    #[test]
    fn quota_and_append_mode() {
        let dir = std::env::temp_dir();
        let id = "00000000-0000-4000-8000-0000000000aa";
        let mut rt = RunTemp::create(&dir, id).unwrap();
        // 不共用：同一路徑再建立一次必須失敗
        assert!(RunTemp::create(&dir, id).is_err());
        {
            let mut q = TempQuota::new(10, u64::MAX);
            let mut t = TempFile::new(rt.files[0].as_ref());
            assert_eq!(t.reserve(&mut q, 11).unwrap_err().kind, TempErrorKind::Quota);
            t.reserve(&mut q, 6).unwrap();
            t.append(b"abc").unwrap();
            let mut b = [0u8; 3];
            // 還在緩衝裡，尚未寫到磁碟：不能讀
            assert_eq!(t.read_at(0, &mut b).unwrap_err().kind, TempErrorKind::Internal);
            t.flush().unwrap();
            t.read_at(0, &mut b).unwrap();
            assert_eq!(&b, b"abc");
            assert_eq!(t.read_at(1, &mut b).unwrap_err().kind, TempErrorKind::Internal);
            assert_eq!(t.append(b"defg").unwrap_err().kind, TempErrorKind::Internal);
        }
        assert!(rt.close().is_empty(), "delete-on-close removed both files");
    }
}
