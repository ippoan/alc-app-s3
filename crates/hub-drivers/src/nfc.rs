//! Unit NFC (ST25R3916, I2C) 読み取り (issue #84 / #96 / #101 + plan/nfc-card-identity.md)。
//!
//! I2C バスの所有は C++ 側 (components/nfc_shim → M5UnitUnified) に持たせる。
//! ここでは esp-idf-hal の `I2cDriver` を作らず、I2C ポート番号 (I2C_NUM_1) と
//! GPIO 番号だけを FFI 越しに渡す — I2C0 (内部バス、電源IC/タッチ、main.rs) と
//! I2C1 (NFC 専用) を完全分離し、Rust/C++ 二重の I2C ドライバ install を避ける。
//!
//! 配線: DIN Base Port A (SDA=G2 / SCL=G1、AtomS3 ベンチ (crates/atoms3-nfc) と
//! 同一ピン番号)。Port B (旧配線 G8/G9) は issue #84 検討時の暫定割当で、
//! G9 は Base LAN PoE v1.2 の W5500 CS が使うため戻せない。SCL=G1 は Base
//! 本体の DB9 (TX=G1) と衝突するので、その DB9 は使わないこと。ack しなければ
//! `sda`/`scl` の実引数を入替えて再試行すること。
//!
//! 存在検知ゲート + F(交通系IDm)→A(HCE/UID)→B(免許証) 逐次掃引は
//! crates/atoms3-nfc/src/main.rs (issue #96 で実機確認済み) の移植。
//!
//! # ボード非依存 (issue #134)
//!
//! **本モジュールは CoreS3 専用ではない。** I2C ポート番号はピンと同じく
//! 引数で受け、検知の通知は [`NfcEvent`] のコールバックで外へ出す。
//! CoreS3 は「ビープ + 免許証なら `UiCommand::License`」を、NFC タイムカード端末
//! (crates/atoms3-timecard) は「打刻イベントを WS uplink へ積む + LED」を
//! それぞれコールバック側で行う。**読み取りループを写して 3 実装目を作らないこと。**
//!
//! ログ通知 (`SharedStatus::push_event` — 「ログ確認」画面に既存の rs232.rs 等と
//! 同じ形式で表示される) と `EVT NFC_LICENSE` のホスト出力はボード非依存なので
//! 本モジュールに残す。

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyIOPin, Pin};

use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_core::nfc_tap::TapGate;

extern "C" {
    fn nfc_shim_init(i2c_port: i32, sda_gpio: i32, scl_gpio: i32) -> i32;
    fn nfc_shim_poll_felica_idm(out_hex: *mut u8, out_cap: i32) -> i32;
    fn nfc_shim_poll_nfca_uid(out_hex: *mut u8, out_cap: i32) -> i32;
    fn nfc_shim_read_license_expiry(
        out_issue: *mut u8,
        issue_cap: i32,
        out_expiry: *mut u8,
        expiry_cap: i32,
    ) -> i32;
    fn nfc_shim_measure_amplitude() -> i32;
    fn nfc_shim_measure_phase() -> i32;
    fn nfc_shim_transceive_apdu_a(
        cmd: *const u8,
        cmd_len: i32,
        out: *mut u8,
        out_cap: i32,
    ) -> i32;
}

/// 初期化の再試行間隔。Unit の電源投入直後や活線挿抜では ack しないことがあり、
/// **1 度で諦めるとスレッドごと終了して再起動するまで NFC が死ぬ**。画面も
/// スピーカーも無い常設機ではそれに気付けないので、諦めずに待ち続ける
const INIT_RETRY: Duration = Duration::from_secs(5);

/// 初期化を成功するまで再試行する。戻り値は成功したか (現状 `true` のみ —
/// 将来この関数に諦める条件を足すときのための口)。
/// rc が変わったときだけログを出す (5 秒ごとに同じ行を吐き続けない)
fn init_with_retry(i2c_port: i32, sda_num: i32, scl_num: i32, status: &SharedStatus) -> bool {
    let mut last_rc = i32::MIN;
    loop {
        let rc = unsafe { nfc_shim_init(i2c_port, sda_num, scl_num) };
        if rc == 0 {
            return true;
        }
        if rc != last_rc {
            let msg = format!(
                "NFC 初期化失敗 rc={rc} (配線/バス役割 port={i2c_port} sda={sda_num} scl={scl_num} を確認)"
            );
            log::error!("nfc: {msg} — {INIT_RETRY:?} ごとに再試行する");
            println!("EVT NFC_INIT_NG rc={rc}");
            crate::crashlog::note(&format!("EVT NFC_INIT_NG rc={rc}"));
            push_event(status, &msg);
            last_rc = rc;
        }
        FreeRtos::delay_ms(INIT_RETRY.as_millis() as u32);
    }
}

