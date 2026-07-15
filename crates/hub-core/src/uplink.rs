//! cf-alc-recorder への測定データ送信 (WS) の純粋部分 (ippoan/alc-app-s3#21)。
//!
//! フレーム形式は cf-alc-recorder/README.md (ippoan/alc-app#108) が正:
//!
//! - 上り: `{"type":"measurement","seq":N,"recorded_at_ms":T,"kind":K,"payload":{..}}`
//!   → `{"type":"ack","seq":N}` / `{"type":"error","seq":N,"message":".."}`
//! - 上り: `{"type":"command_result","id":"..","payload":{..}}` / `{"type":"ping"}`
//! - 下り: `{"type":"connected"}` / `{"type":"pong"}` /
//!   `{"type":"command","id":"..","payload":{..}}`
//!
//! WS 接続・NVS 保存・画面/ホスト通知などの副作用は firmware 側 (ws_uplink.rs)
//! が担い、ここではフレームの組立/解析と送信キューの帳簿のみを行う。
//! 再送は同じ seq のまま行い、サーバ側 UNIQUE (tenant_id, device_id, seq) で
//! 冪等化される。**seq は ack 後も再利用しない** (再利用すると ON CONFLICT
//! DO NOTHING で新データが黙って落ちる) ため、採番カウンタ (last_seq) は
//! キューが空になっても永続化する。

use std::collections::VecDeque;

use serde_json::{json, Map, Value};

/// hibernation を起こさない keep-alive フレーム (完全一致でサーバが auto-response)
pub const PING_FRAME: &str = r#"{"type":"ping"}"#;

/// 送信キューの 1 エントリ。payload はコンパクトな JSON オブジェクト文字列
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub seq: u64,
    pub recorded_at_ms: u64,
    pub kind: String,
    pub payload: String,
}

/// 下り (server → CoreS3) フレーム
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Downlink {
    /// accept 直後の通知
    Connected,
    /// ping への応答
    Pong,
    /// measurement の受領確認 — キューから消してよい
    Ack { seq: u64 },
    /// measurement の処理失敗 (例: upstream_502)。キューに残して再送する
    ServerError { seq: Option<u64>, message: String },
    /// 下り push (MEASURE 指示 / timecard イベント / 設定変更)
    Command { id: String, payload: String },
}

/// payload 文字列を JSON オブジェクトとして検証し Value を返す
fn payload_object(payload: &str) -> Result<Value, String> {
    let v: Value =
        serde_json::from_str(payload).map_err(|e| format!("payload の JSON 解析失敗: {e}"))?;
    if !v.is_object() {
        return Err("payload は JSON オブジェクトではありません".into());
    }
    Ok(v)
}

/// 上り measurement フレームを組み立てる
pub fn measurement_frame(entry: &QueueEntry) -> Result<String, String> {
    let payload = payload_object(&entry.payload)?;
    Ok(json!({
        "type": "measurement",
        "seq": entry.seq,
        "recorded_at_ms": entry.recorded_at_ms,
        "kind": entry.kind,
        "payload": payload,
    })
    .to_string())
}

/// 上り command_result フレームを組み立てる
pub fn command_result_frame(id: &str, payload: &str) -> Result<String, String> {
    let payload = payload_object(payload)?;
    Ok(json!({ "type": "command_result", "id": id, "payload": payload }).to_string())
}

/// 必須の文字列フィールドを取り出す
fn str_field(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} (文字列) がありません"))
}

