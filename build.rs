fn main() {
    // L'interface Slint sert hors macOS (macOS utilise AppKit, voir src/ui_mac.rs),
    // ou sur Mac avec `--features slint-on-mac` pour la vérifier.
    let mac = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if !mac || std::env::var_os("CARGO_FEATURE_SLINT_ON_MAC").is_some() {
        let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
        slint_build::compile_with_config("ui/app.slint", config).expect("compilation de l'interface Slint");
    }
    println!("cargo:rerun-if-changed=ui");
}
