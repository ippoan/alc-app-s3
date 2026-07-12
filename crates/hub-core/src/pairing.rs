//! auth-worker device pairing の純粋部分 (JSON 組立/解析 + ポーリング計画)。
//!
//! HTTP 送受信・NVS 保存・画面遷移などの副作用は firmware 側 (auth_link.rs) が
//! 担い、ここでは auth-worker の headless pairing API
//! (`/device/pair/start` → `/device/pair/token` poll → `/device/token`) の
//! リクエスト本文とレスポンス解釈のみを行う (ippoan/alc-app-s3#20)。
//!
//! エラーはそのままホストへ返せる日本語メッセージ。serde derive ではなく
//! 手動で組み立て/分解する (cfg.rs と同方針 — llvm-cov のライン網羅を
//! 自前コードで保証するため)。

use serde_json::{json, Map, Value};

/// `POST /device/pair/start` の応答 (201)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairStart {
    /// box が保持する秘密。`/device/pair/token` の poll に使う
    pub device_code: String,
    /// 承認ページで人間が照合する短い符号 (`XXXX-XXXX`)
    pub user_code: String,
    /// 承認ページ URL (user_code プリフィル付きがあれば優先)
    pub verification_uri: String,
    /// pairing の有効期限 [秒]
    pub expires_in_s: u64,
    /// poll 間隔 [秒]
    pub interval_s: u64,
}

/// 承認後に 1 回だけ受け取れる device credential。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCredential {
    pub device_id: String,
    pub device_secret: String,
    pub tenant_id: String,
}

/// `POST /device/pair/token` (poll) の応答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollResult {
    /// 未承認 — interval 後に再 poll
    Pending,
    /// 承認済み — credential を保存する (この応答は 1 回限り)
    Approved(DeviceCredential),
    /// credential 受領済みの device_code で再 poll した (410)
    Consumed,
    /// 有効期限切れ (410)
    Expired,
}

/// `POST /device/token` の応答 (短命 device JWT)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceToken {
    pub access_token: String,
    pub expires_in_s: u64,
    pub tenant_id: String,
}

/// `POST /device/pair/start` のリクエスト本文。
pub fn start_request_body(label: &str, role: &str) -> String {
    json!({ "label": label, "role": role }).to_string()
}

/// `POST /device/pair/token` のリクエスト本文。
pub fn poll_request_body(device_code: &str) -> String {
    json!({ "device_code": device_code }).to_string()
}

/// `POST /device/token` のリクエスト本文。
pub fn token_request_body(device_id: &str, device_secret: &str) -> String {
    json!({ "device_id": device_id, "device_secret": device_secret }).to_string()
}

/// 応答本文を JSON オブジェクトとして読む。`{"error":...}` はエラーに変換する。
fn parse_object(s: &str) -> Result<Map<String, Value>, String> {
    let v: Value = serde_json::from_str(s).map_err(|e| format!("JSON 解析失敗: {e}"))?;
    let obj = v.as_object().ok_or("JSON オブジェクトではありません")?;
    if let Some(err) = obj.get("error").and_then(|e| e.as_str()) {
        return Err(format!("サーバエラー: {err}"));
    }
    Ok(obj.clone())
}

/// 必須の文字列フィールドを取り出す。
fn str_field(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} (文字列) がありません"))
}

/// 必須の非負整数フィールドを取り出す。
fn u64_field(obj: &Map<String, Value>, key: &str) -> Result<u64, String> {
    obj.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("{key} (数値) がありません"))
}

/// `POST /device/pair/start` の応答を解釈する。
pub fn parse_start_response(body: &str) -> Result<PairStart, String> {
    let obj = parse_object(body)?;
    let verification_uri = match obj.get("verification_uri_complete").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => str_field(&obj, "verification_uri")?,
    };
    Ok(PairStart {
        device_code: str_field(&obj, "device_code")?,
        user_code: str_field(&obj, "user_code")?,
        verification_uri,
        expires_in_s: u64_field(&obj, "expires_in")?,
        interval_s: u64_field(&obj, "interval")?,
    })
}

