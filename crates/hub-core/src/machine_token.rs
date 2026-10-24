//! 機体の鍵の証明 (auth-worker の用途 `hub-record`) の純粋部 (Refs ippoan/alc-cam#2)。
//!
//! 測定の記録を、hub の credential の ID ではなく機体の鍵の ID (`alarm:<fingerprint>`) で
//! 残すための仕組み。WS の接続そのものは今までどおり hub の device JWT で張り、
//! 機体の鍵 (NVS `alarm_sk`) で取った短命の JWT を `X-Machine-Token` として並べて添える。
//! cf-alc-recorder はそれを確かめ、記録の device_id を鍵の sub にする (下り command・OTA の
//! 宛先は hub の ID のまま)。
//!
//! 取り方は PWA と同じ口:
//!
//! 1. `GET {auth}/device/alarm-nonce?usage=hub-record` → `{nonce, expires_in}`
//! 2. 機体の鍵で nonce に署名 (`AUTH SIGN` と同じ [`sign_nonce`])
//! 3. `POST {auth}/device/alarm-token` `{nonce, pubkey, sig, usage: "hub-record"}`
//!    → `{access_token, token_type, expires_in, tenant_id}` (`/device/token` と同じ形)
//!
//! **取れなくても測定は止めない。** 鍵が無い・未登録 (401) の機体は証明なしで繋ぎ、
//! 記録は hub の ID で入る (サーバーの受け入れ期間)。取れなかった回は [`retry_after_ms`]
//! だけ待ってから取り直す。

use serde_json::json;

use crate::alarm_key::{check_nonce, pubkey_b64, sign_nonce};

/// auth-worker の用途 (= nonce の purpose)。role は `device-hub-record`。
pub const HUB_RECORD_USAGE: &str = "hub-record";

/// サーバーが「鍵の証明が無いので受けない」と返すときの error frame の message。
pub const MACHINE_TOKEN_REQUIRED: &str = "machine_token_required";

/// 鍵が未登録・失効・用途違いで断られたときの取り直し間隔 [ms]。
/// 登録は人の操作なので、詰めて叩いても通らない。
pub const RETRY_REJECTED_MS: u64 = 3_600_000;

/// 通信の失敗・rate limit など一時的な失敗の取り直し間隔 [ms]。
pub const RETRY_TRANSIENT_MS: u64 = 300_000;

/// 取れなかった理由。取り直しの間隔とイベントの語に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintFailure {
    /// 機体に鍵が無い (`alarm_sk` 未生成)。
    NoKey,
    /// 断られた (未登録・失効・用途違い = 400 / 401 / 403)。
    Rejected,
    /// 一時的な失敗 (通信・429・5xx・応答が読めない)。
    Transient,
}

impl MintFailure {
    /// ホストへのイベント (`EVT MACHINE_TOKEN NG <語>`) に載せる語。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MintFailure::NoKey => "no_key",
            MintFailure::Rejected => "rejected",
            MintFailure::Transient => "transient",
        }
    }
}

/// HTTP の status から失敗の種類を決める (2xx は呼ばない)。
#[must_use]
pub fn classify_status(status: u16) -> MintFailure {
    match status {
        400 | 401 | 403 => MintFailure::Rejected,
        _ => MintFailure::Transient,
    }
}

/// 取れなかったあと、次に取りに行くまでの待ち [ms]。
#[must_use]
pub fn retry_after_ms(failure: MintFailure) -> u64 {
    match failure {
        MintFailure::NoKey | MintFailure::Rejected => RETRY_REJECTED_MS,
        MintFailure::Transient => RETRY_TRANSIENT_MS,
    }
}

/// `GET /device/alarm-nonce` の URL (用途 hub-record)。
#[must_use]
pub fn alarm_nonce_url(base: &str) -> String {
    format!("{base}/device/alarm-nonce?usage={HUB_RECORD_USAGE}")
}

/// `POST /device/alarm-token` の URL。
#[must_use]
pub fn alarm_token_url(base: &str) -> String {
    format!("{base}/device/alarm-token")
}

/// `GET /device/alarm-nonce` の応答から nonce を取り出す (小文字 hex 32 文字だけ受ける)。
pub fn parse_nonce_response(body: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("nonce の応答が JSON でない: {e}"))?;
    let nonce = v
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .ok_or("nonce の応答に nonce が無い")?;
    check_nonce(nonce).map_err(|_| "nonce の形が不正".to_string())?;
    Ok(nonce.to_string())
}

/// `POST /device/alarm-token` の本文 (nonce に機体の鍵で署名する)。
pub fn token_request_body(seed: &[u8; 32], nonce: &str) -> Result<String, String> {
    let sig = sign_nonce(seed, nonce).map_err(|_| "nonce の形が不正".to_string())?;
    Ok(json!({
        "nonce": nonce,
        "pubkey": pubkey_b64(seed),
        "sig": sig,
        "usage": HUB_RECORD_USAGE,
    })
    .to_string())
}

/// WSS ハンドシェイクに載せる証明のヘッダ 1 行 (末尾 CRLF 込み。`auth_header_line` と同じ作法)。
#[must_use]
pub fn machine_header_line(jwt: &str) -> String {
    format!("X-Machine-Token: {jwt}\r\n")
}

/// 証明を取りに行ってよいか (取れなかったあとの待ちが明けたか)。
#[must_use]
pub fn mint_due(now_ms: u64, retry_at_ms: u64) -> bool {
    now_ms >= retry_at_ms
}

