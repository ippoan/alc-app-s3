//! 測定データの WebSocket 送信 (ippoan/alc-app-s3#21) — スパイク段階。
//!
//! 現段階は「esp_websocket_client (managed component) + WSS + Bearer ヘッダが
//! xtensa でビルドできること」の確認が目的。cf-alc-recorder (ippoan/alc-app#106)
//! が立ち次第、NVS 送信キュー + seq/ACK 再送 + RadioCoex 送信ウィンドウ制御を
//! ここに実装する。
//!
//! 認証: auth_link::mint_token で得た device JWT を WSS ハンドシェイクの
//! Authorization ヘッダに載せる。

use anyhow::{Context, Result};
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, FrameType, WebSocketEventType,
};

/// 接続タイムアウト
const CONNECT_TIMEOUT_S: u64 = 10;

/// WSS で 1 フレーム送信して切断する (スパイク用の最小実装)。
///
/// 戻り値 Ok = ハンドシェイク成功 + 送信完了。本実装 (#21) では接続を
/// 使い回す送信キューに置き換える。
pub fn send_once(url: &str, bearer_jwt: &str, payload: &str) -> Result<()> {
    let headers = format!("Authorization: Bearer {bearer_jwt}\r\n");
    let config = EspWebSocketClientConfig {
        // auth_link と同じ公開 CA バンドルでサーバ証明書を検証する
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        headers: Some(&headers),
        ..Default::default()
    };

    let mut client = EspWebSocketClient::new(
        url,
        &config,
        core::time::Duration::from_secs(CONNECT_TIMEOUT_S),
        |event| match event {
            Ok(ev) => log::info!("ws_uplink: event {:?}", ev.event_type),
            Err(e) => log::warn!("ws_uplink: event error {e:?}"),
        },
    )
    .context("WebSocket 接続に失敗")?;

    client
        .send(FrameType::Text(false), payload.as_bytes())
        .context("WebSocket 送信に失敗")?;
    Ok(())
}

/// イベント型が使用可能なことのコンパイル確認 (スパイク)。
#[allow(dead_code)]
fn event_is_connected(ev: &WebSocketEventType<'_>) -> bool {
    matches!(ev, WebSocketEventType::Connected)
}
