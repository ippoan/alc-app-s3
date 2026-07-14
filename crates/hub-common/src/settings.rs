//! NVS に永続化される設定。
//!
//! 現状は画面向き (rotation) のみ。ホストリンクの `ROTATE` コマンドで変更され、
//! 次回起動時も維持される。キオスクの設置向き (壁掛け・逆さ付け等) への対応。

use std::sync::{Arc, Mutex};

use alc_hub_core::cfg::{DeviceConfig, WifiConfig};
use alc_hub_core::protocol::valid_rotation;
use anyhow::Result;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

const NAMESPACE: &str = "alcui";
const KEY_ROTATION: &str = "rotation";
const KEY_WIFI_SSID: &str = "wifi_ssid";
const KEY_WIFI_PASS: &str = "wifi_pass";
/// 測定ログ (改行区切りの直近測定) を保存する NVS キー
const KEY_MEAS_LOG: &str = "meas_log";
/// 永続化する測定ログの最大行数 (NVS 文字列サイズを抑える)
const MAX_LOG_LINES: usize = 20;
// auth-worker device credential (ippoan/alc-app-s3#20)。
// device_secret は秘密のため DeviceConfig (CFG GET) には決して含めない。
const KEY_DEV_ID: &str = "dev_id";
const KEY_DEV_SECRET: &str = "dev_secret";
const KEY_DEV_TENANT: &str = "dev_tenant";
/// auth-worker ベース URL の上書き (staging テスト用、`AUTH URL` コマンド)
const KEY_AUTH_URL: &str = "auth_url";
// WS 送信 (cf-alc-recorder、ippoan/alc-app-s3#21)
/// 未 ack の送信キュー (uplink::UplinkQueue::serialize の改行区切り)
const KEY_WS_QUEUE: &str = "ws_queue";
/// seq 採番カウンタ。ack 後も再利用しない (サーバ側 UNIQUE 冪等化のため)
const KEY_WS_SEQ: &str = "ws_seq";
/// cf-alc-recorder WS URL の上書き (`WS URL` コマンド)
const KEY_WS_URL: &str = "ws_url";
/// プリンター宛先 host:port (印刷ブリッジ、`PRINTER ADDR` コマンド。#38)
const KEY_PRINTER_ADDR: &str = "printer_addr";

#[derive(Clone)]
pub struct Settings {
    nvs: Arc<Mutex<EspNvs<NvsDefault>>>,
}

impl Settings {
    pub fn new(partition: EspDefaultNvsPartition) -> Result<Self> {
        let nvs = EspNvs::new(partition, NAMESPACE, true)?;
        Ok(Self {
            nvs: Arc::new(Mutex::new(nvs)),
        })
    }

    /// 画面向き (0 / 90 / 180 / 270 度)。未設定・不正値は 0。
    pub fn rotation(&self) -> u16 {
        let Ok(nvs) = self.nvs.lock() else { return 0 };
        match nvs.get_u16(KEY_ROTATION) {
            Ok(Some(v)) if valid_rotation(v) => v,
            _ => 0,
        }
    }

    pub fn set_rotation(&self, deg: u16) -> Result<()> {
        anyhow::ensure!(valid_rotation(deg), "不正な角度: {deg}");
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_u16(KEY_ROTATION, deg)?;
        Ok(())
    }

    /// 保存済み Wi-Fi 認証情報 (SSID, パスワード)。未設定は None
    pub fn wifi_credentials(&self) -> Option<(String, String)> {
        let nvs = self.nvs.lock().ok()?;
        let mut ssid_buf = [0u8; 64];
        let ssid = nvs.get_str(KEY_WIFI_SSID, &mut ssid_buf).ok()??.to_string();
        if ssid.is_empty() {
            return None;
        }
        let mut pass_buf = [0u8; 96];
        let pass = nvs
            .get_str(KEY_WIFI_PASS, &mut pass_buf)
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_string();
        Some((ssid, pass))
    }

