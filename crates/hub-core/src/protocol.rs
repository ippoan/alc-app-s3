//! ホストリンク行プロトコルの解析 (純粋部分)。
//!
//! I/O・画面遷移・NVS 保存などの副作用は firmware 側 (host_link.rs) が担い、
//! ここでは「1 行 → コマンド or エラー応答文字列」の変換のみを行う。

/// ホスト (Windows PC / Android タブレット) からのコマンド
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCommand {
    Ping,
    ShowQr { payload: String, timeout_ms: u64 },
    Measure,
    Result { ok: bool, value: String },
    ShowError { message: String },
    Reset,
    Rotate(u16),
    Status,
    /// 設定のエクスポート (`CFG <json>` を応答)
    CfgGet,
    /// 設定のインポート (JSON は cfg::DeviceConfig::from_json で解釈)
    CfgSet { json: String },
    /// 保存済み Wi-Fi 設定での接続テスト (結果は `EVT WIFI_TEST ...`)
    WifiTest,
    /// BLE の全ボンド消去 → 次接続で再ペアリング (血圧計の暗号化接続復旧)
    BlePair,
    /// device credential の直接注入 (USB 前提の provisioning — ホストが
    /// auth-worker `/device/pair` 系で取得した credential をシリアルで渡す)
    AuthSet {
        device_id: String,
        device_secret: String,
        tenant_id: String,
    },
    /// 保存済み device credential の破棄 (ローカルのみ。サーバ側 revoke は
    /// operator が auth-worker で行う)
    AuthUnpair,
    /// ペアリング状態の問い合わせ (`AUTH PAIRED ...` / `AUTH UNPAIRED` を応答)
    AuthStatus,
    /// auth-worker ベース URL の上書き (staging テスト用。NVS 保存)
    AuthUrl { url: String },
    /// 保存済み credential で device JWT を取得する自己診断
    AuthToken,
    /// cf-alc-recorder WS URL の上書き (staging テスト用。NVS 保存)
    WsUrl { url: String },
    /// WS 送信の状態問い合わせ (`WS CONNECTED=1 QUEUE=3 SEQ=42` を応答)
    WsStatus,
    /// ヒープ状態の問い合わせ
    /// (`HEAP FREE_INT=<n> MIN_INT=<n> FREE_PSRAM=<n> ...` を応答、Refs #27)
    Heap,
    /// ヒープ詳細ダンプ: タスク別スタック余裕 + ヒープブロック概況
    /// (`HEAPDUMP ...` 複数行を応答)
    HeapDump,
    /// OTA 更新: firmware (app 単体イメージ) の URL からダウンロードして
    /// もう一方の OTA スロットへ書き込み、再起動する (`EVT OTA_* ...` を出力)
    Ota { url: String },
    /// PDF を URL から取得しプリンター 9100 (raw) へストリーミング印刷
    /// (印刷ブリッジ用。宛先は `PRINTER ADDR` で保存済みのもの。
    /// 進捗・結果は `EVT PRINT_* ...`)
    Print { url: String },
    /// プリンター宛先 `host:port` の保存 (NVS)。検証は printer::valid_addr
    PrinterAddr { addr: String },
    /// プリンター宛先の問い合わせ (`PRINTER <addr>` / `PRINTER UNSET` を応答)
    PrinterStatus,
}

/// 画面向きとして有効な角度か
pub fn valid_rotation(deg: u16) -> bool {
    matches!(deg, 0 | 90 | 180 | 270)
}

