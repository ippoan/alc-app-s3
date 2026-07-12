//! Improv Wi-Fi Serial のハンドラ (I/O・副作用側)。
//!
//! フレーム解析・構築は alc-hub-core::improv (純粋・テスト済み)。
//! ESP Web Tools はファームウェア書き込み後、このプロトコルで
//! SSID/パスワードの設定 UI を出す。host_link.rs のバイトストリームから
//! IMPROV フレームだけがここへ渡される。

use std::io::Write;

use alc_hub_core::improv as proto;

use alc_hub_common::settings::Settings;
use alc_hub_common::status::{now_ms, SharedStatus};

use crate::wifi::Wifi;

pub struct Improv {
    settings: Settings,
    wifi: Wifi,
    status: SharedStatus,
    provisioned: bool,
}

impl Improv {
    pub fn new(settings: Settings, wifi: Wifi, status: SharedStatus, provisioned: bool) -> Self {
        Self {
            settings,
            wifi,
            status,
            provisioned,
        }
    }

    fn state(&self) -> u8 {
        if self.provisioned {
            proto::STATE_PROVISIONED
        } else {
            proto::STATE_READY
        }
    }

    /// IMPROV パケット 1 つを処理する
    pub fn handle_packet(&mut self, ptype: u8, data: &[u8]) {
        // ダイアログ操作中 (パケットが届いている間 + 入力時間) は BLE スキャンを
        // 止め、Wi-Fi 接続がコエグジストの電波取り合いで遅れないようにする
        self.wifi.pause_ble_for(60_000);
        if ptype != proto::TYPE_RPC_COMMAND {
            return; // ホスト側からは RPC コマンドのみ受ける
        }
        let Some((cmd, payload)) = proto::parse_rpc(data) else {
            send(&proto::build_error(proto::ERROR_INVALID_RPC));
            return;
        };
        match cmd {
            proto::CMD_WIFI_SETTINGS => self.wifi_settings(payload),
            proto::CMD_REQUEST_STATE => {
                send(&proto::build_state(self.state()));
                if self.provisioned {
                    send(&proto::build_rpc_result(cmd, &[]));
                }
            }
            proto::CMD_REQUEST_INFO => {
                send(&proto::build_rpc_result(
                    cmd,
                    &[
                        "alc-hub-cores3",
                        alc_hub_common::config::FIRMWARE_VERSION,
                        "ESP32-S3",
                        "M5Stack CoreS3",
                    ],
                ));
            }
            proto::CMD_REQUEST_SCAN => self.scan(cmd),
            _ => send(&proto::build_error(proto::ERROR_UNKNOWN_RPC)),
        }
    }

    fn wifi_settings(&mut self, payload: &[u8]) {
        let Some(s) = proto::parse_wifi_settings(payload) else {
            send(&proto::build_error(proto::ERROR_INVALID_RPC));
            return;
        };
        send(&proto::build_state(proto::STATE_PROVISIONING));
        log::info!("improv: Wi-Fi 設定受信 ssid={}", s.ssid);

        match self.wifi.connect(&s.ssid, &s.password) {
            Ok(ip) => {
                if let Err(e) = self.settings.set_wifi_credentials(&s.ssid, &s.password) {
                    log::error!("improv: Wi-Fi 設定の保存失敗: {e:?}");
                }
                self.provisioned = true;
                send(&proto::build_state(proto::STATE_PROVISIONED));
                // リダイレクト URL は無し (Web UI を持たないため空文字列 1 個)
                let url = format!("http://{ip}/");
                send(&proto::build_rpc_result(proto::CMD_WIFI_SETTINGS, &[&url]));
            }
            Err(e) => {
                log::warn!("improv: Wi-Fi 接続失敗: {e:?}");
                self.wifi.mark_disconnected();
                if let Ok(mut st) = self.status.lock() {
                    st.push_event(now_ms(), "WiFi 接続失敗");
                }
                send(&proto::build_error(proto::ERROR_UNABLE_TO_CONNECT));
                send(&proto::build_state(proto::STATE_READY));
            }
        }
    }

    fn scan(&mut self, cmd: u8) {
        match self.wifi.scan() {
            Ok(mut aps) => {
                // RSSI 降順・重複 SSID 除去
                aps.sort_by(|a, b| b.1.cmp(&a.1));
                let mut seen = std::collections::HashSet::new();
                for (ssid, rssi, auth) in aps {
                    if ssid.is_empty() || !seen.insert(ssid.clone()) {
                        continue;
                    }
                    let rssi = rssi.to_string();
                    let auth = if auth { "YES" } else { "NO" };
                    send(&proto::build_rpc_result(cmd, &[&ssid, &rssi, auth]));
                }
            }
            Err(e) => log::warn!("improv: スキャン失敗: {e:?}"),
        }
        // 終端: 空の RPC result
        send(&proto::build_rpc_result(cmd, &[]));
    }
}

/// バイナリパケットをホストへ送出 (テキストログと同一ストリーム。
/// ESP Web Tools 側は IMPROV マジックでフレームを拾う)
fn send(packet: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(packet);
    let _ = out.flush();
}
