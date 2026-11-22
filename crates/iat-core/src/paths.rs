//! 路徑工具，行為對照 C++ 版 `win_util.cpp` 的同名函式。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// GetFullPathNameW 的結果（std::path::absolute 在 Windows 上就是呼叫它）；失敗時原樣傳回。
pub fn full_path(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 最後一個 `\` 或 `/` 之後的部分（不做正規化，與 C++ 版相同）。
pub fn file_name_of(p: &Path) -> String {
    let s = p.as_os_str().to_string_lossy();
    match s.rfind(['\\', '/']) {
        Some(k) => s[k + 1..].to_string(),
        None => s.into_owned(),
    }
}

/// 完整路徑的上層目錄；沒有分隔符時為 `.`。
pub fn parent_dir_of(p: &Path) -> PathBuf {
    let f = full_path(p);
    let s = f.as_os_str().to_string_lossy();
    match s.rfind(['\\', '/']) {
        Some(k) => PathBuf::from(&s[..k]),
        None => PathBuf::from("."),
    }
}

pub fn is_directory(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_dir())
}

/// 兩個路徑的完整路徑不分大小寫相同。
pub fn same_path(a: &Path, b: &Path) -> bool {
    let (fa, fb): (OsString, OsString) = (full_path(a).into(), full_path(b).into());
    crate::win::same_path_text(&fa, &fb)
}

/// 去掉結尾的分隔符，但保留磁碟根目錄（例如 `C:\`）。
pub fn trim_dir(p: PathBuf) -> PathBuf {
    let mut s = p.as_os_str().to_string_lossy().into_owned();
    while s.len() > 3 && (s.ends_with('\\') || s.ends_with('/')) {
        s.pop();
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_trimming() {
        assert_eq!(file_name_of(Path::new(r"C:\a b\中文 檔.png")), "中文 檔.png");
        assert_eq!(file_name_of(Path::new("x/y.bin")), "y.bin");
        assert_eq!(trim_dir(PathBuf::from(r"C:\Temp\")), PathBuf::from(r"C:\Temp"));
        assert_eq!(trim_dir(PathBuf::from(r"C:\")), PathBuf::from(r"C:\"));
    }
}
