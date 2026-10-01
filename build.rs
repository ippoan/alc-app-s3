fn main() {
    embuild::espidf::sysenv::output();
    // ESP-IDF の panic handler の本体 (`esp_panic_handler`) を src/panic_capture.rs の
    // wrap の関数へ差し替える (Refs ippoan/alc-app#403)。
    // **この crate の bin (CoreS3) にだけ効く** — `.cargo/config.toml` の rustflags に
    // 足すと全機種に掛かり、wrap を持たない AtomS3 系のリンクが落ちる
    println!("cargo:rustc-link-arg-bins=-Wl,--wrap=esp_panic_handler");
}
