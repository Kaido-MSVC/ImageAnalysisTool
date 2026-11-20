//! 編譯 ui/app.slint，並把 lang/ 底下的翻譯（gettext .po）編進執行檔。
//! 不使用預設的翻譯 context（元件名稱），.po 檔裡的字串因此沒有 msgctxt。

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("ui/app.slint compiles");
    println!("cargo:rerun-if-changed=lang");
}
