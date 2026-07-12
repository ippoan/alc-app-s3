//! Wi-Fi STA 管理 (Improv Wi-Fi Serial から設定される)。
//!
//! 主経路はあくまで LAN Module 13.2 (PoE) だが、LAN 配線が無い拠点向けの
//! 代替経路として Wi-Fi STA を持つ (plan/cores3-hub-consolidation.md の
//! セルラー検討と同じ位置付け)。ESP32-S3 は 2.4GHz (11b/g/n) のみ対応。
//!
//! 注意: BLE (NimBLE) と Wi-Fi の同時使用はコエグジスト動作になる。
//! メモリ・スループットの実機確認は TODO (README 参照)。

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{delay::FreeRtos, modem::Modem},
    nvs::EspDefaultNvsPartition,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};

use crate::status::{now_ms, SharedStatus};

/// スキャン結果 1 件: (SSID, RSSI, 認証あり)
pub type ScanEntry = (String, i8, bool);

#[derive(Clone)]
pub struct Wifi {
    inner: Arc<Mutex<EspWifi<'static>>>,
    sysloop: EspSystemEventLoop,
    status: SharedStatus,
}

impl Wifi {
    pub fn new(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        status: SharedStatus,
    ) -> Result<Self> {
        let wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(wifi)),
            sysloop,
            status,
        })
    }

    /// STA として接続 (ブロッキング)。成功時は IP アドレス文字列を返す。
    pub fn connect(&self, ssid: &str, password: &str) -> Result<String> {
        let mut guard = self.inner.lock().expect("wifi lock");

        let auth_method = if password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal // WPA/WPA2/WPA3 は自動ネゴシエーション
        };
        let config = Configuration::Client(ClientConfiguration {
            ssid: ssid
                .try_into()
                .map_err(|_| anyhow!("SSID が長すぎます (最大32バイト)"))?,
            password: password
                .try_into()
                .map_err(|_| anyhow!("パスワードが長すぎます (最大64バイト)"))?,
            auth_method,
            ..Default::default()
        });

        let mut wifi = BlockingWifi::wrap(&mut *guard, self.sysloop.clone())
            .context("BlockingWifi 初期化失敗")?;
        wifi.set_configuration(&config)?;
        if !wifi.is_started().unwrap_or(false) {
            wifi.start().context("Wi-Fi start 失敗")?;
        }
        // 再接続時は一旦切断してから
        let _ = wifi.disconnect();
        wifi.connect().context("Wi-Fi 接続失敗")?;
        wifi.wait_netif_up().context("IP アドレス取得失敗")?;

        let ip = guard
            .sta_netif()
            .get_ip_info()
            .map(|i| i.ip.to_string())
            .unwrap_or_default();

        if let Ok(mut st) = self.status.lock() {
            st.wifi_connected = true;
            st.wifi_ip = ip.clone();
            st.push_event(now_ms(), &format!("WiFi 接続 {ip}"));
        }
        Ok(ip)
    }

    /// 周辺ネットワークのスキャン (Improv の REQUEST_SCAN 用)
    pub fn scan(&self) -> Result<Vec<ScanEntry>> {
        let mut guard = self.inner.lock().expect("wifi lock");
        if !guard.is_started().unwrap_or(false) {
            guard.start().context("Wi-Fi start 失敗")?;
            // ドライバ起動直後のスキャンは失敗しやすい
            FreeRtos::delay_ms(100);
        }
        let aps = guard.scan().context("スキャン失敗")?;
        Ok(aps
            .into_iter()
            .map(|ap| {
                (
                    ap.ssid.to_string(),
                    ap.signal_strength,
                    ap.auth_method.map_or(false, |m| m != AuthMethod::None),
                )
            })
            .collect())
    }

    /// 接続断を状態へ反映 (失敗時のクリーンアップ)
    pub fn mark_disconnected(&self) {
        if let Ok(mut st) = self.status.lock() {
            st.wifi_connected = false;
            st.wifi_ip.clear();
        }
    }
}
