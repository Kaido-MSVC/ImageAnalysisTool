//! 唯讀輸入檔的定位讀取（對應 C++ 版 `ReadOnlyFile::read_at`），並累計讀取量。

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt;

pub struct Input<'a> {
    file: &'a File,
    size: u64,
    read: Cell<u64>,
}

impl<'a> Input<'a> {
    pub fn new(file: &'a File) -> Self {
        let size = file.metadata().map_or(0, |m| m.len());
        Self { file, size, read: Cell::new(0) }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn bytes_read(&self) -> u64 {
        self.read.get()
    }

    /// 從 `off` 讀到 `buf` 填滿或到檔尾為止，回傳實際讀到的位元組數。
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match self.file.seek_read(&mut buf[n..], off + n as u64) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // Windows 在檔尾之後讀取會回報 ERROR_HANDLE_EOF (38)
                Err(e) if e.raw_os_error() == Some(38) => break,
                Err(e) => return Err(e),
            }
        }
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}
