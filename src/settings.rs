//! NVS に永続化される設定。
//!
//! 現状は画面向き (rotation) のみ。ホストリンクの `ROTATE` コマンドで変更され、
//! 次回起動時も維持される。キオスクの設置向き (壁掛け・逆さ付け等) への対応。

use std::sync::{Arc, Mutex};

use anyhow::Result;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

const NAMESPACE: &str = "alcui";
const KEY_ROTATION: &str = "rotation";

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
            Ok(Some(v)) if matches!(v, 0 | 90 | 180 | 270) => v,
            _ => 0,
        }
    }

    pub fn set_rotation(&self, deg: u16) -> Result<()> {
        anyhow::ensure!(matches!(deg, 0 | 90 | 180 | 270), "不正な角度: {deg}");
        let mut nvs = self.nvs.lock().expect("settings nvs lock");
        nvs.set_u16(KEY_ROTATION, deg)?;
        Ok(())
    }
}
