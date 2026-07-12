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
        let mut nvs = self.nvs.lock().expect("settings nvs lock");
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
        let mut nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_str(KEY_WIFI_SSID, ssid)?;
        nvs.set_str(KEY_WIFI_PASS, password)?;
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
