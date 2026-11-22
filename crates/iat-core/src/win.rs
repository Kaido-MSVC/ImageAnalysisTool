//! Win32 包裝。這是 iat-core 裡唯一允許 unsafe 的模組（AV1 包裝在獨立的 iat-av1 crate），理由見
//! docs/decisions.md D-R03：英文系統錯誤訊息、檔案識別碼、不取代既有檔的改名、
//! 系統亂數與主控台中斷處理，std 都沒有提供安全且行為相同的介面。
//! 階段 6 加上 worker 需要的部分（D-R02 第三次修訂）：Job Object、指定繼承清單的建立程序、
//! 匿名管線、繼承的 handle，以及 worker 的全域配置器（配置失敗時不再配置，直接回報後結束）。
//!
//! 每個函式只做一次 FFI 呼叫加上必要的轉換，對外的介面全部是安全的。
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use windows_sys::Win32::Foundation::{
    ERROR_SHARING_VIOLATION, GetHandleInformation, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_128, FILE_ID_INFO, FileIdInfo, GetDiskFreeSpaceExW, GetFileAttributesW, GetFileInformationByHandleEx,
    INVALID_FILE_ATTRIBUTES, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode};
use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectAssociateCompletionPortInformation, JobObjectBasicAndIoAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Win32 結構的大小（DWORD），填進 cb／dwLength 這類欄位。
#[expect(clippy::cast_possible_truncation, reason = "Win32 結構大小遠小於 4 GiB")]
const fn dword_size_of<T>() -> u32 {
    std::mem::size_of::<T>() as u32
}

/// `"<code>: <英文系統訊息>"`，與 C++ 版 `win_error_text` 相同（固定用 en-US，不跟系統語言）。
pub fn error_text(code: u32) -> String {
    const LANG_EN_US: u32 = 0x0409;
    const BUF_CHARS: u32 = 1024;
    let mut buf = [0u16; BUF_CHARS as usize];
    // SAFETY: buf 是可寫的 1024 個 u16；未使用 ALLOCATE_BUFFER，也不傳插入參數。
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            std::ptr::null(),
            code,
            LANG_EN_US,
            buf.as_mut_ptr(),
            BUF_CHARS,
            std::ptr::null(),
        )
    } as usize;
    let msg = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    format!("{code}: {}", msg.trim_end_matches(['\n', '\r', ' ']))
}

/// io::Error 的 Win32 錯誤碼；沒有時用 ERROR_GEN_FAILURE (31)。
pub fn os_code(e: &std::io::Error) -> u32 {
    e.raw_os_error().map_or(31, i32::cast_unsigned)
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
            dword_size_of::<FILE_ID_INFO>(),
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
/// 目前可用的實體記憶體（bytes，`GlobalMemoryStatusEx` 的 `ullAvailPhys`）；查詢失敗時為 None。
/// GUI 用來判斷建議的 `job_commit_mib` 是否超過可用記憶體。
pub fn available_physical_memory() -> Option<u64> {
    let mut st = MEMORYSTATUSEX { dwLength: dword_size_of::<MEMORYSTATUSEX>(), ..Default::default() };
    // SAFETY: st 是本地變數，dwLength 已設為結構大小（API 的要求）。
    let ok = unsafe { GlobalMemoryStatusEx(&mut st) };
    (ok != 0).then_some(st.ullAvailPhys)
}

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

// ---------------------------------------------------------------- worker（D-R02 第三次修訂）

/// winnt.h 的 Job 通知訊息（windows-sys 放在 SystemServices，為了兩個常數不另開那個 feature）。
const JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT: u32 = 9;
const JOB_OBJECT_MSG_JOB_MEMORY_LIMIT: u32 = 10;

fn last_error() -> u32 {
    // SAFETY: 只讀取本執行緒的 last error。
    unsafe { GetLastError() }
}

/// 取得 `FromRawHandle` 的擁有權。
///
/// # Safety
///
/// `h` 必須是本程序擁有、之後沒有其他地方會關閉的有效 handle。
unsafe fn own(h: HANDLE) -> OwnedHandle {
    // SAFETY: 由呼叫者保證。
    unsafe { OwnedHandle::from_raw_handle(h as RawHandle) }
}

/// 建立或設定 worker 失敗：哪一個 Win32 呼叫，以及它的錯誤碼。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnError {
    pub call: &'static str,
    pub code: u32,
}