/// 1 行を解析する。
///
/// - 空行 → `Ok(None)` (無視)
/// - 解析エラー → `Err(ホストへ返す ERR 応答行)`
pub fn parse_line(line: &str, default_qr_timeout_ms: u64) -> Result<Option<HostCommand>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("").to_ascii_uppercase();
    let command = match cmd.as_str() {
        "PING" => HostCommand::Ping,
        "QR" => match it.next() {
            Some(payload) => {
                let timeout_ms = it
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s * 1000)
                    .unwrap_or(default_qr_timeout_ms);
                HostCommand::ShowQr {
                    payload: payload.to_string(),
                    timeout_ms,
                }
            }
            None => return Err("ERR QR: payload がありません".into()),
        },
        "MEASURE" => HostCommand::Measure,
        "RESULT" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some(v @ ("OK" | "NG")) => HostCommand::Result {
                ok: v == "OK",
                value: it.next().unwrap_or("").to_string(),
            },
            _ => return Err("ERR RESULT: OK|NG が必要です".into()),
        },
        "ERROR" => HostCommand::ShowError {
            message: line
                .splitn(2, char::is_whitespace)
                .nth(1)
                .unwrap_or("")
                .trim()
                .to_string(),
        },
        "RESET" => HostCommand::Reset,
        "ROTATE" => match it.next().and_then(|s| s.parse::<u16>().ok()) {
            Some(deg) if valid_rotation(deg) => HostCommand::Rotate(deg),
            _ => return Err("ERR ROTATE: 0|90|180|270 が必要です".into()),
        },
        "STATUS" => HostCommand::Status,
        "HEAP" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            None => HostCommand::Heap,
            Some("DUMP") => HostCommand::HeapDump,
            _ => return Err("ERR HEAP: 引数は DUMP のみ (無引数 = 概況)".into()),
        },
        // OTA 更新 (URL は大文字小文字を保持)
        "OTA" => match it.next() {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                HostCommand::Ota {
                    url: url.to_string(),
                }
            }
            _ => return Err("ERR OTA: http(s):// で始まる firmware URL が必要です".into()),
        },
        // 印刷 (URL は大文字小文字を保持。宛先は PRINTER ADDR で事前設定)
        "PRINT" => match it.next() {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                HostCommand::Print {
                    url: url.to_string(),
                }
            }
            _ => return Err("ERR PRINT: http(s):// で始まる PDF URL が必要です".into()),
        },
        "PRINTER" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("ADDR") => match it.next() {
                Some(addr) if crate::printer::valid_addr(addr) => HostCommand::PrinterAddr {
                    addr: addr.to_string(),
                },
                _ => return Err("ERR PRINTER: ADDR には host:port が必要です".into()),
            },
            Some("STATUS") => HostCommand::PrinterStatus,
            _ => return Err("ERR PRINTER: ADDR|STATUS が必要です".into()),
        },
        "CFG" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("GET") => HostCommand::CfgGet,
            Some("SET") => {
                // JSON は空白を含み得るため 3 トークン目以降を丸ごと取る
                let json = line
                    .splitn(3, char::is_whitespace)
                    .nth(2)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if json.is_empty() {
                    return Err("ERR CFG: SET に JSON がありません".into());
                }
                HostCommand::CfgSet { json }
            }
            _ => return Err("ERR CFG: GET|SET が必要です".into()),
        },
        "WIFI" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("TEST") => HostCommand::WifiTest,
            _ => return Err("ERR WIFI: TEST が必要です".into()),
        },
        // 測定データの WS 送信 (cf-alc-recorder)
        "WS" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("STATUS") => HostCommand::WsStatus,
            Some("URL") => match it.next() {
                Some(url) if url.starts_with("wss://") || url.starts_with("ws://") => {
                    HostCommand::WsUrl {
                        url: url.to_string(),
                    }
                }
                _ => return Err("ERR WS: URL には ws(s):// で始まる URL が必要です".into()),
            },
            _ => return Err("ERR WS: URL|STATUS が必要です".into()),
        },
        // `PAIR` または `BLE PAIR`
        "PAIR" => HostCommand::BlePair,
        "BLE" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("PAIR") => HostCommand::BlePair,
            _ => return Err("ERR BLE: PAIR が必要です".into()),
        },
        // auth-worker デバイス登録 (BLE の PAIR とは別系統)
        "AUTH" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("SET") => match (it.next(), it.next(), it.next()) {
                (Some(id), Some(secret), Some(tenant)) => HostCommand::AuthSet {
                    device_id: id.to_string(),
                    device_secret: secret.to_string(),
                    tenant_id: tenant.to_string(),
                },
                _ => {
                    return Err(
                        "ERR AUTH: SET には device_id device_secret tenant_id が必要です".into(),
                    )
                }
            },
            Some("UNPAIR") => HostCommand::AuthUnpair,
            Some("STATUS") => HostCommand::AuthStatus,
            Some("TOKEN") => HostCommand::AuthToken,
            Some("URL") => match it.next() {
                Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                    HostCommand::AuthUrl {
                        url: url.to_string(),
                    }
                }
                _ => return Err("ERR AUTH: URL には http(s):// で始まる URL が必要です".into()),
            },
            _ => return Err("ERR AUTH: SET|UNPAIR|STATUS|TOKEN|URL が必要です".into()),
        },
        _ => return Err(format!("ERR 不明なコマンド: {cmd}")),
    };
    Ok(Some(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 60_000; // 既定タイムアウト

    #[test]
    fn empty_and_whitespace_lines_are_ignored() {
        assert_eq!(parse_line("", T), Ok(None));
        assert_eq!(parse_line("   ", T), Ok(None));
    }

    #[test]
    fn ping_is_case_insensitive() {
        assert_eq!(parse_line("ping", T), Ok(Some(HostCommand::Ping)));
    }

    #[test]
    fn qr_with_timeout() {
        assert_eq!(
            parse_line("QR https://example.com/t/abc 30", T),
            Ok(Some(HostCommand::ShowQr {
                payload: "https://example.com/t/abc".into(),
                timeout_ms: 30_000,
            }))
        );
    }

    #[test]
    fn qr_default_timeout() {
        assert_eq!(
            parse_line("QR token123", T),
            Ok(Some(HostCommand::ShowQr {
                payload: "token123".into(),
                timeout_ms: T,
            }))
        );
    }

    #[test]
    fn qr_without_payload_is_error() {
        assert!(parse_line("QR", T).is_err());
    }

    #[test]
    fn measure() {
        assert_eq!(parse_line("MEASURE", T), Ok(Some(HostCommand::Measure)));
    }

    #[test]
    fn result_ok_with_value() {
        assert_eq!(
            parse_line("RESULT OK 0.000", T),
            Ok(Some(HostCommand::Result {
                ok: true,
                value: "0.000".into(),
            }))
        );
    }

    #[test]
    fn result_ng_without_value() {
        assert_eq!(
            parse_line("RESULT ng", T),
            Ok(Some(HostCommand::Result {
                ok: false,
                value: "".into(),
            }))
        );
    }

    #[test]
    fn result_invalid_verdict_is_error() {
        assert!(parse_line("RESULT MAYBE", T).is_err());
        assert!(parse_line("RESULT", T).is_err());
    }

    #[test]
    fn error_with_and_without_message() {
        assert_eq!(
            parse_line("ERROR 通信に失敗しました", T),
            Ok(Some(HostCommand::ShowError {
                message: "通信に失敗しました".into(),
            }))
        );
        assert_eq!(
            parse_line("ERROR", T),
            Ok(Some(HostCommand::ShowError {
                message: "".into(),
            }))
        );
    }

    #[test]
    fn reset_and_status() {
        assert_eq!(parse_line("RESET", T), Ok(Some(HostCommand::Reset)));
        assert_eq!(parse_line("STATUS", T), Ok(Some(HostCommand::Status)));
    }

    #[test]
    fn heap() {
        assert_eq!(parse_line("HEAP", T), Ok(Some(HostCommand::Heap)));
        assert_eq!(parse_line("heap", T), Ok(Some(HostCommand::Heap)));
    }

    #[test]
    fn heap_dump() {
        assert_eq!(parse_line("HEAP DUMP", T), Ok(Some(HostCommand::HeapDump)));
        assert_eq!(parse_line("heap dump", T), Ok(Some(HostCommand::HeapDump)));
        assert!(parse_line("HEAP FULL", T).is_err());
    }

    #[test]
    fn ota_url_preserves_case() {
        assert_eq!(
            parse_line("OTA https://Ippoan.github.io/alc-app-s3/firmware/app.bin", T),
            Ok(Some(HostCommand::Ota {
                url: "https://Ippoan.github.io/alc-app-s3/firmware/app.bin".into(),
            }))
        );
        assert_eq!(
            parse_line("ota http://192.168.11.2:8000/app.bin", T),
            Ok(Some(HostCommand::Ota {
                url: "http://192.168.11.2:8000/app.bin".into(),
            }))
        );
    }

    #[test]
    fn ota_errors() {
        assert!(parse_line("OTA", T).is_err());
        assert!(parse_line("OTA ftp://x/app.bin", T).is_err());
        assert!(parse_line("OTA example.com/app.bin", T).is_err());
    }

    #[test]
    fn print_url_preserves_case() {
        assert_eq!(
            parse_line("PRINT https://Example.com/Tenko.pdf", T),
            Ok(Some(HostCommand::Print {
                url: "https://Example.com/Tenko.pdf".into(),
            }))
        );
        assert_eq!(
            parse_line("print http://192.168.11.2:8000/t.pdf", T),
            Ok(Some(HostCommand::Print {
                url: "http://192.168.11.2:8000/t.pdf".into(),
            }))
        );
    }

    #[test]
    fn print_errors() {
        assert!(parse_line("PRINT", T).is_err());
        assert!(parse_line("PRINT ftp://x/t.pdf", T).is_err());
        assert!(parse_line("PRINT example.com/t.pdf", T).is_err());
    }

    #[test]
    fn printer_addr_and_status() {
        assert_eq!(
            parse_line("PRINTER ADDR 192.168.11.60:9100", T),
            Ok(Some(HostCommand::PrinterAddr {
                addr: "192.168.11.60:9100".into(),
            }))
        );
        assert_eq!(
            parse_line("printer status", T),
            Ok(Some(HostCommand::PrinterStatus))
        );
    }

    #[test]
    fn printer_errors() {
        assert!(parse_line("PRINTER", T).is_err());
        assert!(parse_line("PRINTER ADDR", T).is_err());
        assert!(parse_line("PRINTER ADDR hostonly", T).is_err());
        assert!(parse_line("PRINTER ADDR host:0", T).is_err());
        assert!(parse_line("PRINTER RESET", T).is_err());
    }

    #[test]
    fn rotate_valid_angles() {
        for deg in [0u16, 90, 180, 270] {
            assert_eq!(
                parse_line(&format!("ROTATE {deg}"), T),
                Ok(Some(HostCommand::Rotate(deg)))
            );
        }
    }

    #[test]
    fn rotate_invalid_is_error() {
        assert!(parse_line("ROTATE 45", T).is_err());
        assert!(parse_line("ROTATE abc", T).is_err());
        assert!(parse_line("ROTATE", T).is_err());
    }

    #[test]
    fn valid_rotation_domain() {
        assert!(valid_rotation(0));
        assert!(!valid_rotation(45));
    }

    #[test]
    fn unknown_command_is_error() {
        assert_eq!(
            parse_line("FOO bar", T),
            Err("ERR 不明なコマンド: FOO".to_string())
        );
    }

    #[test]
    fn cfg_get_and_set() {
        assert_eq!(parse_line("CFG GET", T), Ok(Some(HostCommand::CfgGet)));
        assert_eq!(
            parse_line(r#"CFG SET {"rotation": 90, "wifi": null}"#, T),
            Ok(Some(HostCommand::CfgSet {
                json: r#"{"rotation": 90, "wifi": null}"#.into(),
            }))
        );
    }

    #[test]
    fn cfg_errors() {
        assert!(parse_line("CFG", T).is_err());
        assert!(parse_line("CFG PUT", T).is_err());
        assert!(parse_line("CFG SET", T).is_err());
        assert!(parse_line("CFG SET   ", T).is_err());
    }

    #[test]
    fn wifi_test() {
        assert_eq!(parse_line("WIFI TEST", T), Ok(Some(HostCommand::WifiTest)));
        assert!(parse_line("WIFI", T).is_err());
        assert!(parse_line("WIFI CONNECT", T).is_err());
    }

    #[test]
    fn ble_pair() {
        assert_eq!(parse_line("PAIR", T), Ok(Some(HostCommand::BlePair)));
        assert_eq!(parse_line("ble pair", T), Ok(Some(HostCommand::BlePair)));
        assert!(parse_line("BLE", T).is_err());
        assert!(parse_line("BLE SCAN", T).is_err());
    }

    #[test]
    fn auth_subcommands() {
        assert_eq!(
            parse_line("auth unpair", T),
            Ok(Some(HostCommand::AuthUnpair))
        );
        assert_eq!(
            parse_line("AUTH STATUS", T),
            Ok(Some(HostCommand::AuthStatus))
        );
        assert_eq!(
            parse_line("AUTH TOKEN", T),
            Ok(Some(HostCommand::AuthToken))
        );
    }

    #[test]
    fn auth_set_takes_three_args_case_preserved() {
        assert_eq!(
            parse_line("AUTH SET dev_AbC s3crET-xyz 11111111-2222-3333-4444-555555555555", T),
            Ok(Some(HostCommand::AuthSet {
                device_id: "dev_AbC".into(),
                device_secret: "s3crET-xyz".into(),
                tenant_id: "11111111-2222-3333-4444-555555555555".into(),
            }))
        );
        assert!(parse_line("AUTH SET", T).is_err());
        assert!(parse_line("AUTH SET id", T).is_err());
        assert!(parse_line("AUTH SET id secret", T).is_err());
        // 旧 QR ペアリングの PAIR は廃止 (USB provisioning に一本化)
        assert!(parse_line("AUTH PAIR", T).is_err());
    }

    #[test]
    fn auth_url_preserves_case() {
        assert_eq!(
            parse_line("AUTH URL https://Auth-Staging.ippoan.org", T),
            Ok(Some(HostCommand::AuthUrl {
                url: "https://Auth-Staging.ippoan.org".into(),
            }))
        );
        assert_eq!(
            parse_line("AUTH URL http://192.168.1.10:8787", T),
            Ok(Some(HostCommand::AuthUrl {
                url: "http://192.168.1.10:8787".into(),
            }))
        );
    }

    #[test]
    fn auth_errors() {
        assert!(parse_line("AUTH", T).is_err());
        assert!(parse_line("AUTH REVOKE", T).is_err());
        assert!(parse_line("AUTH URL", T).is_err());
        assert!(parse_line("AUTH URL ftp://x", T).is_err());
        assert!(parse_line("AUTH URL auth.ippoan.org", T).is_err());
    }

    #[test]
    fn ws_subcommands() {
        assert_eq!(parse_line("WS STATUS", T), Ok(Some(HostCommand::WsStatus)));
        assert_eq!(
            parse_line("ws url wss://alc-recorder-staging.m-tama-ramu.workers.dev/ws", T),
            Ok(Some(HostCommand::WsUrl {
                url: "wss://alc-recorder-staging.m-tama-ramu.workers.dev/ws".into(),
            }))
        );
        assert_eq!(
            parse_line("WS URL ws://192.168.1.10:8787/ws", T),
            Ok(Some(HostCommand::WsUrl {
                url: "ws://192.168.1.10:8787/ws".into(),
            }))
        );
    }

    #[test]
    fn ws_errors() {
        assert!(parse_line("WS", T).is_err());
        assert!(parse_line("WS SEND", T).is_err());
        assert!(parse_line("WS URL", T).is_err());
        assert!(parse_line("WS URL https://x/ws", T).is_err());
    }
}
