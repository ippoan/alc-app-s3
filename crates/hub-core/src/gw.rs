//! Windows GW (ippoan/alc-gw) 連携のフレーム組立/解析 (純粋部分)。
//!
//! GW ハブ (ws://<GW-IP>:9000、LAN 内) と交換する JSON メッセージ。
//! 仕様は alc-gw README「CoreS3 連携」節が正。I/O・再接続・状態管理は
//! firmware 側 (hub-drivers::gw_link) が担う。

use serde_json::{json, Value};

/// 接続直後に送る自己紹介。GW はこれで readers 表示のデバイス名を確定する
pub fn hello_frame(device: &str, fw: &str) -> String {
    json!({"src": "cores3", "type": "hello", "device": device, "fw": fw}).to_string()
}

/// 測定の中継フレーム。payload は recorder が組む ble-medical-gateway 互換
/// JSON 文字列 (cf-alc-recorder へ送るものと同一) で、オブジェクトであることを
/// 検証してから包む
pub fn measurement_frame(kind: &str, payload: &str) -> Result<String, String> {
    let v: Value =
        serde_json::from_str(payload).map_err(|e| format!("payload の JSON 解析失敗: {e}"))?;
    if !v.is_object() {
        return Err("payload は JSON オブジェクトではありません".into());
    }
    Ok(json!({"src": "cores3", "type": "measurement", "kind": kind, "payload": v}).to_string())
}

/// 体温計・血圧計の BLE 接続状態。HubStatus の (ble_connected, ble_device)
/// から組む (ble_device は DeviceKind::json_name の "thermometer" /
/// "blood_pressure")
pub fn ble_status_frame(connected: bool, device: &str) -> String {
    json!({
        "src": "cores3",
        "type": "ble_status",
        "thermo": connected && device == "thermometer",
        "bp": connected && device == "blood_pressure",
    })
    .to_string()
}

/// GW からの下りコマンド
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GwDownlink {
    /// `{"src":"gw","type":"ble_command","command":"reset"}` — BLE 操作
    BleCommand(String),
    /// `{"src":"gw","type":"fc1200_command","command":"reset"}` — FC-1200 操作
    /// (点呼UI の測定開始が "reset" として届く)
    Fc1200Command(String),
}

/// GW からの下りフレームを解析する
pub fn parse_downlink(text: &str) -> Result<GwDownlink, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON 解析失敗: {e}"))?;
    let obj = v.as_object().ok_or("JSON オブジェクトではありません")?;
    let command = obj
        .get("command")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .ok_or("command (文字列) がありません")?
        .to_string();
    match obj.get("type").and_then(|t| t.as_str()) {
        Some("ble_command") => Ok(GwDownlink::BleCommand(command)),
        Some("fc1200_command") => Ok(GwDownlink::Fc1200Command(command)),
        Some(other) => Err(format!("不明な type: {other}")),
        None => Err("type がありません".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello() {
        assert_eq!(
            hello_frame("cores3-01", "0.1.0+abc1234"),
            r#"{"device":"cores3-01","fw":"0.1.0+abc1234","src":"cores3","type":"hello"}"#,
        );
    }

    #[test]
    fn measurement_wraps_payload_object() {
        let frame =
            measurement_frame("temperature", r#"{"type":"temperature","value":36.5}"#).unwrap();
        let v: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["src"], "cores3");
        assert_eq!(v["type"], "measurement");
        assert_eq!(v["kind"], "temperature");
        assert_eq!(v["payload"]["value"], 36.5);
    }

    #[test]
    fn measurement_rejects_bad_payload() {
        assert!(measurement_frame("alcohol", "not json").is_err());
        assert!(measurement_frame("alcohol", r#"[1,2]"#).is_err());
    }

    #[test]
    fn ble_status_maps_device_name() {
        assert_eq!(
            ble_status_frame(true, "thermometer"),
            r#"{"bp":false,"src":"cores3","thermo":true,"type":"ble_status"}"#,
        );
        assert_eq!(
            ble_status_frame(true, "blood_pressure"),
            r#"{"bp":true,"src":"cores3","thermo":false,"type":"ble_status"}"#,
        );
        // 未接続は device 名に関わらず両方 false
        assert_eq!(
            ble_status_frame(false, "thermometer"),
            r#"{"bp":false,"src":"cores3","thermo":false,"type":"ble_status"}"#,
        );
    }

    #[test]
    fn parse_downlink_commands() {
        assert_eq!(
            parse_downlink(r#"{"src":"gw","type":"ble_command","command":"reset"}"#),
            Ok(GwDownlink::BleCommand("reset".into())),
        );
        assert_eq!(
            parse_downlink(r#"{"src":"gw","type":"fc1200_command","command":"sensor_lifetime"}"#),
            Ok(GwDownlink::Fc1200Command("sensor_lifetime".into())),
        );
    }

    #[test]
    fn parse_downlink_errors() {
        assert!(parse_downlink("not json").is_err());
        assert!(parse_downlink("[1]").is_err());
        assert!(parse_downlink(r#"{"type":"ble_command"}"#).is_err());
        assert!(parse_downlink(r#"{"type":"ble_command","command":""}"#).is_err());
        assert!(parse_downlink(r#"{"type":"nfc_command","command":"x"}"#).is_err());
        assert!(parse_downlink(r#"{"command":"reset"}"#).is_err());
    }
}
