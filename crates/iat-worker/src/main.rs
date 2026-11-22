//! iat-worker.exe：在父程序（iat.exe 或 iat-gui.exe）的 Job Object 裡分析不可信的輸入。
//! 輸入與暫存檔只以繼承的唯讀 handle 傳入，worker 從不開啟路徑；結果寫到 stdout。
//! 協定見 `iat_core::worker`（docs/decisions.md D-R02 第三次修訂）。

/// 配置失敗時把固定的 `worker_error: resource_limit` 寫到 stdout 後結束，不再配置記憶體。
#[global_allocator]
static ALLOCATOR: iat_core::win::WorkerAllocator = iat_core::win::WorkerAllocator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(iat_core::worker_main(&args));
}
