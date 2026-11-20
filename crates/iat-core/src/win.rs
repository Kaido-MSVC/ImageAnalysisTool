//! Win32 包裝。這是 iat-core 裡唯一允許 unsafe 的模組（AV1 包裝在獨立的 iat-av1 crate），理由見
//! docs/decisions.md D-R03：英文系統錯誤訊息、檔案識別碼、不取代既有檔的改名、
//! 系統亂數與主控台中斷處理，std 都沒有提供安全且行為相同的介面。
//!
//! 每個函式只做一次 FFI 呼叫加上必要的轉換，對外的介面全部是安全的。
#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use windows_sys::Win32::Foundation::{ERROR_SHARING_VIOLATION, GetLastError};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_128, FILE_ID_INFO, FileIdInfo, GetDiskFreeSpaceExW, GetFileAttributesW, GetFileInformationByHandleEx,
    INVALID_FILE_ATTRIBUTES, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// `"<code>: <英文系統訊息>"`，與 C++ 版 `win_error_text` 相同（固定用 en-US，不跟系統語言）。
pub fn error_text(code: u32) -> String {
    const LANG_EN_US: u32 = 0x0409;
    let mut buf = [0u16; 1024];
    // SAFETY: buf 是可寫的 1024 個 u16；未使用 ALLOCATE_BUFFER，也不傳插入參數。
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            std::ptr::null(),
            code,
            LANG_EN_US,
            buf.as_mut_ptr(),
            buf.len() as u32,
            std::ptr::null(),
        )
    } as usize;
    let msg = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    format!("{code}: {}", msg.trim_end_matches(['\n', '\r', ' ']))
}

/// io::Error 的 Win32 錯誤碼；沒有時用 ERROR_GEN_FAILURE (31)。
pub fn os_code(e: &std::io::Error) -> u32 {
    e.raw_os_error().map_or(31, |c| c as u32)
}

/// 磁碟區序號加 128 位元檔案 ID；硬連結與同一檔案的不同路徑會得到相同值。
pub fn file_identity(f: &File) -> Option<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO { VolumeSerialNumber: 0, FileId: FILE_ID_128 { Identifier: [0; 16] } };
    // SAFETY: handle 來自仍開啟的 File；info 的大小與 FileIdInfo 類別相符。
    let ok = unsafe {
        GetFileInformationByHandleEx(
            f.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    (ok != 0).then_some((info.VolumeSerialNumber, info.FileId.Identifier))
}

/// 呼叫者可用的剩餘空間（bytes）。
pub fn disk_free_bytes(dir: &Path) -> Result<u64, u32> {
    let w = wide(dir.as_os_str());
    let mut avail = 0u64;
    // SAFETY: w 以 NUL 結尾；其餘輸出指標可為 null。
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
    // SAFETY: 只讀取本執行緒的 last error。
    if ok != 0 { Ok(avail) } else { Err(unsafe { GetLastError() }) }
}

/// 與 C++ 版 `path_exists` 相同：取得屬性成功，或因共用衝突而失敗都算存在。
pub fn path_exists(p: &Path) -> bool {
    let w = wide(p.as_os_str());
    // SAFETY: w 以 NUL 結尾。
    let a = unsafe { GetFileAttributesW(w.as_ptr()) };
    // SAFETY: 只讀取本執行緒的 last error。
    a != INVALID_FILE_ATTRIBUTES || unsafe { GetLastError() } == ERROR_SHARING_VIOLATION
}

/// 兩個完整路徑以不分大小寫的序數比較（CompareStringOrdinal），與 C++ 版相同。
pub fn same_path_text(a: &OsStr, b: &OsStr) -> bool {
    let wa: Vec<u16> = a.encode_wide().collect();
    let wb: Vec<u16> = b.encode_wide().collect();
    let (Ok(la), Ok(lb)) = (i32::try_from(wa.len()), i32::try_from(wb.len())) else { return false };
    // SAFETY: 長度明確傳入，不需要 NUL 結尾。
    unsafe { CompareStringOrdinal(wa.as_ptr(), la, wb.as_ptr(), lb, 1) == CSTR_EQUAL }
}

/// 改名但不取代既有檔（MoveFileExW 不帶 REPLACE_EXISTING），失敗時回傳 Win32 錯誤碼。
pub fn move_no_replace(from: &Path, to: &Path) -> Result<(), u32> {
    let (wf, wt) = (wide(from.as_os_str()), wide(to.as_os_str()));
    // SAFETY: 兩個字串都以 NUL 結尾。
    let ok = unsafe { MoveFileExW(wf.as_ptr(), wt.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    // SAFETY: 只讀取本執行緒的 last error。
    if ok != 0 { Ok(()) } else { Err(unsafe { GetLastError() }) }
}

/// 16 bytes 系統亂數（BCryptGenRandom，系統偏好的 RNG）；失敗時回傳 NTSTATUS。
pub fn random_16() -> Result<[u8; 16], i32> {
    let mut b = [0u8; 16];
    // SAFETY: b 是 16 bytes 可寫緩衝；使用系統偏好 RNG 時演算法 handle 必須為 null。
    let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), b.as_mut_ptr(), 16, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    // NTSTATUS 小於 0 是錯誤；STATUS_SUCCESS 為 0。
    if status < 0 { Err(status) } else { Ok(b) }
}

static CTRL_FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

extern "system" fn on_ctrl(kind: u32) -> i32 {
    if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT || kind == CTRL_CLOSE_EVENT {
        if let Some(f) = CTRL_FLAG.get() {
            f.store(true, Ordering::SeqCst);
        }
        return 1;
    }
    0
}

/// Ctrl+C／Ctrl+Break／關閉主控台時把 `flag` 設為 true（與 C++ 版 `on_ctrl` 相同）。
pub fn install_ctrl_handler(flag: Arc<AtomicBool>) {
    if CTRL_FLAG.set(flag).is_ok() {
        // SAFETY: on_ctrl 是 'static 的 extern "system" 函式，只碰 atomic。
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_is_english() {
        assert_eq!(error_text(2), "2: The system cannot find the file specified.");
    }

    #[test]
    fn ordinal_compare_ignores_case() {
        assert!(same_path_text(OsStr::new(r"C:\A\b.json"), OsStr::new(r"c:\a\B.JSON")));
        assert!(!same_path_text(OsStr::new(r"C:\a"), OsStr::new(r"C:\b")));
    }
}
