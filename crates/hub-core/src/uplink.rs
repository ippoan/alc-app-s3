//! cf-alc-recorder への測定データ送信 (WS) の純粋部分 (ippoan/alc-app-s3#21)。
//!
//! フレーム形式は cf-alc-recorder/README.md (ippoan/alc-app#108) が正:
//!
//! - 上り: `{"type":"measurement","seq":N,"recorded_at_ms":T,"kind":K,"payload":{..}}`
//!   (点呼中の測定にはさらに `"session_id":"<boot>-<n>"` が載る、Refs #112。
//!    点呼外の単発計測では **key ごと省く** = 旧フレームと同一)
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
//!
//! キューの実体は flash 側の [`UplinkStore`] (firmware では専用 NVS
//! パーティション `punchq` の 1 件 1 キー) にあり、RAM ([`UplinkQueue`]) が
//! 持つのは**送信窓**と seq の索引だけ (Refs #142)。保持件数を増やしても
//! RAM 使用量はほぼ変わらない。

use std::collections::{BTreeMap, VecDeque};

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
    /// 1 回の点呼を束ねる識別子 (Refs #112)。点呼外の単発計測では None。
    /// **None のときはフレームにも NVS にも出さない** — 旧サーバ / 旧 NVS データとの
    /// 互換を保つため (受け側は欠落を「セッション不明」として扱う)。
    pub session_id: Option<String>,
    /// 記録時点の稼働時間 [ms] (esp_timer)。NTP 未同期で記録した測定の時刻を、
    /// 同期後の送信時に `fix_unsynced_times` で実時刻へ補正するための足場。
    /// NVS には `uptime_ms` として保存 (旧データは None)。フレームには出さない
    pub uptime_ms: Option<u64>,
    /// 記録した起動の boot_id。稼働時間は起動ごとに 0 に戻るので、**同じ起動の
    /// うちだけ**補正できる (再起動をまたいだ古いエントリは補正しない)
    pub boot_id: Option<u32>,
}

/// これ未満の epoch ms は「NTP 未同期 (1970 起点の稼働時間)」とみなす
/// (clock::MIN_SYNCED_SECS と同じ境界)
pub const MIN_SYNCED_MS: u64 = crate::clock::MIN_SYNCED_SECS as u64 * 1000;

/// WS が繋がっても時計が未同期なら、補正できる測定がある間はこの時間まで送信を
/// 待って NTP 同期を待つ。実測: LAN 直結起動で体温が稼働 23 秒に届き、同期は
/// 23〜37 秒の間に完了した (それより前に送ると補正の機会を失う)。NTP が塞がれて
/// いる環境で測定を止めないよう、超過したら未同期のまま送る
pub const CLOCK_WAIT_MS: u64 = 60_000;

/// 送信を待つべきか: 今の時計が未同期で、補正できる (同じ起動・稼働時間つき・
/// 未同期時刻の) エントリがあり、接続からまだ CLOCK_WAIT_MS 経っていない
pub fn should_wait_for_clock(now_epoch_ms: u64, connected_for_ms: u64, has_correctable: bool) -> bool {
    now_epoch_ms < MIN_SYNCED_MS && has_correctable && connected_for_ms < CLOCK_WAIT_MS
}

/// OTA 直後の image が WS に繋がらないまま、この時間が経ったら前の image に戻す
/// (Refs #217)。起動からの稼働時間で測る
pub const OTA_VERIFY_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// OTA 直後の未確定状態 (PENDING_VERIFY または NEW) の image をどうするか
/// (Refs #217)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaGuard {
    /// まだ決めない (または決める必要が無い)
    Nothing,
    /// この image を確定して rollback を解除する
    Confirm,
    /// この image を無効にして前の image で再起動する
    Rollback,
}

/// OTA 直後の image を確定するか、前の image に戻すかを決める (Refs #217)。
///
/// 健康の判定は「認証付きの WS に繋がったか」で、繋がった時点の確定は呼び側が
/// 接続イベントで行う (ここは `ws_ever_connected` なら何もしない)。
///
/// - 未登録 (`!paired`) の機は WS で判定できないので、すぐ確定する (今までどおり)
/// - 登録済みで [`OTA_VERIFY_TIMEOUT_MS`] 経っても繋がらない: IP があれば戻す。
///   IP が無い (ネットワークが無い) なら image のせいか判定できないので確定する
///   — 判定できない image を黙って捨てない
pub fn ota_guard(
    pending: bool,
    paired: bool,
    has_ip: bool,
    ws_ever_connected: bool,
    uptime_ms: u64,
) -> OtaGuard {
    if !pending || ws_ever_connected {
        return OtaGuard::Nothing;
    }
    if !paired {
        return OtaGuard::Confirm;
    }
    if uptime_ms < OTA_VERIFY_TIMEOUT_MS {
        return OtaGuard::Nothing;
    }
    if has_ip {
        OtaGuard::Rollback
    } else {
        OtaGuard::Confirm
    }
}

/// NTP 未同期で記録されたエントリの recorded_at_ms を、現在の壁時計と稼働時間の
/// 差から実時刻に直す。補正できるのは
///
/// - エントリが未同期時刻 (< MIN_SYNCED_MS) で、今は同期済み (>= MIN_SYNCED_MS)
/// - 同じ起動 (boot_id 一致) で、記録時の稼働時間を持っている
///
/// のとき。`実時刻 = 今の壁時計 - (今の稼働時間 - 記録時の稼働時間)`。
/// 稼働時間が逆転している (ありえないが) 場合は補正しない
pub fn corrected_recorded_at(
    entry: &QueueEntry,
    now_epoch_ms: u64,
    now_uptime_ms: u64,
    boot_id: u32,
) -> Option<u64> {
    if entry.recorded_at_ms >= MIN_SYNCED_MS || now_epoch_ms < MIN_SYNCED_MS {
        return None;
    }
    if entry.boot_id != Some(boot_id) {
        return None;
    }
    let uptime = entry.uptime_ms?;
    let elapsed = now_uptime_ms.checked_sub(uptime)?;
    Some(now_epoch_ms.saturating_sub(elapsed))
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
    let mut frame = json!({
        "type": "measurement",
        "seq": entry.seq,
        "recorded_at_ms": entry.recorded_at_ms,
        "kind": entry.kind,
        "payload": payload,
    });
    // None のときは key ごと省く (旧フレームと 1 バイトも変わらない形にする)
    if let Some(session_id) = &entry.session_id {
        frame["session_id"] = json!(session_id);
    }
    Ok(frame.to_string())
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

/// 下り command payload から Windows GW (alc-gw) ハブの WS URL を取り出す
/// (`{"action":"gw_url","url":"ws://<GW-IP>:9000"}`)。ws(s) 以外・欠落は None。
/// auth-worker /device/setup からの遠隔設定用 (シリアルの `GW URL` と同じ保存先)
pub fn command_gw_url(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let url = v.get("url")?.as_str()?;
    (url.starts_with("ws://") || url.starts_with("wss://")).then(|| url.to_string())
}

/// 下り command payload から印刷対象 PDF の URL を取り出す
/// (`{"action":"print","url":"https://..."}`、印刷ブリッジ #38)。
/// http(s) 以外・欠落は None。
pub fn command_print_url(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let url = v.get("url")?.as_str()?;
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_string())
}