impl SpawnError {
    fn last(call: &'static str) -> Self {
        Self { call, code: last_error() }
    }
}

/// 套了記憶體上限的 Job Object，加上接收記憶體上限通知的 completion port。
/// 關閉 Job 的 handle 時，裡面的程序一併結束（KILL_ON_JOB_CLOSE）。
pub struct WorkerJob {
    job: OwnedHandle,
    port: OwnedHandle,
}

/// 在 Job 裡執行的 worker 與它的 stdout、stderr 管線。
pub struct WorkerProcess {
    pub job: WorkerJob,
    pub child: WorkerChild,
    pub stdout: File,
    pub stderr: File,
}

fn set_inheritable(h: RawHandle, on: bool) -> bool {
    // SAFETY: h 是呼叫者仍開啟的 handle；只改它的繼承旗標。
    unsafe { SetHandleInformation(h as HANDLE, HANDLE_FLAG_INHERIT, if on { HANDLE_FLAG_INHERIT } else { 0 }) != 0 }
}

/// 匿名管線：(讀取端，給本程序；寫入端，給 worker)。寫入端交給 [`spawn_worker`] 繼承後關閉。
///
/// # Errors
///
/// CreatePipe 或設定繼承旗標失敗。
pub fn pipe() -> Result<(File, OwnedHandle), SpawnError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: dword_size_of::<SECURITY_ATTRIBUTES>(),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: 兩個輸出指標指向本地變數；sa 在呼叫期間有效。
    if unsafe { CreatePipe(&mut r, &mut w, &sa, 0) } == 0 {
        return Err(SpawnError::last("CreatePipe"));
    }
    // SAFETY: CreatePipe 成功，兩個 handle 都屬於本程序且只在這裡取得擁有權。
    let (r, w) = unsafe { (File::from_raw_handle(r as RawHandle), own(w)) };
    if !set_inheritable(r.as_raw_handle(), false) {
        return Err(SpawnError::last("SetHandleInformation"));
    }
    Ok((r, w))
}

impl WorkerJob {
    /// Job 與程序的 commit 上限都設為 `commit_limit` bytes，只允許一個程序（同 C++ 版 `run_worker`）。
    ///
    /// # Errors
    ///
    /// 建立或設定 Job、completion port 失敗。
    pub fn new(commit_limit: u64) -> Result<Self, SpawnError> {
        // SAFETY: 不帶名稱與安全屬性。
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(SpawnError::last("CreateJobObjectW"));
        }
        // SAFETY: 新建的 handle，只在這裡取得擁有權。
        let job = unsafe { own(job) };
        // SAFETY: 新的 completion port，不關聯檔案。
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(SpawnError::last("CreateIoCompletionPort"));
        }
        // SAFETY: 同上。
        let port = unsafe { own(port) };
        let limit = usize::try_from(commit_limit).unwrap_or(usize::MAX);
        let mut li = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        li.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        li.BasicLimitInformation.ActiveProcessLimit = 1;
        li.JobMemoryLimit = limit;
        li.ProcessMemoryLimit = limit;
        // SAFETY: li 的大小與 JobObjectExtendedLimitInformation 類別相符。
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&li as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                dword_size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            )
        };
        if ok == 0 {
            return Err(SpawnError::last("SetInformationJobObject"));
        }
        let acp = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: std::ptr::without_provenance_mut(1),
            CompletionPort: port.as_raw_handle() as HANDLE,
        };
        // SAFETY: acp 的大小與 JobObjectAssociateCompletionPortInformation 類別相符；port 仍開啟。
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectAssociateCompletionPortInformation,
                (&acp as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                dword_size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>(),
            )
        };
        if ok == 0 {
            return Err(SpawnError::last("SetInformationJobObject"));
        }
        Ok(Self { job, port })
    }

    /// 讀完 completion port 上的通知（最多等 `timeout_ms`）；有記憶體上限通知時回傳 true。
    pub fn drain_notifications(&self, timeout_ms: u32) -> bool {
        let mut seen = false;
        loop {
            let (mut msg, mut key, mut ov): (u32, usize, *mut OVERLAPPED) = (0, 0, std::ptr::null_mut());
            // SAFETY: port 仍開啟；三個輸出指標指向本地變數。
            let ok = unsafe {
                GetQueuedCompletionStatus(self.port.as_raw_handle() as HANDLE, &mut msg, &mut key, &mut ov, timeout_ms)
            };
            if ok == 0 {
                return seen;
            }
            seen |= msg == JOB_OBJECT_MSG_JOB_MEMORY_LIMIT || msg == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT;
        }
    }

    /// 結束 Job 裡的所有程序。
    pub fn terminate(&self, exit_code: u32) {
        // SAFETY: job 仍開啟。失敗時程序在 Job 關閉時照樣會被結束（KILL_ON_JOB_CLOSE）。
        unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, exit_code) };
    }

    /// (Job 的 commit 峰值, 讀取的位元組數)；查詢失敗時為 0。
    pub fn accounting(&self) -> (u64, u64) {
        let mut li = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: li 的大小與類別相符；不需要回傳長度。
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&mut li as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                dword_size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
                std::ptr::null_mut(),
            )
        };
        let peak = if ok != 0 { li.PeakJobMemoryUsed as u64 } else { 0 };
        let mut io = JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION::default();
        // SAFETY: 同上。
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle() as HANDLE,
                JobObjectBasicAndIoAccountingInformation,
                (&mut io as *mut JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION).cast(),
                dword_size_of::<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION>(),
                std::ptr::null_mut(),
            )
        };
        (peak, if ok != 0 { io.IoInfo.ReadTransferCount } else { 0 })
    }
}

