//! クラッシュログ捕捉の純粋部分 (ippoan/alc-app-s3#43)。
//!
//! panic / WDT / brownout 等の異常リセットの前後関係を後追いできるよう、
//! 「panic 前のログを `.noinit` リングバッファに保持 → 復帰後に
//! kind="crash_log" として WS 送信キューへ積む」流れの、ホストでテスト可能な
//! 計算部分をここに置く:
//!
//! - リングバッファ操作 (`ring_append` / `ring_snapshot` / `ring_valid`)
//! - ログのサニタイズ (ANSI カラーコード・制御文字の除去、`sanitize_log`)
//! - reset reason の分類 (`reset_reason_name` / `is_crash_reset`)
//! - WS payload の組立 (`crash_payload`)
//!
//! `.noinit` メモリの確保・`esp_log_set_vprintf` hook・`esp_reset_reason()`
//! などの副作用は firmware 側 (hub-drivers/src/crashlog.rs) が担う。

use crate::pwalog::PwaLog;

/// `esp_reset_reason_t` の値 → 短い名前。ESP-IDF の安定 API 値
/// (esp_system.h) をそのまま受ける — sys クレートに依存しない。
pub fn reset_reason_name(code: i32) -> &'static str {
    match code {
        1 => "poweron",
        2 => "ext",
        3 => "sw",
        4 => "panic",
        5 => "int_wdt",
        6 => "task_wdt",
        7 => "wdt",
        8 => "deepsleep",
        9 => "brownout",
        10 => "sdio",
        11 => "usb",
        12 => "jtag",
        13 => "efuse",
        14 => "pwr_glitch",
        15 => "cpu_lockup",
        _ => "unknown",
    }
}

/// クラッシュ由来とみなす reset reason (= crash_log を送る対象)。
/// poweron / sw (esp_restart = OTA・RESET コマンド) / usb 等の正常系は除く。
pub fn is_crash_reset(code: i32) -> bool {
    matches!(code, 4 | 5 | 6 | 7 | 9 | 14 | 15)
}

/// 起動ごとにリングへ入れる区切り行 (#215)。リングはソフトリセット (usb / sw /
/// panic / WDT) をまたいで残るので、前の起動の行と今回の行の境目を示す。
pub fn boot_separator(code: i32) -> String {
    format!("--- BOOT reset={} ({code}) ---", reset_reason_name(code))
}

/// 起動時の PSRAM 版リングの生の帳簿と置き場 (`EVT RING_BOOT`、#226)。
///
/// 再起動をまたいでリングが消えた件の切り分け用 — firmware の `init()` が
/// `preserved` を決める前に読んだ帳簿の値そのもの (`raw_*`) と、その判定結果、
/// リング本体の番地 (`ring_addr`)、リンカの `.ext_ram_noinit` 区間の先頭
/// (`noinit_addr`) を 1 行に並べる。番地は 16 進 8 桁のゼロ埋め
pub fn ring_boot_line(
    raw_magic: u32,
    raw_pos: u32,
    raw_len: u32,
    preserved: bool,
    ring_addr: u32,
    noinit_addr: u32,
) -> String {
    format!(
        "EVT RING_BOOT raw_magic=0x{raw_magic:08x} raw_pos={raw_pos} raw_len={raw_len} \
         preserved={} ring=0x{ring_addr:08x} noinit=0x{noinit_addr:08x}",
        u8::from(preserved)
    )
}

/// PSRAM 版リングの書き戻し (`esp_cache_msync`) が失敗した累計回数と、最後の
/// エラー (`esp_err_t` の値を 10 進で) (`EVT RING_MSYNC`、#226)。
pub fn ring_msync_line(fail: u32, err: i32) -> String {
    format!("EVT RING_MSYNC fail={fail} err={err}")
}

