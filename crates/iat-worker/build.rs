//! 嵌入 Windows 資源：程式圖示與版本資訊（assets/icons，階段 7）。

fn main() {
    let icon = "../../assets/icons/iat-rust.ico";
    println!("cargo:rerun-if-changed={icon}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon)
        .set("ProductName", "iat")
        .set("FileDescription", "iat worker (runs the analysis inside a Job Object)")
        .set("OriginalFilename", "iat-worker.exe")
        .set("InternalName", "iat-worker")
        .set("LegalCopyright", "Copyright (c) 2026 Kaido-MSVC");
    res.compile().expect("Windows resources compile (rc.exe from the Windows SDK)");
}
