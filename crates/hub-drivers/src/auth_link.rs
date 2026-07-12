//! auth-worker とのデバイス登録 (headless pairing) と device JWT の取得。
//!
//! フロー (ippoan/alc-app-s3#20、純粋部は alc-hub-core::pairing):
//!
//! 1. `AUTH PAIR` (ホスト) → `POST {auth}/device/pair/start` で user_code を得て
//!    画面に user_code + 承認 URL の QR を表示
//! 2. 管理者がスマホ等で承認ページを開きログイン・承認
//! 3. `POST {auth}/device/pair/token` を interval 間隔で poll し、approved なら
//!    credential (device_id / device_secret / tenant_id) を NVS に保存
//! 4. 以降 `mint_token` で短命 device JWT を取得できる (WS 送信 #21 で使用)
//!
//! HTTP は blocking (esp_http_client + 証明書バンドル)。TLS ハンドシェイクを
//! 呼び出しスレッドのスタックで行うため、専用スレッドは大きめに確保する。
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT AUTH_PAIR user_code=XXXX-XXXX` | pairing 開始 (承認待ち) |
//! | `EVT AUTH_PAIRED <tenant_id>` | 承認され credential を保存した |
//! | `EVT AUTH_PAIR_NG <理由>` | pairing 失敗 (期限切れ・通信不能等) |
//! | `EVT AUTH_TOKEN OK <expires_in_s>` | `AUTH TOKEN` 自己診断成功 |
//! | `EVT AUTH_TOKEN NG <理由>` | 同 失敗 |

use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use alc_hub_core::pairing::{
    parse_poll_response, parse_start_response, parse_token_response, poll_request_body,
    start_request_body, token_request_body, DeviceToken, PollResult, PollSchedule,
};
use anyhow::{Context, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;

use alc_hub_common::{
    config,
    settings::Settings,
    status::{now_ms, SharedStatus},
    ui_api::UiCommand,
};

/// auth_link スレッドへの依頼 (host_link から送られる)
pub enum AuthCommand {
    /// pairing を開始する (`AUTH PAIR`)
    Pair,
    /// 保存済み credential で device JWT を取得する自己診断 (`AUTH TOKEN`)
    MintTest,
}

/// 応答本文の最大サイズ (JWT を含む /device/token 応答でも十分)
const MAX_BODY: usize = 8 * 1024;
/// HTTP タイムアウト
const HTTP_TIMEOUT_S: u64 = 15;

pub fn start(
    rx: Receiver<AuthCommand>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
) -> Result<()> {
    // mbedTLS ハンドシェイクが呼び出しスレッドのスタックを使うため大きめ
    std::thread::Builder::new()
        .name("auth_link".into())
        .stack_size(20 * 1024)
        .spawn(move || {
            for cmd in rx {
                match cmd {
                    AuthCommand::Pair => run_pairing(&ui_tx, &status, &settings),
                    AuthCommand::MintTest => run_mint_test(&settings),
                }
            }
        })?;
    Ok(())
}

/// pairing 一連 (start → 画面表示 → poll → NVS 保存)。エラーは画面 + EVT 出力。
fn run_pairing(ui_tx: &Sender<UiCommand>, status: &SharedStatus, settings: &Settings) {
    let wifi_up = status.lock().map(|s| s.wifi_connected).unwrap_or(false);
    if !wifi_up {
        println!("EVT AUTH_PAIR_NG Wi-Fi 未接続");
        let _ = ui_tx.send(UiCommand::PairingResult {
            ok: false,
            message: "Wi-Fi が未接続です".into(),
        });
        return;
    }

    let base = settings.auth_url();
    let label = device_label();
    let start = match post_json(
        &format!("{base}/device/pair/start"),
        &start_request_body(&label, config::DEVICE_ROLE),
    )
    .map_err(|e| e.to_string())
    .and_then(|(_, body)| parse_start_response(&body))
    {
        Ok(p) => p,
        Err(e) => {
            log::warn!("auth_link: pair/start 失敗: {e}");
            println!("EVT AUTH_PAIR_NG {e}");
            let _ = ui_tx.send(UiCommand::PairingResult {
                ok: false,
                message: "登録を開始できませんでした".into(),
            });
            return;
        }
    };

    println!("EVT AUTH_PAIR user_code={}", start.user_code);
    let _ = ui_tx.send(UiCommand::ShowPairing {
        user_code: start.user_code.clone(),
        url: start.verification_uri.clone(),
        timeout_ms: start.expires_in_s * 1000,
    });

    let mut sched = PollSchedule::new(now_ms(), start.expires_in_s, start.interval_s);
    let poll_url = format!("{base}/device/pair/token");
    let poll_body = poll_request_body(&start.device_code);
    loop {
        if sched.expired(now_ms()) {
            finish_pairing(ui_tx, false, "承認されませんでした (期限切れ)");
            return;
        }
        if !sched.due(now_ms()) {
            FreeRtos::delay_ms(200);
            continue;
        }
        sched.advance(now_ms());

        let result = post_json(&poll_url, &poll_body)
            .map_err(|e| e.to_string())
            .and_then(|(_, body)| parse_poll_response(&body));
        match result {
            Ok(PollResult::Pending) => {}
            Ok(PollResult::Approved(cred)) => {
                if let Err(e) =
                    settings.set_device_credential(&cred.device_id, &cred.device_secret, &cred.tenant_id)
                {
                    // credential は 1 回限りのため保存失敗は致命的 — 再ペアリングを促す
                    log::error!("auth_link: credential 保存失敗: {e:?}");
                    finish_pairing(ui_tx, false, "保存に失敗しました。再登録してください");
                    return;
                }
                println!("EVT AUTH_PAIRED {}", cred.tenant_id);
                finish_pairing(ui_tx, true, "デバイスを登録しました");
                return;
            }
            Ok(PollResult::Consumed) => {
                finish_pairing(ui_tx, false, "この登録コードは処理済みです");
                return;
            }
            Ok(PollResult::Expired) => {
                finish_pairing(ui_tx, false, "承認されませんでした (期限切れ)");
                return;
            }
            // 一時的な通信エラーは次の interval で再試行 (期限まで)
            Err(e) => log::warn!("auth_link: poll 失敗 (再試行): {e}"),
        }
    }
}

fn finish_pairing(ui_tx: &Sender<UiCommand>, ok: bool, message: &str) {
    if !ok {
        println!("EVT AUTH_PAIR_NG {message}");
    }
    let _ = ui_tx.send(UiCommand::PairingResult {
        ok,
        message: message.into(),
    });
}

/// `AUTH TOKEN` 自己診断: 保存済み credential で JWT を mint できるか確認する。
/// token 自体はホストへ出力しない (シリアルログに残さない)。
fn run_mint_test(settings: &Settings) {
    let Some((id, secret)) = settings.device_credential() else {
        println!("EVT AUTH_TOKEN NG 未登録 (AUTH PAIR で登録してください)");
        return;
    };
    match mint_token(&settings.auth_url(), &id, &secret) {
        Ok(t) => println!("EVT AUTH_TOKEN OK {}", t.expires_in_s),
        Err(e) => println!("EVT AUTH_TOKEN NG {e}"),
    }
}

/// 保存済み credential を短命 device JWT に交換する (WS 送信 #21 でも使用)。
pub fn mint_token(base: &str, device_id: &str, device_secret: &str) -> Result<DeviceToken, String> {
    post_json(
        &format!("{base}/device/token"),
        &token_request_body(device_id, device_secret),
    )
    .map_err(|e| e.to_string())
    .and_then(|(_, body)| parse_token_response(&body))
}

/// pairing 用のデバイスラベル。個体識別のため efuse MAC の下位 3 バイトを付ける
fn device_label() -> String {
    let mut mac = [0u8; 6];
    // ESP_MAC_WIFI_STA の factory MAC (Wi-Fi 未初期化でも読める)
    let ok = unsafe {
        esp_idf_svc::sys::esp_read_mac(
            mac.as_mut_ptr(),
            esp_idf_svc::sys::esp_mac_type_t_ESP_MAC_WIFI_STA,
        )
    } == esp_idf_svc::sys::ESP_OK;
    if ok {
        format!("cores3-{:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
    } else {
        "cores3".to_string()
    }
}

/// JSON POST (blocking)。応答の (HTTP status, body) を返す。
/// レスポンス解釈は純粋部 (pairing.rs) が行うため、非 2xx でも本文を返す
/// (auth-worker はエラー時も `{"error":...}` / `{"status":...}` を返す)。
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
