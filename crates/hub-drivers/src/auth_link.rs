//! auth-worker との device JWT 交換 (HTTPS)。
//!
//! provisioning は USB 前提 (ippoan/alc-app-s3#20 の方針変更): ホストが
//! auth-worker `/device/pair` 系で取得した credential を `AUTH SET` で注入し、
//! CoreS3 は保存済み credential を `POST /device/token` で短命 device JWT に
//! 交換するだけ。純粋部は alc-hub-core::pairing。
//!
//! HTTP は blocking (esp_http_client + 証明書バンドル)。TLS ハンドシェイクを
//! 呼び出しスレッドのスタックで行うため、専用スレッドは大きめに確保する。
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT AUTH_TOKEN OK <expires_in_s>` | `AUTH TOKEN` 自己診断成功 |
//! | `EVT AUTH_TOKEN NG <理由>` | 同 失敗 |

use alc_hub_core::pairing::{parse_token_response, token_request_body, DeviceToken};
use anyhow::{Context, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;

use alc_hub_common::settings::Settings;
use alc_hub_common::status::SharedStatus;

/// 応答本文の最大サイズ (JWT を含む /device/token 応答でも十分)
const MAX_BODY: usize = 8 * 1024;
/// HTTP タイムアウト
const HTTP_TIMEOUT_S: u64 = 15;
/// AUTH TOKEN 自己診断で Wi-Fi 接続を待つ上限。ポート open のリセット後は
/// Wi-Fi 再接続に ~30 秒かかるため長めに取る
const WIFI_WAIT_MS: u64 = 45_000;

/// `AUTH TOKEN` 自己診断を一時スレッドで実行する。TLS ハンドシェイクの
/// スタック (20KB) は診断中だけ確保し、終わったら返す (定常ヒープ節約 —
/// 実測で空きヒープ 41KB の機体に常駐 20KB は重すぎる)。
/// token 自体はホストへ出力しない (シリアルログに残さない)。
///
/// HTTPS を叩く前に Wi-Fi 接続を待つ (ポート open のリセット直後は Wi-Fi が
/// 未接続で、待たずに叩くと「リクエスト送信に失敗」になる。待機はデバイスの
/// 責務 — ホスト側にポーリングさせない)。
pub fn spawn_mint_test(settings: Settings, status: SharedStatus) {
    let spawned = std::thread::Builder::new()
        .name("auth_mint".into())
        .stack_size(20 * 1024)
        .spawn(move || {
            let Some((id, secret)) = settings.device_credential() else {
                println!("EVT AUTH_TOKEN NG 未登録 (AUTH SET で credential を注入してください)");
                return;
            };
            if !wait_for_wifi(&status, WIFI_WAIT_MS) {
                println!("EVT AUTH_TOKEN NG Wi-Fi 未接続 (Improv で Wi-Fi 設定を確認してください)");
                return;
            }
            match mint_token(&settings.auth_url(), &id, &secret) {
                Ok(t) => println!("EVT AUTH_TOKEN OK {}", t.expires_in_s),
                Err(e) => println!("EVT AUTH_TOKEN NG {e}"),
            }
        });
    if spawned.is_err() {
        println!("EVT AUTH_TOKEN NG スレッド起動失敗 (メモリ不足)");
    }
}

/// Wi-Fi が接続されるまで最大 `timeout_ms` 待つ。接続できたら true。
fn wait_for_wifi(status: &SharedStatus, timeout_ms: u64) -> bool {
    let mut waited = 0u64;
    loop {
        if status.lock().map(|s| s.wifi_connected).unwrap_or(false) {
            return true;
        }
        if waited >= timeout_ms {
            return false;
        }
        FreeRtos::delay_ms(500);
        waited += 500;
    }
}

/// 保存済み credential を短命 device JWT に交換する (ws_uplink でも使用)。
pub fn mint_token(base: &str, device_id: &str, device_secret: &str) -> Result<DeviceToken, String> {
    post_json(
        &format!("{base}/device/token"),
        &token_request_body(device_id, device_secret),
    )
    .map_err(|e| e.to_string())
    .and_then(|(_, body)| parse_token_response(&body))
}

/// JSON POST (blocking)。応答の (HTTP status, body) を返す。
/// レスポンス解釈は純粋部 (pairing.rs) が行うため、非 2xx でも本文を返す
/// (auth-worker はエラー時も `{"error":...}` を返す)。
fn post_json(url: &str, body: &str) -> Result<(u16, String)> {
    let mut conn = EspHttpConnection::new(&HttpConfiguration {
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(core::time::Duration::from_secs(HTTP_TIMEOUT_S)),
        ..Default::default()
    })
    .context("HTTP 接続の初期化に失敗")?;

    let len = body.len().to_string();
    conn.initiate_request(
        Method::Post,
        url,
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", &len),
        ],
    )
    .context("リクエスト送信に失敗")?;
    conn.write_all(body.as_bytes()).context("本文送信に失敗")?;
    conn.initiate_response().context("応答受信に失敗")?;

    let status = conn.status();
    let mut out: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = conn.read(&mut buf).context("本文読み出しに失敗")?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > MAX_BODY {
            anyhow::bail!("応答が大きすぎます");
        }
    }
    Ok((status, String::from_utf8_lossy(&out).into_owned()))
}
