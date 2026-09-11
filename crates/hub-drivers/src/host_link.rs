//! ホストリンク: CoreS3 ネイティブ USB-C (USB Serial/JTAG) 経由の入出力。
//!
//! 同一ストリームに 2 種類のトラフィックが流れる:
//! 1. 行指向テキストプロトコル (Windows PC / Android タブレット)
//! 2. Improv Wi-Fi Serial のバイナリフレーム (ESP Web Tools の Wi-Fi 設定)
//!
//! 受信バイト列は IMPROV マジックで振り分け、それ以外を行として解釈する。
//! Wi-Fi を起こさないビルド (`lan` feature、#217) では `Wifi` / `Improv` が
//! 無く、Improv フレームは読み捨てて `EVT IMPROV_UNAVAILABLE lan` を最初の
//! 1 回だけ、`WIFI TEST` には `EVT WIFI_TEST NG lan` を返す。
//!
//! # 受信コマンド (ホスト → CoreS3)
//!
//! | コマンド | 説明 |
//! |---|---|
//! | `PING` | 疎通確認。`PONG` を返す |
//! | `QR <payload> [timeout_s]` | QR コード画面を表示 (顔認証後のトークン等) |
//! | `MEASURE` | 測定中画面を表示 |
//! | `RESULT OK\|NG [value]` | 測定結果画面を表示 (value 例: `0.000`) |
//! | `ERROR <message>` | エラー画面を表示 |
//! | `RESET` | 待機画面へ戻す |
//! | `STAGE NFC\|TEMP\|ALCOHOL\|PC` | PC (運行者タブ) の点呼の段に点呼画面を合わせる (`OK STAGE <label>`。応答はリングにも残る、get_log で読める)。NFC = 待機画面、TEMP / ALCOHOL = その欄を強調、PC = PC の画面だけで進む段。結果は `RESULT` |
//! | `ROTATE <0\|90\|180\|270>` | 画面向きを変更 (NVS 保存、次回起動も維持) |
//! | `STATUS` | `STATUS LAN=0 RS232=1 BLE=0 WIFI=0 ROT=0 BOARD=cores3 ALARM=idle/none/-/0` を返す (`ALARM=` の 4 番目は `grace=` の猶予の残り ms、猶予外は 0) |
//! | `HB OK` / `HB NG <reason>` | 運行者 PWA の heartbeat (3 秒ごと)。末尾に任意で `call=0\|1`、意図した reload の直前は `grace=<秒>` (#192)。沈黙警告の判定器へ渡す。**応答しない** |
//! | `AUTH SET <id> <secret> <tenant>` | device credential を注入 (USB provisioning) |
//! | `AUTH UNPAIR` | 保存済み device credential を破棄 (ローカルのみ) |
//! | `AUTH STATUS` | `AUTH PAIRED <tenant> <id>` / `AUTH UNPAIRED` を返す |
//! | `AUTH URL <url>` | auth-worker ベース URL を上書き (staging テスト用) |
//! | `AUTH TOKEN` | device JWT 取得の自己診断 (`EVT AUTH_TOKEN ...`) |
//! | `AUTH TICKET` | 端末登録の一回券を auth-worker から取得し応答 (`AUTH TICKET <ticket> EXPIRES=<秒>` / `ERR AUTH TICKET: <理由>`)。運行者 PWA の端末登録用 (ippoan/auth-worker#519、ippoan/alc-app-s3#204)。**CoreS3 のみ**、他機は `unsupported` |
//! | `AUTH KEYGEN [FORCE]` | 警告デバイス管理者認証用の ed25519 鍵対を生成し `AUTH PUBKEY <base64url>` を返す (既に在れば `ERR AUTH: key exists`、`FORCE` で作り直し。秘密鍵は NVS のみ、Refs #205) |
//! | `AUTH PUBKEY` | 生成済み公開鍵を `AUTH PUBKEY <base64url>` で返す (無ければ `ERR AUTH: no key`) |
//! | `AUTH SIGN <nonce>` | nonce (小文字 hex 32 文字) にその 32 バイトそのもので署名し `AUTH SIG <pubkey base64url> <sig base64url>` を返す (鍵が無ければ `ERR AUTH: no key`、nonce の形式不正は `ERR AUTH: bad nonce`) |
//! | `WS URL <url>` | cf-alc-recorder WS URL を上書き (staging テスト用) |
//! | `WS STATUS` | `WS CONNECTED=1 QUEUE=3 SEQ=42` を返す |
//! | `BUS5V STATUS` | M-Bus 5V 出力の現況 `BUS5V USB=1 OUT=1 BATTERY=0 BUS_IN=0` を返す。**設定は無い** — USB ホスト (PC) が列挙されていて、かつ M-Bus が外部給電でない (`BUS_IN=0`) 間だけ Core が 5V を出す固定動作で、hub-ui が 1 秒ごとに追随する (#202)。`BUS_IN` は起動時の W5500 probe で確定する M-Bus の外部給電判定 (`1`=PoE 等で外部給電中 `0`=無し `?`=未判定、Refs #211)。WS 下り command `{action:"bus5v_status"}` / `{action:"reboot"}` (auth-worker 端末一覧) でも遠隔で照会・再起動できる |
//! | `TENKO BP ON\|OFF` / `TENKO STATUS` | 点呼に血圧を含めるか (NVS、既定 OFF) / `TENKO BP=0` を返す |
//! | `HEAP` | `HEAP FREE_INT=<n> MIN_INT=<n> FREE_PSRAM=<n> TOTAL_INT=<n> TOTAL_PSRAM=<n>` を返す (Refs #27) |
//! | `HEAP DUMP` | `HEAPDUMP ...` 複数行 (ヒープブロック概況 + タスク別スタック余裕) |
//! | `LOG DUMP` | `LOGDUMP ...` 複数行 (`.noinit` リングの直近ログ。事象の事後解析用) |
//! | `PWALOG <id> <行>` / `PWALOG END <id> <n>` | キオスク PWA の診断ログ。WS 下り `get_log` で出す `EVT WS_COMMAND <id> …` への返事で、get_log の応答の `pwa_log` にだけ入る (最大 2 秒待つ、#215、pwalog.rs)。**応答しない**・リングには入れない |
//!
//! # 送信イベント (CoreS3 → ホスト)
//!
//! | イベント | 説明 |
//! |---|---|
//! | `FC1200 <hex>` | RS232 (FC-1200) からの受信データ (パススルー) |
//! | `EVT QR_TIMEOUT` | QR 画面が有効期限切れで閉じた |
//! | `EVT RESULT_CLOSED` | 結果画面が自動クローズした |
//! | `EVT TENKO_START` | 画面メニューから点呼が開始された |
//! | `{"type":...}` | BLE (NT-100B / NBP-1BLE) の測定データ・状態。
//!   ble-medical-gateway のシリアル JSON 互換 (ble.rs 参照) |
//!
//! ログ出力 (`I (123) ...` 等) も同じコンソールに混在するため、ホスト側は
//! 既知プレフィックス (OK/ERR/PONG/STATUS/FC1200/EVT/`{`) の行のみ解釈すること。

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use alc_hub_core::cfg::DeviceConfig;
use alc_hub_core::improv as improv_proto;
use alc_hub_core::protocol::{parse_line, HostCommand};
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;