/// 検知したカード。`start` に渡したコールバックへ、読めた分岐ごとに 1 回届く
/// (同じカードを載せっぱなしにしても再通知はしない — dedupe はループ側が持つ)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NfcEvent {
    /// 交通系 IC 等の FeliCa IDm (8 バイトを大文字 hex 16 文字にしたもの)
    Felica { idm: String },
    /// NFC-A の UID (HCE / NTAG 等)
    NfcaUid { uid: String },
    /// 電子車検証 (Type-A ISO-DEP + SELECT MF 成功の簡易判定、issue #105)
    CarInspection { uid: String },
    /// 従来 IC 運転免許証の PIN なし読み取り (EF 2F01)。日付は YYYYMMDD
    License { issue: String, expiry: String },
    /// 何かかざされたが読めなかった (免許証の途中死・カード引き抜き等)。
    /// rc の意味は [`license_rc_reason`] 参照。カード無し (-2) では届かない
    ReadFailed { rc: i32 },
}

/// 検知通知の受け口。`FnMut` を trait object にせず総称で受けると
/// `start` が呼び出し側ごとに単相化されるだけで済む (vtable も Box も不要)
pub trait NfcSink: Send + 'static {
    fn on_event(&mut self, event: &NfcEvent);
}

impl<F: FnMut(&NfcEvent) + Send + 'static> NfcSink for F {
    fn on_event(&mut self, event: &NfcEvent) {
        self(event)
    }
}

/// 存在検知 (アンテナ振幅) のトリガ閾値。カード無しのベースラインは完全に
/// 安定 (AtomS3 実測: 60サンプル連続でノイズ0)、カード接近で 2 下がる。
/// |amp - baseline| がこの値以上で「何かかざされた」と判定し F→A→B の
/// 逐次ポーリングを開始する (issue #96 続き)。CoreS3 環境固有のベースライン
/// ノイズは heartbeat ログ (tick%100) で実機再確認が必要 (issue #101)
const PRESENCE_DELTA: i32 = 2;

// タップ運用 (かざしてすぐ離す) のため空白時間を最小化 (AtomS3 ベンチと同値)
const POLL_INTERVAL_MS: u32 = 20;

/// トリガ固着の保険: 何も読めないまま3秒続いたら誤トリガとみなし再較正
/// (温度ドリフト等でベースラインが実態とずれたケースの自己回復)
const TRIGGER_STUCK: Duration = Duration::from_secs(3);

/// NFC 読み取りスレッドを起動する。
///
/// - `i2c_port`: nfc_shim (C++ 側) に立てさせる I2C ポート番号。**Rust 側で
///   同じポートに `I2cDriver` を作らないこと** (二重 install で abort する)。
///   CoreS3 は内部バスが I2C_NUM_0 なので 1、AtomS3 系は他に I2C を使わないので 0
/// - `sink`: 検知の通知先。`|e: &NfcEvent| { ... }` のクロージャで足りる
pub fn start(
    i2c_port: i32,
    sda: AnyIOPin,
    scl: AnyIOPin,
    status: SharedStatus,
    sink: impl NfcSink,
) -> Result<()> {
    // Pin::pin() は PinId (u8) を返す。ownership は FFI 側 (C++/M5HAL) が握るため
    // 番号だけ取り出して drop する (esp-idf-hal 側では未使用)
    let sda_num = sda.pin() as i32;
    let scl_num = scl.pin() as i32;
    drop(sda);
    drop(scl);

    crate::task::name_next_psram(c"nfc", 8 * 1024);
    std::thread::Builder::new()
        .name("nfc".into())
        // APDU 組立 (String) + FFI 経由の hex 文字列バッファがあるため rs232.rs と同等
        .stack_size(8 * 1024)
        .spawn(move || run(i2c_port, sda_num, scl_num, status, sink))?;
    Ok(())
}