/// `get_log` command (#195) の応答上限の既定 (バイト)。
pub const LOG_DEFAULT_BYTES: usize = 3000;
/// `get_log` command の応答上限の最大 (バイト)。command_result は NVS キュー
/// ([`MAX_LINE_BYTES`]) を通らず socket 直書きだが、JSON エスケープの膨張分の
/// 余裕を見てここで固定する。
pub const LOG_MAX_BYTES: usize = 3800;

/// 下り `get_log` command payload (`{"action":"get_log","max_bytes":N}`) の
/// `max_bytes` を取り出す。省略・整数でない値は [`LOG_DEFAULT_BYTES`]、
/// 指定があれば `1..=`[`LOG_MAX_BYTES`] にクランプする。
pub fn command_log_max_bytes(payload: &str) -> usize {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|v| v.get("max_bytes")?.as_i64())
        .map_or(LOG_DEFAULT_BYTES, |n| {
            n.clamp(1, LOG_MAX_BYTES as i64) as usize
        })
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

/// store のキーに使う seq の 16 進表記 (小文字)。NVS のキーは NUL 込み 16 バイト
/// = **15 文字**までなので、この長さの上限を [`MAX_KEY_LEN`] で固定する
pub fn seq_key(seq: u64) -> String {
    format!("{seq:x}")
}

/// NVS のキー長上限 (文字数、NUL を除く)
pub const MAX_KEY_LEN: usize = 15;

/// 1 件の行の上限バイト数。NVS の文字列は NUL 込み 4000 バイトまでなので、
/// これ以上の行は保存できない (push を Err で弾く)
pub const MAX_LINE_BYTES: usize = 4000;

/// 未 ack エントリの保存先。**キューの本体は flash 側のこれ**で、
/// [`UplinkQueue`] が RAM に持つのは送信窓 (先頭数件) と seq の索引だけ。
/// 保持件数は store の容量で決まり、RAM 使用量は件数に依存しない。
///
/// firmware では専用 NVS パーティション `punchq` の 1 件 1 キー実装、
/// パーティションを持たない機 (OTA だけで更新した機) では既定 nvs の文字列
/// 1 キー実装が入る (hub-drivers::punchq)。テストは [`MemStore`]。
pub trait UplinkStore {
    /// seq で 1 件保存する (同じ seq への上書きも put)。
    /// 容量不足なら `Err(StoreFull)` — 呼び側が最古を消して 1 回だけ再試行する
    fn put(&mut self, seq: u64, line: &str) -> Result<(), StoreFull>;
    /// 1 件消す。存在しない seq は無視してよい
    fn remove(&mut self, seq: u64);
    /// 1 件読む。無い / 読めないなら None
    fn get(&self, seq: u64) -> Option<String>;
    /// 保存済み seq の一覧 (順不同でよい。呼び側で昇順に並べる)。
    /// open 時に 1 回だけ呼ぶ
    fn seqs(&self) -> Vec<u64>;
}

/// 保存先の容量不足 (NVS の `ESP_ERR_NVS_NOT_ENOUGH_SPACE` など)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreFull;

/// テスト用の RAM 実装。`capacity` = 保持できる件数
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemStore {
    items: BTreeMap<u64, String>,
    capacity: usize,
}

impl MemStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: BTreeMap::new(),
            capacity,
        }
    }

    /// 保存されている行 (seq 昇順)。テストの確認用
    pub fn lines(&self) -> Vec<String> {
        self.items.values().cloned().collect()
    }
}

impl UplinkStore for MemStore {
    fn put(&mut self, seq: u64, line: &str) -> Result<(), StoreFull> {
        if !self.items.contains_key(&seq) && self.items.len() >= self.capacity {
            return Err(StoreFull);
        }
        self.items.insert(seq, line.to_string());
        Ok(())
    }

    fn remove(&mut self, seq: u64) {
        self.items.remove(&seq);
    }

    fn get(&self, seq: u64) -> Option<String> {
        self.items.get(&seq).cloned()
    }

    fn seqs(&self) -> Vec<u64> {
        self.items.keys().copied().collect()
    }
}

/// 容量確保のために捨てた最古のエントリ (`EVT WS_DROPPED <seq> <kind>` 用)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedEntry {
    pub seq: u64,
    /// 捨てた測定の種別。行が読めなかった場合は `"?"`
    pub kind: String,
}

/// ack の結果。`refilled` が 0 より大きいときは**窓に新しい送信対象が載った**ので、
/// 呼び側は再送周期を待たずに送ってよい (待つと flash に溜まった分の排出が
/// 「窓 20 件 × 再送周期」に律速される、Refs #142)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acked {
    /// 該当 seq がキューにあり消し込めた
    pub removed: bool,
    /// 空いた窓へ保存先から新しく読み込んだ件数
    pub refilled: usize,
}

/// push 成功。`dropped` は容量確保のために捨てた最古のエントリ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub seq: u64,
    pub dropped: Option<DroppedEntry>,
}

/// push 失敗。`dropped` が Some なら**捨てただけで新しい方も保存できていない**
/// (呼び側は両方をログに出す)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushFailed {
    pub reason: String,
    pub dropped: Option<DroppedEntry>,
}