use alc_hub_common::control::PairFlag;
use alc_hub_common::{
    config,
    settings::Settings,
    status::{now_ms, SharedStatus},
    ui_api::UiCommand,
};
use alc_hub_core::alarm::SharedMonitor;
use alc_hub_wifi::{improv::Improv, wifi::Wifi};

use crate::console;

/// `EVT IMPROV_UNAVAILABLE` を出したか (Improv を持たないビルドで 1 回だけ出す)
static IMPROV_UNAVAILABLE_SENT: AtomicBool = AtomicBool::new(false);

/// `wifi` / `improv` は Wi-Fi を起こさないビルド (`lan`、#217) では `None`
pub fn start(
    tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
    wifi: Option<Wifi>,
    pair_flag: PairFlag,
    mut improv: Option<Improv>,
    alarm: SharedMonitor,
) -> Result<()> {
    // stdin のブロッキング読み出しを可能にする (console.rs と同じ設置)。
    // 本 crate は Improv (バイナリフレーム) を混ぜるため console::spawn_reader は
    // 使わず、行の切り出しだけ console::take_line を共有する
    crate::console::install_usb_serial_jtag();

    crate::task::name_next(c"host_link");
    std::thread::Builder::new()
        .name("host_link".into())
        .stack_size(12 * 1024)
        .spawn(move || {
            let mut chunk = [0u8; 64];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                match std::io::stdin().lock().read(&mut chunk) {
                    Ok(0) => FreeRtos::delay_ms(20),
                    Ok(n) => {
                        acc.extend_from_slice(&chunk[..n]);
                        drain_buffer(
                            &mut acc,
                            &tx,
                            &status,
                            &settings,
                            wifi.as_ref(),
                            &pair_flag,
                            &mut improv,
                            &alarm,
                        );
                    }
                    Err(_) => FreeRtos::delay_ms(100),
                }
            }
        })?;
    Ok(())
}

