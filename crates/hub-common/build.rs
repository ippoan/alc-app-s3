//! ビルド時に git の短縮 SHA を `FW_GIT_SHA` として埋め込む。
//!
//! firmware のバージョン識別子を `<crate version>+<sha>` にして、CI が
//! GitHub Pages の manifest.json に載せる `<version>+<short-sha>`
//! (build.yml: `github.sha | cut -c1-7`) と突き合わせられるようにする
//! (OTA の「更新必要か」判定用、Refs #25)。git が無い / 履歴が浅い環境では
//! "dev" にフォールバックする。
use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=FW_GIT_SHA={sha}");
    // HEAD が動いたら再ビルドして SHA を更新する
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