/// USB-Serial-JTAG 起因の reset (usb = 11 / jtag = 12) か。
///
/// 運行者 PC の PWA タブを閉じると Windows の driver がハンドル解放で
/// DTR → RTS を落とし、途中の「DTR=0 かつ RTS=1」でチップが reset する
/// (issue #194)。この reset は core reset で DRAM (`.noinit`) が保持されるので、
/// 警告デバイスの武装状態を復元してよい reset かどうかの判定に使う。
/// sw (esp_restart = OTA・RESET コマンド) は**含めない** — 意図した再起動は
/// 未武装で始めるのが従来どおり。
pub fn is_usb_serial_reset(code: i32) -> bool {
    matches!(code, 11 | 12)
}

/// リングの帳簿 (pos = 次の書き込み位置, len = 有効バイト数) が
/// 容量 `cap` に対して破綻していないか。`.noinit` は電源断でゴミになるため、
/// magic チェックと併せて復元可否の判定に使う。
pub fn ring_valid(cap: usize, pos: u32, len: u32) -> bool {
    (pos as usize) < cap && (len as usize) <= cap
}

/// リングへ追記する。容量を超えた分は最古のバイトから上書きされる。
///
/// 帳簿 (pos/len) が壊れていても **panic せず** リセットして書き始める —
/// `.noinit` は電源投入直後や init 前にゴミを含み得るため、この関数は
/// どんな入力でも安全でなければならない (実害: atoms3-print が init 前の
/// `note()` で index out of bounds → boot loop、2026-07-14)。
pub fn ring_append(data: &mut [u8], pos: &mut u32, len: &mut u32, bytes: &[u8]) {
    let cap = data.len();
    if cap == 0 {
        return;
    }
    if !ring_valid(cap, *pos, *len) {
        *pos = 0;
        *len = 0;
    }
    for &b in bytes {
        data[*pos as usize] = b;
        *pos = (*pos + 1) % cap as u32;
        if (*len as usize) < cap {
            *len += 1;
        }
    }
}

/// リングに `write_len` バイトを `pos_before` から書き込んだときに、実際に
/// 触られた領域 (`(offset, len)` の並び、1〜2 区間) と区間数。
///
/// PSRAM 版のリング (`.ext_ram_noinit`、#217) は書き込みのたびにこの区間を
/// `esp_cache_msync` で書き戻す (#226) — USB のようなハードリセットは
/// ソフトの shutdown 処理を経ないため、書いた直後に書き戻すのがリセットを
/// またいで残す唯一の方法 (firmware 側 `hub-drivers::crashlog` の doc 冒頭)。
/// `pos_before` は書き込み前の帳簿の `pos` (`< cap` である前提、[`ring_valid`])。
/// `write_len >= cap` (1 周以上書いた) ときは全域が触られたとみなし 1 区間を返す。
///
/// 戻り値は固定長配列 + 区間数 (`ranges[..count]` が有効) — 呼び出し元
/// (`ring_write`) は vprintf hook から `RING_LOCK` を握ったまま呼ぶため、
/// ログ 1 行ごとのヒープ確保を避ける (`Vec` を返さない、#226)。
pub fn ring_write_ranges(cap: usize, pos_before: u32, write_len: usize) -> ([(usize, usize); 2], usize) {
    let mut ranges = [(0, 0); 2];
    if cap == 0 || write_len == 0 {
        return (ranges, 0);
    }
    if write_len >= cap {
        ranges[0] = (0, cap);
        return (ranges, 1);
    }
    let pos_before = pos_before as usize % cap;
    let end = pos_before + write_len;
    if end <= cap {
        ranges[0] = (pos_before, write_len);
        (ranges, 1)
    } else {
        ranges[0] = (pos_before, cap - pos_before);
        ranges[1] = (0, end - cap);
        (ranges, 2)
    }
}