/// 把 `args` 加上帶引號的執行檔路徑組成命令列。`args` 只含數字與不需引號的記號。
fn command_line(exe: &Path, args: &str) -> Vec<u16> {
    let mut s: Vec<u16> = "\"".encode_utf16().collect();
    s.extend(exe.as_os_str().encode_wide());
    s.extend(format!("\" {args}").encode_utf16());
    s.push(0);
    s
}

/// 暫停狀態建立 worker、放進 `job`、再讓它開始執行（限制在任何 worker 程式碼執行前就生效）。
/// 只繼承 stdin（NUL）、stdout／stderr 管線的寫入端、`extra`（例如進度通道的寫入端，之後關閉）
/// 與 `inherit` 裡的檔案（PROC_THREAD_ATTRIBUTE_HANDLE_LIST）。
///
/// # Errors
///
/// 管線、程序建立或放進 Job 失敗；失敗時已建立的程序會被結束。
pub fn spawn_worker(
    job: WorkerJob,
    exe: &Path,
    args: &str,
    inherit: &[&File],
    extra: OwnedHandle,
) -> Result<WorkerProcess, SpawnError> {
    let (out_r, out_w) = pipe()?;
    let (err_r, err_w) = pipe()?;
    let chan_w = extra;
    let nul = File::open("NUL").map_err(|e| SpawnError { call: "CreateFileW(NUL)", code: os_code(&e) })?;
    let mut list: Vec<HANDLE> = vec![
        nul.as_raw_handle() as HANDLE,
        out_w.as_raw_handle() as HANDLE,
        err_w.as_raw_handle() as HANDLE,
        chan_w.as_raw_handle() as HANDLE,
    ];
    for f in inherit {
        let h = f.as_raw_handle() as HANDLE;
        if !list.contains(&h) {
            list.push(h);
        }
    }
    for &h in &list {
        if !set_inheritable(h as RawHandle, true) {
            return Err(SpawnError::last("SetHandleInformation"));
        }
    }
    let result = create_suspended(&job, exe, args, &list, &nul, &out_w, &err_w);
    // 子程序已經（或沒能）繼承；本程序的檔案恢復成不可繼承，管線的寫入端由 drop 關閉。
    for f in inherit {
        set_inheritable(f.as_raw_handle(), false);
    }
    drop((nul, out_w, err_w, chan_w));
    let process = result?;
    Ok(WorkerProcess { job, child: WorkerChild(process), stdout: out_r, stderr: err_r })
}