    /// Wi-Fi 認証情報を保存 (Improv Wi-Fi Serial から呼ばれる)
    pub fn set_wifi_credentials(&self, ssid: &str, password: &str) -> Result<()> {
        anyhow::ensure!(!ssid.is_empty() && ssid.len() <= 32, "SSID は 1〜32 バイト");
        anyhow::ensure!(password.len() <= 64, "パスワードは 64 バイト以下");
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_str(KEY_WIFI_SSID, ssid)?;
        nvs.set_str(KEY_WIFI_PASS, password)?;
        Ok(())
    }

    /// 永続化された測定ログ (古い→新しい)。リブートしても残る。
    pub fn measurement_log(&self) -> Vec<String> {
        let Ok(nvs) = self.nvs.lock() else {
            return Vec::new();
        };
        let mut buf = [0u8; 2048];
        match nvs.get_str(KEY_MEAS_LOG, &mut buf) {
            Ok(Some(s)) if !s.is_empty() => s.lines().map(str::to_string).collect(),
            _ => Vec::new(),
        }
    }

    /// 測定ログに 1 行追記し、直近 MAX_LOG_LINES 行だけ NVS に残す。
    /// recorder スレッドから呼ばれる (BLE コールバックからは呼ばない)。
    pub fn append_measurement_log(&self, line: &str) {
        let mut lines = self.measurement_log();
        lines.push(line.to_string());
        let start = lines.len().saturating_sub(MAX_LOG_LINES);
        let kept = lines[start..].join("\n");
        if let Ok(nvs) = self.nvs.lock() {
            if let Err(e) = nvs.set_str(KEY_MEAS_LOG, &kept) {
                log::warn!("settings: 測定ログ保存失敗: {e:?}");
            }
        }
    }

    /// auth-worker device credential (device_id, device_secret)。未登録は None。
    /// secret はホストへ出力しないこと (CFG GET にも含めない)
    pub fn device_credential(&self) -> Option<(String, String)> {
        let nvs = self.nvs.lock().ok()?;
        let mut id_buf = [0u8; 64];
        let id = nvs.get_str(KEY_DEV_ID, &mut id_buf).ok()??.to_string();
        if id.is_empty() {
            return None;
        }
        let mut sec_buf = [0u8; 128];
        let secret = nvs.get_str(KEY_DEV_SECRET, &mut sec_buf).ok()??.to_string();
        if secret.is_empty() {
            return None;
        }
        Some((id, secret))
    }

    /// credential に紐づく tenant_id (AUTH STATUS / WS 送信用)。未登録は None
    pub fn device_tenant(&self) -> Option<String> {
        let nvs = self.nvs.lock().ok()?;
        let mut buf = [0u8; 64];
        let t = nvs.get_str(KEY_DEV_TENANT, &mut buf).ok()??.to_string();
        (!t.is_empty()).then_some(t)
    }

    /// ペアリング承認後に 1 回だけ受け取れる credential を保存する
    pub fn set_device_credential(&self, id: &str, secret: &str, tenant: &str) -> Result<()> {
        anyhow::ensure!(!id.is_empty() && id.len() < 64, "device_id が不正です");
        anyhow::ensure!(
            !secret.is_empty() && secret.len() < 128,
            "device_secret が不正です"
        );
        anyhow::ensure!(tenant.len() < 64, "tenant_id が不正です");
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_str(KEY_DEV_ID, id)?;
        nvs.set_str(KEY_DEV_SECRET, secret)?;
        nvs.set_str(KEY_DEV_TENANT, tenant)?;
        Ok(())
    }

    /// 保存済み credential を破棄する (`AUTH UNPAIR`)。サーバ側の revoke は
    /// operator が auth-worker 側で行う
    pub fn clear_device_credential(&self) -> Result<()> {
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.remove(KEY_DEV_ID)?;
        nvs.remove(KEY_DEV_SECRET)?;
        nvs.remove(KEY_DEV_TENANT)?;
        Ok(())
    }

    /// auth-worker ベース URL (`AUTH URL` で上書き、既定は config 定数)
    pub fn auth_url(&self) -> String {
        let fallback = || crate::config::AUTH_WORKER_URL_DEFAULT.to_string();
        let Ok(nvs) = self.nvs.lock() else {
            return fallback();
        };
        let mut buf = [0u8; 128];
        match nvs.get_str(KEY_AUTH_URL, &mut buf) {
            Ok(Some(s)) if !s.is_empty() => s.to_string(),
            _ => fallback(),
        }
    }

