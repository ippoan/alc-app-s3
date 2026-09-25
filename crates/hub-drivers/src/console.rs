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
//! | [`start_common`] | 機種固有の分岐を持たない機の入口 (上記の共通分だけを連結する) |
//! | [`handle_common`] | 機種に依らないコマンド (PING / HEAP / LOG / AUTH / WS) |
//! | [`handle_omron`] | `OMRON BP ON\|OFF` / `OMRON STATUS` (BLE 血圧計を積む機だけが呼ぶ) |
//! | [`handle_ota_lan_guarded`] | LAN 専用機の `OTA <url>` (リンクアップ前を弾く) |
//!
//! 解析そのものは `alc_hub_core::protocol::parse_line` (純粋・テスト済み) が持つ。
//! ここは副作用 (NVS 保存・応答出力) だけを担当する。

use alc_hub_core::protocol::{parse_line, HostCommand, HostKind};
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;
use std::io::Read;

use alc_hub_common::control::PairFlag;
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
        // 受信は無変換にする。既定の CR→LF 変換は Improv のバイナリフレーム中の
        // 0x0D (13 文字のパスワード長など) を書き換え、チェックサム不一致で黙って
        // 捨てさせていた。テキスト行は take_line が CR/LF どちらでも切る
        sys::esp_vfs_dev_usb_serial_jtag_set_rx_line_endings(
            sys::esp_line_endings_t_ESP_LINE_ENDINGS_LF,
        );
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
                                Some(line) => {
                                    // 応答を必ず行頭から出す (#268)。ここに置けば
                                    // 個々の `println!` を書き換えずに 4 機種
                                    // (alarm / print / timecard / bp-station) を覆える
                                    alc_hub_common::hostout::begin_line();
                                    on_line(&line);
                                }
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

/// `AUTH SIGNBP` が hub-ble の初回スキャンを待つ上限 (Refs #269)。
///
/// 待つのは**起動直後の窓だけ** — `bp_read` は hub-ble のループの先頭で
/// 一度立てば以後ずっと true なので、2 回目以降の問い合わせは待たずに返る。
/// ホスト (PWA) は `port.open()` でチップをリセットするため、問い合わせは
/// ほぼ毎回この窓に当たる。
///
/// **BLE を起こさない機は 1 ミリ秒も待たない** — `bp_report` が
/// `ble_running=false` を見て即 `Ready { bonded: false }` を返す
/// (警告デバイス / 印刷ブリッジ / AtomS3 Lite build / `OMRON BP OFF`)。
///
/// 値の根拠: hub-ble のスキャン 1 周 (`SCAN_DURATION_MS` = 5 秒) + 余裕 1 秒。
/// **有限にすること**が要点で、超えたら `ERR AUTH: bp not ready` を返して
/// ホストへ返事を渡す (ホスト側の `AUTH SIGNBP` は 10 秒で切り上げるので、
/// こちらが黙って待ち続けると向こうのタイムアウトに食われる)。
const BP_READY_WAIT_MS: u32 = 6_000;

/// [`BP_READY_WAIT_MS`] を待つあいだの再読み間隔。
const BP_READY_POLL_MS: u32 = 100;

/// 血圧計のボンド状態が読めるようになるのを、上限付きで待つ (Refs #269)。
///
/// 待っても読めなければ [`alc_hub_core::device::BpReport::NotReady`] を返す
/// (呼び出し側が `ERR AUTH: bp not ready` を出す)。**窓を無限にしないこと** —
/// ホストが再試行しないと血圧を永久に測らない端末になる。
fn wait_bp_report(status: &SharedStatus) -> alc_hub_core::device::BpReport {
    let mut waited = 0;
    loop {
        // ★ **読めていれば 1 ミリ秒も待たない** — 判定が先、`delay_ms` は後。
        // 2 回目以降の `AUTH SIGNBP` (`bp_read` が既に true) と、BLE を
        // 起こさない機 (`ble_running` が false) はここで即座に返る。待たせると
        // 測定台の起動が毎回 6 秒重くなる
        let report = alc_hub_common::status::bp_report(status);
        if report != alc_hub_core::device::BpReport::NotReady || waited >= BP_READY_WAIT_MS {
            return report;
        }
        FreeRtos::delay_ms(BP_READY_POLL_MS);
        waited += BP_READY_POLL_MS;
    }
}