fn create_suspended(
    job: &WorkerJob,
    exe: &Path,
    args: &str,
    list: &[HANDLE],
    nul: &File,
    out_w: &OwnedHandle,
    err_w: &OwnedHandle,
) -> Result<OwnedHandle, SpawnError> {
    let mut size = 0usize;
    // SAFETY: 只查詢需要的大小（清單指標為 null）；這個呼叫照規定會以 ERROR_INSUFFICIENT_BUFFER 失敗。
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
    // 屬性清單需要指標對齊，用 usize 陣列配置。
    let mut buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = buf.as_mut_ptr().cast();
    // SAFETY: buf 至少有 size bytes，且在 DeleteProcThreadAttributeList 之前一直存在。
    if unsafe { InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) } == 0 {
        return Err(SpawnError::last("InitializeProcThreadAttributeList"));
    }
    // SAFETY: attrs 已初始化；list 在 CreateProcessW 返回前一直存在。
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            list.as_ptr().cast(),
            std::mem::size_of_val(list),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        let e = SpawnError::last("UpdateProcThreadAttribute");
        // SAFETY: attrs 已初始化，只刪除一次。
        unsafe { DeleteProcThreadAttributeList(attrs) };
        return Err(e);
    }
    // SAFETY: STARTUPINFOEXW 是純資料結構，全 0 是合法的初始值。
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = dword_size_of::<STARTUPINFOEXW>();
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = nul.as_raw_handle() as HANDLE;
    si.StartupInfo.hStdOutput = out_w.as_raw_handle() as HANDLE;
    si.StartupInfo.hStdError = err_w.as_raw_handle() as HANDLE;
    si.lpAttributeList = attrs;
    let mut cmd = command_line(exe, args);
    let mut pi = PROCESS_INFORMATION {
        hProcess: std::ptr::null_mut(),
        hThread: std::ptr::null_mut(),
        dwProcessId: 0,
        dwThreadId: 0,
    };
    // SAFETY: cmd 是可寫、以 NUL 結尾的命令列；si 與它指向的屬性清單、handle 在呼叫期間有效；
    // 不帶環境與工作目錄（沿用本程序的）。
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    let create_error = SpawnError::last("CreateProcessW");
    // SAFETY: attrs 已初始化，只刪除一次；CreateProcessW 已經返回。
    unsafe { DeleteProcThreadAttributeList(attrs) };
    if created == 0 {
        return Err(create_error);
    }
    // SAFETY: CreateProcessW 成功，兩個 handle 屬於本程序，只在這裡取得擁有權。
    let (process, thread) = unsafe { (own(pi.hProcess), own(pi.hThread)) };
    // SAFETY: 兩個 handle 都仍開啟。
    if unsafe { AssignProcessToJobObject(job.job.as_raw_handle() as HANDLE, process.as_raw_handle() as HANDLE) } == 0 {
        let e = SpawnError::last("AssignProcessToJobObject");
        // SAFETY: 暫停中、還沒執行任何程式碼的子程序。
        unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) };
        return Err(e);
    }
    // SAFETY: thread 是剛建立、暫停中的主執行緒。
    if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } == u32::MAX {
        let e = SpawnError::last("ResumeThread");
        job.terminate(1);
        return Err(e);
    }
    Ok(process)
}

/// worker 程序的 handle。
pub struct WorkerChild(OwnedHandle);

impl WorkerChild {
    /// 等候 worker 結束最多 `ms` 毫秒；已結束時回傳 true。
    pub fn wait(&self, ms: u32) -> bool {
        // SAFETY: process handle 仍開啟。
        unsafe { WaitForSingleObject(self.0.as_raw_handle() as HANDLE, ms) == WAIT_OBJECT_0 }
    }

    /// 結束代碼（還在執行時為 STILL_ACTIVE 259）；查詢失敗時為 u32::MAX。
    pub fn exit_code(&self) -> u32 {
        let mut code = 0u32;
        // SAFETY: process handle 仍開啟；輸出指標指向本地變數。
        let ok = unsafe { GetExitCodeProcess(self.0.as_raw_handle() as HANDLE, &mut code) };
        if ok != 0 { code } else { u32::MAX }
    }
}

/// 把繼承來的 handle 包成 File。
///
/// # Safety
///
/// `h` 必須是有效、可繼承，而且本程序沒有其他擁有者的 handle；同一個值只能交出一次。
unsafe fn adopt_inherited(h: HANDLE) -> File {
    // SAFETY: 由呼叫者保證。
    unsafe { File::from_raw_handle(h as RawHandle) }
}

/// [`take_inherited_files`] 已經呼叫過。
static INHERITED_TAKEN: AtomicBool = AtomicBool::new(false);