/// `POST /device/pair/token` (poll) の応答を解釈する。
pub fn parse_poll_response(body: &str) -> Result<PollResult, String> {
    let obj = parse_object(body)?;
    match obj.get("status").and_then(|v| v.as_str()) {
        Some("pending") => Ok(PollResult::Pending),
        Some("approved") => Ok(PollResult::Approved(DeviceCredential {
            device_id: str_field(&obj, "device_id")?,
            device_secret: str_field(&obj, "device_secret")?,
            tenant_id: str_field(&obj, "tenant_id")?,
        })),
        Some("consumed") => Ok(PollResult::Consumed),
        Some("expired") => Ok(PollResult::Expired),
        Some(other) => Err(format!("不明な status: {other}")),
        None => Err("status がありません".into()),
    }
}

/// `POST /device/token` の応答を解釈する。
pub fn parse_token_response(body: &str) -> Result<DeviceToken, String> {
    let obj = parse_object(body)?;
    Ok(DeviceToken {
        access_token: str_field(&obj, "access_token")?,
        expires_in_s: u64_field(&obj, "expires_in")?,
        tenant_id: str_field(&obj, "tenant_id")?,
    })
}

/// poll の実行計画 (いつ poll するか / いつ諦めるか)。
///
/// 実時間の取得・待機は呼び出し側 (auth_link) が行い、本構造体は
/// 「now_ms を与えられたら判定する」だけの純粋ロジック。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollSchedule {
    next_at_ms: u64,
    deadline_ms: u64,
    interval_ms: u64,
}

impl PollSchedule {
    /// interval は最低 1 秒に丸める (サーバ応答の異常値で busy loop しない)。
    pub fn new(now_ms: u64, expires_in_s: u64, interval_s: u64) -> Self {
        let interval_ms = interval_s.max(1) * 1000;
        Self {
            next_at_ms: now_ms + interval_ms,
            deadline_ms: now_ms + expires_in_s * 1000,
            interval_ms,
        }
    }