/// リング内容を古い順に取り出す。帳簿が不正なら空を返す (fail-safe)。
pub fn ring_snapshot(data: &[u8], pos: u32, len: u32) -> Vec<u8> {
    if !ring_valid(data.len(), pos, len) {
        return Vec::new();
    }
    let cap = data.len();
    let len = len as usize;
    let start = (pos as usize + cap - len) % cap;
    (0..len).map(|i| data[(start + i) % cap]).collect()
}

/// リング内容を人間可読なテキストへ。UTF-8 として壊れたバイト (リング上書きで
/// 途中から始まった行・電源断のビット化け) は lossy 変換で吸収し、
/// ESP-IDF ログの ANSI カラーコード (ESC[0;32m 等) と `\n` 以外の制御文字を
/// 取り除く。
pub fn sanitize_log(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            // CSI シーケンスは英字で終端する (ESC[0;32m の 'm' 等)
            if c.is_ascii_alphabetic() {
                in_esc = false;
            }
            continue;
        }
        match c {
            '\u{1b}' => in_esc = true,
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// 末尾 `max_bytes` バイト以内に収まる部分文字列 (UTF-8 文字境界を守る)。
/// 新しいログほど末尾にあるため、切るのは先頭側。
pub fn tail_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// 末尾 `max_bytes` バイト以内に収まる**行の並び**と、切り詰めたかどうか。
///
/// [`tail_str`] の結果を行境界にスナップする — 切った位置が行の途中なら、
/// その欠けた先頭行を捨てる。全体が収まれば `(text, false)`。
/// `get_log` command (#195) の窓 ([`window_lines`]) の先頭側を切るのに使う。
pub fn tail_lines(text: &str, max_bytes: usize) -> (&str, bool) {
    let tail = tail_str(text, max_bytes);
    let start = text.len() - tail.len();
    let snapped = if start > 0 && text.as_bytes()[start - 1] != b'\n' {
        tail.find('\n').map_or("", |i| &tail[i + 1..])
    } else {
        tail
    };
    (snapped, snapped.len() < text.len())
}

/// 末尾から `offset` バイト遡った位置を終端とし、そこから前へ `max_bytes`
/// バイト以内に収まる**行の並び**と、実際に使った offset (#217)。
///
/// リング (CoreS3 では 256 KB、それ以外の機種は 4 KB) は 1 回の応答
/// ([`crate::uplink::LOG_MAX_BYTES`]) に収まらないので、`get_log` command は
/// これで末尾以外の窓も返す。`offset = 0` は
/// [`tail_lines`] と同じ。終端が行の途中なら、その欠けた末尾行を捨てて手前の
/// 行境界に寄せ、戻り値の offset もその分だけ大きくなる — 呼び側は
/// `offset + 返したバイト数` を次の offset にすれば隙間なく遡れる (読むあいだに
/// 追記された分だけ重なる)。`offset` が全体以上なら空。
pub fn window_lines(text: &str, offset: usize, max_bytes: usize) -> (&str, usize) {
    let end = text.len() - offset.min(text.len());
    let end = if end == text.len() {
        end
    } else {
        text.as_bytes()[..end]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1)
    };
    (tail_lines(&text[..end], max_bytes).0, text.len() - end)
}

