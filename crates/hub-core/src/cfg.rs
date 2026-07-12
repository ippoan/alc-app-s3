//! デバイス設定のエクスポート/インポート (JSON)。
//!
//! Pages の設定カードが `CFG GET` / `CFG SET <json>` で読み書きする。
//! スキーマ:
//!
//! ```json
//! {"version":1,"rotation":180,"wifi":{"ssid":"...","password":"..."}}
//! ```
//!
//! - フィールドは省略可能 (省略されたものは変更しない)
//! - serde derive ではなく手動で組み立て/分解する (llvm-cov のライン網羅を
//!   自前コードで保証するため)

use serde_json::{json, Map, Value};

use crate::protocol::valid_rotation;

pub const CONFIG_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WifiConfig {
    pub ssid: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceConfig {
    pub rotation: Option<u16>,
    pub wifi: Option<WifiConfig>,
}

impl DeviceConfig {
    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert("version".into(), CONFIG_VERSION.into());
        if let Some(r) = self.rotation {
            obj.insert("rotation".into(), r.into());
        }
        if let Some(w) = &self.wifi {
            obj.insert(
                "wifi".into(),
                json!({ "ssid": w.ssid, "password": w.password }),
            );
        }
        Value::Object(obj).to_string()
    }

    /// JSON から読み込み、値の妥当性まで検証する。
    /// エラーはそのままホストへ返せる日本語メッセージ。
    pub fn from_json(s: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("JSON 解析失敗: {e}"))?;
        let obj = v.as_object().ok_or("JSON オブジェクトではありません")?;

        let rotation = match obj.get("rotation") {
            None | Some(Value::Null) => None,
            Some(r) => {
                let deg = r
                    .as_u64()
                    .ok_or("rotation は数値 (0|90|180|270) で指定してください")?;
                let deg = u16::try_from(deg).map_err(|_| "rotation が大きすぎます")?;
                if !valid_rotation(deg) {
                    return Err(format!("rotation が不正です: {deg} (0|90|180|270)"));
                }
                Some(deg)
            }
        };

        let wifi = match obj.get("wifi") {
            None | Some(Value::Null) => None,
            Some(w) => {
                let wobj = w.as_object().ok_or("wifi はオブジェクトで指定してください")?;
                let ssid = wobj
                    .get("ssid")
                    .and_then(|s| s.as_str())
                    .ok_or("wifi.ssid (文字列) が必要です")?
                    .to_string();
                if ssid.is_empty() || ssid.len() > 32 {
                    return Err("wifi.ssid は 1〜32 バイトで指定してください".into());
                }
                let password = match wobj.get("password") {
                    None | Some(Value::Null) => String::new(),
                    Some(p) => p
                        .as_str()
                        .ok_or("wifi.password は文字列で指定してください")?
                        .to_string(),
                };
                if password.len() > 64 {
                    return Err("wifi.password は 64 バイト以下で指定してください".into());
                }
                Some(WifiConfig { ssid, password })
            }
        };

        Ok(Self { rotation, wifi })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full() {
        let cfg = DeviceConfig {
            rotation: Some(180),
            wifi: Some(WifiConfig {
                ssid: "Buffalo-2G-40E0".into(),
                password: "secret pass".into(),
            }),
        };
        let parsed = DeviceConfig::from_json(&cfg.to_json()).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn empty_config_serializes_version_only() {
        let cfg = DeviceConfig::default();
        assert_eq!(cfg.to_json(), r#"{"version":1}"#);
        assert_eq!(DeviceConfig::from_json(&cfg.to_json()).unwrap(), cfg);
    }

    #[test]
    fn partial_rotation_only() {
        let cfg = DeviceConfig::from_json(r#"{"rotation":90}"#).unwrap();
        assert_eq!(cfg.rotation, Some(90));
        assert_eq!(cfg.wifi, None);
    }

    #[test]
    fn wifi_password_defaults_to_empty() {
        let cfg = DeviceConfig::from_json(r#"{"wifi":{"ssid":"ap"}}"#).unwrap();
        assert_eq!(cfg.wifi.unwrap().password, "");
        let cfg = DeviceConfig::from_json(r#"{"wifi":{"ssid":"ap","password":null}}"#).unwrap();
        assert_eq!(cfg.wifi.unwrap().password, "");
    }

    #[test]
    fn null_fields_are_ignored() {
        let cfg = DeviceConfig::from_json(r#"{"rotation":null,"wifi":null}"#).unwrap();
        assert_eq!(cfg, DeviceConfig::default());
    }

    #[test]
    fn invalid_json_and_shape() {
        assert!(DeviceConfig::from_json("{oops").is_err());
        assert!(DeviceConfig::from_json("[1,2]").is_err());
    }

    #[test]
    fn invalid_rotation() {
        assert!(DeviceConfig::from_json(r#"{"rotation":"90"}"#).is_err());
        assert!(DeviceConfig::from_json(r#"{"rotation":45}"#).is_err());
        assert!(DeviceConfig::from_json(r#"{"rotation":99999}"#).is_err());
    }

    #[test]
    fn invalid_wifi() {
        assert!(DeviceConfig::from_json(r#"{"wifi":"ap"}"#).is_err());
        assert!(DeviceConfig::from_json(r#"{"wifi":{}}"#).is_err());
        assert!(DeviceConfig::from_json(r#"{"wifi":{"ssid":""}}"#).is_err());
        let long_ssid = "x".repeat(33);
        assert!(
            DeviceConfig::from_json(&format!(r#"{{"wifi":{{"ssid":"{long_ssid}"}}}}"#)).is_err()
        );
        assert!(DeviceConfig::from_json(r#"{"wifi":{"ssid":"ap","password":1}}"#).is_err());
        let long_pass = "x".repeat(65);
        assert!(DeviceConfig::from_json(&format!(
            r#"{{"wifi":{{"ssid":"ap","password":"{long_pass}"}}}}"#
        ))
        .is_err());
    }
}