fn run(i2c_port: i32, sda_num: i32, scl_num: i32, status: SharedStatus, mut sink: impl NfcSink) {
    if !init_with_retry(i2c_port, sda_num, scl_num, &status) {
        return;
    }
    // 画面を持たない機 (atoms3-timecard) では push_event が誰にも見えないので
    // シリアルにも出す。ログは USB CDC がホストに掴まれる前 (起動 1 秒以内) の
    // ぶんが落ちるので、`EVT NFC_READY` は起動後でもコンソールから
    // `LOG DUMP` で拾える (crashlog リングに載る) ことに意味がある
    log::info!("nfc: 待受開始 port={i2c_port} sda={sda_num} scl={scl_num}");
    println!("EVT NFC_READY port={i2c_port} sda={sda_num} scl={scl_num}");
    crate::crashlog::note("EVT NFC_READY");
    push_event(&status, "NFC 待受開始 (存在検知ゲート + F→A→B 逐次ポーリング)");

    // 重複抑止は **debounce**「離れて N ms 経つまで、まだ同じタップ」
    // (alc_hub_core::nfc_tap、issue #103)。
    // 以前は「直前値と違えば発火 / 読めなければ直前値をクリア」のエッジ判定
    // だったが、モバイル FeliCa は応答が断続的で 20ms ポーリングが 1 回
    // 空振りしただけで直前値が消え、**1 タップで 2 回発火**していた。
    // 打刻イベント (#134) では 1 タップが別 seq の 2 行になり、サーバ側の
    // seq 冪等では防げない (別イベントが 2 つ生まれたケースのため) —
    // 1 回かざした人が出勤と退勤を同時に打つ壊れ方になるので端末側で塞ぐ。
    // 系統ごとに別の gate を持つ (使い回すと交通系の直後の免許証が抑止される)
    let mut felica_gate = TapGate::default();
    let mut nfca_gate = TapGate::default();
    let mut car_gate = TapGate::default();
    let mut license_gate = TapGate::default();
    // -2 (カード無し) は定常状態なのでログしない。未実行センチネルは i32::MIN。
    // **これは失敗ログの抑止専用** — 成功時の発火判定は license_gate が持つ
    let mut last_license_rc = i32::MIN;

    // 存在検知のベースライン (-1 = 未較正、初回測定値で初期化)。
    // 振幅はカード系、位相はスマホ系 (モバイルSuica 等、振幅に出にくい) を拾う
    let mut baseline: i32 = -1;
    let mut baseline_ph: i32 = -1;
    let mut triggered_since: Option<Instant> = None;
    let mut tick: u32 = 0;

    loop {
        tick = tick.wrapping_add(1);

        // --- 待機: プロトコル非依存の存在検知 (アンテナ振幅+位相) ---
        // モード切替もポーリングも行わず振幅・位相だけを見る。ベースラインは
        // 非トリガ時のみ ±1 ずつ追従させ温度ドリフトを吸収する (カードが
        // 載っている間は追従しないので、置きっぱなしでも基準が汚れない)
        let amp = unsafe { nfc_shim_measure_amplitude() };
        let ph = unsafe { nfc_shim_measure_phase() };
        let mut triggered = false;
        if amp >= 0 {
            if baseline < 0 {
                baseline = amp;
            }
            if (amp - baseline).abs() >= PRESENCE_DELTA {
                triggered = true;
            } else {
                baseline += (amp - baseline).signum();
            }
        } else {
            triggered = true; // 測定失敗時は常時ポーリングへフォールバック (安全側)
        }
        if ph >= 0 {
            if baseline_ph < 0 {
                baseline_ph = ph;
            }
            if (ph - baseline_ph).abs() >= PRESENCE_DELTA {
                triggered = true;
            } else {
                baseline_ph += (ph - baseline_ph).signum();
            }
        }

        if tick % 100 == 0 {
            log::info!(
                "nfc heartbeat tick={tick} amp={amp}/{baseline} ph={ph}/{baseline_ph} last_rc={last_license_rc}"
            );
        }

        if !triggered {
            triggered_since = None;
            FreeRtos::delay_ms(POLL_INTERVAL_MS);
            continue;
        }
        match triggered_since {
            None => triggered_since = Some(Instant::now()),
            Some(t0) if t0.elapsed() > TRIGGER_STUCK => {
                log::info!("nfc presence: 再較正 amp={amp} ph={ph} (トリガ固着 {TRIGGER_STUCK:?})");
                baseline = amp;
                baseline_ph = ph;
                triggered_since = None;
                FreeRtos::delay_ms(POLL_INTERVAL_MS);
                continue;
            }
            _ => {}
        }

        // --- 何かかざされた: F (交通系IDm、日常の主役) → A (HCE/UID) → B (免許証) ---
        // 軽い単発交換 (F/A の検出は数ms) を先に、重い APDU セッション (B) を
        // 最後に試す。主要経路の交通系タップが最速になる並び
        let mut got = false;

        match poll_felica_idm() {
            Ok(Some(idm)) => {
                if felica_gate.should_fire(&idm, now_ms()) {
                    // push_event はイベントログ (UI/WS) 行のみで serial には出ない。
                    // log::info! を並置して scripts/nfc_serial_beep.py (COM 監視、
                    // 既定 --match "NFC|免許|IDm") で検知音を鳴らせるようにする (issue #101)
                    log::info!("NFC IDm={idm}");
                    push_event(&status, &format!("NFC IDm={idm}"));
                    sink.on_event(&NfcEvent::Felica { idm });
                }
                got = true;
            }
            // 読めなかったことを理由に状態をクリアしない (issue #103)。
            // 空振りで状態が消えることが 2 重発火の原因だった
            Ok(None) => {}
            Err(e) => log::warn!("nfc: FeliCa poll error: {e:#}"),
        }

        if !got {
            match poll_nfca_uid() {
                Ok(Some(uid)) => {
                    // 電子車検証は Type-A + ISO14443-4 (ISO-DEP、RATS 応答あり) で
                    // 応答することを実機確認済み (issue #105)。UID が取れた時点で
                    // このカードがただの UID タグ (NTAG 等) かスマートカードかを
                    // SELECT MF で追加確認する (詳細は detect_car_inspection_a
                    // のコメント参照)。tap のたびに ISO-DEP セッション1回分の
                    // コストが乗るが、非対応カードは RATS 非対応で即座に弾かれる
                    // ため実害は小さい
                    if detect_car_inspection_a() {
                        if car_gate.should_fire(&uid, now_ms()) {
                            log::info!("電子車検証 検知 (UID={uid})");
                            push_event(&status, "電子車検証 検知");
                            sink.on_event(&NfcEvent::CarInspection { uid });
                        }
                    } else if nfca_gate.should_fire(&uid, now_ms()) {
                        log::info!("NFC-A UID={uid}");
                        push_event(&status, &format!("NFC-A UID={uid}"));
                        sink.on_event(&NfcEvent::NfcaUid { uid });
                    }
                    got = true;
                }
                // issue #103: 空振りで状態をクリアしない (上の FeliCa と同じ理由)
                Ok(None) => {}
                Err(e) => log::warn!("nfc: NFC-A poll error: {e:#}"),
            }
        }

        if !got {
            let (rc, issue, expiry) = read_license_expiry();
            if rc == 0 {
                // 免許証も同じクールダウンに載せる (issue #103)。key は交付日 +
                // 有効期限 = alc-app タブレットが使う employees.nfc_id と同じ 16 桁
                if license_gate.should_fire(&format!("{issue}{expiry}"), now_ms()) {
                    log::info!("免許証 交付 {issue} 期限 {expiry}");
                    push_event(&status, &format!("免許証 交付 {issue} 期限 {expiry}"));
                    // ホストにも通知 (画面遷移は UI が判断する)
                    println!("EVT NFC_LICENSE issue={issue} expiry={expiry}");
                    sink.on_event(&NfcEvent::License { issue, expiry });
                }
                got = true;
            } else if rc != -2 && rc != last_license_rc {
                // 途中死はカード引き抜き等でも出る
                log::warn!("nfc: 免許証 読み取り失敗 rc={rc} ({})", license_rc_reason(rc));
                sink.on_event(&NfcEvent::ReadFailed { rc });
            }
            last_license_rc = rc;
        }

        if got {
            triggered_since = None;
        }

        FreeRtos::delay_ms(POLL_INTERVAL_MS);
    }
}