    pub fn set_auth_url(&self, url: &str) -> Result<()> {
        anyhow::ensure!(
            (url.starts_with("https://") || url.starts_with("http://")) && url.len() < 128,
            "URL が不正です"
        );
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_str(KEY_AUTH_URL, url.trim_end_matches('/'))?;
        Ok(())
    }

    /// 未 ack の WS 送信キュー (uplink::UplinkQueue::restore へ渡す)
    pub fn ws_queue(&self) -> String {
        let Ok(nvs) = self.nvs.lock() else {
            return String::new();
        };
        let mut buf = [0u8; 4096];
        match nvs.get_str(KEY_WS_QUEUE, &mut buf) {
            Ok(Some(s)) => s.to_string(),
            _ => String::new(),
        }
    }

    pub fn set_ws_queue(&self, lines: &str) {
        let nvs = self.nvs.lock().expect("settings nvs lock");
        if let Err(e) = nvs.set_str(KEY_WS_QUEUE, lines) {
            log::warn!("settings: WS キュー保存失敗: {e:?}");
        }
    }

    /// WS 送信の seq 採番カウンタ (未保存は 0)
    pub fn ws_last_seq(&self) -> u64 {
        let Ok(nvs) = self.nvs.lock() else { return 0 };
        nvs.get_u64(KEY_WS_SEQ).ok().flatten().unwrap_or(0)
    }

    pub fn set_ws_last_seq(&self, seq: u64) {
        let nvs = self.nvs.lock().expect("settings nvs lock");
        if let Err(e) = nvs.set_u64(KEY_WS_SEQ, seq) {
            log::warn!("settings: WS seq 保存失敗: {e:?}");
        }
    }

    /// cf-alc-recorder の WS URL (`WS URL` で上書き、既定は config 定数)
    pub fn ws_url(&self) -> String {
        let fallback = || crate::config::RECORDER_WS_URL_DEFAULT.to_string();
        let Ok(nvs) = self.nvs.lock() else {
            return fallback();
        };
        let mut buf = [0u8; 160];
        match nvs.get_str(KEY_WS_URL, &mut buf) {
            Ok(Some(s)) if !s.is_empty() => s.to_string(),
            _ => fallback(),
        }
    }

    pub fn set_ws_url(&self, url: &str) -> Result<()> {
        anyhow::ensure!(
            (url.starts_with("wss://") || url.starts_with("ws://")) && url.len() < 160,
            "URL が不正です"
        );
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_str(KEY_WS_URL, url)?;
        Ok(())
    }

    /// プリンター宛先 host:port (`PRINTER ADDR` で保存)。未設定は None
    pub fn printer_addr(&self) -> Option<String> {
        let nvs = self.nvs.lock().ok()?;
        let mut buf = [0u8; 128];
        let addr = nvs.get_str(KEY_PRINTER_ADDR, &mut buf).ok()??.to_string();
        (!addr.is_empty()).then_some(addr)
    }

    pub fn set_printer_addr(&self, addr: &str) -> Result<()> {
        anyhow::ensure!(
            alc_hub_core::printer::valid_addr(addr) && addr.len() < 128,
            "宛先が不正です (host:port)"
        );
        let nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_str(KEY_PRINTER_ADDR, addr)?;
        Ok(())
    }

    /// 現在の設定を DeviceConfig にまとめる (CFG GET / エクスポート用)
    pub fn export(&self) -> DeviceConfig {
        DeviceConfig {
            rotation: Some(self.rotation()),
            wifi: self
                .wifi_credentials()
                .map(|(ssid, password)| WifiConfig { ssid, password }),
        }
    }

    /// DeviceConfig を適用する (CFG SET / インポート用)。
    /// 指定されたフィールドだけを更新する。
    pub fn apply(&self, cfg: &DeviceConfig) -> Result<()> {
        if let Some(deg) = cfg.rotation {
            self.set_rotation(deg)?;
        }
        if let Some(w) = &cfg.wifi {
            self.set_wifi_credentials(&w.ssid, &w.password)?;
        }
        Ok(())
    }
}
