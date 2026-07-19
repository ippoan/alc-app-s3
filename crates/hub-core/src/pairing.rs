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
//!
//! `POST /device/hub-token` / `POST /device/introspect` (ippoan/alc-app-s3#83、
//! ippoan/auth-worker#406) の request/response もここに置く。GW との相互認証
//! ハンドシェイクで使う — hub-token は GW の `auth_challenge` nonce を束縛した
//! 短命トークン、introspect はその GW が返す `auth_ok` token の検証。

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

/// `POST /device/hub-token` の応答 (拠点相互認証用の短命 hub-token)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubToken {
    pub access_token: String,
    pub expires_in_s: u64,
    pub site_id: String,
}

/// `POST /device/hub-token` のリクエスト本文。`nonce` は GW の `auth_challenge`
/// が指定したものをそのまま渡す (リプレイ不能化、ippoan/alc-app-s3#83)。
pub fn hub_token_request_body(device_id: &str, device_secret: &str, nonce: &str) -> String {
    json!({ "device_id": device_id, "device_secret": device_secret, "nonce": nonce }).to_string()
}

/// `POST /device/hub-token` の応答を解釈する。
pub fn parse_hub_token_response(body: &str) -> Result<HubToken, String> {
    let obj = parse_object(body)?;
    Ok(HubToken {
        access_token: str_field(&obj, "access_token")?,
        expires_in_s: u64_field(&obj, "expires_in")?,
        site_id: str_field(&obj, "site_id")?,
    })
}

/// `POST /device/introspect` の応答。`valid:false` はサーバエラーではなく
/// 「トークンが無効」という正常な判定結果なので `parse_introspect_response` は
/// これを `Err` にせず `Ok(IntrospectResult{valid:false,..})` として返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectResult {
    pub valid: bool,
    pub site_id: Option<String>,
    pub role: Option<String>,
}

impl IntrospectResult {
    /// この introspect 結果が「自分 (`own_device_id`、site_id は自分の
    /// device_id と同一が既定、ippoan/auth-worker#406) の拠点に属する正規の
    /// GW (`role=device-gateway`)」を示しているか判定する (1:1 強制)。
    pub fn authorizes_gateway(&self, own_device_id: &str) -> bool {
        self.valid
            && self.role.as_deref() == Some("device-gateway")
            && self.site_id.as_deref() == Some(own_device_id)
    }
}

/// `POST /device/introspect` のリクエスト本文。
pub fn introspect_request_body(device_id: &str, device_secret: &str, token: &str) -> String {
    json!({ "device_id": device_id, "device_secret": device_secret, "token": token }).to_string()
}

/// `POST /device/introspect` の応答を解釈する。
pub fn parse_introspect_response(body: &str) -> Result<IntrospectResult, String> {
    let obj = parse_object(body)?;
    let valid = obj
        .get("valid")
        .and_then(|v| v.as_bool())
        .ok_or("valid (真偽値) がありません")?;
    if !valid {
        return Ok(IntrospectResult {
            valid: false,
            site_id: None,
            role: None,
        });
    }
    Ok(IntrospectResult {
        valid: true,
        site_id: Some(str_field(&obj, "site_id")?),
        role: Some(str_field(&obj, "role")?),
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

    #[test]
    fn hub_token_request_body_shape() {
        assert_eq!(
            hub_token_request_body("id1", "sec1", "nonce1"),
            r#"{"device_id":"id1","device_secret":"sec1","nonce":"nonce1"}"#
        );
    }

    #[test]
    fn hub_token_response() {
        let body = r#"{"access_token":"eyJ...","token_type":"Bearer","expires_in":60,
            "site_id":"dev-abc"}"#;
        let t = parse_hub_token_response(body).unwrap();
        assert_eq!(t.access_token, "eyJ...");
        assert_eq!(t.expires_in_s, 60);
        assert_eq!(t.site_id, "dev-abc");
    }

    #[test]
    fn hub_token_response_errors() {
        assert_eq!(
            parse_hub_token_response(r#"{"error":"forbidden"}"#),
            Err("サーバエラー: forbidden".into())
        );
        assert!(parse_hub_token_response("{oops").is_err());
        assert_eq!(
            parse_hub_token_response(r#"{"access_token":"a","expires_in":60}"#),
            Err("site_id (文字列) がありません".into())
        );
    }

    #[test]
    fn introspect_request_body_shape() {
        assert_eq!(
            introspect_request_body("id1", "sec1", "tok1"),
            r#"{"device_id":"id1","device_secret":"sec1","token":"tok1"}"#
        );
    }

    #[test]
    fn introspect_response_valid() {
        let body =
            r#"{"valid":true,"site_id":"dev-abc","role":"device-gateway","claims":{"nonce":"n1"}}"#;
        let r = parse_introspect_response(body).unwrap();
        assert_eq!(
            r,
            IntrospectResult {
                valid: true,
                site_id: Some("dev-abc".into()),
                role: Some("device-gateway".into()),
            }
        );
    }

    #[test]
    fn introspect_response_invalid_is_not_an_error() {
        assert_eq!(
            parse_introspect_response(r#"{"valid":false}"#),
            Ok(IntrospectResult {
                valid: false,
                site_id: None,
                role: None,
            })
        );
    }

    #[test]
    fn introspect_response_errors() {
        assert_eq!(
            parse_introspect_response(r#"{"error":"unauthorized"}"#),
            Err("サーバエラー: unauthorized".into())
        );
        assert_eq!(
            parse_introspect_response(r#"{}"#),
            Err("valid (真偽値) がありません".into())
        );
        assert_eq!(
            parse_introspect_response(r#"{"valid":true,"role":"device-gateway"}"#),
            Err("site_id (文字列) がありません".into())
        );
        assert_eq!(
            parse_introspect_response(r#"{"valid":true,"site_id":"dev-abc"}"#),
            Err("role (文字列) がありません".into())
        );
    }

    #[test]
    fn authorizes_gateway_requires_valid_matching_role_and_site() {
        let ok = IntrospectResult {
            valid: true,
            site_id: Some("dev-abc".into()),
            role: Some("device-gateway".into()),
        };
        assert!(ok.authorizes_gateway("dev-abc"));
        assert!(!ok.authorizes_gateway("dev-other")); // site_id 不一致 (1:1 強制)

        let wrong_role = IntrospectResult {
            valid: true,
            site_id: Some("dev-abc".into()),
            role: Some("device-hub".into()),
        };
        assert!(!wrong_role.authorizes_gateway("dev-abc")); // GW 以外の役割は拒否

        let invalid = IntrospectResult {
            valid: false,
            site_id: Some("dev-abc".into()),
            role: Some("device-gateway".into()),
        };
        assert!(!invalid.authorizes_gateway("dev-abc")); // valid:false は無条件で拒否
    }
}