/// SELECT MF (`00 A4 00 00`)。実機診断の結果 (issue #105、2026-07-21):
/// AlcoholChecker (ippoan/AlcoholChecker) の AID ベース SELECT DF
/// (`78 77 81 02 80 00`) は実機で SW=6A82 (該当ファイル無し) となり誤りだった
/// — 電子車検証は AID ベース選択ではなく、免許証 (Type-B) と同じ伝統的な
/// MF/EF 階層構造で、SELECT MF が SW=9000 で成功することを確認した。
/// ⚠ 現状は「Type-A ISO14443-4 対応カードで SELECT MF が成功する」ことのみを
/// 車検証の判定条件にしている簡易ヒューリスティクスであり、他の Type-A
/// スマートカード (MF/EF構造を持つもの) との誤判定リスクはゼロではない。
/// 車検証固有の EF (免許証の EF 2F01 に相当するもの) を特定し SELECT できれば
/// より確実な判定になる — 未特定のため followup 課題として残す
const APDU_SELECT_MF: [u8; 4] = [0x00, 0xA4, 0x00, 0x00];

/// Type-A ISO-DEP 経由で電子車検証 (簡易判定: SELECT MF 成功) を確認する。
/// ISO14443-4 非対応カード (単純メモリタグ等) では通信自体が成立せず false
fn detect_car_inspection_a() -> bool {
    let mut out = [0u8; 16];
    let n = unsafe {
        nfc_shim_transceive_apdu_a(
            APDU_SELECT_MF.as_ptr(),
            APDU_SELECT_MF.len() as i32,
            out.as_mut_ptr(),
            out.len() as i32,
        )
    };
    if n < 2 {
        return false; // カード無し/セッション失敗/SW未満の短いレスポンス
    }
    let n = n as usize;
    out[n - 2] == 0x90 && out[n - 1] == 0x00
}

