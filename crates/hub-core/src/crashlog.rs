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
