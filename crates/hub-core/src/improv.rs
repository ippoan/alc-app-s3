//! Improv Wi-Fi Serial プロトコルのフレーム解析・構築 (純粋部分)。
//!
//! https://www.improv-wifi.com/serial/
//! ESP Web Tools がファームウェア書き込み後にこのプロトコルで Wi-Fi 設定
//! (SSID/パスワード入力・ネットワーク選択) を行う。シリアル I/O と Wi-Fi
//! 接続の副作用は firmware 側 (src/improv.rs) が担う。
//!
//! パケット形式:
//! `"IMPROV" (6) + version (1) + type (1) + len (1) + data (len) + checksum (1)`
//! checksum は先頭からの全バイト和の下位 8bit。

pub const HEADER: &[u8; 6] = b"IMPROV";
pub const VERSION: u8 = 0x01;

// パケット種別
pub const TYPE_CURRENT_STATE: u8 = 0x01;
pub const TYPE_ERROR_STATE: u8 = 0x02;
pub const TYPE_RPC_COMMAND: u8 = 0x03;
pub const TYPE_RPC_RESULT: u8 = 0x04;

// デバイス状態
pub const STATE_READY: u8 = 0x02;
pub const STATE_PROVISIONING: u8 = 0x03;
pub const STATE_PROVISIONED: u8 = 0x04;

// エラーコード
pub const ERROR_NONE: u8 = 0x00;
pub const ERROR_INVALID_RPC: u8 = 0x01;
pub const ERROR_UNKNOWN_RPC: u8 = 0x02;
pub const ERROR_UNABLE_TO_CONNECT: u8 = 0x03;

// RPC コマンド
pub const CMD_WIFI_SETTINGS: u8 = 0x01;
pub const CMD_REQUEST_STATE: u8 = 0x02;
pub const CMD_REQUEST_INFO: u8 = 0x03;
pub const CMD_REQUEST_SCAN: u8 = 0x04;

/// 受信バッファ先頭の解析結果
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// Improv パケットの可能性があるがバイト数不足
    NeedMore,
    /// Improv パケットではない (テキスト行として処理すべき)
    NotImprov,
    /// 完全なパケット
    Packet {
        ptype: u8,
        data: Vec<u8>,
        consumed: usize,
    },
    /// ヘッダは一致するが version/checksum 不正 (読み捨てる)
    Corrupt { consumed: usize },
}

/// バッファ先頭から Improv パケットの解析を試みる
pub fn try_parse(acc: &[u8]) -> Frame {
    let n = acc.len().min(HEADER.len());
    if acc[..n] != HEADER[..n] {
        return Frame::NotImprov;
    }
    if acc.len() < 10 {
        return Frame::NeedMore;
    }
    let len = acc[8] as usize;
    let total = 10 + len;
    if acc.len() < total {
        return Frame::NeedMore;
    }
    let sum = acc[..total - 1]
        .iter()
        .fold(0u8, |a, b| a.wrapping_add(*b));
    if acc[6] != VERSION || sum != acc[total - 1] {
        return Frame::Corrupt { consumed: total };
    }
    Frame::Packet {
        ptype: acc[7],
        data: acc[9..total - 1].to_vec(),
        consumed: total,
    }
}

/// パケットを構築する (checksum 付与込み)
pub fn build_packet(ptype: u8, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(10 + data.len());
    p.extend_from_slice(HEADER);
    p.push(VERSION);
    p.push(ptype);
    p.push(data.len() as u8);
    p.extend_from_slice(data);
    let sum = p.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    p.push(sum);
    p
}

/// Current State パケット
pub fn build_state(state: u8) -> Vec<u8> {
    build_packet(TYPE_CURRENT_STATE, &[state])
}

/// Error State パケット
pub fn build_error(code: u8) -> Vec<u8> {
    build_packet(TYPE_ERROR_STATE, &[code])
}

/// RPC Result パケット (cmd エコー + 文字列リスト)
pub fn build_rpc_result(cmd: u8, strings: &[&str]) -> Vec<u8> {
    let mut payload = Vec::new();
    for s in strings {
        let b = s.as_bytes();
        payload.push(b.len() as u8);
        payload.extend_from_slice(b);
    }
    let mut data = Vec::with_capacity(2 + payload.len());
    data.push(cmd);
    data.push(payload.len() as u8);
    data.extend_from_slice(&payload);
    build_packet(TYPE_RPC_RESULT, &data)
}

/// RPC Command の data 部を (cmd, payload) に分解
pub fn parse_rpc(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() < 2 {
        return None;
    }
    let cmd = data[0];
    let len = data[1] as usize;
    if data.len() < 2 + len {
        return None;
    }
    Some((cmd, &data[2..2 + len]))
}

/// Wi-Fi 設定 (CMD_WIFI_SETTINGS の payload): ssid_len, ssid, pass_len, pass
#[derive(Debug, PartialEq, Eq)]
pub struct WifiSettings {
    pub ssid: String,
    pub password: String,
}