fn poll_felica_idm() -> Result<Option<String>> {
    let mut buf = [0u8; 32];
    let n = unsafe { nfc_shim_poll_felica_idm(buf.as_mut_ptr(), buf.len() as i32) };
    if n == 0 {
        return Ok(None);
    }
    if n < 0 {
        bail!("nfc_shim_poll_felica_idm rc={n}");
    }
    Ok(Some(
        String::from_utf8_lossy(&buf[..n as usize]).into_owned(),
    ))
}

fn poll_nfca_uid() -> Result<Option<String>> {
    let mut buf = [0u8; 32];
    let n = unsafe { nfc_shim_poll_nfca_uid(buf.as_mut_ptr(), buf.len() as i32) };
    if n == 0 {
        return Ok(None);
    }
    if n < 0 {
        bail!("nfc_shim_poll_nfca_uid rc={n}");
    }
    Ok(Some(
        String::from_utf8_lossy(&buf[..n as usize]).into_owned(),
    ))
}

/// 従来 IC 運転免許証の PIN なし有効期限読み取り (EF 2F01)。戻り値は
/// (rc, 交付日, 有効期限)。rc==0 のときのみ日付が有効
fn read_license_expiry() -> (i32, String, String) {
    let mut issue = [0u8; 16];
    let mut expiry = [0u8; 16];
    let rc = unsafe {
        nfc_shim_read_license_expiry(
            issue.as_mut_ptr(),
            issue.len() as i32,
            expiry.as_mut_ptr(),
            expiry.len() as i32,
        )
    };
    if rc != 0 {
        return (rc, String::new(), String::new());
    }
    (rc, cstr_bytes_to_str(&issue), cstr_bytes_to_str(&expiry))
}

fn cstr_bytes_to_str(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// components/nfc_shim/nfc_shim.cpp の nfc_shim_read_license_expiry() コメント準拠
pub fn license_rc_reason(rc: i32) -> &'static str {
    match rc {
        0 => "OK",
        -1 => "初期化未完了 or バッファ不足",
        -2 => "カード無し",
        -3 => "ATTRIB 失敗",
        -4 => "SELECT MF 失敗 (免許証以外の Type-B カードの可能性)",
        -5 => "SELECT EF 2F01 失敗",
        -6 => "READ BINARY 失敗",
        -7 => "データ長が想定より短い (EF 長が事前想定と違う、実機で要再調整)",
        _ => "不明なエラーコード",
    }
}

fn push_event(status: &SharedStatus, line: &str) {
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), line);
    }
}
