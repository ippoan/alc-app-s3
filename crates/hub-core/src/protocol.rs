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
}