/// worker 端：一次取得父程序在命令列上傳來、已繼承的全部 handle（依 `values` 的順序）。
///
/// 整個程序只能呼叫一次，並在開啟任何檔案之前呼叫。以下情況回傳錯誤（說明第幾個值）而不取用任何 handle：
/// 已經呼叫過、值重複、是標準輸入／輸出／錯誤的 handle、不是有效的 handle，或沒有繼承旗標
/// （繼承來的 handle 保留這個旗標；系統與本程序自己開的 handle 沒有）。
///
/// # Errors
///
/// 見上；文字為英文，worker 原樣寫進 `bad_argument`。
pub fn take_inherited_files(values: &[u64]) -> Result<Vec<File>, String> {
    if INHERITED_TAKEN.swap(true, Ordering::SeqCst) {
        return Err("inherited handles were already taken".into());
    }
    // SAFETY: 只讀取本程序的標準 handle 值。
    let std_handles = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|n| unsafe { GetStdHandle(n) });
    let mut handles: Vec<HANDLE> = Vec::with_capacity(values.len());
    for (i, &v) in values.iter().enumerate() {
        let h = usize::try_from(v).map_err(|_| format!("handle {i} is out of range"))? as HANDLE;
        if h.is_null() || h == INVALID_HANDLE_VALUE || std_handles.contains(&h) || handles.contains(&h) {
            return Err(format!("handle {i} is null, a standard handle or repeated"));
        }
        let mut flags = 0u32;
        // SAFETY: 只查詢旗標；無效的 handle 會讓呼叫失敗而不是未定義行為。
        if unsafe { GetHandleInformation(h, &mut flags) } == 0 || flags & HANDLE_FLAG_INHERIT == 0 {
            return Err(format!("handle {i} is not a valid inherited handle"));
        }
        handles.push(h);
    }
    // SAFETY: 整個程序只走到這裡一次（INHERITED_TAKEN）；每個值都不同、不是標準 handle，而且帶著
    // 繼承旗標。worker 從不把自己開的 handle 設為可繼承，所以這些 handle 只能來自父程序的繼承清單，
    // 本程序沒有其他擁有者。
    Ok(handles.into_iter().map(|h| unsafe { adopt_inherited(h) }).collect())
}

/// worker 端的程序設定（同 C++ 版 `worker_main`）：主控台中斷只交給父程序處理，
/// 不顯示嚴重錯誤與 GP fault 的對話方塊。
pub fn worker_process_setup() {
    // SAFETY: handler 為 null、第二個參數為 TRUE：本程序忽略 Ctrl+C。
    unsafe { SetConsoleCtrlHandler(None, 1) };
    // SAFETY: 只設定本程序的錯誤模式旗標。
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
}

/// worker 配置失敗時寫到 stdout 的固定內容（父程序判為 `resource_limit`）。事先準備好，
/// 寫出時不再配置記憶體。
const ALLOC_FAILED_JSON: &[u8] = br#"{"worker_error":"resource_limit","message":"allocation failed in worker"}"#;
/// 配置失敗時的結束代碼（同 C++ 版 worker 的 bad_alloc）。
pub const ALLOC_FAILED_EXIT: u32 = 3;

/// 正在解碼的一側（0：沒有或該格式在 C++ 版是執行層級；1：original；2：output）。
/// 配置失敗時據此告訴父程序是哪一側（D-R02 第四次修訂）。
pub(crate) static ALLOC_SIDE: AtomicU8 = AtomicU8::new(0);
/// 這一側正在做的配置屬於哪個階段（`engine::AllocStage` 的值），隨出事的一側一起送出。
pub(crate) static ALLOC_STAGE: AtomicU8 = AtomicU8::new(0);
/// 通道的 handle 值（0：沒有）。
static ALLOC_CHANNEL: AtomicUsize = AtomicUsize::new(0);
/// 通道紀錄的種類：配置失敗的一側（格式同 `worker` 的通道紀錄，17 bytes）。
pub(crate) const REC_ALLOC_FAILED: u8 = 3;

/// worker 端：登記通道，配置失敗時把出事的一側寫進去。`'static` 保證這個 handle 到程序結束都不會被
/// 關閉，所以存下的值不會指到之後重新使用同一個值的其他物件。
pub fn set_alloc_failure_channel(channel: &'static File) {
    ALLOC_CHANNEL.store(channel.as_raw_handle() as usize, Ordering::SeqCst);
}