/// バッファ先頭から処理できる単位 (IMPROV フレーム / テキスト行) を消費する
#[allow(clippy::too_many_arguments)]
fn drain_buffer(
    acc: &mut Vec<u8>,
    tx: &Sender<UiCommand>,
    status: &SharedStatus,
    settings: &Settings,
    wifi: Option<&Wifi>,
    pair_flag: &PairFlag,
    improv: &mut Option<Improv>,
    alarm: &SharedMonitor,
) {
    loop {
        if acc.is_empty() {
            return;
        }
        match improv_proto::try_parse(acc) {
            improv_proto::Frame::Packet {
                ptype,
                data,
                consumed,
            } => {
                match improv {
                    Some(improv) => improv.handle_packet(ptype, &data),
                    None => {
                        if !IMPROV_UNAVAILABLE_SENT.swap(true, Ordering::Relaxed) {
                            alc_hub_common::evtlog::emit("EVT IMPROV_UNAVAILABLE lan");
                        }
                    }
                }
                acc.drain(..consumed);
            }
            improv_proto::Frame::Corrupt { consumed } => {
                acc.drain(..consumed);
            }
            improv_proto::Frame::NeedMore => return,
            improv_proto::Frame::NotImprov => {
                // テキスト行として改行まで処理
                let Some(line) = console::take_line(acc) else {
                    console::discard_overlong(acc);
                    return;
                };
                handle_line(&line, tx, status, settings, wifi, pair_flag, alarm);
            }
        }
    }
}