/// 下りフレームを解析する
pub fn parse_downlink(text: &str) -> Result<Downlink, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON 解析失敗: {e}"))?;
    let obj = v.as_object().ok_or("JSON オブジェクトではありません")?;
    match obj.get("type").and_then(|t| t.as_str()) {
        Some("connected") => Ok(Downlink::Connected),
        Some("pong") => Ok(Downlink::Pong),
        Some("ack") => Ok(Downlink::Ack {
            seq: obj
                .get("seq")
                .and_then(|s| s.as_u64())
                .ok_or("ack に seq (数値) がありません")?,
        }),
        Some("error") => Ok(Downlink::ServerError {
            seq: obj.get("seq").and_then(|s| s.as_u64()),
            message: obj
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        Some("command") => Ok(Downlink::Command {
            id: str_field(obj, "id")?,
            payload: obj
                .get("payload")
                .filter(|p| p.is_object())
                .map(|p| p.to_string())
                .unwrap_or_else(|| "{}".to_string()),
        }),
        Some(other) => Err(format!("不明な type: {other}")),
        None => Err("type がありません".into()),
    }
}

/// 下り command payload の action フィールド (小文字化)。
/// 例: `{"action":"MEASURE"}` → Some("measure")。無し/不正は None
pub fn command_action(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload).ok()?;
    Some(v.get("action")?.as_str()?.to_ascii_lowercase())
}

/// 下り command payload から OTA firmware URL を取り出す
/// (`{"action":"ota","url":"https://..."}`)。http(s) 以外・欠落は None。
pub fn command_ota_url(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let url = v.get("url")?.as_str()?;
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_string())
}

/// 下り command payload から印刷対象 PDF の URL を取り出す
/// (`{"action":"print","url":"https://..."}`、印刷ブリッジ #38)。
/// http(s) 以外・欠落は None。
pub fn command_print_url(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let url = v.get("url")?.as_str()?;
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_string())
}

/// WS push 印刷 (#38) の 1 チャンク。`print_data` command payload
/// (`{"action":"print_data","seq":N,"chunk":"<base64>","last":bool}`) を
/// デコードした結果。`data` は base64 デコード済みの生バイト列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintChunk {
    /// 送信側 (DO) が採番する連番。欠落検出・進捗用
    pub seq: u64,
    /// base64 デコード済みの PDF バイト列 (そのまま 9100 へ流す)
    pub data: Vec<u8>,
    /// 最終チャンクなら true (受信側は flush してセッションを閉じる)
    pub last: bool,
}

/// 下り `print_data` command payload をデコードする (#38、WS push 印刷)。
/// `seq` (数値必須) / `chunk` (base64 文字列必須) / `last` (bool、既定 false) を
/// 取り出し、`chunk` を base64 デコードする。いずれか欠落・型不一致・base64
/// 不正は None。副作用は無い (9100 送信は firmware 側)。
pub fn command_print_chunk(payload: &str) -> Option<PrintChunk> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let seq = v.get("seq")?.as_u64()?;
    let chunk = v.get("chunk")?.as_str()?;
    let last = v.get("last").and_then(|b| b.as_bool()).unwrap_or(false);
    let data = base64_decode(chunk)?;
    Some(PrintChunk { seq, data, last })
}

/// 標準 base64 (RFC4648、`+` `/`、末尾 `=` パディング) をデコードする純粋関数。
/// 印刷チャンク (#38) 用。lib を足さず手書きにしている理由は、CCoW では
/// `Cargo.lock` を再生成できず新規依存追加が `--locked` CI を壊すため
/// (本関数は coverage 100% 対象なので小さく testable に保つ)。
/// alphabet 外・長さが 4 の倍数でない・パディング位置不正は None。
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for quad in bytes.chunks(4) {
        let c0 = val(quad[0])?;
        let c1 = val(quad[1])?;
        out.push((c0 << 2) | (c1 >> 4));
        if quad[2] == b'=' {
            // 1 バイト出力。4 文字目も `=` でなければ不正パディング
            if quad[3] != b'=' {
                return None;
            }
        } else {
            let c2 = val(quad[2])?;
            out.push((c1 << 4) | (c2 >> 2));
            if quad[3] != b'=' {
                let c3 = val(quad[3])?;
                out.push((c2 << 6) | c3);
            }
        }
    }
    Some(out)
}

/// 送信キューの帳簿。実際の送受信・永続化は呼び出し側が行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UplinkQueue {
    entries: VecDeque<QueueEntry>,
    /// 最後に採番した seq。**キューが空でも減らない・再利用しない**
    last_seq: u64,
    max: usize,
}

