//! firmware エントリポイントの配線規約の機械検査 (再発防止、Refs #43)。
//!
//! 2026-07-14 の実害: heap.rs の `note()` は全 firmware 共通で crashlog リングに
//! 書くのに、`crashlog::init()` を cores3 の main にしか配線しなかったため、
//! atoms3-print が未初期化リングへの書き込みで boot loop になった (#46)。
//! ring_write 側も自己修復化したが、init() が無い firmware は panic hook /
//! クラッシュ復帰レポートも失うため、「heap::start を使うバイナリは必ず
//! crashlog::init() をそれより前に呼ぶ」をソース走査で強制する。
//!
//! 新しい firmware バイナリ (crates/*/src/main.rs) を追加すると自動で検査対象に
//! 入る。意図的に外す場合はこのテストに除外理由を書いて except すること。

use std::fs;
use std::path::PathBuf;

/// workspace 内の firmware エントリポイント (root src/main.rs + crates/*/src/main.rs)
fn firmware_mains() -> Vec<PathBuf> {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut mains = vec![ws.join("src/main.rs")];
    let crates = fs::read_dir(ws.join("crates")).expect("crates/ を列挙できる");
    for entry in crates.flatten() {
        let main = entry.path().join("src/main.rs");
        if main.is_file() {
            mains.push(main);
        }
    }
    mains
}

#[test]
fn heap_start_requires_crashlog_init_wired_before_it() {
    let mut checked = 0;
    for main in firmware_mains() {
        let raw = fs::read_to_string(&main).unwrap_or_else(|e| panic!("{main:?} 読めない: {e}"));
        // コメント行は走査対象外 (「heap::start より前に呼ぶ」等の説明文で誤検知しない)
        let src: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let Some(heap_at) = src.find("heap::start") else {
            continue; // heap 監視を使わないバイナリは対象外
        };
        let init_at = src.find("crashlog::init").unwrap_or_else(|| {
            panic!(
                "{main:?}: heap::start を使うのに crashlog::init() が配線されていない。\n\
                 heap.rs の note() は crashlog リングに書くため、init を heap::start より\n\
                 前に呼ぶこと (atoms3-print boot loop #46 の再発防止)"
            )
        });
        assert!(
            init_at < heap_at,
            "{main:?}: crashlog::init() は heap::start より前に呼ぶこと (現状: init={init_at} > heap={heap_at})"
        );
        checked += 1;
    }
    // 検査が空振りしていないこと (cores3 + atoms3-print の 2 バイナリは必ず対象)
    assert!(checked >= 2, "検査対象が {checked} 個しかない (パス解決が壊れている?)");
}
