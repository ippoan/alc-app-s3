//! 行指向ホストコンソールの**共通部** (USB Serial/JTAG)。
//!
//! 同じ行プロトコルを話す口が 3 つある (CoreS3 の [`crate::host_link`]、
//! AtomS3 印刷ブリッジ、NFC タイムカード端末) ため、機種に依らない部分を
//! ここへ寄せてある。**新しい機種のコンソールを丸写しで作らないこと**
//! — とくに `AUTH SET` (device credential を NVS へ書く口) が機種ごとに
//! 散ると、provisioning の挙動が機種ごとに割れる。
//!
//! 提供するもの:
//!
//! | | 内容 |
//! |---|---|
//! | [`install_usb_serial_jtag`] | USB Serial/JTAG ドライバの VFS 接続 (stdin をブロッキング読みにする) |
//! | [`take_line`] | 受信バッファから 1 行を切り出す (改行待ち + ゴミ捨て) |
//! | [`spawn_reader`] | stdin を読んで行ごとにコールバックを呼ぶスレッド |
//! | [`handle_common`] | 機種に依らないコマンド (PING / HEAP / LOG / AUTH / WS) |
//! | [`handle_ota_lan_guarded`] | LAN 専用機の `OTA <url>` (リンクアップ前を弾く) |
//!
//! 解析そのものは `alc_hub_core::protocol::parse_line` (純粋・テスト済み) が持つ。
//! ここは副作用 (NVS 保存・応答出力) だけを担当する。

use alc_hub_core::protocol::HostCommand;
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;
use std::io::Read;

use alc_hub_common::{settings::Settings, status::SharedStatus};

/// 行としてバッファする最大長 (超えたら読み捨て — バイナリノイズ対策)
pub const MAX_LINE: usize = 512;

/// USB Serial/JTAG ドライバを VFS に接続し、stdin のブロッキング読み出しを
/// 可能にする (`CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y` 前提)。
pub fn install_usb_serial_jtag() {
    unsafe {
        let mut cfg = sys::usb_serial_jtag_driver_config_t {
            tx_buffer_size: 1024,
            rx_buffer_size: 1024,
        };
        sys::usb_serial_jtag_driver_install(&mut cfg);
        sys::esp_vfs_usb_serial_jtag_use_driver();
    }
}

/// 受信バッファの先頭から 1 行 (CR か LF まで) を取り出す。改行がまだ来て
/// いなければ `None`。改行の来ないゴミが [`MAX_LINE`] を超えたら捨てる。
pub fn take_line(acc: &mut Vec<u8>) -> Option<String> {
    let pos = acc.iter().position(|&b| b == b'\n' || b == b'\r')?;
    let line_bytes: Vec<u8> = acc.drain(..=pos).collect();
    Some(
        String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1])
            .trim()
            .to_string(),
    )
}

/// 改行の来ないゴミを捨てる (行が取れなかったときに呼ぶ)
pub fn discard_overlong(acc: &mut Vec<u8>) {
    if acc.len() > MAX_LINE {
        acc.clear();
    }
}

/// stdin を読み、完成した行ごとに `on_line` を呼ぶスレッドを起動する。
/// Improv (バイナリフレーム) を混在させる CoreS3 は本関数を使わず
/// [`crate::host_link`] が自前で振り分ける。
///
/// スタックは**内部RAM から取る** (`name_next`)。`AUTH SET` 等で NVS へ書く
/// ため、PSRAM スタックにすると flash 書き込み中のキャッシュ無効で落ちる
/// (`alc_hub_common::task::name_next_psram` の doc 参照)。
pub fn spawn_reader(
    name: &'static core::ffi::CStr,
    stack_size: usize,
    mut on_line: impl FnMut(&str) + Send + 'static,
) -> Result<()> {
    install_usb_serial_jtag();
    crate::task::name_next(name);
    std::thread::Builder::new()
        .name(name.to_string_lossy().into_owned())
        .stack_size(stack_size)
        .spawn(move || {
            let mut chunk = [0u8; 64];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                match std::io::stdin().lock().read(&mut chunk) {
                    Ok(0) => FreeRtos::delay_ms(20),
                    Ok(n) => {
                        acc.extend_from_slice(&chunk[..n]);
                        loop {
                            match take_line(&mut acc) {
                                Some(line) => on_line(&line),
                                None => {
                                    discard_overlong(&mut acc);
                                    break;
                                }
                            }
                        }
                    }
                    Err(_) => FreeRtos::delay_ms(100),
                }
            }
        })?;
    Ok(())
}