/// 寫完整段靜態資料（失敗就放棄）；不配置記憶體。
fn write_all_raw(h: HANDLE, data: &[u8]) {
    let mut done = 0usize;
    while done < data.len() {
        let mut n = 0u32;
        let rest = &data[done..];
        let len = u32::try_from(rest.len()).unwrap_or(u32::MAX); // 超過 4 GiB 時分段寫
        // SAFETY: rest 指向至少 len bytes 的有效資料；n 指向本地變數；不使用 overlapped。h 無效時 WriteFile 只回傳失敗。
        let ok = unsafe { WriteFile(h, rest.as_ptr(), len, &mut n, std::ptr::null_mut()) };
        if ok == 0 || n == 0 {
            break;
        }
        done += n as usize;
    }
}

/// 把出事的一側寫到通道、固定字串寫到 stdout 後終止程序；不配置記憶體。
///
/// 用 `TerminateProcess` 而不是 `ExitProcess`：後者會執行 TLS 回呼（Rust 的 `thread_local`
/// 解構）與各 DLL 的 `DLL_PROCESS_DETACH`，那些程式碼可能再配置記憶體而回到這裡，或取用被終止的
/// 執行緒留下的鎖而卡住。`WriteFile` 是同步寫入，返回時資料已經在管線裡。
fn allocation_failed() -> ! {
    let side = ALLOC_SIDE.load(Ordering::SeqCst);
    let chan = ALLOC_CHANNEL.load(Ordering::SeqCst);
    if side != 0 && chan != 0 {
        let mut rec = [0u8; 17];
        rec[0] = REC_ALLOC_FAILED;
        rec[1] = side - 1;
        rec[9] = ALLOC_STAGE.load(Ordering::SeqCst);
        write_all_raw(chan as HANDLE, &rec);
    }
    // SAFETY: 取得本程序的標準輸出 handle。
    let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    write_all_raw(out, ALLOC_FAILED_JSON);
    // SAFETY: 取得本程序的虛擬 handle，不必關閉。
    let me = unsafe { GetCurrentProcess() };
    // SAFETY: 終止本程序；不執行 TLS 回呼、DLL detach 或任何 Rust 程式碼。
    unsafe { TerminateProcess(me, ALLOC_FAILED_EXIT) };
    // TerminateProcess 終止自己時不會返回；萬一返回，也不能回到配置器的呼叫者。
    loop {
        std::hint::spin_loop();
    }
}

/// worker 的全域配置器：系統配置器加上「配置失敗就回報 resource_limit 並結束」。
/// Job Object 的 commit 上限讓配置失敗；Rust 預設會印訊息到 stderr 後中止，父程序就只能判為
/// worker_crashed（D-R02 第三次修訂）。
pub struct WorkerAllocator;

// SAFETY: 每個方法都直接轉給 System；只在 System 回傳 null（配置失敗）時改為結束程序，
// 所以回傳的指標一律符合 GlobalAlloc 的約定。
unsafe impl GlobalAlloc for WorkerAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 呼叫者滿足 GlobalAlloc::alloc 的前提，原樣轉給 System。
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            allocation_failed();
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 同 alloc。
        let p = unsafe { System.alloc_zeroed(layout) };
        if p.is_null() {
            allocation_failed();
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr 由本配置器（也就是 System）以同一個 layout 配置。
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: 呼叫者滿足 GlobalAlloc::realloc 的前提，原樣轉給 System。
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            allocation_failed();
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_is_english() {
        assert_eq!(error_text(2), "2: The system cannot find the file specified.");
    }

    /// 整個測試程序只有這裡呼叫 take_inherited_files（它只能呼叫一次）。
    #[test]
    fn inherited_handles_are_checked_and_taken_once() {
        // 標準輸出的 handle（測試程序裡未必可繼承）與 null 都不收；失敗時不取用任何 handle
        // SAFETY: 只讀取本程序的標準 handle 值。
        let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) } as usize as u64;
        let e = take_inherited_files(&[0]).unwrap_err();
        assert!(e.contains("handle 0 is null"), "{e}");
        assert_eq!(take_inherited_files(&[out]).unwrap_err(), "inherited handles were already taken");
    }

    #[test]
    fn ordinal_compare_ignores_case() {
        assert!(same_path_text(OsStr::new(r"C:\A\b.json"), OsStr::new(r"c:\a\B.JSON")));
        assert!(!same_path_text(OsStr::new(r"C:\a"), OsStr::new(r"C:\b")));
    }
}