/// transport に載せるヘッダ全体の失効時刻 [稼働 ms] = hub の JWT と証明の早い方。
/// 差し替え (`set_headers`) の契機はこの値で決める (どちらか一方が切れても再接続が 401 になる)。
#[must_use]
pub fn headers_expire_at(hub_expires_at: u64, machine_expires_at: Option<u64>) -> u64 {
    machine_expires_at.map_or(hub_expires_at, |m| m.min(hub_expires_at))
}

/// 証明を取った鍵が書き換わった (または消えた) か。証明を持っていなければ false。
/// 書き換わったら証明を捨てて繋ぎ直す (前の鍵の ID で記録し続けないため)。
#[must_use]
pub fn key_changed<T: PartialEq + ?Sized>(minted: Option<&T>, current: Option<&T>) -> bool {
    match minted {
        Some(minted) => current != Some(minted),
        None => false,
    }
}

/// サーバーの error frame の message が「鍵の証明が要る」か。
#[must_use]
pub fn is_machine_token_required(message: &str) -> bool {
    message == MACHINE_TOKEN_REQUIRED
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn b64url_decode(s: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0u32;
        for c in s.bytes() {
            let v = ALPHABET.iter().position(|&a| a == c).expect("base64url") as u32;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
                buf &= (1 << bits) - 1;
            }
        }
        out
    }

    #[test]
    fn urls_carry_the_hub_record_usage() {
        assert_eq!(
            alarm_nonce_url("https://auth.example"),
            "https://auth.example/device/alarm-nonce?usage=hub-record"
        );
        assert_eq!(
            alarm_token_url("https://auth.example"),
            "https://auth.example/device/alarm-token"
        );
    }

    #[test]
    fn parse_nonce_response_accepts_only_lowercase_hex_32() {
        assert_eq!(
            parse_nonce_response(&format!(r#"{{"nonce":"{NONCE}","expires_in":60}}"#)),
            Ok(NONCE.to_string())
        );
        assert!(parse_nonce_response("not json").is_err());
        assert!(parse_nonce_response(r#"{"expires_in":60}"#).is_err());
        assert!(parse_nonce_response(r#"{"nonce":1}"#).is_err());
        assert!(parse_nonce_response(r#"{"nonce":"0123456789ABCDEF0123456789abcdef"}"#).is_err());
        assert!(parse_nonce_response(r#"{"nonce":"short"}"#).is_err());
    }

    #[test]
    fn token_request_body_is_signed_with_the_machine_key() {
        let seed = [7u8; 32];
        let body: serde_json::Value =
            serde_json::from_str(&token_request_body(&seed, NONCE).unwrap()).unwrap();
        assert_eq!(body["nonce"], NONCE);
        assert_eq!(body["usage"], "hub-record");
        assert_eq!(body["pubkey"], pubkey_b64(&seed));
        // 署名は nonce の ASCII そのもの (`AUTH SIGN` と同じ契約)
        let pk: [u8; 32] = b64url_decode(body["pubkey"].as_str().unwrap())
            .try_into()
            .unwrap();
        let sig = Signature::from_slice(&b64url_decode(body["sig"].as_str().unwrap())).unwrap();
        VerifyingKey::from_bytes(&pk)
            .unwrap()
            .verify(NONCE.as_bytes(), &sig)
            .expect("nonce への署名として検証できる");
        assert!(token_request_body(&seed, "bad").is_err());
    }

    #[test]
    fn machine_header_line_ends_with_crlf() {
        assert_eq!(machine_header_line("abc"), "X-Machine-Token: abc\r\n");
    }

    #[test]
    fn classify_and_retry() {
        for s in [400, 401, 403] {
            assert_eq!(classify_status(s), MintFailure::Rejected, "{s}");
        }
        for s in [429, 500, 503, 0] {
            assert_eq!(classify_status(s), MintFailure::Transient, "{s}");
        }
        assert_eq!(retry_after_ms(MintFailure::NoKey), RETRY_REJECTED_MS);
        assert_eq!(retry_after_ms(MintFailure::Rejected), RETRY_REJECTED_MS);
        assert_eq!(retry_after_ms(MintFailure::Transient), RETRY_TRANSIENT_MS);
        assert_eq!(MintFailure::NoKey.label(), "no_key");
        assert_eq!(MintFailure::Rejected.label(), "rejected");
        assert_eq!(MintFailure::Transient.label(), "transient");
    }

    #[test]
    fn mint_due_and_expiry() {
        assert!(mint_due(10, 10));
        assert!(mint_due(11, 10));
        assert!(!mint_due(9, 10));
        assert_eq!(headers_expire_at(100, None), 100);
        assert_eq!(headers_expire_at(100, Some(50)), 50);
        assert_eq!(headers_expire_at(100, Some(150)), 100);
    }

    #[test]
    fn key_changed_only_when_holding_a_token() {
        assert!(!key_changed::<str>(None, Some("a")));
        assert!(!key_changed::<str>(None, None));
        assert!(!key_changed(Some("a"), Some("a")));
        assert!(key_changed(Some("a"), Some("b")));
        assert!(key_changed(Some("a"), None));
    }

    #[test]
    fn machine_token_required_message() {
        assert!(is_machine_token_required("machine_token_required"));
        assert!(!is_machine_token_required("upstream_500"));
    }
}