/// `get_log` command (#195) の command_result payload (JSON オブジェクト文字列)。
/// `text` (リングの sanitize 済み全文) を末尾から `offset` バイト遡った位置から
/// 前へ `max_bytes` 以内、行境界で切って返す ([`window_lines`])。リング (CoreS3 では
/// 256 KB、それ以外の機種は 4 KB) は 1 回では取り切れない — `truncated` と `total_bytes` で伝え、実際に
/// 使った `offset` を返す。呼び側は `offset + bytes` を次の offset にして遡る
/// (#217)。文字列のエスケープは serde_json に任せる。
///
/// `reset_history` は直近 8 回の reset 理由の履歴 (Refs #211)。`Some` なら
/// `boot_history` キーを新しい順 (先頭が現在の起動) で足す。履歴を持たない機種
/// (VoiceS3R 等、`HubStatus::reset_history` が既定の `None`) では `None` を渡し、
/// キー自体を出さない。
///
/// `pwa` はキオスク PWA から中継した診断ログ (#215、[`crate::pwalog`])。
/// `pwa_log` / `pwa_log_error` キーは常に出す ([`PwaLog::json_fields`])。
/// `max_bytes` は `text` だけに掛かり、`pwa_log` (最大
/// [`crate::pwalog::MAX_BYTES`]) はその外に足す — `max_bytes` の上限
/// ([`crate::uplink::LOG_MAX_BYTES`]) は送信側の制約ではないため
pub fn log_payload(
    text: &str,
    offset: usize,
    max_bytes: usize,
    uptime_ms: u64,
    reset_history: Option<u64>,
    pwa: &PwaLog,
) -> String {
    let (window, offset) = window_lines(text, offset, max_bytes);
    let (pwa_log, pwa_log_error) = pwa.json_fields();
    let mut payload = serde_json::json!({
        "text": window,
        "bytes": window.len(),
        "total_bytes": text.len(),
        "truncated": window.len() < text.len(),
        "offset": offset,
        "uptime_ms": uptime_ms,
        "pwa_log": pwa_log,
        "pwa_log_error": pwa_log_error,
    });
    if let Some(packed) = reset_history {
        let boot_history: Vec<_> = crate::boot_history::codes(packed)
            .into_iter()
            .map(|code| {
                serde_json::json!({
                    "reset_reason": reset_reason_name(code),
                    "reset_code": code,
                })
            })
            .collect();
        payload["boot_history"] = serde_json::Value::Array(boot_history);
    }
    payload.to_string()
}

