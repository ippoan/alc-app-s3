//! プリンター 9100 (raw) 印刷の純粋部分 (ippoan/alc-app-s3#38)。
//!
//! PDF の HTTP GET と TCP 送信の副作用は firmware 側 (hub-drivers/printer.rs)
//! が担い、ここでは宛先アドレスの検証とストリームコピーの帳簿のみを行う。
//! reader/writer はクロージャで受ける (HTTP 側は embedded-svc の Read、
//! TCP 側は std::io::Write と、トレイトが揃わないため)。

/// 9100 raw 印刷の既定ポート
pub const RAW_PRINT_PORT: u16 = 9100;

/// IPP の既定ポート。宛先がこのポートのときは RAW ではなく IPP Print-Job で送る
/// (ippoan/alc-app-s3#68: Canon LBP241 は RAW 9100 が本機からの接続だけ
/// 受信ウィンドウを開かず write が EAGAIN する。IPP (HTTP) 経路は正常)。
pub const IPP_PORT: u16 = 631;

/// IPP エンドポイントのパス (Canon/AirPrint 標準)
pub const IPP_PATH: &str = "/ipp/print";

/// 宛先 `host:port` が IPP ポートか (631 なら IPP Print-Job で送る)
pub fn is_ipp_addr(addr: &str) -> bool {
    addr.rsplit_once(':')
        .is_some_and(|(_, port)| port == "631")
}

/// IPP 属性 1 個 (value-tag, name, value) をエンコードして追記する
fn push_ipp_attr(buf: &mut Vec<u8>, tag: u8, name: &str, value: &str) {
    buf.push(tag);
    buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
    buf.extend_from_slice(name.as_bytes());
    buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
    buf.extend_from_slice(value.as_bytes());
}

/// IPP/1.1 Print-Job (0x0002) の operation attributes 部を組み立てる。
/// この直後にドキュメント本体 (PDF 等) を連結して HTTP POST の body にする。
/// document-format は octet-stream (プリンター側の PDL 自動判別に任せる —
/// LBP241 は PDF を format-supported に広告しないが自動判別では印字できる)。
pub fn ipp_print_job_header(host: &str, user: &str) -> Vec<u8> {
    let mut buf = vec![
        0x01, 0x01, // version 1.1
        0x00, 0x02, // operation: Print-Job
        0x00, 0x00, 0x00, 0x01, // request-id
        0x01, // operation-attributes-tag
    ];
    push_ipp_attr(&mut buf, 0x47, "attributes-charset", "utf-8");
    push_ipp_attr(&mut buf, 0x48, "attributes-natural-language", "en");
    push_ipp_attr(&mut buf, 0x45, "printer-uri", &format!("ipp://{host}{IPP_PATH}"));
    push_ipp_attr(&mut buf, 0x42, "requesting-user-name", user);
    push_ipp_attr(&mut buf, 0x49, "document-format", "application/octet-stream");
    buf.push(0x03); // end-of-attributes-tag
    buf
}

/// IPP POST の HTTP リクエストヘッダ。`content_len` が Some なら
/// Content-Length、None なら Transfer-Encoding: chunked (本体長不明時)。
pub fn http_post_head(host_addr: &str, content_len: Option<usize>) -> String {
    let framing = match content_len {
        Some(n) => format!("Content-Length: {n}"),
        None => "Transfer-Encoding: chunked".into(),
    };
    format!(
        "POST {IPP_PATH} HTTP/1.1\r\nHost: {host_addr}\r\nContent-Type: application/ipp\r\n{framing}\r\nConnection: close\r\n\r\n"
    )
}

/// chunked encoding の 1 チャンクのヘッダ (サイズの16進 + CRLF)
pub fn chunk_head(len: usize) -> String {
    format!("{len:x}\r\n")
}

/// chunked encoding の終端 (最終チャンク + 空トレーラ)
pub const CHUNK_END: &str = "0\r\n\r\n";

