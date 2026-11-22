//! 編譯 ui/app.slint，並把 lang/ 底下的翻譯（gettext .po）編進執行檔。
//! 不使用預設的翻譯 context（元件名稱），.po 檔裡的字串因此沒有 msgctxt。
//! 另外嵌入 Windows 資源：程式圖示與版本資訊（assets/icons，階段 7）。

fn main() {
    let icon = "../../assets/icons/iat-rust.ico";
    println!("cargo:rerun-if-changed={icon}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon)
        .set("ProductName", "iat")
        .set("FileDescription", "iat - image analysis tool")
        .set("OriginalFilename", "iat-gui.exe")
        .set("InternalName", "iat-gui")
        .set("LegalCopyright", "Copyright (c) 2026 Kaido-MSVC");
    res.compile().expect("Windows resources compile (rc.exe from the Windows SDK)");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("ui/app.slint compiles");
    println!("cargo:rerun-if-changed=lang");
}
