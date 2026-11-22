//! 把建置用的 rustc 版本交給程式（報告的 `tool.compiler`）。

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "rustc unknown".into());
    println!("cargo:rustc-env=IAT_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RUSTC");
}