    /// pairing 自体の有効期限が切れたか
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms >= self.deadline_ms
    }

    /// 次の poll 時刻に達したか
    pub fn due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_at_ms
    }

    /// poll 実行後に次回時刻を進める
    pub fn advance(&mut self, now_ms: u64) {
        self.next_at_ms = now_ms + self.interval_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bodies() {
        assert_eq!(
            start_request_body("cores3-abc", "device-hub"),
            r#"{"label":"cores3-abc","role":"device-hub"}"#
        );
        assert_eq!(poll_request_body("dc1"), r#"{"device_code":"dc1"}"#);
        assert_eq!(
            token_request_body("id1", "sec1"),
            r#"{"device_id":"id1","device_secret":"sec1"}"#
        );
    }

    #[test]
    fn start_response_full() {
        let body = r#"{
            "device_code": "dc-secret",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://auth.example/device/pair/approve",
            "verification_uri_complete": "https://auth.example/device/pair/approve?user_code=ABCD-EFGH",
            "expires_in": 600,
            "interval": 5
        }"#;
        let p = parse_start_response(body).unwrap();
        assert_eq!(p.device_code, "dc-secret");
        assert_eq!(p.user_code, "ABCD-EFGH");
        assert_eq!(
            p.verification_uri,
            "https://auth.example/device/pair/approve?user_code=ABCD-EFGH"
        );
        assert_eq!(p.expires_in_s, 600);
        assert_eq!(p.interval_s, 5);
    }

    #[test]
    fn start_response_falls_back_to_verification_uri() {
        let body = r#"{"device_code":"d","user_code":"AAAA-BBBB",
            "verification_uri":"https://auth.example/approve","expires_in":600,"interval":5}"#;
        let p = parse_start_response(body).unwrap();
        assert_eq!(p.verification_uri, "https://auth.example/approve");
        // complete が空文字列でも fallback する
        let body = r#"{"device_code":"d","user_code":"AAAA-BBBB","verification_uri_complete":"",
            "verification_uri":"https://auth.example/approve","expires_in":600,"interval":5}"#;
        let p = parse_start_response(body).unwrap();
        assert_eq!(p.verification_uri, "https://auth.example/approve");
    }

    #[test]
    fn start_response_missing_fields() {
        assert!(parse_start_response(r#"{}"#).is_err());
        assert!(parse_start_response(r#"{"device_code":"d"}"#).is_err());
        let no_interval = r#"{"device_code":"d","user_code":"u",
            "verification_uri":"https://x","expires_in":600}"#;
        assert_eq!(
            parse_start_response(no_interval),
            Err("interval (数値) がありません".into())
        );
        let no_expires = r#"{"device_code":"d","user_code":"u",
            "verification_uri":"https://x","interval":5}"#;
        assert_eq!(
            parse_start_response(no_expires),
            Err("expires_in (数値) がありません".into())
        );
    }

    #[test]
    fn invalid_json_and_shape() {
        assert!(parse_start_response("{oops").is_err());
        assert!(parse_poll_response("[1]").is_err());
        assert!(parse_token_response("42").is_err());
    }

    #[test]
    fn error_body_is_propagated() {
        assert_eq!(
            parse_poll_response(r#"{"error":"device_code required"}"#),
            Err("サーバエラー: device_code required".into())
        );
        assert_eq!(
            parse_token_response(r#"{"error":"invalid_credential"}"#),
            Err("サーバエラー: invalid_credential".into())
        );
    }

    #[test]
    fn poll_pending_consumed_expired() {
        assert_eq!(
            parse_poll_response(r#"{"status":"pending"}"#),
            Ok(PollResult::Pending)
        );
        assert_eq!(
            parse_poll_response(r#"{"status":"consumed"}"#),
            Ok(PollResult::Consumed)
        );
        assert_eq!(
            parse_poll_response(r#"{"status":"expired"}"#),
            Ok(PollResult::Expired)
        );
    }

    #[test]
    fn poll_approved() {
        let body = r#"{"status":"approved","device_id":"dev1","device_secret":"s3cr3t",
            "tenant_id":"11111111-2222-3333-4444-555555555555","label":"cores3-abc"}"#;
        let r = parse_poll_response(body).unwrap();
        assert_eq!(
            r,
            PollResult::Approved(DeviceCredential {
                device_id: "dev1".into(),
                device_secret: "s3cr3t".into(),
                tenant_id: "11111111-2222-3333-4444-555555555555".into(),
            })
        );
    }

    #[test]
    fn poll_approved_missing_credential_is_error() {
        assert!(parse_poll_response(r#"{"status":"approved"}"#).is_err());
        assert!(
            parse_poll_response(r#"{"status":"approved","device_id":"d","device_secret":"s"}"#)
                .is_err()
        );
    }

    #[test]
    fn poll_unknown_or_missing_status() {
        assert_eq!(
            parse_poll_response(r#"{"status":"denied"}"#),
            Err("不明な status: denied".into())
        );
        assert_eq!(
            parse_poll_response(r#"{}"#),
            Err("status がありません".into())
        );
        // status が文字列でない場合も「無し」として扱う
        assert_eq!(
            parse_poll_response(r#"{"status":1}"#),
            Err("status がありません".into())
        );
    }

    #[test]
    fn token_response() {
        let body = r#"{"access_token":"eyJ...","token_type":"Bearer","expires_in":3600,
            "tenant_id":"t1"}"#;
        let t = parse_token_response(body).unwrap();
        assert_eq!(t.access_token, "eyJ...");
        assert_eq!(t.expires_in_s, 3600);
        assert_eq!(t.tenant_id, "t1");
    }

    #[test]
    fn token_response_missing_fields() {
        assert!(parse_token_response(r#"{}"#).is_err());
        assert!(parse_token_response(r#"{"access_token":"a","expires_in":10}"#).is_err());
        // 空文字列の必須フィールドはエラー
        assert!(
            parse_token_response(r#"{"access_token":"","expires_in":10,"tenant_id":"t"}"#).is_err()
        );
    }

    #[test]
    fn poll_schedule_flow() {
        let mut s = PollSchedule::new(1_000, 600, 5);
        assert!(!s.due(1_000));
        assert!(!s.due(5_999));
        assert!(s.due(6_000));
        assert!(!s.expired(600_999));
        assert!(s.expired(601_000));
        s.advance(6_100);
        assert!(!s.due(6_500));
        assert!(s.due(11_100));
    }

    #[test]
    fn poll_schedule_clamps_zero_interval() {
        let s = PollSchedule::new(0, 10, 0);
        assert!(s.due(1_000));
        assert!(!s.due(999));
    }
}