/// 1 行を処理する。解析は alc-hub-core::protocol (純粋・テスト済み)、
/// 副作用 (画面遷移・NVS 保存・応答出力) はここで行う。
#[allow(clippy::too_many_arguments)]
fn handle_line(
    line: &str,
    tx: &Sender<UiCommand>,
    status: &SharedStatus,
    settings: &Settings,
    wifi: Option<&Wifi>,
    pair_flag: &PairFlag,
    alarm: &SharedMonitor,
) {
    // キオスク PWA の診断ログの返事 (`PWALOG <id> …`、#215)。get_log の応答へ
    // 中継するだけで、コマンドとしては解釈しない (ERR も返さない)。リング・
    // crash_log にも入れない。信頼の境界は他のシリアルコマンドと同じ (pwalog.rs)
    if alc_hub_core::pwalog::parse(line).is_some() {
        crate::pwalog::offer(line);
        return;
    }
    let command = match parse_line(line, config::QR_DEFAULT_TIMEOUT_MS) {
        Ok(Some(command)) => command,
        Ok(None) => return, // 空行
        Err(err_response) => {
            println!("{err_response}");
            return;
        }
    };

    // 機種に依らないコマンド (PING / HEAP / LOG / AUTH / WS) は共通実装へ。
    // 捌かれなかったものだけがここへ落ちてくる (console.rs 参照)
    let Some(command) = console::handle_common(command, status, settings, true) else {
        return;
    };

    match command {
        HostCommand::ShowQr {
            payload,
            timeout_ms,
        } => {
            let _ = tx.send(UiCommand::ShowQr {
                payload,
                timeout_ms,
            });
            alc_hub_common::evtlog::emit("OK QR");
        }
        HostCommand::Measure => {
            let _ = tx.send(UiCommand::Measure);
            alc_hub_common::evtlog::emit("OK MEASURE");
        }
        HostCommand::Result { ok, value } => {
            let _ = tx.send(UiCommand::Result { ok, value });
            alc_hub_common::evtlog::emit("OK RESULT");
        }
        HostCommand::ShowError { message } => {
            let _ = tx.send(UiCommand::Error { message });
            alc_hub_common::evtlog::emit("OK ERROR");
        }
        HostCommand::Reset => {
            let _ = tx.send(UiCommand::Reset);
            alc_hub_common::evtlog::emit("OK RESET");
        }
        HostCommand::Stage(stage) => {
            let _ = tx.send(UiCommand::Stage(stage));
            alc_hub_common::evtlog::emit(&format!("OK STAGE {}", stage.label()));
        }
        HostCommand::Rotate(deg) => match settings.set_rotation(deg) {
            Ok(()) => {
                let _ = tx.send(UiCommand::Rotate(deg));
                println!("OK ROTATE {deg}");
            }
            Err(e) => {
                log::error!("host_link: rotation 保存失敗: {e:?}");
                println!("ERR ROTATE: 保存に失敗しました");
            }
        },
        // 運行者 PWA からの heartbeat (`HB OK`、3 秒ごと)。**応答は返さない**。
        // 途切れたら鳴らすのは鳴動ループ (src/main.rs)、判定は alc_hub_core::alarm。
        // 初回のこの行が沈黙警告を**武装**する (それまでは鳴らない、#187)
        HostCommand::Heartbeat {
            ok,
            reason,
            call,
            grace,
        } => {
            crate::alarm::apply_heartbeat(alarm, ok, reason.as_deref(), call, grace);
        }
        // ★ 行頭 (`STATUS LAN=…`) は変えないこと。ブラウザ側 (`useCoreS3Serial` の
        //   `classify()`) は行頭 `STATUS alarm` を「警告デバイス = 別機種」と判定して
        //   **CoreS3 のポートを reject する**。鳴動状態は**行末**に足す (#187)
        HostCommand::Status => {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            // lock できなかったときも行の形は保つ (ブラウザは key=value で読む)
            let mut alarm_field = "ALARM=unknown".to_string();
            crate::alarm::with_monitor(alarm, |m, now| alarm_field = m.status_field(now));
            println!(
                "STATUS LAN={} RS232={} BLE={} WIFI={} ROT={} BOARD={} {}",
                u8::from(st.lan_link),
                u8::from(st.rs232_active(now_ms(), config::RS232_ACTIVE_WINDOW_MS)),
                u8::from(st.ble_connected),
                u8::from(st.wifi_connected),
                settings.rotation(),
                st.board.label(),
                alarm_field,
            );
        }
        // 設定エクスポート: 1 行 JSON を CFG プレフィックスで返す
        HostCommand::CfgGet => println!("CFG {}", settings.export().to_json()),
        // 設定インポート: パスワードは伏せて応答
        HostCommand::CfgSet { json } => match DeviceConfig::from_json(&json) {
            Ok(cfg) => match settings.apply(&cfg) {
                Ok(()) => {
                    if let Some(deg) = cfg.rotation {
                        let _ = tx.send(UiCommand::Rotate(deg));
                    }
                    println!("OK CFG");
                }
                Err(e) => {
                    log::error!("host_link: CFG 適用失敗: {e:?}");
                    println!("ERR CFG: 保存に失敗しました");
                }
            },
            Err(msg) => println!("ERR CFG: {msg}"),
        },
        // 保存済み Wi-Fi 設定での接続テスト。失敗時は原因を切り分けて返す
        HostCommand::WifiTest => {
            // Wi-Fi を起こさないビルド (`lan`、#217)
            let Some(wifi) = wifi else {
                alc_hub_common::evtlog::emit("EVT WIFI_TEST NG lan");
                return;
            };
            match settings.wifi_credentials() {
                Some((ssid, pass)) => match wifi.connect_with_diagnosis(&ssid, &pass) {
                    Ok(ip) => alc_hub_common::evtlog::emit(&format!("EVT WIFI_TEST OK {ip}")),
                    Err(reason) => {
                        wifi.mark_disconnected();
                        if let Ok(mut st) = status.lock() {
                            st.push_event(now_ms(), "WiFi テスト失敗");
                        }
                        // SSID を含むので println のまま (evtlog の例外、#215)
                        println!("EVT WIFI_TEST NG {reason}");
                    }
                },
                None => alc_hub_common::evtlog::emit(
                    "EVT WIFI_TEST NG 保存済み Wi-Fi 設定がありません",
                ),
            }
        }
        // BLE 再ペアリング: ボンド消去を BLE スレッドへ依頼 (血圧計の暗号化復旧)。
        // 実際の消去と EVT PAIR_CLEARED 出力は ble タスク側で行う
        HostCommand::BlePair => {
            pair_flag.store(true, core::sync::atomic::Ordering::SeqCst);
            println!("OK PAIR");
        }
        // Windows GW (alc-gw) 連携 (gw_link.rs)
        HostCommand::GwUrl { url } => match settings.set_gw_url(&url) {
            Ok(()) => println!("OK GW URL"),
            Err(e) => {
                log::error!("host_link: GW URL 保存失敗: {e:?}");
                println!("ERR GW: URL の保存に失敗しました");
            }
        },
        HostCommand::GwStatus => {
            let (connected, discovered) = status
                .lock()
                .map(|s| (s.gw_connected, s.gw_discovered_url.clone()))
                .unwrap_or((false, String::new()));
            println!(
                "GW CONNECTED={} URL={} DISCOVERED={}",
                u8::from(connected),
                settings.gw_url().unwrap_or_else(|| "UNSET".into()),
                if discovered.is_empty() { "NONE".into() } else { discovered },
            );
        }
        // M-Bus 5V の現況 (設定は無い — USB ホストの有無に hub-ui が追随する、#202)
        HostCommand::Bus5vStatus => {
            let (usb_host, ext_5v_out, battery_present, bus_in) = status
                .lock()
                .map(|st| (st.usb_host, st.ext_5v_out, st.battery_present, st.bus_in))
                .unwrap_or((false, false, false, None));
            let bus_in_str = match bus_in {
                Some(true) => "1",
                Some(false) => "0",
                None => "?",
            };
            println!(
                "BUS5V USB={} OUT={} BATTERY={} BUS_IN={}",
                u8::from(usb_host),
                u8::from(ext_5v_out),
                u8::from(battery_present),
                bus_in_str,
            );
        }
        // 点呼の構成: 血圧はオプション (tenko.rs)。NVS に保存し、UI が次の点呼から読む
        HostCommand::TenkoBp { enabled } => match settings.set_tenko_bp(enabled) {
            Ok(()) => {
                if let Ok(mut st) = status.lock() {
                    st.tenko_bp = enabled;
                }
                println!("OK TENKO BP={}", u8::from(enabled));
            }
            Err(e) => {
                log::error!("host_link: TENKO BP 保存失敗: {e:?}");
                println!("ERR TENKO: 保存に失敗しました");
            }
        },
        HostCommand::TenkoStatus => println!("TENKO BP={}", u8::from(settings.tenko_bp())),
        // OTA 更新 (進捗・結果は EVT OTA_* で届く。シリアル経路は WS 進捗 sink
        // 無し = None。ota.rs 参照)
        HostCommand::Ota { url } => {
            crate::ota::spawn_update(url, status.clone(), None);
            println!("OK OTA");
        }
        // 印刷系は AtomS3 印刷ブリッジ (atoms3-print) 専用 (#38)。CoreS3 は
        // プリンター配線を持たないため未対応と明示する
        HostCommand::Print { .. } | HostCommand::PrinterAddr { .. } | HostCommand::PrinterStatus => {
            println!("ERR UNSUPPORTED (kiosk hub)");
        }
        // console::handle_common が捌いたはずのもの (到達しない)
        other => log::debug!("host_link: handled by console::handle_common: {other:?}"),
    }
}