/// 保存用の 1 行 JSON (parse_line と対)
pub fn line(entry: &QueueEntry) -> String {
    // payload は open/push で検証済みのため必ずオブジェクト
    let payload: Value = serde_json::from_str(&entry.payload).expect("validated payload");
    let mut line = json!({
        "seq": entry.seq,
        "recorded_at_ms": entry.recorded_at_ms,
        "kind": entry.kind,
        "payload": payload,
    });
    if let Some(session_id) = &entry.session_id {
        line["session_id"] = json!(session_id);
    }
    if let Some(uptime_ms) = entry.uptime_ms {
        line["uptime_ms"] = json!(uptime_ms);
    }
    if let Some(boot_id) = entry.boot_id {
        line["boot_id"] = json!(boot_id);
    }
    line.to_string()
}

/// 保存用の 1 行 JSON を QueueEntry へ戻す (line と対)。壊れた行は None
pub fn parse_line(line: &str) -> Option<QueueEntry> {
    let v: Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;
    Some(QueueEntry {
        seq: obj.get("seq")?.as_u64()?,
        recorded_at_ms: obj.get("recorded_at_ms")?.as_u64()?,
        kind: obj.get("kind")?.as_str()?.to_string(),
        payload: obj.get("payload").filter(|p| p.is_object())?.to_string(),
        // 旧フォーマット (session_id を持たない NVS データ) は None で復元する
        session_id: obj
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        // 同じく旧データは None (補正対象外になるだけ)
        uptime_ms: obj.get("uptime_ms").and_then(|v| v.as_u64()),
        boot_id: obj
            .get("boot_id")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok()),
    })
}

/// 送信キューの帳簿。実際の送受信は呼び出し側が行い、永続化は `store` が担う。
///
/// **flash がキューの本体、RAM は送信窓** (Refs #142): 未 ack エントリの実体は
/// すべて `store` にあり、RAM には「いま送ってよい先頭 `window` 件」と全 seq の
/// 索引 (u64 の Vec) だけを持つ。ack で窓が空けば store から次を読み込む
/// (`refill`)。保持件数を増やしても RAM 使用量はほぼ変わらないため、
/// PSRAM の有無に依存しない。
pub struct UplinkQueue {
    store: Box<dyn UplinkStore>,
    /// store 上の全 seq (昇順)
    index: Vec<u64>,
    /// 送信窓。**必ず `index` の先頭からの連続した prefix** を保つ
    entries: VecDeque<QueueEntry>,
    window: usize,
    /// 最後に採番した seq。**キューが空でも減らない・再利用しない**
    last_seq: u64,
    /// NTP 未同期で記録した (seq, boot_id)。fix_unsynced_times の対象を
    /// 索引全走査せずに引くために持つ
    unsynced: Vec<(u64, u32)>,
    /// **今の接続で送信済み**の seq (窓の中のものだけ。RAM のみで flash には
    /// 書かない)。ack 駆動の即時送信 (Refs #142) が、まだ ack 待ちの分まで
    /// 送り直さないようにするための印。再接続時は reset_sent で全部落とす
    sent: Vec<u64>,
}

impl UplinkQueue {
    /// 保存先を開いて復元する。`last_seq` は永続化済みの採番カウンタ。
    /// **seq の単調性のため `max(last_seq, store 上の最大 seq)` を採る** —
    /// store の切り替え (移行・フォールバック) をまたいで seq を再利用すると
    /// ack が別のエントリを消し込む。壊れた行は捨てる (戻り値 .1 = その件数)
    pub fn open(last_seq: u64, store: Box<dyn UplinkStore>, window: usize) -> (Self, usize) {
        let mut index = store.seqs();
        index.sort_unstable();
        let last_seq = last_seq.max(index.last().copied().unwrap_or(0));
        let mut queue = Self {
            store,
            index,
            entries: VecDeque::new(),
            window,
            last_seq,
            unsynced: Vec::new(),
            sent: Vec::new(),
        };
        let (_, skipped) = queue.refill();
        (queue, skipped)
    }

    /// 窓を `window` 件まで store から埋める。壊れて読めない行は store と索引から
    /// 落とす。戻り値は (新しく窓へ載せた件数, 壊れていて落とした件数)
    fn refill(&mut self) -> (usize, usize) {
        let mut loaded = 0;
        let mut skipped = 0;
        while self.entries.len() < self.window {
            let Some(&seq) = self.index.get(self.entries.len()) else {
                break;
            };
            match self.store.get(seq).as_deref().and_then(parse_line) {
                Some(entry) => {
                    self.entries.push_back(entry);
                    loaded += 1;
                }
                None => {
                    self.store.remove(seq);
                    self.index.remove(self.entries.len());
                    skipped += 1;
                }
            }
        }
        (loaded, skipped)
    }

    /// 1 件を store・索引・窓・未同期リストから消す
    fn forget(&mut self, seq: u64) {
        self.store.remove(seq);
        self.index.retain(|&s| s != seq);
        self.entries.retain(|e| e.seq != seq);
        self.unsynced.retain(|&(s, _)| s != seq);
        self.sent.retain(|&s| s != seq);
    }

    /// 最古の 1 件を捨てる (容量不足のとき。現行方針 = 新しい方を残す)
    fn drop_oldest(&mut self) -> Option<DroppedEntry> {
        let seq = *self.index.first()?;
        let kind = self
            .entries
            .front()
            .filter(|e| e.seq == seq)
            .map(|e| e.kind.clone())
            .or_else(|| {
                self.store
                    .get(seq)
                    .as_deref()
                    .and_then(parse_line)
                    .map(|e| e.kind)
            })
            .unwrap_or_else(|| "?".to_string());
        self.forget(seq);
        Some(DroppedEntry { seq, kind })
    }

    /// 測定を採番して積む。payload が不正・行が長すぎる・保存先が空かない場合は
    /// Err (そのとき採番はしない = seq を無駄にしない)
    pub fn push(
        &mut self,
        kind: &str,
        recorded_at_ms: u64,
        payload: &str,
    ) -> Result<Pushed, PushFailed> {
        self.push_with_session(kind, recorded_at_ms, payload, None)
    }

    /// 点呼セッション付きで積む (Refs #112)。`session_id` が None なら
    /// [`Self::push`] と完全に同じ (点呼外の単発計測)。
    pub fn push_with_session(
        &mut self,
        kind: &str,
        recorded_at_ms: u64,
        payload: &str,
        session_id: Option<&str>,
    ) -> Result<Pushed, PushFailed> {
        self.push_record(kind, recorded_at_ms, payload, session_id, None, None)
    }

