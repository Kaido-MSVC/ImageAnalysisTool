//! 唯讀輸入檔的定位讀取（對應 C++ 版 `ReadOnlyFile::read_at`），並累計讀取量。

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt;

/// 格式判定讀取的檔頭長度（與 C++ 版 `sniff_format` 相同）。
pub const HEAD_LEN: usize = 64;

pub struct Input<'a> {
    file: &'a File,
    size: u64,
    head: [u8; HEAD_LEN],
    head_len: usize,
    read: Cell<u64>,
}

impl<'a> Input<'a> {
    /// 取得檔案大小並讀取檔頭；任何一步的 I/O 錯誤都回傳給呼叫者（開檔階段統一報錯）。
    ///
    /// # Errors
    ///
    /// 查詢檔案大小或讀取檔頭失敗時回傳該 I/O 錯誤。
    pub fn new(file: &'a File) -> io::Result<Self> {
        let size = file.metadata()?.len();
        let mut input = Self { file, size, head: [0; HEAD_LEN], head_len: 0, read: Cell::new(0) };
        let mut head = [0u8; HEAD_LEN];
        input.head_len = input.read_at(0, &mut head)?;
        input.head = head;
        Ok(input)
    }

    pub fn file(&self) -> &'a File {
        self.file
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// 檔頭（最多 [`HEAD_LEN`] bytes，檔案較短時為整個檔案）。
    pub fn head(&self) -> &[u8] {
        &self.head[..self.head_len]
    }

    pub fn bytes_read(&self) -> u64 {
        self.read.get()
    }

    /// 從 `off` 讀到 `buf` 填滿或到檔尾為止，回傳實際讀到的位元組數（計入讀取量）。
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let n = read_full_at(self.file, off, buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

/// 從 `off` 讀到 `buf` 填滿或到檔尾為止，不計入讀取量（外層雜湊用）。
///
/// 一律指定位移讀取：Windows 的 `seek_read` 會移動檔案指標，不能和循序讀取混用。
pub(crate) fn read_full_at(file: &File, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.seek_read(&mut buf[n..], off + n as u64) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // Windows 在檔尾之後讀取會回報 ERROR_HANDLE_EOF (38)
            Err(e) if e.raw_os_error() == Some(38) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_is_read_at_open_and_counted() {
        let dir = std::env::temp_dir().join("iat-core-input-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("short.bin");
        std::fs::write(&p, b"abcde").unwrap();
        let f = File::open(&p).unwrap();
        let input = Input::new(&f).unwrap();
        assert_eq!(input.size(), 5);
        assert_eq!(input.head(), b"abcde");
        assert_eq!(input.bytes_read(), 5);
        drop(f);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