/// IPP レスポンス (HTTP 応答全体) から IPP status-code を取り出す。
/// HTTP 200 以外・応答が不完全な場合はエラー。成功判定 (successful-ok 系 =
/// 0x0000〜0x0002) は呼び出し側で行う。
pub fn ipp_response_status(resp: &[u8]) -> Result<u16, String> {
    let head = String::from_utf8_lossy(&resp[..resp.len().min(64)]);
    let Some(line) = head.lines().next() else {
        return Err("IPP 応答が空です".into());
    };
    let code = line.split_whitespace().nth(1).unwrap_or("");
    if !line.starts_with("HTTP/1.") || code != "200" {
        return Err(format!("IPP HTTP 応答が 200 以外: {line}"));
    }
    let Some(body_at) = resp.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err("IPP 応答にヘッダ終端がありません".into());
    };
    let body = &resp[body_at + 4..];
    if body.len() < 4 {
        return Err("IPP 応答 body が短すぎます".into());
    }
    Ok(u16::from_be_bytes([body[2], body[3]]))
}

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

    /// copy_stream をテスト条件で駆動する共通ドライバ。
    /// クロージャの定義箇所を 1 箇所に集約する (呼ばれないケース専用の
    /// クロージャを test 毎に作ると、その行が「未実行」としてカバレッジを
    /// 割るため)。fail_read_at = 読み込み済みがこのバイト数に達したら read
    /// エラー、fail_write_at = n 回目の write でエラー。
    fn run(
        data: Vec<u8>,
        chunk_size: usize,
        fail_read_at: Option<usize>,
        fail_write_at: Option<usize>,
    ) -> (Result<usize, String>, Vec<u8>, Vec<usize>) {
        let mut pos = 0usize;
        let mut out = Vec::new();
        let mut writes = 0usize;
        let mut progress_calls = Vec::new();
        let mut chunk = vec![0u8; chunk_size];
        let result = copy_stream(
            |buf| {
                if let Some(f) = fail_read_at {
                    if pos >= f {
                        return Err("timeout".into());
                    }
                }
                let end = fail_read_at.unwrap_or(data.len()).min(data.len());
                let n = buf.len().min(end - pos);
                buf[..n].copy_from_slice(&data[pos..pos + n]);
                pos += n;
                Ok(n)
            },
            |bytes| {
                writes += 1;
                if fail_write_at.is_some_and(|f| writes >= f) {
                    return Err("reset".into());
                }
                out.extend_from_slice(bytes);
                Ok(())
            },
            &mut chunk,
            |t| progress_calls.push(t),
        );
        (result, out, progress_calls)
    }

    #[test]
    fn copy_stream_copies_all_bytes_with_progress() {
        let src: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let (result, out, calls) = run(src.clone(), 256, None, None);
        assert_eq!(result.unwrap(), 1000);
        assert_eq!(out, src);
        assert_eq!(calls, vec![256, 512, 768, 1000]);
    }

    #[test]
    fn copy_stream_rejects_empty_source() {
        let (result, out, calls) = run(Vec::new(), 16, None, None);
        let err = result.unwrap_err();
        assert!(err.contains("0 バイト"), "{err}");
        assert!(out.is_empty());
        assert!(calls.is_empty());
    }

    #[test]
    fn copy_stream_reports_read_error() {
        let (result, out, _) = run(vec![7u8; 64], 16, Some(32), None);
        let err = result.unwrap_err();
        assert!(err.contains("ダウンロード中断"), "{err}");
        assert_eq!(out.len(), 32); // 失敗前までは書けている
    }

    #[test]
    fn is_ipp_addr_only_for_port_631() {
        assert!(is_ipp_addr("192.168.11.60:631"));
        assert!(is_ipp_addr("printer.local:631"));
        assert!(!is_ipp_addr("192.168.11.60:9100"));
        assert!(!is_ipp_addr("hostonly"));
    }

    #[test]
    fn ipp_print_job_header_layout() {
        let buf = ipp_print_job_header("172.18.21.63", "alc-hub");
        // version 1.1 / Print-Job (0x0002) / request-id 1 / op-attrs tag
        assert_eq!(&buf[..9], &[1, 1, 0, 2, 0, 0, 0, 1, 1]);
        assert_eq!(*buf.last().unwrap(), 0x03);
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("attributes-charset"), "{s}");
        assert!(s.contains("ipp://172.18.21.63/ipp/print"), "{s}");
        assert!(s.contains("alc-hub"), "{s}");
        assert!(s.contains("application/octet-stream"), "{s}");
    }

    #[test]
    fn http_post_head_content_length_and_chunked() {
        let fixed = http_post_head("172.18.21.63:631", Some(1063));
        assert!(fixed.starts_with("POST /ipp/print HTTP/1.1\r\n"), "{fixed}");
        assert!(fixed.contains("Host: 172.18.21.63:631\r\n"), "{fixed}");
        assert!(fixed.contains("Content-Length: 1063\r\n"), "{fixed}");
        assert!(fixed.ends_with("\r\n\r\n"), "{fixed}");
        let chunked = http_post_head("p:631", None);
        assert!(chunked.contains("Transfer-Encoding: chunked\r\n"), "{chunked}");
    }

    #[test]
    fn chunk_head_is_hex_crlf() {
        assert_eq!(chunk_head(867), "363\r\n");
        assert_eq!(CHUNK_END, "0\r\n\r\n");
    }

    #[test]
    fn ipp_response_status_ok() {
        let mut resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\n\r\n".to_vec();
        resp.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(ipp_response_status(&resp), Ok(0x0000));
    }

    #[test]
    fn ipp_response_status_rejects_bad_responses() {
        let empty = ipp_response_status(b"").unwrap_err();
        assert!(empty.contains("空"), "{empty}");
        let not_200 = ipp_response_status(b"HTTP/1.1 426 Upgrade Required\r\n\r\n").unwrap_err();
        assert!(not_200.contains("200 以外"), "{not_200}");
        let not_http = ipp_response_status(b"garbage response\r\n\r\n").unwrap_err();
        assert!(not_http.contains("200 以外"), "{not_http}");
        let no_body = ipp_response_status(b"HTTP/1.1 200 OK\r\n").unwrap_err();
        assert!(no_body.contains("ヘッダ終端"), "{no_body}");
        let short = ipp_response_status(b"HTTP/1.1 200 OK\r\n\r\n\x01\x01").unwrap_err();
        assert!(short.contains("短すぎ"), "{short}");
    }

    #[test]
    fn copy_stream_reports_write_error() {
        // 1 回目の write は成功させ progress も 1 回動かす (2 回目で失敗)
        let (result, out, calls) = run(vec![1u8; 8], 4, None, Some(2));
        let err = result.unwrap_err();
        assert!(err.contains("送信失敗"), "{err}");
        assert_eq!(out.len(), 4);
        assert_eq!(calls, vec![4]);
    }
}
