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

/// 打刻の `EVT TIMECARD` 行の配線規約 (Refs ippoan/rust-alc-api#644)。
///
/// 3 つを機械検査する:
///
/// 1. **CoreS3 (root `src/main.rs`) が打刻の行を出している** — IC カード
///    (FeliCa IDm / NFC-A UID) には点呼動線の `EVT NFC_LICENSE` に当たる行が
///    無く、出さないと PC は WS でクラウドを一周しないと打刻を知れない
///    (WS 断で十数秒、起動直後は WS が繋がるまで動かない)
/// 2. **行の綴りを firmware 側に直書きしない** — CoreS3 と VoiceS3R が同じ
///    出来事に別の行を出し始めると受け側 (alc-app) が 2 つ覚えることになる。
///    綴りは `alc_hub_core::timecard::evt_line` の 1 か所 (host test で固定)
/// 3. **`evtlog::emit` に渡さない** — `emit` の行は `.noinit` リングにも残り、
///    `LOG DUMP` / WS 下り `get_log` で**遠隔から読み出せる**。`card_id` は人を
///    特定できる値なので `println!` (シリアルだけ) で出す
///    (`alc_hub_common::evtlog` の「どの行を emit にするか」)
#[test]
fn timecard_evt_line_is_wired_spelled_once_and_never_emitted() {
    let mut punching = 0;
    let mut cores3_wired = false;
    for main in firmware_mains() {
        let raw = fs::read_to_string(&main).unwrap_or_else(|e| panic!("{main:?} 読めない: {e}"));
        // コメント行は走査対象外 (規約の説明文そのもので誤検知しないため)
        let src: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !src.contains("\"EVT TIMECARD"),
            "{main:?}: `EVT TIMECARD` の綴りを直書きしないこと \
             (alc_hub_core::timecard::evt_line を使う — CoreS3 と VoiceS3R で同一の行にする)"
        );
        if !src.contains("timecard::evt_line") {
            continue; // 打刻を出さないバイナリ (atoms3-print / atoms3-alarm 等)
        }
        for emit in ["emit(&alc_hub_core::timecard::evt_line", "emit(&evt_line"] {
            assert!(
                !src.contains(emit),
                "{main:?}: 打刻の行を evtlog::emit に渡さないこと — card_id が \
                 .noinit リングに残り LOG DUMP / get_log で遠隔から読める。println! で出す"
            );
        }
        punching += 1;
        // CoreS3 は workspace root の src/main.rs (crates/ の下ではない)
        cores3_wired |= main == PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src/main.rs");
    }
    assert!(
        cores3_wired,
        "CoreS3 (root src/main.rs) が打刻の EVT 行を出していない — IC カードの打刻が \
         PC に届くまで WS をクラウドまで一周することになる (#644)"
    );
    // CoreS3 + VoiceS3R (atoms3-timecard) の 2 バイナリは必ず対象
    assert!(punching >= 2, "検査対象が {punching} 個しかない (パス解決が壊れている?)");
}