pub fn parse_wifi_settings(payload: &[u8]) -> Option<WifiSettings> {
    if payload.is_empty() {
        return None;
    }
    let ssid_len = payload[0] as usize;
    if payload.len() < 1 + ssid_len + 1 {
        return None;
    }
    let ssid = String::from_utf8(payload[1..1 + ssid_len].to_vec()).ok()?;
    let pass_len = payload[1 + ssid_len] as usize;
    let pass_start = 2 + ssid_len;
    if payload.len() < pass_start + pass_len {
        return None;
    }
    let password = String::from_utf8(payload[pass_start..pass_start + pass_len].to_vec()).ok()?;
    Some(WifiSettings { ssid, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packet なら中身を取り出す (テスト用。非 Packet は None)
    fn as_packet(f: Frame) -> Option<(u8, Vec<u8>, usize)> {
        match f {
            Frame::Packet {
                ptype,
                data,
                consumed,
            } => Some((ptype, data, consumed)),
            _ => None,
        }
    }

    #[test]
    fn roundtrip_state_packet() {
        let p = build_state(STATE_READY);
        let (ptype, data, consumed) = as_packet(try_parse(&p)).unwrap();
        assert_eq!(ptype, TYPE_CURRENT_STATE);
        assert_eq!(data, vec![STATE_READY]);
        assert_eq!(consumed, p.len());
        // 非パケット入力は as_packet で None になる
        assert_eq!(as_packet(try_parse(b"PING\n")), None);
    }

    #[test]
    fn error_packet_type() {
        let p = build_error(ERROR_UNABLE_TO_CONNECT);
        assert_eq!(p[7], TYPE_ERROR_STATE);
        assert_eq!(p[9], ERROR_UNABLE_TO_CONNECT);
    }

    #[test]
    fn not_improv_for_text() {
        assert_eq!(try_parse(b"PING\n"), Frame::NotImprov);
    }

    #[test]
    fn need_more_for_partial_header_and_body() {
        assert_eq!(try_parse(b"IMP"), Frame::NeedMore);
        let p = build_state(STATE_READY);
        assert_eq!(try_parse(&p[..9]), Frame::NeedMore);
        assert_eq!(try_parse(&p[..p.len() - 1]), Frame::NeedMore);
    }

    #[test]
    fn corrupt_checksum_is_skipped() {
        let mut p = build_state(STATE_READY);
        let last = p.len() - 1;
        p[last] = p[last].wrapping_add(1);
        assert_eq!(try_parse(&p), Frame::Corrupt { consumed: p.len() });
    }

    #[test]
    fn corrupt_version_is_skipped() {
        let mut p = build_state(STATE_READY);
        p[6] = 0x7F;
        // version を書き換えたので checksum も合わせて再計算
        let last = p.len() - 1;
        let sum = p[..last].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        p[last] = sum;
        assert_eq!(try_parse(&p), Frame::Corrupt { consumed: p.len() });
    }

    #[test]
    fn rpc_result_layout() {
        let p = build_rpc_result(CMD_REQUEST_INFO, &["fw", "1.0"]);
        let (ptype, data, _) = as_packet(try_parse(&p)).unwrap();
        assert_eq!(ptype, TYPE_RPC_RESULT);
        // cmd, payload_len, (len,"fw"), (len,"1.0")
        assert_eq!(data[0], CMD_REQUEST_INFO);
        assert_eq!(data[1] as usize, data.len() - 2);
        assert_eq!(&data[2..], &[2, b'f', b'w', 3, b'1', b'.', b'0']);
    }

    #[test]
    fn parse_rpc_command() {
        assert_eq!(parse_rpc(&[CMD_REQUEST_STATE, 0]), Some((CMD_REQUEST_STATE, &[][..])));
        assert_eq!(parse_rpc(&[CMD_WIFI_SETTINGS, 2, 0xAA, 0xBB]).unwrap().1, &[0xAA, 0xBB]);
        assert_eq!(parse_rpc(&[0x01]), None); // 長さ不足
        assert_eq!(parse_rpc(&[0x01, 5, 0x00]), None); // payload 不足
    }

    #[test]
    fn parse_wifi_settings_ok() {
        // ssid="ap", pass="secret12"
        let mut payload = vec![2u8, b'a', b'p', 8];
        payload.extend_from_slice(b"secret12");
        assert_eq!(
            parse_wifi_settings(&payload),
            Some(WifiSettings {
                ssid: "ap".into(),
                password: "secret12".into(),
            })
        );
    }

    #[test]
    fn parse_wifi_settings_open_network() {
        // パスワード無し (オープンネットワーク)
        let payload = [2u8, b'a', b'p', 0];
        let s = parse_wifi_settings(&payload).unwrap();
        assert_eq!(s.ssid, "ap");
        assert_eq!(s.password, "");
    }

    #[test]
    fn parse_wifi_settings_malformed() {
        assert_eq!(parse_wifi_settings(&[]), None);
        assert_eq!(parse_wifi_settings(&[5, b'a']), None); // ssid 不足
        assert_eq!(parse_wifi_settings(&[1, b'a', 9, b'x']), None); // pass 不足
        // 不正 UTF-8
        assert_eq!(parse_wifi_settings(&[1, 0xFF, 0]), None);
    }
}