/// 機種に依らないコマンドを処理する。
///
/// 戻り値は **処理しなかったコマンド** — `Some(command)` が返ったら
/// 呼び出し側が機種固有として捌く (捌けなければ `ERR UNSUPPORTED`)。
/// 「処理したら None」なので `if let Some(cmd) = handle_common(..)` の形で書ける。
///
/// ここに入れるのは「どの機種でも同じ応答であるべきもの」だけ:
/// 疎通 (`PING`) / 診断 (`HEAP` / `HEAP DUMP` / `LOG DUMP`) /
/// device credential (`AUTH *`) / WS 常時接続 (`WS URL` / `WS STATUS`) /
/// 機種の名乗り (`DEVICE`)。
///
/// `STATUS` は機種ごとに項目が違うので**含めない**。`OTA` も、LAN 専用機は
/// リンクアップを待つ必要がある一方で Wi-Fi 機はそうでないため含めない
/// ([`handle_ota_lan_guarded`] 参照)。
///
/// `kind` は呼び出し元 (機種ごとの `console.rs` / [`start_common`]) が渡す
/// 自分の機種。`DEVICE` の名乗りと `AUTH TICKET` の可否
/// ([`HostKind::claim_ticket`]) の両方をここから引く — この券は
/// USB で繋がった**運行者 PWA ブラウザ**への受け渡しが前提 (host_link.rs =
/// CoreS3 のみ) なので、それ以外の機種は `ERR AUTH TICKET: unsupported` を
/// 返す (VoiceS3R は元々ネットワーク自体を持たない)。
#[must_use]
pub fn handle_common(
    command: HostCommand,
    status: &SharedStatus,
    settings: &Settings,
    kind: HostKind,
) -> Option<HostCommand> {
    match command {
        HostCommand::Ping => println!("PONG"),
        // 機種の名乗り (Refs ippoan/alc-app#353)。ブラウザ側の機種識別は
        // これ 1 本に集約する — `STATUS` の応答は機種ごとに違うので識別には
        // 使わない。`BOARD=` (板種) は元 `STATUS` にあったが、板種は不変なので
        // 名乗りの側が筋 ([`HostKind::has_board`])
        HostCommand::Device => {
            let ver = alc_hub_common::config::firmware_version_full();
            if kind.has_board() {
                let board = status.lock().map(|s| s.board).unwrap_or_default();
                println!(
                    "DEVICE {} VER={} BOARD={}",
                    kind.label(),
                    ver,
                    board.label()
                );
            } else {
                println!("DEVICE {} VER={}", kind.label(), ver);
            }
        }
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
            if !kind.claim_ticket() {
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
        // ★ **署名対象は nonce そのもの。ここに何かを足さないこと** — 管理者ログイン
        // (`/auth/device-login`) がこの署名を使っており、足すと 401 になる。
        // ボンド状態を返すのは下の `AUTH SIGNBP` (応答 prefix ごと別の口、Refs #249)
        HostCommand::AuthSign { nonce } => match settings.alarm_sk() {
            Some(seed) => match alc_hub_core::alarm_key::auth_sig_line(&seed, &nonce) {
                Ok(line) => println!("{line}"),
                Err(_) => println!("ERR AUTH: bad nonce"),
            },
            None => println!("ERR AUTH: no key"),
        },
        // キオスク端末の認証 (`/device/alarm-token`) 用。署名対象は
        // `<nonce>|bp=<1|0>` で、ブラウザは素通しするだけなので、血圧計の有無が
        // **鍵で裏付けられた端末の申告**になる。値は hub-ble が観測したボンド状態
        // (HubStatus::bp_bonded) を使う — `OMRON BP ON|OFF` (意思設定) とは
        // 別物なので取り違えないこと。
        //
        // ★ **観測前 (`bp_read` が false) は署名しない** (#269)。既定値の
        // `false` を `bp=0` として署名すると、血圧計が繋がっている端末が
        // 「無い」と鍵付きで申告してしまう。判定は hub-core の述語 1 本
        // (`device::bp_report` → `alarm_key::auth_sigbp_response`)
        HostCommand::AuthSignBp { nonce } => match settings.alarm_sk() {
            Some(seed) => println!(
                "{}",
                alc_hub_core::alarm_key::auth_sigbp_response(&seed, &nonce, wait_bp_report(status))
            ),
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
        // 指静脈 (ippoan/vein-match#20)。読み取りと案内音声は `vein` feature の
        // 機 (Vein Station) だけが自前の console で捌く。それ以外の全機種は
        // ここで同じ文言を返す — ホストはこの行で「この端末では使えない」と分かる
        #[cfg(not(feature = "vein"))]
        HostCommand::VeinCapture | HostCommand::VeinSay(_) => {
            println!("{}", alc_hub_core::vein::UNSUPPORTED_LINE)
        }
        other => return Some(other),
    }
    None
}

/// Omron 血圧計 (HEM-6231T) を拾うかの設定 (`OMRON BP ON|OFF` / `OMRON STATUS`)。
///
/// **[`handle_common`] には入れない** — BLE を積まない機 (印刷ブリッジ・警告
/// デバイス) が `OK OMRON BP=1` と答えると「設定したのに拾わない」になる。
/// 呼ぶのは `alc-hub-ble` を依存に持つ機だけ (CoreS3 の [`crate::host_link`] と
/// タイムカード端末 = Atom VoiceS3R)。**機種ごとに書き写さないこと** —
/// NVS のキーと応答文言が割れると `/device/setup` からの設定が機種で分かれる。
///
/// 戻り値は [`handle_common`] と同じ規約 (処理しなかったコマンドを `Some` で返す)。
///
/// `status.omron_bp` も併せて更新する — hub-ble はスキャンのたびにこちらを読む
/// ので、CoreS3 では**再起動なしで**切り替わる。タイムカード端末は BLE タスク
/// 自体を起動時の設定で立てるかどうか決めるため、**OFF → ON は再起動が要る**
/// (`crates/atoms3-timecard/src/main.rs` の `EVT BLE_DISABLED` 参照)。
#[must_use]
pub fn handle_omron(
    command: HostCommand,
    status: &SharedStatus,
    settings: &Settings,
) -> Option<HostCommand> {
    match command {
        HostCommand::OmronBp { enabled } => match settings.set_omron_bp(enabled) {
            Ok(()) => {
                if let Ok(mut st) = status.lock() {
                    st.omron_bp = enabled;
                }
                println!("OK OMRON BP={}", u8::from(enabled));
            }
            Err(e) => {
                log::error!("console: OMRON BP 保存失敗: {e:?}");
                println!("ERR OMRON: 保存に失敗しました");
            }
        },
        HostCommand::OmronStatus => println!("OMRON BP={}", u8::from(settings.omron_bp())),
        other => return Some(other),
    }
    None
}

/// `PAIR` — 血圧計の再ペアリング要求 (Pages の「血圧計を再ペアリング」)。
///
/// ここではフラグを立てるだけで、ボンド消去とペアリング受付の開始は BLE ループ
/// (hub-ble) が行う (`EVT PAIR_CLEARED` / `EVT PAIR_ARMED <秒>` →
/// `EVT PAIR_OK` | `EVT PAIR_ERR` | `EVT PAIR_TIMEOUT`)。
/// **受付を開けている間だけ**機器のペアリング待ちの広告に接続する
pub fn handle_pair(command: HostCommand, pair_flag: &PairFlag) -> Option<HostCommand> {
    match command {
        HostCommand::BlePair => {
            pair_flag.store(true, core::sync::atomic::Ordering::SeqCst);
            println!("OK PAIR");
        }
        other => return Some(other),
    }
    None
}

/// 機種固有の分岐を**持たない**機のコンソール入口 (Refs ippoan/alc-app#353)。
///
/// [`spawn_reader`] → [`handle_common`] → [`handle_omron`] → [`handle_pair`] を
/// その順に連結し、どれも捌かなかった行には `ERR UNSUPPORTED (<kind の label>)`
/// を返す。名乗り (`DEVICE`) は [`handle_common`] が `kind` から直接返す。
///
/// **4 本目の `console.rs` を作らないため**に在る。`STATUS` / `OTA` はホストリンク
/// (LAN・版数・更新) を前提とするので [`handle_common`] には入れていないが、
/// それらを持たない機 (血圧計用 PC の測定台 = `atoms3-nfc` の VoiceS3R build) は
/// 機種固有の分岐が**ゼロ**になり、包むだけの写しができてしまう。
///
/// 機種固有の分岐を持つ機 (CoreS3 の [`crate::host_link`] / 印刷ブリッジ /
/// タイムカード端末 / 警告デバイス) は従来どおり自前の `console.rs` で同じ順に
/// 呼んでから固有分を捌く。**将来それらもこの入口 + 「固有分のクロージャ」へ
/// 寄せられる**が、本番稼働中のため今回は分けてある。
pub fn start_common(
    kind: HostKind,
    status: SharedStatus,
    settings: Settings,
    pair_flag: PairFlag,
) -> Result<()> {
    spawn_reader(c"console", 8 * 1024, move |line| {
        let command = match parse_line(line, 0) {
            Ok(Some(command)) => command,
            Ok(None) => return, // 空行
            Err(err_response) => {
                println!("{err_response}");
                return;
            }
        };
        let Some(command) = handle_common(command, &status, &settings, kind) else {
            return;
        };
        let Some(command) = handle_omron(command, &status, &settings) else {
            return;
        };
        let Some(command) = handle_pair(command, &pair_flag) else {
            return;
        };
        log::debug!("console: unsupported command: {command:?}");
        println!("ERR UNSUPPORTED ({})", kind.label());
    })
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
