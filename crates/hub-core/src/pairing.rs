//! auth-worker device token 交換の純粋部分 (JSON 組立/解析)。
//!
//! provisioning は USB 前提: ホストが auth-worker `/device/pair` 系で取得した
//! credential を `AUTH SET` (シリアル) で注入する (Wi-Fi の Improv 設定と同じ
//! 考え方)。CoreS3 側に QR/承認ページのペアリングフローは持たない。
//!
//! 本モジュールは注入済み credential を短命 device JWT に交換する
//! `POST /device/token` のリクエスト本文とレスポンス解釈のみを行う
//! (HTTP 送受信は auth_link.rs)。エラーはそのままホストへ返せる日本語
//! メッセージ。serde derive ではなく手動で分解する (cfg.rs と同方針)。

use serde_json::{json, Map, Value};

/// `POST /device/token` の応答 (短命 device JWT)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceToken {
    pub access_token: String,
    pub expires_in_s: u64,
    pub tenant_id: String,
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

/// `POST /device/token` の応答を解釈する。
pub fn parse_token_response(body: &str) -> Result<DeviceToken, String> {
    let obj = parse_object(body)?;
    Ok(DeviceToken {
        access_token: str_field(&obj, "access_token")?,
        expires_in_s: u64_field(&obj, "expires_in")?,
        tenant_id: str_field(&obj, "tenant_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body() {
        assert_eq!(
            token_request_body("id1", "sec1"),
            r#"{"device_id":"id1","device_secret":"sec1"}"#
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
    fn error_body_is_propagated() {
        assert_eq!(
            parse_token_response(r#"{"error":"invalid_credential"}"#),
            Err("サーバエラー: invalid_credential".into())
        );
    }

    #[test]
    fn invalid_json_and_shape() {
        assert!(parse_token_response("{oops").is_err());
        assert!(parse_token_response("[1]").is_err());
        assert!(parse_token_response("42").is_err());
    }

    #[test]
    fn token_response_missing_fields() {
        assert_eq!(
            parse_token_response(r#"{}"#),
            Err("access_token (文字列) がありません".into())
        );
        assert_eq!(
            parse_token_response(r#"{"access_token":"a","tenant_id":"t"}"#),
            Err("expires_in (数値) がありません".into())
        );
        assert!(parse_token_response(r#"{"access_token":"a","expires_in":10}"#).is_err());
        // 空文字列の必須フィールドはエラー
        assert!(
            parse_token_response(r#"{"access_token":"","expires_in":10,"tenant_id":"t"}"#).is_err()
        );
    }
}