/// kind="crash_log" の WS payload (JSON オブジェクト文字列) を組み立てる。
/// ログは末尾 `max_log_bytes` に切り詰める (NVS 送信キュー 4KB 制限との同居。
/// 切った場合は truncated:true)。RAM が保持されなかった場合は空文字で呼ぶ —
/// reset reason だけでも「画面が切れた」の原因種別は判別できる。
pub fn crash_payload(
    reason_code: i32,
    version: &str,
    slot: &str,
    log_text: &str,
    max_log_bytes: usize,
) -> String {
    let log = tail_str(log_text, max_log_bytes);
    serde_json::json!({
        "type": "crash_log",
        "reset_reason": reset_reason_name(reason_code),
        "reset_code": reason_code,
        "version": version,
        "slot": slot,
        "truncated": log.len() < log_text.len(),
        "log": log,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_reason_names_cover_all_codes() {
        let expected = [
            (0, "unknown"),
            (1, "poweron"),
            (2, "ext"),
            (3, "sw"),
            (4, "panic"),
            (5, "int_wdt"),
            (6, "task_wdt"),
            (7, "wdt"),
            (8, "deepsleep"),
            (9, "brownout"),
            (10, "sdio"),
            (11, "usb"),
            (12, "jtag"),
            (13, "efuse"),
            (14, "pwr_glitch"),
            (15, "cpu_lockup"),
            (99, "unknown"),
        ];
        for (code, name) in expected {
            assert_eq!(reset_reason_name(code), name, "code={code}");
        }
    }

    #[test]
    fn is_crash_reset_classifies() {
        for code in [4, 5, 6, 7, 9, 14, 15] {
            assert!(is_crash_reset(code), "code={code}");
        }
        for code in [0, 1, 2, 3, 8, 10, 11, 12, 13, 99] {
            assert!(!is_crash_reset(code), "code={code}");
        }
    }

    #[test]
    fn is_usb_serial_reset_classifies() {
        for code in [11, 12] {
            assert!(is_usb_serial_reset(code), "code={code}");
        }
        for code in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 14, 15, 99] {
            assert!(!is_usb_serial_reset(code), "code={code}");
        }
    }

    #[test]
    fn ring_valid_bounds() {
        assert!(ring_valid(8, 0, 0));
        assert!(ring_valid(8, 7, 8));
        assert!(!ring_valid(8, 8, 0)); // pos は cap 未満
        assert!(!ring_valid(8, 0, 9)); // len は cap 以下
    }

    #[test]
    fn ring_append_and_snapshot_without_wrap() {
        let mut data = [0u8; 8];
        let (mut pos, mut len) = (0u32, 0u32);
        ring_append(&mut data, &mut pos, &mut len, b"abc");
        assert_eq!((pos, len), (3, 3));
        assert_eq!(ring_snapshot(&data, pos, len), b"abc");
    }

    #[test]
    fn ring_append_wraps_and_keeps_newest() {
        let mut data = [0u8; 8];
        let (mut pos, mut len) = (0u32, 0u32);
        ring_append(&mut data, &mut pos, &mut len, b"0123456789ab");
        assert_eq!(len, 8);
        assert_eq!(ring_snapshot(&data, pos, len), b"456789ab");
        // 追記を続けても常に直近 cap バイトが残る
        ring_append(&mut data, &mut pos, &mut len, b"XY");
        assert_eq!(ring_snapshot(&data, pos, len), b"6789abXY");
    }

    #[test]
    fn ring_append_resets_corrupt_bookkeeping_instead_of_panicking() {
        // 未初期化 .noinit のゴミ帳簿 (pos が範囲外) でも panic しない
        let mut data = [0u8; 8];
        let (mut pos, mut len) = (251_410_212u32, 99u32);
        ring_append(&mut data, &mut pos, &mut len, b"ok");
        assert_eq!((pos, len), (2, 2));
        assert_eq!(ring_snapshot(&data, pos, len), b"ok");
    }

    #[test]
    fn ring_append_empty_capacity_is_noop() {
        let mut data = [0u8; 0];
        let (mut pos, mut len) = (0u32, 0u32);
        ring_append(&mut data, &mut pos, &mut len, b"abc");
        assert_eq!((pos, len), (0, 0));
    }

    #[test]
    fn ring_snapshot_invalid_bookkeeping_returns_empty() {
        let data = [0u8; 8];
        assert!(ring_snapshot(&data, 99, 4).is_empty());
        assert!(ring_snapshot(&data, 0, 99).is_empty());
    }

    #[test]
    fn ring_write_ranges_no_wrap() {
        let (ranges, n) = ring_write_ranges(8, 2, 3);
        assert_eq!((&ranges[..n], n), (&[(2, 3)][..], 1));
    }

    #[test]
    fn ring_write_ranges_exact_tail() {
        // pos_before + write_len がちょうど cap (折り返さない)
        let (ranges, n) = ring_write_ranges(8, 5, 3);
        assert_eq!((&ranges[..n], n), (&[(5, 3)][..], 1));
    }

    #[test]
    fn ring_write_ranges_wraps() {
        let (ranges, n) = ring_write_ranges(8, 6, 4);
        assert_eq!((&ranges[..n], n), (&[(6, 2), (0, 2)][..], 2));
    }

    #[test]
    fn ring_write_ranges_fills_capacity_exactly() {
        // pos_before=0 かつ write_len==cap もちょうど末尾 (折り返さない) の一種
        let (ranges, n) = ring_write_ranges(8, 0, 8);
        assert_eq!((&ranges[..n], n), (&[(0, 8)][..], 1));
    }

    #[test]
    fn ring_write_ranges_more_than_capacity_covers_whole_ring() {
        // 1 周以上書いた (vsnprintf の巨大出力など) → 全域が触られたとみなす
        let (ranges, n) = ring_write_ranges(8, 3, 20);
        assert_eq!((&ranges[..n], n), (&[(0, 8)][..], 1));
    }

    #[test]
    fn ring_write_ranges_zero_cap_or_len_is_empty() {
        assert_eq!(ring_write_ranges(0, 0, 5).1, 0);
        assert_eq!(ring_write_ranges(8, 3, 0).1, 0);
    }

    #[test]
    fn ring_write_ranges_normalizes_out_of_range_pos() {
        // pos_before は呼び出し元で ring_valid 済みの想定だが、範囲外でも
        // panic せず折りたたむ (ring_append と同じ fail-safe の流儀)
        let (ranges, n) = ring_write_ranges(8, 10, 2);
        assert_eq!((&ranges[..n], n), (&[(2, 2)][..], 1));
    }

    #[test]
    fn sanitize_strips_ansi_and_control_chars() {
        // ESP-IDF のカラーログ: ESC[0;32mI (123) tag: msgESC[0m + CR LF
        let raw = b"\x1b[0;32mI (123) wifi: connected\x1b[0m\r\nnext\tline\n";
        assert_eq!(sanitize_log(raw), "I (123) wifi: connected\nnextline\n");
    }

    #[test]
    fn sanitize_lossy_on_broken_utf8() {
        // リング上書きで多バイト文字が途中から始まるケース
        let raw = [0x82, 0xa0, b'o', b'k', b'\n'];
        let s = sanitize_log(&raw);
        assert!(s.ends_with("ok\n"), "{s:?}");
    }

    #[test]
    fn tail_str_respects_char_boundary() {
        assert_eq!(tail_str("abcdef", 10), "abcdef");
        assert_eq!(tail_str("abcdef", 3), "def");
        // "あ" は 3 バイト — 境界をまたぐ切り出しは次の文字境界へ寄せる
        assert_eq!(tail_str("あい", 4), "い");
    }

    #[test]
    fn tail_lines_returns_all_when_it_fits() {
        assert_eq!(tail_lines("a\nbb\n", 10), ("a\nbb\n", false));
        assert_eq!(tail_lines("a\nbb\n", 5), ("a\nbb\n", false));
    }

    #[test]
    fn tail_lines_drops_partial_first_line() {
        // 6 バイト以内 = "b\nccc\n" だが先頭行 "b" は "bb" の欠け → 捨てる
        assert_eq!(tail_lines("a\nbb\nccc\n", 6), ("ccc\n", true));
        // 切った位置がちょうど行頭なら捨てない
        assert_eq!(tail_lines("a\nbb\nccc\n", 7), ("bb\nccc\n", true));
        // 収まる範囲に改行が無い (1 行が長すぎる) → 空 + truncated
        assert_eq!(tail_lines("abcdefgh", 3), ("", true));
        // UTF-8 境界は tail_str が守る ("あ" は 3 バイト)
        assert_eq!(tail_lines("x\nあい\n", 6), ("", true));
        assert_eq!(tail_lines("x\nあい\n", 7), ("あい\n", true));
    }

    #[test]
    fn tail_lines_empty_text() {
        assert_eq!(tail_lines("", 100), ("", false));
        assert_eq!(tail_lines("", 0), ("", false));
    }

    #[test]
    fn window_lines_offset_zero_is_tail_lines() {
        let text = "a\nbb\nccc\n";
        for max in [0, 3, 6, 7, 100] {
            assert_eq!(window_lines(text, 0, max), (tail_lines(text, max).0, 0));
        }
        // 末尾行が書きかけ (改行前) でも offset 0 なら残す (tail_lines と同じ)
        assert_eq!(window_lines("a\nbb", 0, 100), ("a\nbb", 0));
    }

    #[test]
    fn window_lines_steps_back_on_line_boundaries() {
        let text = "a\nbb\nccc\n"; // 9 バイト
        // 終端がちょうど行境界 (末尾 4 バイト "ccc\n" を飛ばす)
        assert_eq!(window_lines(text, 4, 100), ("a\nbb\n", 4));
        // 終端が行の途中 → 欠けた "cc" を捨てて手前の行境界へ。offset もその分増える
        assert_eq!(window_lines(text, 2, 100), ("a\nbb\n", 4));
        // max_bytes は窓の先頭側を行境界で切る
        assert_eq!(window_lines(text, 4, 3), ("bb\n", 4));
        // offset + bytes を次の offset にすると隙間なく遡れる
        assert_eq!(window_lines(text, 4 + 3, 3), ("a\n", 7));
        // 終端より前に改行が無い → 空 (offset は全体)
        assert_eq!(window_lines(text, 8, 100), ("", 9));
    }

    #[test]
    fn window_lines_offset_beyond_text_is_empty() {
        assert_eq!(window_lines("a\nbb\n", 5, 100), ("", 5));
        assert_eq!(window_lines("a\nbb\n", 99, 100), ("", 5));
        assert_eq!(window_lines("", 3, 100), ("", 0));
    }

    #[test]
    fn log_payload_offset_returns_older_window() {
        let p = log_payload("l1\nl2\nl3\n", 3, 100, 0, None, &PwaLog::NoHost);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["text"], "l1\nl2\n");
        assert_eq!(v["bytes"], 6);
        assert_eq!(v["total_bytes"], 9);
        assert_eq!(v["truncated"], true);
        assert_eq!(v["offset"], 3);
        // 既定 (offset 0) の応答にも offset キーが載る
        let p = log_payload("l1\n", 0, 100, 0, None, &PwaLog::NoHost);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["offset"], 0);
        assert_eq!(v["truncated"], false);
    }

    #[test]
    fn log_payload_reports_sizes_and_truncation() {
        let p = log_payload("l1\nl2 \"q\"\n", 0, 100, 4242, None, &PwaLog::NoHost);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["text"], "l1\nl2 \"q\"\n");
        assert_eq!(v["bytes"], 10);
        assert_eq!(v["total_bytes"], 10);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["uptime_ms"], 4242);

        let p = log_payload("l1\nl2\nl3\n", 0, 4, 0, None, &PwaLog::NoHost);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["text"], "l3\n");
        assert_eq!(v["bytes"], 3);
        assert_eq!(v["total_bytes"], 9);
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn log_payload_omits_boot_history_key_when_none() {
        let p = log_payload("l1\n", 0, 100, 0, None, &PwaLog::NoHost);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert!(v.get("boot_history").is_none());
    }

    #[test]
    fn log_payload_includes_boot_history_newest_first_when_some() {
        // code=11 (usb) が最新、9 (brownout) が 1 つ前
        let packed =
            crate::boot_history::push(crate::boot_history::push(crate::boot_history::EMPTY, 9), 11);
        let p = log_payload("l1\n", 0, 100, 0, Some(packed), &PwaLog::NoHost);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        let hist = v["boot_history"].as_array().unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0]["reset_reason"], "usb");
        assert_eq!(hist[0]["reset_code"], 11);
        assert_eq!(hist[1]["reset_reason"], "brownout");
        assert_eq!(hist[1]["reset_code"], 9);
    }

    #[test]
    fn log_payload_boot_history_empty_is_empty_array() {
        let p = log_payload(
            "l1\n",
            0,
            100,
            0,
            Some(crate::boot_history::EMPTY),
            &PwaLog::NoHost,
        );
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["boot_history"].as_array().unwrap().len(), 0);
    }

    fn payload_with(pwa: PwaLog) -> serde_json::Value {
        let p = log_payload("l1\n", 0, 100, 0, None, &pwa);
        serde_json::from_str(&p).unwrap()
    }

    #[test]
    fn log_payload_pwa_log_no_host() {
        let v = payload_with(PwaLog::NoHost);
        assert!(v["pwa_log"].is_null());
        assert_eq!(v["pwa_log_error"], "no_host");
        // 既存のキーは変わらない
        assert_eq!(v["text"], "l1\n");
    }

    #[test]
    fn log_payload_pwa_log_received() {
        let v = payload_with(PwaLog::Received {
            text: "a\n\"b\"".into(),
            complete: true,
        });
        assert_eq!(v["pwa_log"], "a\n\"b\"");
        assert!(v["pwa_log_error"].is_null());
        // text の max_bytes は pwa_log に食われない
        assert_eq!(v["bytes"], 3);
    }

    #[test]
    fn log_payload_pwa_log_empty_answer() {
        // PWA が 0 行で END を返した
        let v = payload_with(PwaLog::Received {
            text: String::new(),
            complete: true,
        });
        assert_eq!(v["pwa_log"], "");
        assert!(v["pwa_log_error"].is_null());
    }

    #[test]
    fn log_payload_pwa_log_timeout_keeps_partial() {
        let v = payload_with(PwaLog::Received {
            text: "a".into(),
            complete: false,
        });
        assert_eq!(v["pwa_log"], "a");
        assert_eq!(v["pwa_log_error"], "timeout");
    }

    #[test]
    fn boot_separator_names_the_reset() {
        assert_eq!(boot_separator(11), "--- BOOT reset=usb (11) ---");
        assert_eq!(boot_separator(4), "--- BOOT reset=panic (4) ---");
    }

    #[test]
    fn ring_boot_line_formats_all_fields() {
        assert_eq!(
            ring_boot_line(0x4352_4c47, 1234, 262_144, true, 0x3c0a_1000, 0x3c0a_0000),
            "EVT RING_BOOT raw_magic=0x43524c47 raw_pos=1234 raw_len=262144 preserved=1 \
             ring=0x3c0a1000 noinit=0x3c0a0000"
        );
    }

    #[test]
    fn ring_boot_line_zero_pads_hex() {
        assert_eq!(
            ring_boot_line(0, 0, 0, false, 0, 0),
            "EVT RING_BOOT raw_magic=0x00000000 raw_pos=0 raw_len=0 preserved=0 \
             ring=0x00000000 noinit=0x00000000"
        );
        assert_eq!(
            ring_boot_line(0x1f, 7, 8, false, 0x100, 0xabc),
            "EVT RING_BOOT raw_magic=0x0000001f raw_pos=7 raw_len=8 preserved=0 \
             ring=0x00000100 noinit=0x00000abc"
        );
    }

    #[test]
    fn ring_boot_line_max_values() {
        assert_eq!(
            ring_boot_line(u32::MAX, u32::MAX, u32::MAX, true, u32::MAX, u32::MAX),
            "EVT RING_BOOT raw_magic=0xffffffff raw_pos=4294967295 raw_len=4294967295 \
             preserved=1 ring=0xffffffff noinit=0xffffffff"
        );
    }

    #[test]
    fn ring_msync_line_formats() {
        assert_eq!(ring_msync_line(0, 0), "EVT RING_MSYNC fail=0 err=0");
        assert_eq!(ring_msync_line(3, 258), "EVT RING_MSYNC fail=3 err=258");
        assert_eq!(
            ring_msync_line(u32::MAX, i32::MIN),
            "EVT RING_MSYNC fail=4294967295 err=-2147483648"
        );
        assert_eq!(
            ring_msync_line(1, i32::MAX),
            "EVT RING_MSYNC fail=1 err=2147483647"
        );
    }

    #[test]
    fn crash_payload_includes_reason_and_log() {
        let p = crash_payload(4, "0.1.0+abc1234", "ota_0", "line1\nline2\n", 1024);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["type"], "crash_log");
        assert_eq!(v["reset_reason"], "panic");
        assert_eq!(v["reset_code"], 4);
        assert_eq!(v["version"], "0.1.0+abc1234");
        assert_eq!(v["slot"], "ota_0");
        assert_eq!(v["truncated"], false);
        assert_eq!(v["log"], "line1\nline2\n");
    }

    #[test]
    fn crash_payload_truncates_long_log() {
        let long = "x".repeat(2000);
        let p = crash_payload(6, "v", "s", &long, 100);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["truncated"], true);
        assert_eq!(v["log"].as_str().unwrap().len(), 100);
    }
}