/// 機種に依らないコマンドを処理する。
///
/// 戻り値は **処理しなかったコマンド** — `Some(command)` が返ったら
/// 呼び出し側が機種固有として捌く (捌けなければ `ERR UNSUPPORTED`)。
/// 「処理したら None」なので `if let Some(cmd) = handle_common(..)` の形で書ける。
///
/// ここに入れるのは「どの機種でも同じ応答であるべきもの」だけ:
/// 疎通 (`PING`) / 診断 (`HEAP` / `HEAP DUMP` / `LOG DUMP`) /
/// device credential (`AUTH *`) / WS 常時接続 (`WS URL` / `WS STATUS`)。
///
/// `STATUS` は機種ごとに項目が違うので**含めない**。`OTA` も、LAN 専用機は
/// リンクアップを待つ必要がある一方で Wi-Fi 機はそうでないため含めない
/// ([`handle_ota_lan_guarded`] 参照)。
///
/// `claim_ticket_supported` は `AUTH TICKET` (ippoan/alc-app-s3#204) だけの
/// 例外 — この券は USB で繋がった**運行者 PWA ブラウザ**への受け渡しが前提
/// (host_link.rs = CoreS3 のみ)。印刷ブリッジ・タイムカード端末・警告デバイス
/// (VoiceS3R) に USB 越しの PWA は繋がらないため `false` を渡し、
/// `ERR AUTH TICKET: unsupported` を返させる (VoiceS3R は元々ネットワーク自体
/// を持たない)。
#[must_use]
pub fn handle_common(
    command: HostCommand,
    status: &SharedStatus,
    settings: &Settings,
    claim_ticket_supported: bool,
) -> Option<HostCommand> {
    match command {
        HostCommand::Ping => println!("PONG"),
        // ヒープ状態の即時応答 (定期出力 EVT HEAP と同じ計測、heap.rs 参照)
        HostCommand::Heap => {
            let s = crate::heap::stats();
            println!(
                "HEAP FREE_INT={} MIN_INT={} FREE_PSRAM={} TOTAL_INT={} TOTAL_PSRAM={}",
                s.free_int, s.min_int, s.free_psram, s.total_int, s.total_psram,
            );
        }
        // ヒープ詳細: ブロック概況 + タスク別スタック余裕 (heap.rs 参照)
        HostCommand::HeapDump => crate::heap::dump(),
        // 直近ログ: .noinit リングの現在内容 (crashlog.rs 参照)。
        // 事象の後から原因を取りに行くための口
        HostCommand::LogDump => crate::crashlog::dump(),
        // device credential の注入 (USB provisioning — ホストが auth-worker
        // /device/pair 系で取得した credential をそのまま渡す)。secret は
        // 応答に echo しない。**この口は 1 か所に保つこと**
        HostCommand::AuthSet {
            device_id,
            device_secret,
            tenant_id,
        } => match settings.set_device_credential(&device_id, &device_secret, &tenant_id) {
            Ok(()) => println!("OK AUTH SET"),
            Err(e) => {
                log::error!("console: credential 保存失敗: {e:?}");
                println!("ERR AUTH: credential の保存に失敗しました");
            }
        },
        HostCommand::AuthUnpair => match settings.clear_device_credential() {
            Ok(()) => println!("OK AUTH UNPAIR"),
            Err(e) => {
                log::error!("console: credential 破棄失敗: {e:?}");
                println!("ERR AUTH: 破棄に失敗しました");
            }
        },
        HostCommand::AuthStatus => match settings.device_credential() {
            Some((id, _)) => println!(
                "AUTH PAIRED {} {}",
                settings.device_tenant().unwrap_or_default(),
                id,
            ),
            None => println!("AUTH UNPAIRED"),
        },
        // JWT mint (HTTP) は一時スレッドで実行し、結果は EVT AUTH_* で届く
        HostCommand::AuthToken => {
            crate::auth_link::spawn_mint_test(settings.clone(), status.clone());
            println!("OK AUTH TOKEN");
        }
        // 端末登録の一回券 (ippoan/auth-worker#519、ippoan/alc-app-s3#204)。
        // USB からの要求時だけ HTTP を叩き、その場で応答行を返す (AuthToken の
        // 自己診断と違い EVT 経由の非同期にはしない — 運行者 PWA は応答行を
        // 待って端末登録するため)。**JWT / secret はホストへ出さない**
        HostCommand::AuthTicket => {
            if !claim_ticket_supported {
                println!("ERR AUTH TICKET: unsupported");
            } else if settings.device_credential().is_none() {
                println!("ERR AUTH TICKET: not paired");
            } else {
                match crate::auth_link::fetch_claim_ticket(settings) {
                    Ok((ticket, expires_in)) => println!(
                        "{}",
                        alc_hub_core::pairing::auth_ticket_line(&ticket, expires_in)
                    ),
                    Err(e) => println!("ERR AUTH TICKET: {e}"),
                }
            }
        }
        HostCommand::AuthUrl { url } => match settings.set_auth_url(&url) {
            Ok(()) => println!("OK AUTH URL"),
            Err(e) => {
                log::error!("console: auth URL 保存失敗: {e:?}");
                println!("ERR AUTH: URL の保存に失敗しました");
            }
        },
        // 警告デバイス (VoiceS3R) 管理者認証用の ed25519 鍵対 (Refs #205)。
        // 秘密鍵 (seed) は NVS alarm_sk のみに留まり、USB には公開鍵と署名
        // しか出さない。**この口にそれ以外の応答を足さないこと**
        HostCommand::AuthKeygen { force } => {
            if !force && settings.alarm_sk().is_some() {
                println!("ERR AUTH: key exists");
            } else {
                let mut seed = [0u8; 32];
                unsafe { sys::esp_fill_random(seed.as_mut_ptr().cast(), seed.len()) };
                match settings.set_alarm_sk(&seed) {
                    Ok(()) => println!("{}", alc_hub_core::alarm_key::auth_pubkey_line(&seed)),
                    Err(e) => {
                        log::error!("console: alarm_sk 保存失敗: {e:?}");
                        println!("ERR AUTH: 鍵の保存に失敗しました");
                    }
                }
            }
        }
        HostCommand::AuthPubkey => match settings.alarm_sk() {
            Some(seed) => println!("{}", alc_hub_core::alarm_key::auth_pubkey_line(&seed)),
            None => println!("ERR AUTH: no key"),
        },
        HostCommand::AuthSign { nonce } => match settings.alarm_sk() {
            Some(seed) => match alc_hub_core::alarm_key::auth_sig_line(&seed, &nonce) {
                Ok(line) => println!("{line}"),
                Err(_) => println!("ERR AUTH: bad nonce"),
            },
            None => println!("ERR AUTH: no key"),
        },
        // cf-alc-recorder 常時接続 (ws_uplink.rs)
        HostCommand::WsUrl { url } => match settings.set_ws_url(&url) {
            Ok(()) => println!("OK WS URL"),
            Err(e) => {
                log::error!("console: WS URL 保存失敗: {e:?}");
                println!("ERR WS: URL の保存に失敗しました");
            }
        },
        HostCommand::WsStatus => {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            println!(
                "WS CONNECTED={} QUEUE={} SEQ={}",
                u8::from(st.ws_connected),
                st.ws_queue_len,
                st.ws_last_seq,
            );
        }
        other => return Some(other),
    }
    None
}

/// LAN (W5500) 専用機の `OTA <url>`。**リンクアップ前に lwip を叩くと assert
/// リブートする**ため、`ETH_CONNECTED` を待たせる (printer::spawn_print と同じ理由)。
pub fn handle_ota_lan_guarded(url: String, status: &SharedStatus) {
    let lan_up = status.lock().map(|s| s.lan_link).unwrap_or(false);
    if lan_up {
        crate::ota::spawn_update(url, status.clone(), None);
        println!("OK OTA");
    } else {
        println!("ERR OTA: LAN 未接続 (ETH_CONNECTED を待ってください)");
    }
}