    /// 時刻補正の足場 (記録時の稼働時間 + boot_id) 付きで積む。firmware の
    /// 通常経路はこちら (ws_uplink::enqueue)
    pub fn push_record(
        &mut self,
        kind: &str,
        recorded_at_ms: u64,
        payload: &str,
        session_id: Option<&str>,
        uptime_ms: Option<u64>,
        boot_id: Option<u32>,
    ) -> Result<Pushed, PushFailed> {
        // 正規化して保存する (line/parse_line の roundtrip をキー順に依らず
        // 一致させるため。measurement_frame にもこの正規化済み文字列が渡る)
        let payload = payload_object(payload).map_err(|reason| PushFailed {
            reason,
            dropped: None,
        })?;
        let seq = self.last_seq + 1;
        let entry = QueueEntry {
            seq,
            recorded_at_ms,
            kind: kind.to_string(),
            payload: payload.to_string(),
            session_id: session_id.map(str::to_string),
            uptime_ms,
            boot_id,
        };
        let text = line(&entry);
        if text.len() >= MAX_LINE_BYTES {
            return Err(PushFailed {
                reason: format!("1 行が保存上限を超えています ({} バイト)", text.len()),
                dropped: None,
            });
        }
        let mut dropped = None;
        if self.store.put(seq, &text).is_err() {
            // 容量不足: 最古を捨てて 1 回だけ再試行する
            dropped = self.drop_oldest();
            if self.store.put(seq, &text).is_err() {
                return Err(PushFailed {
                    reason: "保存先の容量不足で保存できません".to_string(),
                    dropped,
                });
            }
        }
        self.last_seq = seq;
        self.index.push(seq);
        if recorded_at_ms < MIN_SYNCED_MS && uptime_ms.is_some() {
            if let Some(boot_id) = boot_id {
                self.unsynced.push((seq, boot_id));
            }
        }
        // 窓が空いていて、新しい件が窓末尾のすぐ次なら窓へ直に載せる
        // (そうでなければ store から読み直す = 最古を捨てた後の穴埋め)
        if self.entries.len() < self.window && self.index.len() == self.entries.len() + 1 {
            self.entries.push_back(entry);
        } else {
            self.refill();
        }
        Ok(Pushed { seq, dropped })
    }

    /// 時計が同期すれば補正できるエントリ (この起動で未同期時刻のまま記録した
    /// もの) があるか。送信を NTP 同期まで待つかの判断に使う
    /// (should_wait_for_clock)。**再起動をまたいだ古いエントリは boot_id が
    /// 違うので対象外** — 索引を全走査せず未同期リストだけを見る
    pub fn has_correctable(&self, boot_id: u32) -> bool {
        self.unsynced.iter().any(|&(_, b)| b == boot_id)
    }

    /// NTP 未同期で記録されたエントリの時刻を実時刻へ直す
    /// (corrected_recorded_at)。**store 側も書き戻す**ので、直後に電源が落ちても
    /// 補正は残る。戻り値は補正した件数
    pub fn fix_unsynced_times(
        &mut self,
        now_epoch_ms: u64,
        now_uptime_ms: u64,
        boot_id: u32,
    ) -> usize {
        let targets: Vec<u64> = self
            .unsynced
            .iter()
            .filter(|&&(_, b)| b == boot_id)
            .map(|&(s, _)| s)
            .collect();
        let mut fixed = 0;
        for seq in targets {
            let Some(mut entry) = self.store.get(seq).as_deref().and_then(parse_line) else {
                // 読めない行は索引ごと落とす (refill と同じ扱い)
                self.forget(seq);
                self.refill();
                continue;
            };
            let Some(at) = corrected_recorded_at(&entry, now_epoch_ms, now_uptime_ms, boot_id)
            else {
                continue;
            };
            entry.recorded_at_ms = at;
            if self.store.put(seq, &line(&entry)).is_err() {
                continue;
            }
            if let Some(windowed) = self.entries.iter_mut().find(|e| e.seq == seq) {
                windowed.recorded_at_ms = at;
            }
            self.unsynced.retain(|&(s, _)| s != seq);
            fixed += 1;
        }
        fixed
    }