impl UplinkQueue {
    /// 永続化済みの last_seq とキュー行 (serialize の出力) から復元する。
    /// 壊れた行は読み飛ばす (戻り値 .1 = 読み飛ばした行数)。
    pub fn restore(last_seq: u64, lines: &str, max: usize) -> (Self, usize) {
        let mut entries = VecDeque::new();
        let mut skipped = 0usize;
        let mut max_seq = last_seq;
        for line in lines.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match Self::parse_line(line) {
                Some(e) => {
                    max_seq = max_seq.max(e.seq);
                    entries.push_back(e);
                }
                None => skipped += 1,
            }
        }
        while entries.len() > max {
            entries.pop_front();
        }
        (
            Self {
                entries,
                last_seq: max_seq,
                max,
            },
            skipped,
        )
    }

    fn parse_line(line: &str) -> Option<QueueEntry> {
        let v: Value = serde_json::from_str(line).ok()?;
        let obj = v.as_object()?;
        Some(QueueEntry {
            seq: obj.get("seq")?.as_u64()?,
            recorded_at_ms: obj.get("recorded_at_ms")?.as_u64()?,
            kind: obj.get("kind")?.as_str()?.to_string(),
            payload: obj.get("payload").filter(|p| p.is_object())?.to_string(),
        })
    }

    /// NVS 保存用の改行区切り文字列 (restore と対)
    pub fn serialize(&self) -> String {
        self.entries
            .iter()
            .map(|e| {
                // payload は restore/push で検証済みのため必ずオブジェクト
                let payload: Value = serde_json::from_str(&e.payload).expect("validated payload");
                json!({
                    "seq": e.seq,
                    "recorded_at_ms": e.recorded_at_ms,
                    "kind": e.kind,
                    "payload": payload,
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 測定を採番してキューへ積む。上限超過時は最古のエントリを捨てる
    /// (戻り値 .1 = 捨てたエントリの seq)。payload が不正なら積まない。
    pub fn push(
        &mut self,
        kind: &str,
        recorded_at_ms: u64,
        payload: &str,
    ) -> Result<(u64, Option<u64>), String> {
        // 正規化して保存する (serialize/restore の roundtrip をキー順に依らず
        // 一致させるため。measurement_frame にもこの正規化済み文字列が渡る)
        let payload = payload_object(payload)?.to_string();
        self.last_seq += 1;
        let seq = self.last_seq;
        self.entries.push_back(QueueEntry {
            seq,
            recorded_at_ms,
            kind: kind.to_string(),
            payload,
        });
        let dropped = if self.entries.len() > self.max {
            self.entries.pop_front().map(|e| e.seq)
        } else {
            None
        };
        Ok((seq, dropped))
    }

    /// ack された seq をキューから消す。該当が無ければ false
    pub fn ack(&mut self, seq: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.seq != seq);
        self.entries.len() != before
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 未 ack エントリ (古い順)。再送も同じ seq で行う
    pub fn entries(&self) -> impl Iterator<Item = &QueueEntry> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &str = r#"{"type":"temperature","value":36.5,"unit":"celsius"}"#;

    #[test]
    fn measurement_frame_embeds_payload_as_object() {
        let e = QueueEntry {
            seq: 3,
            recorded_at_ms: 1_752_300_000_000,
            kind: "temperature".into(),
            payload: PAYLOAD.into(),
        };
        let f = measurement_frame(&e).unwrap();
        let v: Value = serde_json::from_str(&f).unwrap();
        assert_eq!(v["type"], "measurement");
        assert_eq!(v["seq"], 3);
        assert_eq!(v["recorded_at_ms"], 1_752_300_000_000u64);
        assert_eq!(v["kind"], "temperature");
        assert_eq!(v["payload"]["value"], 36.5);
    }

    #[test]
    fn measurement_frame_rejects_bad_payload() {
        let mut e = QueueEntry {
            seq: 1,
            recorded_at_ms: 0,
            kind: "k".into(),
            payload: "{oops".into(),
        };
        assert!(measurement_frame(&e).is_err());
        e.payload = "[1,2]".into();
        assert!(measurement_frame(&e).is_err());
    }

    #[test]
    fn command_result_frame_roundtrip() {
        let f = command_result_frame("cmd-1", "{}").unwrap();
        let v: Value = serde_json::from_str(&f).unwrap();
        assert_eq!(v["type"], "command_result");
        assert_eq!(v["id"], "cmd-1");
        assert!(v["payload"].is_object());
        assert!(command_result_frame("cmd-1", "3").is_err());
    }

    #[test]
    fn ping_frame_is_exact_match() {
        // cf-alc-recorder の auto-response は完全一致 (README 参照)
        assert_eq!(PING_FRAME, "{\"type\":\"ping\"}");
    }

    #[test]
    fn parse_downlink_variants() {
        assert_eq!(
            parse_downlink(r#"{"type":"connected"}"#),
            Ok(Downlink::Connected)
        );
        assert_eq!(parse_downlink(r#"{"type":"pong"}"#), Ok(Downlink::Pong));
        assert_eq!(
            parse_downlink(r#"{"type":"ack","seq":7}"#),
            Ok(Downlink::Ack { seq: 7 })
        );
    }

    #[test]
    fn parse_downlink_error_frame() {
        assert_eq!(
            parse_downlink(r#"{"type":"error","seq":7,"message":"upstream_502"}"#),
            Ok(Downlink::ServerError {
                seq: Some(7),
                message: "upstream_502".into(),
            })
        );
        // seq / message 無しの error も受ける
        assert_eq!(
            parse_downlink(r#"{"type":"error"}"#),
            Ok(Downlink::ServerError {
                seq: None,
                message: "".into(),
            })
        );
    }

    #[test]
    fn parse_downlink_command() {
        assert_eq!(
            parse_downlink(r#"{"type":"command","id":"c1","payload":{"action":"measure"}}"#),
            Ok(Downlink::Command {
                id: "c1".into(),
                payload: r#"{"action":"measure"}"#.into(),
            })
        );
        // payload 省略 / 非オブジェクトは {} に落とす
        assert_eq!(
            parse_downlink(r#"{"type":"command","id":"c2"}"#),
            Ok(Downlink::Command {
                id: "c2".into(),
                payload: "{}".into(),
            })
        );
        assert_eq!(
            parse_downlink(r#"{"type":"command","id":"c3","payload":5}"#),
            Ok(Downlink::Command {
                id: "c3".into(),
                payload: "{}".into(),
            })
        );
        assert!(parse_downlink(r#"{"type":"command"}"#).is_err());
    }

    #[test]
    fn parse_downlink_invalid() {
        assert!(parse_downlink("{oops").is_err());
        assert!(parse_downlink("[1]").is_err());
        assert!(parse_downlink(r#"{"type":"nope"}"#).is_err());
        assert!(parse_downlink(r#"{"seq":1}"#).is_err());
        assert!(parse_downlink(r#"{"type":"ack"}"#).is_err());
    }

    #[test]
    fn queue_push_ack_and_seq_monotonic() {
        let (mut q, skipped) = UplinkQueue::restore(0, "", 10);
        assert_eq!(skipped, 0);
        assert!(q.is_empty());
        let (s1, d1) = q.push("temperature", 100, PAYLOAD).unwrap();
        let (s2, d2) = q.push("temperature", 200, PAYLOAD).unwrap();
        assert_eq!((s1, s2), (1, 2));
        assert_eq!((d1, d2), (None, None));
        assert_eq!(q.len(), 2);
        assert!(q.ack(1));
        assert!(!q.ack(1)); // 二重 ack は false
        assert_eq!(q.len(), 1);
        // 空になっても seq は戻らない
        assert!(q.ack(2));
        assert!(q.is_empty());
        let (s3, _) = q.push("temperature", 300, PAYLOAD).unwrap();
        assert_eq!(s3, 3);
        assert_eq!(q.last_seq(), 3);
    }

    #[test]
    fn queue_rejects_bad_payload() {
        let (mut q, _) = UplinkQueue::restore(0, "", 10);
        assert!(q.push("k", 0, "not json").is_err());
        assert!(q.is_empty());
        assert_eq!(q.last_seq(), 0); // 失敗時は採番しない
    }

    #[test]
    fn queue_overflow_drops_oldest() {
        let (mut q, _) = UplinkQueue::restore(0, "", 2);
        q.push("k", 1, PAYLOAD).unwrap();
        q.push("k", 2, PAYLOAD).unwrap();
        let (s3, dropped) = q.push("k", 3, PAYLOAD).unwrap();
        assert_eq!(s3, 3);
        assert_eq!(dropped, Some(1));
        let seqs: Vec<u64> = q.entries().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![2, 3]);
    }

    #[test]
    fn queue_serialize_restore_roundtrip() {
        let (mut q, _) = UplinkQueue::restore(5, "", 10);
        q.push("temperature", 100, PAYLOAD).unwrap();
        q.push("blood_pressure", 200, r#"{"systolic":120}"#).unwrap();
        let saved = q.serialize();
        let (r, skipped) = UplinkQueue::restore(q.last_seq(), &saved, 10);
        assert_eq!(skipped, 0);
        assert_eq!(r, q);
    }

    #[test]
    fn queue_restore_skips_corrupt_lines_and_keeps_seq() {
        let lines = concat!(
            r#"{"seq":8,"recorded_at_ms":1,"kind":"k","payload":{"a":1}}"#,
            "\n",
            "garbage\n",
            "\n",
            r#"{"seq":9,"recorded_at_ms":2,"kind":"k","payload":3}"#, // payload 非オブジェクト
            "\n",
            r#"{"seq":10,"recorded_at_ms":3,"kind":"k","payload":{}}"#,
        );
        // 保存済み last_seq (12) がエントリの最大 seq より大きい場合はそちらを保つ
        let (q, skipped) = UplinkQueue::restore(12, lines, 10);
        assert_eq!(skipped, 2);
        assert_eq!(q.len(), 2);
        assert_eq!(q.last_seq(), 12);
        // last_seq がエントリ最大 seq より小さい (NVS 書き込み順のずれ) 場合は
        // エントリ側に合わせる
        let (q, _) = UplinkQueue::restore(0, lines, 10);
        assert_eq!(q.last_seq(), 10);
    }

    #[test]
    fn command_action_lowercases_and_rejects() {
        assert_eq!(
            command_action(r#"{"action":"MEASURE"}"#),
            Some("measure".into())
        );
        assert_eq!(command_action(r#"{"action":1}"#), None);
        assert_eq!(command_action(r#"{}"#), None);
        assert_eq!(command_action("{oops"), None);
    }

    #[test]
    fn command_print_url_requires_http_scheme() {
        assert_eq!(
            command_print_url(r#"{"action":"print","url":"https://auth.ippoan.org/print/test.pdf"}"#),
            Some("https://auth.ippoan.org/print/test.pdf".into())
        );
        assert_eq!(
            command_print_url(r#"{"action":"print","url":"http://192.168.11.2:8000/t.pdf"}"#),
            Some("http://192.168.11.2:8000/t.pdf".into())
        );
        assert_eq!(command_print_url(r#"{"action":"print","url":"ftp://x"}"#), None);
        assert_eq!(command_print_url(r#"{"action":"print","url":1}"#), None);
        assert_eq!(command_print_url(r#"{"action":"print"}"#), None);
        assert_eq!(command_print_url("{oops"), None);
    }

    #[test]
    fn command_ota_url_requires_http_scheme() {
        assert_eq!(
            command_ota_url(r#"{"action":"ota","url":"https://x/app.bin"}"#),
            Some("https://x/app.bin".into())
        );
        assert_eq!(
            command_ota_url(r#"{"action":"ota","url":"http://192.168.11.2:8000/a.bin"}"#),
            Some("http://192.168.11.2:8000/a.bin".into())
        );
        assert_eq!(command_ota_url(r#"{"action":"ota","url":"ftp://x"}"#), None);
        assert_eq!(command_ota_url(r#"{"action":"ota","url":1}"#), None);
        assert_eq!(command_ota_url(r#"{"action":"ota"}"#), None);
        assert_eq!(command_ota_url("{oops"), None);
    }

    #[test]
    fn command_print_chunk_decodes_base64_payload() {
        // "SGVsbG8=" は base64("Hello")。last 明示 true。
        let c = command_print_chunk(
            r#"{"action":"print_data","seq":7,"chunk":"SGVsbG8=","last":true}"#,
        )
        .unwrap();
        assert_eq!(c.seq, 7);
        assert_eq!(c.data, b"Hello");
        assert!(c.last);
    }

    #[test]
    fn command_print_chunk_last_defaults_false() {
        // last 省略時は false。空 chunk ("" は valid base64) は空バイト列。
        let c = command_print_chunk(r#"{"seq":0,"chunk":""}"#).unwrap();
        assert_eq!(c.seq, 0);
        assert!(c.data.is_empty());
        assert!(!c.last);
    }

    #[test]
    fn command_print_chunk_rejects_malformed() {
        assert!(command_print_chunk("{oops").is_none()); // JSON 不正
        assert!(command_print_chunk(r#"{"chunk":"SGk="}"#).is_none()); // seq 欠落
        assert!(command_print_chunk(r#"{"seq":"1","chunk":"SGk="}"#).is_none()); // seq 非数値
        assert!(command_print_chunk(r#"{"seq":1}"#).is_none()); // chunk 欠落
        assert!(command_print_chunk(r#"{"seq":1,"chunk":9}"#).is_none()); // chunk 非文字列
        assert!(command_print_chunk(r#"{"seq":1,"chunk":"!!!!"}"#).is_none()); // base64 不正
    }

    #[test]
    fn base64_decode_valid() {
        assert_eq!(base64_decode("").unwrap(), b""); // 空
        assert_eq!(base64_decode("SGk=").unwrap(), b"Hi"); // 2 バイト (末尾 1 パディング)
        assert_eq!(base64_decode("SG==").unwrap(), b"H"); // 1 バイト (末尾 2 パディング)
        assert_eq!(base64_decode("SGVsbG8=").unwrap(), b"Hello"); // 5 バイト
        // '+' '/' と 0-9・大小英字を含む全 alphabet 経路を通す。
        // base64("\xfb\xff\xbf") = "+/+/"
        assert_eq!(base64_decode("+/+/").unwrap(), vec![0xfb, 0xff, 0xbf]);
        assert_eq!(base64_decode("0189").unwrap().len(), 3); // 数字経路
    }

    #[test]
    fn base64_decode_rejects_bad_input() {
        assert!(base64_decode("SGk").is_none()); // 長さが 4 の倍数でない
        assert!(base64_decode("SG=x").is_none()); // 不正パディング (3 文字目 = だが 4 文字目 ≠ =)
        assert!(base64_decode("!AAA").is_none()); // 1 文字目 alphabet 外
        assert!(base64_decode("A!AA").is_none()); // 2 文字目 alphabet 外
        assert!(base64_decode("AA!A").is_none()); // 3 文字目 alphabet 外 (非パディング)
        assert!(base64_decode("AAA!").is_none()); // 4 文字目 alphabet 外 (非パディング)
    }

    #[test]
    fn queue_restore_enforces_cap() {
        let lines: Vec<String> = (1..=5)
            .map(|i| format!(r#"{{"seq":{i},"recorded_at_ms":0,"kind":"k","payload":{{}}}}"#))
            .collect();
        let (q, _) = UplinkQueue::restore(0, &lines.join("\n"), 3);
        let seqs: Vec<u64> = q.entries().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
    }
}
