//! プリンター 9100 (raw) 印刷の純粋部分 (ippoan/alc-app-s3#38)。
//!
//! PDF の HTTP GET と TCP 送信の副作用は firmware 側 (hub-drivers/printer.rs)
//! が担い、ここでは宛先アドレスの検証とストリームコピーの帳簿のみを行う。
//! reader/writer はクロージャで受ける (HTTP 側は embedded-svc の Read、
//! TCP 側は std::io::Write と、トレイトが揃わないため)。

/// 9100 raw 印刷の既定ポート
pub const RAW_PRINT_PORT: u16 = 9100;

/// プリンター宛先 `host:port` の検証。
/// host は空でなく空白を含まない任意のホスト名/IP、port は 1-65535。
pub fn valid_addr(addr: &str) -> bool {
    let Some((host, port)) = addr.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return false;
    }
    matches!(port.parse::<u32>(), Ok(p) if (1..=65535).contains(&p))
}

/// read から write へ chunk 単位でコピーし、総バイト数を返す。
///
/// - `read(buf)` は読めたバイト数 (0 = EOF) を返す
/// - `write(bytes)` は全量書き込みに成功したら Ok
/// - `progress(コピー済みバイト数)` は chunk 毎に呼ばれる (間引きは呼び出し側)
///
/// 空ストリーム (0 バイト) はエラー (印刷対象なし = URL 間違いの可能性が高い)。
pub fn copy_stream(
    mut read: impl FnMut(&mut [u8]) -> Result<usize, String>,
    mut write: impl FnMut(&[u8]) -> Result<(), String>,
    chunk: &mut [u8],
    mut progress: impl FnMut(usize),
) -> Result<usize, String> {
    let mut total = 0usize;
    loop {
        let n = match read(chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(format!("ダウンロード中断: {e}")),
        };
        write(&chunk[..n]).map_err(|e| format!("プリンターへの送信失敗: {e}"))?;
        total += n;
        progress(total);
    }
    if total == 0 {
        return Err("受信 0 バイト (URL を確認してください)".into());
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_addr_accepts_host_port() {
        assert!(valid_addr("192.168.11.60:9100"));
        assert!(valid_addr("printer.local:9100"));
        assert!(valid_addr("p:1"));
        assert!(valid_addr("p:65535"));
    }

    #[test]
    fn valid_addr_rejects_bad_input() {
        assert!(!valid_addr(""));
        assert!(!valid_addr("hostonly"));
        assert!(!valid_addr(":9100"));
        assert!(!valid_addr("host:"));
        assert!(!valid_addr("host:0"));
        assert!(!valid_addr("host:65536"));
        assert!(!valid_addr("host:port"));
        assert!(!valid_addr("ho st:9100"));
    }

    /// data を fail_at バイトまで返し、その後エラーする reader を作る
    fn reader_with(data: Vec<u8>, fail_at: Option<usize>) -> impl FnMut(&mut [u8]) -> Result<usize, String> {
        let mut pos = 0usize;
        move |buf: &mut [u8]| {
            if let Some(f) = fail_at {
                if pos >= f {
                    return Err("timeout".into());
                }
            }
            let end = fail_at.unwrap_or(data.len()).min(data.len());
            let n = buf.len().min(end - pos);
            buf[..n].copy_from_slice(&data[pos..pos + n]);
            pos += n;
            Ok(n)
        }
    }

    #[test]
    fn copy_stream_copies_all_bytes_with_progress() {
        let src: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut out = Vec::new();
        let mut calls = Vec::new();
        let mut chunk = [0u8; 256];
        let total = copy_stream(
            reader_with(src.clone(), None),
            |b| {
                out.extend_from_slice(b);
                Ok(())
            },
            &mut chunk,
            |t| calls.push(t),
        )
        .unwrap();
        assert_eq!(total, 1000);
        assert_eq!(out, src);
        assert_eq!(calls, vec![256, 512, 768, 1000]);
    }

    #[test]
    fn copy_stream_rejects_empty_source() {
        let mut chunk = [0u8; 16];
        let err = copy_stream(reader_with(Vec::new(), None), |_| Ok(()), &mut chunk, |_| {})
            .unwrap_err();
        assert!(err.contains("0 バイト"), "{err}");
    }

    #[test]
    fn copy_stream_reports_read_error() {
        let mut out = Vec::new();
        let mut chunk = [0u8; 16];
        let err = copy_stream(
            reader_with(vec![7u8; 64], Some(32)),
            |b| {
                out.extend_from_slice(b);
                Ok(())
            },
            &mut chunk,
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("ダウンロード中断"), "{err}");
        assert_eq!(out.len(), 32); // 失敗前までは書けている
    }

    #[test]
    fn copy_stream_reports_write_error() {
        let mut chunk = [0u8; 16];
        let err = copy_stream(
            reader_with(vec![1u8; 8], None),
            |_| Err("reset".into()),
            &mut chunk,
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("送信失敗"), "{err}");
    }
}
