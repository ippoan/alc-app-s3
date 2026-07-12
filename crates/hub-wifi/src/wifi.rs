//! Wi-Fi STA 管理 (Improv Wi-Fi Serial から設定される)。
//!
//! 主経路はあくまで LAN Module 13.2 (PoE) だが、LAN 配線が無い拠点向けの
//! 代替経路として Wi-Fi STA を持つ (plan/cores3-hub-consolidation.md の
//! セルラー検討と同じ位置付け)。ESP32-S3 は 2.4GHz (11b/g/n) のみ対応。
//!
//! 実装メモ:
//! - 接続待ちは自前のタイムアウト付きポーリング。esp-idf-svc の BlockingWifi
//!   は接続失敗時 (パスワード不一致等) に無期限ブロックし得るため使わない
//! - BLE (NimBLE) とのコエグジスト対策として、接続/スキャン中は busy フラグを
//!   立て、ble.rs 側が BLE スキャンを一時停止する

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{delay::FreeRtos, modem::Modem},
    nvs::EspDefaultNvsPartition,
    wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi},
};

use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_core::coex::RadioCoex;

/// 接続 (アソシエーション + DHCP) の待ち時間上限。
/// 単一 2.4GHz 無線を BLE (医療機器・優先) と共有するため、接続失敗時に
/// radio を長く占有しないよう短めにする (BLE スキャンへの妨害を減らす)。
const CONNECT_TIMEOUT_MS: u64 = 8_000;

/// スキャン結果 1 件: (SSID, RSSI, 認証あり)
pub type ScanEntry = (String, i8, bool);

#[derive(Clone)]
pub struct Wifi {
    inner: Arc<Mutex<EspWifi<'static>>>,
    status: SharedStatus,
    /// BLE とのコエグジスト調停 (hub-ble がスキャン前に参照)
    coex: Arc<RadioCoex>,
}