    /// ack された seq を消し込み、空いた窓を store から埋める。
    /// **窓に新しく載った件数も返す** — 呼び側はそれが 0 より大きいとき、
    /// 再送周期を待たずに送ることで溜まった分を連続排出できる (Refs #142)
    pub fn ack(&mut self, seq: u64) -> Acked {
        if !self.index.contains(&seq) {
            return Acked {
                removed: false,
                refilled: 0,
            };
        }
        self.forget(seq);
        let (refilled, _) = self.refill();
        Acked {
            removed: true,
            refilled,
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// **保存先にある未 ack の総件数** (窓の大きさではない)。STATUS / ログ用
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// いま送ってよい未 ack エントリ (古い順、最大 window 件)。
    /// 再送も同じ seq で行う
    pub fn entries(&self) -> impl Iterator<Item = &QueueEntry> {
        self.entries.iter()
    }

    /// 窓のうち**まだこの接続で送っていない**エントリ (古い順)。
    /// ack で窓が埋まったときの即時送信はこれだけを送る — 窓の全件を送ると
    /// ack 1 件ごとに ack 待ちの最大 window-1 件も送り直すことになる (Refs #142)
    pub fn entries_unsent(&self) -> impl Iterator<Item = &QueueEntry> {
        self.entries.iter().filter(|e| !self.sent.contains(&e.seq))
    }

    /// 送信済みの印を付ける (窓から外れた seq への呼び出しは forget が掃除する)
    pub fn mark_sent(&mut self, seq: u64) {
        if !self.sent.contains(&seq) {
            self.sent.push(seq);
        }
    }

    /// 全部を未送信に戻す。**再接続時に呼ぶ** — 前の接続で送った分はサーバに
    /// 届いたか分からないので、改めて全件送り直す
    pub fn reset_sent(&mut self) {
        self.sent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    const PAYLOAD: &str = r#"{"type":"temperature","value":36.5,"unit":"celsius"}"#;

    #[test]
    fn measurement_frame_embeds_payload_as_object() {
        let e = QueueEntry {
            seq: 3,
            recorded_at_ms: 1_752_300_000_000,
            kind: "temperature".into(),
            payload: PAYLOAD.into(),
            session_id: None,
            uptime_ms: None,
            boot_id: None,
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
    fn measurement_frame_omits_session_id_when_absent_and_emits_it_when_present() {
        // 点呼外の単発計測: key ごと出さない (旧フレームと同一 = 旧サーバでも壊れない)
        let mut e = QueueEntry {
            seq: 3,
            recorded_at_ms: 1_752_300_000_000,
            kind: "temperature".into(),
            payload: PAYLOAD.into(),
            session_id: None,
            uptime_ms: None,
            boot_id: None,
        };
        let v: Value = serde_json::from_str(&measurement_frame(&e).unwrap()).unwrap();
        assert!(v.as_object().unwrap().get("session_id").is_none());

        // 点呼中: そのまま載る
        e.session_id = Some("7-1".into());
        let v: Value = serde_json::from_str(&measurement_frame(&e).unwrap()).unwrap();
        assert_eq!(v["session_id"], "7-1");
    }
    #[test]
    fn measurement_frame_rejects_bad_payload() {
        let mut e = QueueEntry {
            seq: 1,
            recorded_at_ms: 0,
            kind: "k".into(),
            payload: "{oops".into(),
            session_id: None,
            uptime_ms: None,
            boot_id: None,
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
    fn command_gw_url_requires_ws_scheme() {
        assert_eq!(
            command_gw_url(r#"{"action":"gw_url","url":"ws://192.168.11.5:9000"}"#),
            Some("ws://192.168.11.5:9000".into())
        );
        assert_eq!(
            command_gw_url(r#"{"action":"gw_url","url":"wss://gw.example:9000"}"#),
            Some("wss://gw.example:9000".into())
        );
        assert_eq!(
            command_gw_url(r#"{"action":"gw_url","url":"http://x:9000"}"#),
            None
        );
        assert_eq!(command_gw_url(r#"{"action":"gw_url","url":1}"#), None);
        assert_eq!(command_gw_url(r#"{"action":"gw_url"}"#), None);
        assert_eq!(command_gw_url("{oops"), None);
    }

    #[test]
    fn command_log_max_bytes_defaults_and_clamps() {
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log"}"#),
            LOG_DEFAULT_BYTES
        );
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log","max_bytes":500}"#),
            500
        );
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log","max_bytes":99999}"#),
            LOG_MAX_BYTES
        );
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log","max_bytes":0}"#),
            1
        );
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log","max_bytes":-5}"#),
            1
        );
        // 整数でない指定 (文字列 / 小数) は既定に落とす
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log","max_bytes":"3"}"#),
            LOG_DEFAULT_BYTES
        );
        assert_eq!(
            command_log_max_bytes(r#"{"action":"get_log","max_bytes":1.5}"#),
            LOG_DEFAULT_BYTES
        );
        assert_eq!(command_log_max_bytes("{oops"), LOG_DEFAULT_BYTES);
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
    fn entry(recorded_at_ms: u64, uptime_ms: Option<u64>, boot_id: Option<u32>) -> QueueEntry {
        QueueEntry {
            seq: 1,
            recorded_at_ms,
            kind: "alcohol".into(),
            payload: PAYLOAD.into(),
            session_id: None,
            uptime_ms,
            boot_id,
        }
    }

    const SYNCED: u64 = 1_788_000_000_000; // 2026-08 頃

    #[test]
    fn corrected_time_uses_uptime_delta_within_same_boot() {
        // 稼働 5,000ms で記録 (1970 起点 = 未同期)、稼働 65,000ms で同期済み送信
        let e = entry(5_000, Some(5_000), Some(7));
        assert_eq!(
            corrected_recorded_at(&e, SYNCED, 65_000, 7),
            Some(SYNCED - 60_000)
        );
    }

    #[test]
    fn corrected_time_not_applied_when_already_synced_or_still_unsynced() {
        let synced_entry = entry(SYNCED - 1_000, Some(5_000), Some(7));
        assert_eq!(corrected_recorded_at(&synced_entry, SYNCED, 65_000, 7), None);
        let e = entry(5_000, Some(5_000), Some(7));
        // 今もまだ未同期 (稼働時間のまま) なら直せない
        assert_eq!(corrected_recorded_at(&e, 65_000, 65_000, 7), None);
    }

    #[test]
    fn corrected_time_requires_same_boot_and_uptime() {
        let other_boot = entry(5_000, Some(5_000), Some(6));
        assert_eq!(corrected_recorded_at(&other_boot, SYNCED, 65_000, 7), None);
        let legacy = entry(5_000, None, None);
        assert_eq!(corrected_recorded_at(&legacy, SYNCED, 65_000, 7), None);
        let no_uptime = entry(5_000, None, Some(7));
        assert_eq!(corrected_recorded_at(&no_uptime, SYNCED, 65_000, 7), None);
        // 稼働時間が逆転 (記録時 > 今) なら補正しない
        let future = entry(5_000, Some(70_000), Some(7));
        assert_eq!(corrected_recorded_at(&future, SYNCED, 65_000, 7), None);
    }

    // ---- 送信キュー (flash 本体 + 送信窓、Refs #142) ----

    /// テスト用の store。中身を覗け、読み出し失敗・書き込み失敗を注入できる
    /// (実機の NVS が返すエラー経路を再現する)。`UplinkQueue` は Box で所有するので
    /// 検査用にクローンを 1 つ手元へ残す
    #[derive(Clone, Default)]
    struct TestStore {
        inner: Rc<RefCell<MemStore>>,
        /// get が None を返す seq (0 = 無し。seq は 1 始まりなので衝突しない)
        unreadable: Rc<Cell<u64>>,
        /// true の間 put が必ず StoreFull
        put_fails: Rc<Cell<bool>>,
    }

    impl TestStore {
        fn new(capacity: usize) -> Self {
            Self {
                inner: Rc::new(RefCell::new(MemStore::new(capacity))),
                ..Default::default()
            }
        }

        /// 保存されている行 (seq 昇順)
        fn lines(&self) -> Vec<String> {
            self.inner.borrow().lines()
        }

        /// 生の行を直に置く (旧フォーマット / 壊れた行の再現)
        fn seed(&self, seq: u64, line: &str) {
            self.inner.borrow_mut().put(seq, line).unwrap();
        }
    }

    impl UplinkStore for TestStore {
        fn put(&mut self, seq: u64, line: &str) -> Result<(), StoreFull> {
            if self.put_fails.get() {
                return Err(StoreFull);
            }
            self.inner.borrow_mut().put(seq, line)
        }

        fn remove(&mut self, seq: u64) {
            self.inner.borrow_mut().remove(seq);
        }

        fn get(&self, seq: u64) -> Option<String> {
            if self.unreadable.get() == seq {
                return None;
            }
            self.inner.borrow().get(seq)
        }

        fn seqs(&self) -> Vec<u64> {
            self.inner.borrow().seqs()
        }
    }

    fn open_queue(store: &TestStore, window: usize) -> UplinkQueue {
        let (q, skipped) = UplinkQueue::open(0, Box::new(store.clone()), window);
        assert_eq!(skipped, 0);
        q
    }

    /// seq だけ差し替えた保存行
    fn stored_line(seq: u64) -> String {
        line(&QueueEntry {
            seq,
            recorded_at_ms: 100 + seq,
            kind: "timecard".into(),
            payload: PAYLOAD.into(),
            session_id: None,
            uptime_ms: None,
            boot_id: None,
        })
    }

    #[test]
    fn open_sorts_index_and_takes_max_seq() {
        let store = TestStore::new(10);
        // seqs() が順不同で返っても昇順に並べ直す
        for seq in [3u64, 1, 2] {
            store.seed(seq, &stored_line(seq));
        }
        let (q, skipped) = UplinkQueue::open(0, Box::new(store.clone()), 2);
        assert_eq!(skipped, 0);
        // flash 上は 3 件、窓は 2 件まで
        assert_eq!(q.len(), 3);
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        // 保存済み last_seq が無くても store 上の最大 seq を引き継ぐ
        assert_eq!(q.last_seq(), 3);
        // 保存済み last_seq の方が大きければそちらを保つ
        let (q, _) = UplinkQueue::open(9, Box::new(store.clone()), 2);
        assert_eq!(q.last_seq(), 9);
        assert!(!q.is_empty());
    }

    #[test]
    fn window_stays_bounded_and_refills_on_ack() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        for i in 1..=4 {
            q.push("timecard", i, PAYLOAD).unwrap();
        }
        // flash は 4 件、RAM の窓は 2 件
        assert_eq!(q.len(), 4);
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        // ack すると次が窓へ載り、「新しく載った」ことが呼び側へ伝わる
        // (呼び側はこれを見て再送周期を待たずに送る = 溜まった分の連続排出)
        assert_eq!(
            q.ack(1),
            Acked {
                removed: true,
                refilled: 1
            }
        );
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(q.ack(2).refilled, 1);
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4]);
        // 二重 ack / 未知の seq は消し込みも補充もしない
        assert_eq!(
            q.ack(2),
            Acked {
                removed: false,
                refilled: 0
            }
        );
        assert_eq!(q.len(), 2);
        assert_eq!(store.lines().len(), 2);
        // 保存先に残りが無ければ補充は 0 (呼び側は即送信しない)
        assert_eq!(
            q.ack(3),
            Acked {
                removed: true,
                refilled: 0
            }
        );
        // 空になっても seq は戻らない
        assert!(q.ack(4).removed);
        assert!(q.is_empty());
        assert_eq!(q.push("timecard", 5, PAYLOAD).unwrap().seq, 5);
        assert_eq!(q.last_seq(), 5);
    }

    #[test]
    fn push_full_store_drops_oldest_then_retries() {
        let store = TestStore::new(3);
        let mut q = open_queue(&store, 2);
        for i in 1..=3 {
            assert_eq!(q.push("timecard", i, PAYLOAD).unwrap().dropped, None);
        }
        // 4 件目で容量不足 → 最古 (seq=1) を捨てて 1 回だけ再試行する
        let pushed = q.push("timecard", 4, PAYLOAD).unwrap();
        assert_eq!(pushed.seq, 4);
        assert_eq!(
            pushed.dropped,
            Some(DroppedEntry {
                seq: 1,
                kind: "timecard".into()
            })
        );
        assert_eq!(q.len(), 3);
        // 窓は index の先頭 2 件のまま (捨てた穴は store から埋め直す)
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn push_fails_when_store_never_accepts() {
        // 容量 0 の store: 捨てる最古すら無いので dropped は None
        let store = TestStore::new(0);
        let mut q = open_queue(&store, 2);
        let err = q.push("timecard", 1, PAYLOAD).unwrap_err();
        assert!(err.reason.contains("容量不足"), "{}", err.reason);
        assert_eq!(err.dropped, None);
        // 失敗時は採番しない (seq を無駄に進めない)
        assert_eq!(q.last_seq(), 0);
        assert!(q.is_empty());

        // 1 件入ったあとに store 全体が書けなくなった場合は、最古を捨てても失敗する
        let store = TestStore::new(5);
        let mut q = open_queue(&store, 2);
        q.push("timecard", 1, PAYLOAD).unwrap();
        store.put_fails.set(true);
        let err = q.push("timecard", 2, PAYLOAD).unwrap_err();
        assert_eq!(
            err.dropped,
            Some(DroppedEntry {
                seq: 1,
                kind: "timecard".into()
            })
        );
        assert!(q.is_empty());
    }

    #[test]
    fn dropped_kind_comes_from_store_when_outside_window() {
        // 窓 0 件 = 捨てる対象が RAM に無い → store の行から kind を引く
        let store = TestStore::new(1);
        let mut q = open_queue(&store, 0);
        q.push("timecard", 1, PAYLOAD).unwrap();
        let pushed = q.push("temperature", 2, PAYLOAD).unwrap();
        assert_eq!(
            pushed.dropped,
            Some(DroppedEntry {
                seq: 1,
                kind: "timecard".into()
            })
        );

        // 行が壊れていて kind が読めないときは "?"
        let store = TestStore::new(1);
        store.seed(7, "garbage");
        let (mut q, skipped) = UplinkQueue::open(0, Box::new(store.clone()), 0);
        assert_eq!(skipped, 0); // 窓 0 件なので open では読まない
        let pushed = q.push("timecard", 1, PAYLOAD).unwrap();
        assert_eq!(
            pushed.dropped,
            Some(DroppedEntry {
                seq: 7,
                kind: "?".into()
            })
        );
        assert_eq!(pushed.seq, 8); // last_seq は store 上の最大 seq を引き継いでいる
    }

    #[test]
    fn corrupt_lines_are_dropped_on_open_and_on_refill() {
        let store = TestStore::new(10);
        store.seed(1, &stored_line(1));
        store.seed(2, "garbage");
        store.seed(3, &stored_line(3));
        // open: 窓を埋める途中で壊れた行を索引ごと落とす
        let (q, skipped) = UplinkQueue::open(0, Box::new(store.clone()), 3);
        assert_eq!(skipped, 1);
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(q.len(), 2);

        // refill (ack 後の穴埋め) でも同じ
        let store = TestStore::new(10);
        store.seed(1, &stored_line(1));
        store.seed(2, "garbage");
        store.seed(3, &stored_line(3));
        let mut q = open_queue(&store, 1);
        // 壊れた行を飛ばして次の行が載るので refilled は 1
        assert_eq!(
            q.ack(1),
            Acked {
                removed: true,
                refilled: 1
            }
        );
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![3]);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn push_rejects_line_over_nvs_string_limit() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        let big = format!(r#"{{"note":"{}"}}"#, "a".repeat(MAX_LINE_BYTES));
        let err = q.push("timecard", 1, &big).unwrap_err();
        assert!(err.reason.contains("保存上限"), "{}", err.reason);
        assert_eq!(err.dropped, None);
        assert_eq!(q.last_seq(), 0);
        assert!(store.lines().is_empty());
    }

    #[test]
    fn seq_key_is_lowercase_hex_within_nvs_key_limit() {
        assert_eq!(seq_key(0), "0");
        assert_eq!(seq_key(255), "ff");
        assert_eq!(seq_key(1_000_000_000), "3b9aca00");
        // NVS のキーは 15 文字まで。実運用の seq (打刻 1 件 1 採番) は
        // 16^15 に遠く届かないが、境界を固定しておく
        assert_eq!(seq_key(u64::MAX >> 4).len(), MAX_KEY_LEN);
        assert!(seq_key(u64::MAX).len() > MAX_KEY_LEN);
    }

    #[test]
    fn queue_rejects_bad_payload() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        let err = q.push("k", 0, "not json").unwrap_err();
        assert!(err.reason.contains("JSON"), "{}", err.reason);
        assert!(q.is_empty());
        assert_eq!(q.last_seq(), 0); // 失敗時は採番しない
    }

    #[test]
    fn push_with_session_roundtrips_through_store() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 10);
        q.push_with_session("alcohol", 100, PAYLOAD, Some("7-1"))
            .unwrap();
        q.push_with_session("temperature", 200, PAYLOAD, Some("7-1"))
            .unwrap();
        // 点呼外の単発は None のまま
        q.push("temperature", 300, PAYLOAD).unwrap();

        // 再起動を模して同じ store から開き直す
        let restored = open_queue(&store, 10);
        let ids: Vec<Option<String>> = restored.entries().map(|e| e.session_id.clone()).collect();
        assert_eq!(
            ids,
            vec![Some("7-1".into()), Some("7-1".into()), None],
            "保存先の復元でセッションが失われてはならない"
        );
        assert_eq!(restored.last_seq(), 3);
    }

    #[test]
    fn open_accepts_old_lines_without_session_id() {
        // session_id を知らない旧ファームが書いた行を復元しても壊れない
        let store = TestStore::new(10);
        store.seed(
            1,
            &format!(r#"{{"seq":1,"recorded_at_ms":100,"kind":"alcohol","payload":{PAYLOAD}}}"#),
        );
        let q = open_queue(&store, 10);
        assert_eq!(q.entries().count(), 1);
        assert_eq!(q.entries().next().unwrap().session_id, None);
    }

    #[test]
    fn uptime_and_boot_id_roundtrip_through_stored_lines() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 10);
        q.push_record("alcohol", 5_000, PAYLOAD, None, Some(5_000), Some(7))
            .unwrap();
        q.push_with_session("alcohol", 6_000, PAYLOAD, None).unwrap();
        let lines = store.lines();
        assert!(lines[0].contains("\"uptime_ms\":5000") && lines[0].contains("\"boot_id\":7"));
        assert!(!lines[1].contains("uptime_ms") && !lines[1].contains("boot_id"));
        // フレームには出さない (サーバ形式は据え置き)
        let restored = open_queue(&store, 10);
        let frame = measurement_frame(restored.entries().next().unwrap()).unwrap();
        assert!(!frame.contains("uptime_ms") && !frame.contains("boot_id"));
    }

    #[test]
    fn fix_unsynced_times_rewrites_window_and_store() {
        let store = TestStore::new(10);
        // 窓 1 件 = 補正対象が窓の外にも居る状態にする
        let mut q = open_queue(&store, 1);
        q.push_record("alcohol", 5_000, PAYLOAD, None, Some(5_000), Some(7))
            .unwrap();
        q.push_record("temperature", 9_000, PAYLOAD, Some("7-1"), Some(9_000), Some(6))
            .unwrap();
        q.push_record("alcohol", SYNCED, PAYLOAD, None, Some(20_000), Some(7))
            .unwrap();
        // まだ時計が同期していないうちは何も直さない (直す材料が無い)
        assert_eq!(q.fix_unsynced_times(50_000, 65_000, 7), 0);
        assert_eq!(q.fix_unsynced_times(SYNCED + 100_000, 65_000, 7), 1);
        // 窓 (seq=1) が直っている
        assert_eq!(
            q.entries().next().unwrap().recorded_at_ms,
            SYNCED + 40_000
        );
        // store 側も書き戻されている (直後に電源が落ちても補正が残る)
        assert!(store.lines()[0].contains(&format!("\"recorded_at_ms\":{}", SYNCED + 40_000)));
        assert!(store.lines()[1].contains("\"recorded_at_ms\":9000"));
        // 2 回目は補正対象が残っていない
        assert_eq!(q.fix_unsynced_times(SYNCED + 100_000, 65_000, 7), 0);
    }

    #[test]
    fn fix_unsynced_times_survives_store_failures() {
        // 書き戻しに失敗した分は「補正済み」に数えない (次の周期で再試行できる)
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        q.push_record("alcohol", 5_000, PAYLOAD, None, Some(5_000), Some(7))
            .unwrap();
        store.put_fails.set(true);
        assert_eq!(q.fix_unsynced_times(SYNCED, 65_000, 7), 0);
        store.put_fails.set(false);
        assert_eq!(q.fix_unsynced_times(SYNCED, 65_000, 7), 1);

        // 読めなくなった行は索引ごと落とす (窓も詰め直す)
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 1);
        q.push_record("alcohol", 5_000, PAYLOAD, None, Some(5_000), Some(7))
            .unwrap();
        q.push("timecard", SYNCED, PAYLOAD).unwrap();
        store.unreadable.set(1);
        assert_eq!(q.fix_unsynced_times(SYNCED, 65_000, 7), 0);
        assert_eq!(q.len(), 1);
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn has_correctable_and_wait_for_clock() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        assert!(!q.has_correctable(7));
        // 旧データ (足場なし) / 別起動 / 同期済み は対象外
        q.push_with_session("alcohol", 5_000, PAYLOAD, None).unwrap();
        q.push_record("alcohol", 5_000, PAYLOAD, None, Some(5_000), Some(6))
            .unwrap();
        q.push_record("alcohol", SYNCED, PAYLOAD, None, Some(5_000), Some(7))
            .unwrap();
        assert!(!q.has_correctable(7));
        q.push_record("temperature", 23_000, PAYLOAD, None, Some(23_000), Some(7))
            .unwrap();
        assert!(q.has_correctable(7));
        // ack すれば未同期リストからも消える
        assert!(q.ack(4).removed);
        assert!(!q.has_correctable(7));

        // 未同期 + 補正候補あり + 接続直後 → 待つ
        assert!(should_wait_for_clock(23_000, 0, true));
        assert!(should_wait_for_clock(23_000, CLOCK_WAIT_MS - 1, true));
        // 待ち時間超過 / 候補なし / 同期済み → 送る
        assert!(!should_wait_for_clock(23_000, CLOCK_WAIT_MS, true));
        assert!(!should_wait_for_clock(23_000, 0, false));
        assert!(!should_wait_for_clock(SYNCED, 0, true));
    }

    #[test]
    fn unsent_marks_track_the_window_not_the_flash() {
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        for i in 1..=4 {
            q.push("timecard", i, PAYLOAD).unwrap();
        }
        let unsent = |q: &UplinkQueue| q.entries_unsent().map(|e| e.seq).collect::<Vec<_>>();
        // 積んだ直後はどれも未送信
        assert_eq!(unsent(&q), vec![1, 2]);
        // 送った分だけ落ちる (二重の mark_sent は増やさない)
        q.mark_sent(1);
        q.mark_sent(1);
        assert_eq!(unsent(&q), vec![2]);
        q.mark_sent(2);
        assert!(unsent(&q).is_empty());
        // ack で窓に載った次のぶんは未送信 = 即時送信の対象になる
        assert_eq!(q.ack(1).refilled, 1);
        assert_eq!(unsent(&q), vec![3]);
        // 送信済みの印は窓の中だけの話で、entries() 側は変わらない
        assert_eq!(q.entries().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
        // 再接続: 全部を送り直す
        q.mark_sent(3);
        assert!(unsent(&q).is_empty());
        q.reset_sent();
        assert_eq!(unsent(&q), vec![2, 3]);
    }

    #[test]
    fn ack_clears_the_sent_mark_so_seq_reuse_cannot_hide_an_entry() {
        // ack で消えた seq の印が残っていると、万一同じ seq が窓へ戻ったときに
        // 「送信済み」と誤認して永久に送られない。forget が印も落とすことを固定する
        let store = TestStore::new(10);
        let mut q = open_queue(&store, 2);
        q.push("timecard", 1, PAYLOAD).unwrap();
        q.mark_sent(1);
        assert!(q.entries_unsent().next().is_none());
        assert!(q.ack(1).removed);
        // 同じ seq の行を保存先へ戻して開き直しても未送信として扱われる
        store.seed(1, &stored_line(1));
        let q = open_queue(&store, 2);
        assert_eq!(q.entries_unsent().map(|e| e.seq).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn mem_store_is_a_bounded_map() {
        // punchq / legacy と同じ契約を RAM で満たす参照実装
        let mut s = MemStore::new(1);
        assert!(s.put(2, "b").is_ok());
        // 同じ seq への上書きは容量を消費しない
        assert!(s.put(2, "B").is_ok());
        assert_eq!(s.put(3, "c"), Err(StoreFull));
        assert_eq!(s.get(2).as_deref(), Some("B"));
        assert_eq!(s.get(3), None);
        assert_eq!(s.seqs(), vec![2]);
        assert_eq!(s.lines(), vec!["B".to_string()]);
        s.remove(2);
        assert!(s.seqs().is_empty());
    }

    #[test]
    fn ota_guard_does_nothing_unless_pending_and_never_connected() {
        // USB で焼いた機 / 確定済みの機
        assert_eq!(
            ota_guard(false, true, true, false, OTA_VERIFY_TIMEOUT_MS),
            OtaGuard::Nothing
        );
        // 一度でも WS が繋がった (確定は接続イベントで済んでいる)
        assert_eq!(
            ota_guard(true, true, true, true, OTA_VERIFY_TIMEOUT_MS),
            OtaGuard::Nothing
        );
    }

    #[test]
    fn ota_guard_confirms_unpaired_device_at_once() {
        // 未登録の機は WS で判定できないので起動直後に確定する
        assert_eq!(ota_guard(true, false, false, false, 0), OtaGuard::Confirm);
    }

    #[test]
    fn ota_guard_waits_until_timeout_then_rolls_back_only_with_ip() {
        let before = OTA_VERIFY_TIMEOUT_MS - 1;
        assert_eq!(ota_guard(true, true, true, false, before), OtaGuard::Nothing);
        assert_eq!(ota_guard(true, true, false, false, before), OtaGuard::Nothing);
        // IP があるのに繋がらない = image のせいとみなして戻す
        assert_eq!(
            ota_guard(true, true, true, false, OTA_VERIFY_TIMEOUT_MS),
            OtaGuard::Rollback
        );
        // IP が無い = 判定できないので捨てずに確定する
        assert_eq!(
            ota_guard(true, true, false, false, OTA_VERIFY_TIMEOUT_MS),
            OtaGuard::Confirm
        );
    }
}