impl Wifi {
    pub fn new(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        status: SharedStatus,
    ) -> Result<Self> {
        let wifi = EspWifi::new(modem, sysloop, Some(nvs))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(wifi)),
            status,
            coex: Arc::new(RadioCoex::new()),
        })
    }

    /// BLE 側が参照するコエグジスト調停ハンドル
    pub fn coex_handle(&self) -> Arc<RadioCoex> {
        Arc::clone(&self.coex)
    }

    /// Improv セッション中など、一定時間 BLE スキャンを止めておく
    pub fn pause_ble_for(&self, ms: u64) {
        self.coex.pause_ble_for(now_ms(), ms);
    }

    /// STA として接続 (最大 20 秒ブロック)。成功時は IP アドレス文字列を返す。
    pub fn connect(&self, ssid: &str, password: &str) -> Result<String> {
        self.coex.set_wifi_busy(true);
        let result = self.connect_inner(ssid, password);
        self.coex.set_wifi_busy(false);
        result
    }

    fn connect_inner(&self, ssid: &str, password: &str) -> Result<String> {
        let mut wifi = self.inner.lock().expect("wifi lock");

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

        wifi.set_configuration(&config)
            .context("Wi-Fi 設定の適用失敗")?;
        if !wifi.is_started().unwrap_or(false) {
            wifi.start().context("Wi-Fi start 失敗")?;
        }
        // 再接続時は一旦切断してから
        let _ = wifi.disconnect();
        FreeRtos::delay_ms(200);
        wifi.connect().context("Wi-Fi 接続開始失敗")?;

        // アソシエーション + DHCP をポーリング (タイムアウト付き)
        let deadline = now_ms() + CONNECT_TIMEOUT_MS;
        let ip = loop {
            if wifi.is_connected().unwrap_or(false) {
                if let Ok(info) = wifi.sta_netif().get_ip_info() {
                    if info.ip != Ipv4Addr::UNSPECIFIED {
                        break info.ip.to_string();
                    }
                }
            }
            if now_ms() >= deadline {
                let _ = wifi.disconnect(); // 接続試行を止めておく
                anyhow::bail!(
                    "接続タイムアウト ({}s) — SSID/パスワード/電波状況を確認",
                    CONNECT_TIMEOUT_MS / 1000
                );
            }
            FreeRtos::delay_ms(250);
        };

        if let Ok(mut st) = self.status.lock() {
            st.wifi_connected = true;
            st.wifi_ip = ip.clone();
            st.push_event(now_ms(), &format!("WiFi 接続 {ip}"));
        }
        Ok(ip)
    }

    /// 周辺ネットワークのスキャン (Improv の REQUEST_SCAN 用)
    pub fn scan(&self) -> Result<Vec<ScanEntry>> {
        self.coex.set_wifi_busy(true);
        let result = self.scan_inner();
        self.coex.set_wifi_busy(false);
        result
    }

    fn scan_inner(&self) -> Result<Vec<ScanEntry>> {
        let mut wifi = self.inner.lock().expect("wifi lock");
        if !wifi.is_started().unwrap_or(false) {
            wifi.start().context("Wi-Fi start 失敗")?;
            // ドライバ起動直後のスキャンは失敗しやすい
            FreeRtos::delay_ms(100);
        }
        let aps = wifi.scan().context("スキャン失敗")?;
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

    /// 接続テスト: 失敗時はその場でスキャンして原因を切り分けたメッセージを返す
    /// (SSID が見えない = 2.4GHz/SSID 間違い、見える = パスワード/認証の可能性)
    pub fn connect_with_diagnosis(&self, ssid: &str, password: &str) -> Result<String, String> {
        match self.connect(ssid, password) {
            Ok(ip) => Ok(ip),
            Err(e) => {
                let base = format!("{e:#}");
                let Ok(aps) = self.scan() else {
                    return Err(base);
                };
                match aps.iter().find(|(s, _, _)| s == ssid) {
                    Some((_, rssi, auth)) => Err(format!(
                        "AP は検出 (RSSI {rssi}dBm, 認証{}) — パスワード/認証方式を確認: {base}",
                        if *auth { "あり" } else { "なし" }
                    )),
                    None => Err(format!(
                        "SSID '{ssid}' が見つからない (検出 {} 件) — 2.4GHz 帯か・SSID を確認: {base}",
                        aps.len()
                    )),
                }
            }
        }
    }

    /// STA が接続中 (アソシエーション成立) か
    pub fn is_up(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|w| w.is_connected().ok())
            .unwrap_or(false)
    }

    /// 接続断を状態へ反映 (失敗時のクリーンアップ)
    pub fn mark_disconnected(&self) {
        if let Ok(mut st) = self.status.lock() {
            st.wifi_connected = false;
            st.wifi_ip.clear();
        }
    }

    /// 保存済み認証情報で接続を維持する常駐ループ (別スレッドで回す)。
    /// 切断を検出したら再接続する。単一 2.4GHz 無線を BLE (医療機器・優先) と
    /// 共有するため、失敗が続く場合はバックオフを段階的に伸ばし、Wi-Fi の
    /// 再接続試行が BLE の広告受信を妨げ続けないようにする。
    /// (主経路は LAN Module 13.2 で Wi-Fi は補助)
    pub fn keepalive(&self, ssid: String, password: String) {
        const BACKOFF_MIN_MS: u32 = 15_000;
        const BACKOFF_MAX_MS: u32 = 5 * 60_000; // 5 分
        let mut backoff = BACKOFF_MIN_MS;
        loop {
            if self.is_up() {
                backoff = BACKOFF_MIN_MS; // 接続中はバックオフをリセット
                FreeRtos::delay_ms(10_000);
                continue;
            }
            match self.connect(&ssid, &password) {
                Ok(ip) => {
                    log::info!("wifi: (再)接続成功 {ip}");
                    backoff = BACKOFF_MIN_MS;
                    FreeRtos::delay_ms(10_000);
                }
                Err(e) => {
                    log::warn!("wifi: (再)接続失敗 (次回まで {}s): {e:?}", backoff / 1000);
                    self.mark_disconnected();
                    if let Ok(mut st) = self.status.lock() {
                        st.push_event(now_ms(), "WiFi 再接続失敗");
                    }
                    // 失敗が続くほど間隔を伸ばし、BLE への妨害を最小化する
                    FreeRtos::delay_ms(backoff);
                    backoff = (backoff * 2).min(BACKOFF_MAX_MS);
                }
            }
        }
    }
}
