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
//! (crates/atoms3-timecard) は「打刻イベントを WS uplink へ積む」を
//! それぞれコールバック側で行う。**読み取りループを写して 3 実装目を作らないこと。**
//!
//! ログ通知 (`SharedStatus::push_event` — 「ログ確認」画面に既存の rs232.rs 等と
//! 同じ形式で表示される) と `EVT NFC_LICENSE` のホスト出力はボード非依存なので
//! 本モジュールに残す。

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyIOPin, Pin};

use alc_hub_common::evtlog;
use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_core::desfire;
use alc_hub_core::nfc_sticky::{self, Sticky};
use alc_hub_core::nfc_tap::{TapGate, TapOutcome, DEFAULT_COMMIT_WINDOW_MS};

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
    fn nfc_shim_prepare_mode_b();
    fn nfc_shim_transceive_apdu_a(
        cmd: *const u8,
        cmd_len: i32,
        out: *mut u8,
        out_cap: i32,
    ) -> i32;
    fn nfc_shim_isodep_a_open(out_ats: *mut u8, ats_cap: i32) -> i32;
    fn nfc_shim_isodep_a_transceive(
        cmd: *const u8,
        cmd_len: i32,
        out: *mut u8,
        out_cap: i32,
    ) -> i32;
    fn nfc_shim_isodep_a_close() -> i32;
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
            alc_hub_common::evtlog::emit(&format!("EVT NFC_INIT_NG rc={rc}"));
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
    /// **確定窓のあいだに 2 枚のカードが見えた** (issue #143)。
    /// どちらの人のタップか決められないので**どちらも登録しない** —
    /// 受け手はエラー表示だけを行い、打刻/点呼のレコードを作らないこと。
    /// 検出できない条件は `alc_hub_core::nfc_tap` のモジュール doc 参照
    MultipleCards,
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

/// トリガ固着の保険: 何も読めないままこの時間が続いたら誤トリガとみなし、
/// ベースラインを**トリガが立つ前の値へ戻す** (温度ドリフト等の自己回復)。
///
/// **★ 既知の読み取り所要より確実に長くすること。**
/// `alc_hub_core::nfc_tap` の実測どおり **免許証は 3.4〜4.1 秒に 1 回しか読めない**。
/// ここが 3 秒だったため、**読める前に再較正が走っていた** (#155 で実機実測):
/// 1 周 448ms → 3 秒では 6 回しか試せないのに、免許証が読めたのは 9 周目 (3922ms)。
/// **「3 回に 1 回しか反応しない」の直接の原因**だったので、実測の上限 4.1 秒に
/// 倍近い余裕を取る。固着したままでも**ポーリングは続く**ので実害は少なく、
/// 逆に短すぎると読める前に打ち切ってしまう — **長い側に倒すのが安全側**
const TRIGGER_STUCK: Duration = Duration::from_secs(8);

/// トリガ後に F/A/B をどの順で回すか (#155 step 4)。**呼び出し側が選ぶ。**
///
/// 1 周 (F → A → B、実測 448ms) の各先頭でモード切替 = **電界断 (10ms) が 3 回**入る。
/// FeliCa は低消費電力で F の窓 1 回で読めるが、免許証 (Type-B、暗号コプロ付き) は
/// F/A の電源断 2 回の直後の B 窓 150ms で WUPB→ATTRIB→APDU×3 を完走する必要があり、
/// 結合が弱いと B 窓の先頭で電源が立ち上がりきらず **痕跡ゼロで落ちる** (実機 4 build:
/// 無反応 4/10, 2/5, 7/10, 8/10。音・常時ポーリング・PSRAM・同時負荷は全て否定済み)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PollOrder {
    /// F → A → B (従来。検証機 atoms3-nfc 用。CoreS3 も #162 で [`LicenseFirst`](Self::LicenseFirst) に寄せた)
    FelicaFirst,
    /// **B 先行 + B 粘着** (NFC タイムカード端末)。待機中は B モードのまま電界 ON なので、
    /// トリガ後の 1 周目は切替ゼロで B から読む。B が応答した (成功以外の途中死も含む) なら
    /// 次周は F/A を飛ばして **B だけを電界断なしで再試行**する (粘着。解く条件と理由は
    /// `alc_hub_core::nfc_sticky` のモジュール doc)。B が -2 なら F → A を回し、**周末に
    /// B モードへ戻して**待機と次周先頭を B に揃える (戻さないと待機中のモードが A に変わり、
    /// 次周の B で電界断が復活し、存在検知のベースラインも B 前提から外れる)
    LicenseFirst,
}

/// 存在検知 (アンテナ振幅・位相、`PRESENCE_DELTA`) を**カード読み取りのゲートに使うかどうか** (#175)。
///
/// 存在検知は RF ポーリングを省くための最適化にすぎない。ゲートを外しても読めるものは同じで、
/// 変わるのは「待機中も WUPB を打つか」だけ。待機中も電界は ON (`measure_ad` は tx_en を
/// 触らない) なので、増えるのは WUPB の変調と I2C の転送
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresenceGate {
    /// 振幅・位相がベースラインから動いたときだけ F/A/B を回す (従来。検証機 atoms3-nfc 用)。
    ///
    /// CoreS3 も免許証で点呼を始める (#125) ので同じ穴があり、#162 で LicenseFirst +
    /// AlwaysPoll に寄せた (候補 C)
    Adaptive,
    /// 存在検知でゲートせず**待機中も毎周 poll を回す** (NFC タイムカード端末)。
    ///
    /// 本番機 (VoiceS3R + Unit NFC、振幅 34) の実測 (#175、2026-09-07): 免許証を上から真っ直ぐ
    /// 置いて 4.5 秒保持 ×5 でゲートが開いたのは **1/5** (横から滑らせても 1/5、速い再タップは 5/5)。
    /// 開かない間は heartbeat が `amp=34/34 ph=181/181` のまま = **置き位置・向きによって振幅も
    /// 位相も動かない置き方がある**。ゲートに頼る限り「載せても数秒鳴らない」が残るので、
    /// 読み取り側 (#163 B 先行・粘着 / #167 ATQB / #169 #174 TapGate) が直った今、待機中も B を回す。
    ///
    /// **[`PollOrder::LicenseFirst`] と組んで使う。** B が無応答の待機周は F → A → B 戻しを
    /// [`nfc_sticky::FA_EVERY_CYCLES`] 周に 1 回に間引き、残りは B のまま切替 (電界断) ゼロ。
    /// FelicaFirst と組むと毎周 F → A → B で電界断 3 回/周 (#155 step 3 と同じ条件) になるので
    /// 使わない (構造では禁止しない)。固着復帰 (`TRIGGER_STUCK`) は常時トリガでは意味を失うので
    /// 丸ごと飛ばす。振幅・位相の測定と baseline の追従は続ける (heartbeat の設置診断に使う)
    AlwaysPoll,
}

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
    order: PollOrder,
    gate: PresenceGate,
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
        .spawn(move || run(i2c_port, sda_num, scl_num, order, gate, status, sink))?;
    Ok(())
}

fn run(
    i2c_port: i32,
    sda_num: i32,
    scl_num: i32,
    order: PollOrder,
    gate: PresenceGate,
    status: SharedStatus,
    mut sink: impl NfcSink,
) {
    if !init_with_retry(i2c_port, sda_num, scl_num, &status) {
        return;
    }
    // 画面を持たない機 (atoms3-timecard) では push_event が誰にも見えないので
    // シリアルにも出す。ログは USB CDC がホストに掴まれる前 (起動 1 秒以内) の
    // ぶんが落ちるので、`EVT NFC_READY` は起動後でもコンソールから
    // `LOG DUMP` で拾える (crashlog リングに載る) ことに意味がある
    log::info!("nfc: 待受開始 port={i2c_port} sda={sda_num} scl={scl_num}");
    alc_hub_common::evtlog::emit(&format!(
        "EVT NFC_READY port={i2c_port} sda={sda_num} scl={scl_num}"
    ));
    push_event(&status, "NFC 待受開始 (存在検知ゲート + F→A→B 逐次ポーリング)");
    let license_first = order == PollOrder::LicenseFirst;
    if license_first {
        // 起動マーカー。**NFC_READY より前に置かない** — それより前のコンソール出力は
        // 取りこぼされる (起動直後と es8311 dump_regs 直後の約 200ms)
        alc_hub_common::evtlog::emit("EVT NFC_POLL_ORDER LicenseFirst");
        log::info!("nfc: B 先行 + B 粘着でポーリングする (PollOrder::LicenseFirst)");
        // step 4b: 「まだ載っている」(present) は RF の応答で決める (下の touch の直前)
        alc_hub_common::evtlog::emit("EVT NFC_PRESENT_SRC rf");
    }
    let always_poll = gate == PresenceGate::AlwaysPoll;
    if always_poll {
        // 起動マーカー (同じく NFC_READY より後)
        alc_hub_common::evtlog::emit("EVT NFC_PRESENCE_GATE AlwaysPoll");
        log::info!("nfc: 存在検知をゲートに使わず常時ポーリングする (PresenceGate::AlwaysPoll)");
    }

    // 重複抑止は **debounce**「離れて N ms 経つまで、まだ同じタップ」
    // (alc_hub_core::nfc_tap、issue #103)。
    // 以前は「直前値と違えば発火 / 読めなければ直前値をクリア」のエッジ判定
    // だったが、モバイル FeliCa は応答が断続的で 20ms ポーリングが 1 回
    // 空振りしただけで直前値が消え、**1 タップで 2 回発火**していた。
    // 打刻イベント (#134) では 1 タップが別 seq の 2 行になり、サーバ側の
    // seq 冪等では防げない (別イベントが 2 つ生まれたケースのため) —
    // 1 回かざした人が出勤と退勤を同時に打つ壊れ方になるので端末側で塞ぐ。
    //
    // **判定の足場は「読めたか」ではなく「載っているか」。** カードが載ったままでも
    // 読み取りは頻繁に失敗する (実機ログ: Failed to RequestResponse /
    // SELECT EF 2F01 失敗 / deselect failed が成功の前後に出る)。とくに免許証は
    // 再読が 3.4〜4.1 秒に 1 回しか成功せず、クールダウンを毎回越えて再発火して
    // いた (2026-09-04 実機: 1 枚で 7.5 秒に 3 打刻)。そこで**存在検知が立って
    // いる間は毎周期 touch** し、読み取り失敗を「離れた」と数えない。
    //
    // gate は全系統で 1 つ。分けると「別系統で読めた 2 枚目」を 2 枚と数えられず、
    // 同じカードが読み取りの揺れで別経路に落ちたときも 1 タップ 1 回に収まらない。
    //
    // **発火は遅延確定 (issue #143)。** 読めた時点では `observe` に記録するだけで、
    // 確定窓 (DEFAULT_COMMIT_WINDOW_MS) のあいだに別キーが現れなければ `poll` が
    // Fire を返す。現れたら MultipleCards = **どちらも登録しない** (財布に 2 枚
    // 入っていると、どちらの人の打刻か決められないまま 2 人ぶん記録してしまう)
    //
    // **`observe` には各 poll を打つ直前の時刻も渡す (issue #171)。** 読み取りは同期で、
    // 免許証の APDU は S(WTX) で最大 2 秒、車検証の ISO-DEP も同じ上限。その間 touch も
    // poll も入らないので、読了時刻だけで連続性を見ると載せたままでも cooldown (1 秒) を
    // 超えて新タップ = 二重打刻になる。読めたならその読みを始めた時点で載っていた
    let mut tap_gate: TapGate<NfcEvent> = TapGate::default();
    // -2 (カード無し) は定常状態なのでログしない。未実行センチネルは i32::MIN。
    // **これは失敗ログの抑止専用** — 成功時の発火判定は license_gate が持つ
    let mut last_license_rc = i32::MIN;
    // 直近に構造 probe を打った車検証の UID と時刻 (issue #110)。同じカードを
    // 置きっぱなしにしても 10 秒に 1 回しか probe しない
    let mut last_carins: Option<(String, u64)> = None;

    // 存在検知のベースライン (-1 = 未較正、初回測定値で初期化)。
    // 振幅はカード系、位相はスマホ系 (モバイルSuica 等、振幅に出にくい) を拾う
    let mut baseline: i32 = -1;
    let mut baseline_ph: i32 = -1;
    let mut triggered_since: Option<Instant> = None;
    // トリガが立つ**直前**のベースライン。固着したときはここへ戻す。
    // **現在値を新しい基準にしてはいけない** — カードが載ったままの値を基準化すると、
    // 以後そのカードは差分を作れず**永久に見えなくなる** (#155 で実機確認)
    let mut baseline_before_trigger: Option<(i32, i32)> = None;
    /// 固着 → 巻き戻し → また固着、が続いた回数。**2 回目で現在値を採る** (#155)
    const STUCK_ROLLBACK_LIMIT: u32 = 2;
    let mut stuck_rollbacks: u32 = 0;
    let mut tick: u32 = 0;
    // LicenseFirst の B 粘着 (遷移は alc_hub_core::nfc_sticky)。cycle は計器用の周回番号
    let mut sticky = Sticky::default();
    let mut cycle: u32 = 0;
    // 計器行 (`nfc cycle=`) は rc / 粘着 / 「B が周を取ったか」/ release のどれかが前周と
    // 変わった周だけ出す。載せっぱなしや常時ポーリングの待機で毎周出すと crashlog のリングを
    // 押し流すため (tuple の中身は周末の `inst` を参照)
    let mut last_inst: Option<(i32, bool, bool, bool)> = None;

    loop {
        tick = tick.wrapping_add(1);

        // --- 保留の確定 (issue #143) ---
        // **存在検知ゲート (下の `if !triggered { … continue; }`) より前に置くこと。**
        // 下に置くと、カードが離れた周期には到達しない — 1 枚を確定窓より短く
        // かざして離したときに保留が確定せず**打刻が消える** (TapGate 単体テストは
        // 通るのに実機だけで落ちる形)。発火はこの 1 か所に集約する
        deliver(tap_gate.poll(now_ms()), &status, &mut sink);

        // --- 待機: プロトコル非依存の存在検知 (アンテナ振幅+位相) ---
        // モード切替もポーリングも行わず振幅・位相だけを見る。ベースラインは
        // 非トリガ時のみ ±1 ずつ追従させ温度ドリフトを吸収する (カードが
        // 載っている間は追従しないので、置きっぱなしでも基準が汚れない)
        let amp = unsafe { nfc_shim_measure_amplitude() };
        let ph = unsafe { nfc_shim_measure_phase() };
        let mut triggered = false;
        // 存在検知が**測定できたうえで**「載っている」と言えたか。
        // 測定失敗のフォールバック (下の else) は触らない — 測定が壊れている間
        // ずっと touch し続けると、二度と発火しなくなる。
        // **FelicaFirst ではこの present がトリガ中のベースライン凍結に由来する**ので、
        // 位相が動くとカードが離れても張り付く (実機: 読めた 9 タップ中 7 が発火せず)。
        // LicenseFirst は下 (touch の直前) で RF の応答に上書きする。FelicaFirst 側の追随は #162
        let mut present = false;
        if amp >= 0 {
            if baseline < 0 {
                baseline = amp;
            }
            if (amp - baseline).abs() >= PRESENCE_DELTA {
                triggered = true;
                present = true;
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
                present = true;
            } else {
                baseline_ph += (ph - baseline_ph).signum();
            }
        }

        if tick % 100 == 0 {
            log::info!(
                "nfc heartbeat tick={tick} amp={amp}/{baseline} ph={ph}/{baseline_ph} last_rc={last_license_rc}"
            );
        }

        // 常時ポーリング (#175): 存在検知の当たり外れを読み取りの可否に持ち込まない。
        // 上の amp/ph 判定と baseline の追従はそのまま走らせる (heartbeat の設置診断に使う)。
        // `present` は触らない — LicenseFirst は下 (touch の直前) で RF の応答に上書きする
        let triggered = always_poll || triggered;
        if !triggered {
            triggered_since = None;
            FreeRtos::delay_ms(POLL_INTERVAL_MS);
            continue;
        }
        match triggered_since {
            // 固着の保険は存在検知をゲートに使っているときだけ意味がある。常時ポーリングでは
            // 常にトリガなので、放っておくと 8 秒ごとに「固着」と判定して baseline を弄り
            // ログを出し続けるだけになる。`triggered_since` 等は None のまま凍結 (害なし)
            _ if always_poll => {}
            None => {
                triggered_since = Some(Instant::now());
                // 立ち上がりの値を控える。固着したらここへ戻す
                baseline_before_trigger = Some((baseline, baseline_ph));
            }
            Some(t0) if t0.elapsed() > TRIGGER_STUCK => {
                // 固着の抜け方は 2 段構え (#155)。**どちらか一方だけでは破綻する:**
                //
                // 1 回目は**トリガ前の値へ巻き戻す。現在値を基準にしない** —
                // カードが載ったままの値を基準化すると、**以後そのカードが
                // 見えなくなる** (実機で確認)。
                //
                // **2 回続けて固着したら現在値を採る。** 巻き戻しだけにしていたら、
                // 環境が本当にドリフトしたとき (実機: 位相の地合いが 176→181 へ移動)
                // **巻き戻した基準に永久に戻れず、8 秒ごとに固着し続けた**。
                // そのあいだ常時ポーリング状態になり、**読み取りが軒並み失敗する**
                // (`ATTRIB 失敗` / `select 失敗` が続いた)。
                // 「固着してもポーリングは回るから読める」という当初の読みは
                // **実機で否定された** (2026-09-06)。
                let rolled_back = if stuck_rollbacks < STUCK_ROLLBACK_LIMIT - 1 {
                    if let Some((b, bp)) = baseline_before_trigger {
                        baseline = b;
                        baseline_ph = bp;
                    }
                    stuck_rollbacks += 1;
                    true
                } else {
                    // 巻き戻しても直らなかった = 地合いが動いている。現在値を採る
                    baseline = amp;
                    baseline_ph = ph;
                    stuck_rollbacks = 0;
                    false
                };
                log::info!(
                    "nfc presence: トリガ固着 {TRIGGER_STUCK:?} — {} (amp={amp}/{baseline} ph={ph}/{baseline_ph})",
                    if rolled_back {
                        "ベースラインを戻す"
                    } else {
                        "戻しても直らないので現在値を基準にする"
                    }
                );
                triggered_since = None;
                baseline_before_trigger = None;
                FreeRtos::delay_ms(POLL_INTERVAL_MS);
                continue;
            }
            _ => {}
        }

        // --- 何かかざされた: F (交通系IDm、日常の主役) → A (HCE/UID) → B (免許証) ---
        // 軽い方から先に、重い方を後に試す。主要経路の交通系タップが最速になる並び。
        //
        // **★ 実測 (#155、Atom VoiceS3R): F=91ms / A=140ms / B=200ms で 1 周 448ms。**
        // ここは元々「F/A の検出は数ms」と書いてあったが、**実測と 10〜30 倍ずれていた。**
        // この 448ms が「反応まで 1 秒以上」の土台になっている
        // (F/A が何に消えているかは別 issue)。**定数を決めるときはこの実測を見ること。**
        //
        // 各 poll の**あいだにも確定窓の経過を見る** — 1 周 448ms もあるので、
        // ループ先頭でしか見ないと窓 (250ms) が閉じても最大 448ms 発火が遅れる
        // (実機で読了 373ms → 発火 911ms、うち ~290ms がこの待ち)。
        // `TapGate::poll` は「毎周期呼ぶこと」が規約なので、回数を増やすのは安全側
        let mut got = false;
        cycle = cycle.wrapping_add(1);
        // step 4b (LicenseFirst のみ): この周に実際に打った poll のどれかが応答したか。
        // `present` (tap_gate.touch = 「まだ同じタップ」) をこれで上書きする — 理由は touch の直前
        let mut rf_present = false;
        // 計器行の材料 (周末に 1 行にまとめて出す)
        let mut b_rc = i32::MIN;
        let mut fa = "-";
        let mut sticky_word = "-";
        // F → A を回すか。FelicaFirst は常に回す。LicenseFirst は B の結果と
        // (AlwaysPoll の待機周では) 周回番号の偶奇で決める — `nfc_sticky::run_fa`
        let mut run_fa = true;

        // --- B 先行 (LicenseFirst、#155 step 4) ---
        // 待機中は B モードのまま電界 ON なので、ここは切替 (電界断) ゼロで入れる
        if license_first {
            let rc = poll_license(&mut tap_gate, &mut sink, &mut last_license_rc);
            if rc == 0 {
                got = true;
            }
            let (next, release) = nfc_sticky::next(sticky, rc);
            sticky = next;
            b_rc = rc;
            rf_present = rc != nfc_sticky::RC_NO_CARD && rc != nfc_sticky::RC_NOT_READY;
            run_fa = nfc_sticky::run_fa(got, sticky.on, rf_present, always_poll, cycle);
            // skip = B が周を取った (読了 / 粘着)、idle = AlwaysPoll の待機周の間引き
            fa = if run_fa {
                "run"
            } else if got || sticky.on {
                "skip"
            } else {
                "idle"
            };
            sticky_word = release.label(sticky.on);
            deliver(tap_gate.poll(now_ms()), &status, &mut sink);
        }

        // --- F → A (→ B) ---
        // LicenseFirst で読了済み / 粘着中 / AlwaysPoll の間引き周はここを丸ごと飛ばす
        // (電界断ゼロを守る)
        if run_fa {
            let started = now_ms();
            match poll_felica_idm() {
                Ok(Some(idm)) => {
                    // ここでは発火しない — 確定窓を抜けた後に `deliver` が出す (issue #143)
                    tap_gate.observe(&idm, NfcEvent::Felica { idm: idm.clone() }, started, now_ms());
                    got = true;
                }
                // 読めなかったことを理由に状態をクリアしない (issue #103)。
                // 空振りで状態が消えることが 2 重発火の原因だった
                Ok(None) => {}
                Err(e) => log::warn!("nfc: FeliCa poll error: {e:#}"),
            }

            // 確定窓の経過チェック (F の後)。重い A/B に入る前に発火できる
            deliver(tap_gate.poll(now_ms()), &status, &mut sink);

            if !got {
                let started = now_ms();
                match poll_nfca_uid() {
                    // スマホ (HCE) のランダム UID (ISO/IEC 14443-3 §6.4.4: 4B で UID0=0x08) は
                    // **gate に載せない**。載せると同じ周で読めた FeliCa (モバイル Suica) の
                    // IDm と「別のカード 2 枚」(#143) になり、両方捨ててエラー音が鳴る
                    // (実機 2026-09-06: ATQB ゲートで HCE を活性化しなくなった直後から
                    // 確定窓 250ms に IDm と UID が揃うようになった)。打刻 ID としても
                    // 毎回変わって無意味 (#166 は on_card で弾くが、それは gate の後で遅い)
                    Ok(Some(uid)) if alc_hub_core::nfca_uid::is_random_nfca_uid(&uid) => {
                        log::info!("nfc: nfca random UID — gate に載せない");
                    }
                    Ok(Some(uid)) => {
                        // 電子車検証は Type-A + ISO14443-4 (ISO-DEP、RATS 応答あり) で
                        // 応答することを実機確認済み (issue #105)。UID が取れた時点で
                        // このカードがただの UID タグ (NTAG 等) かスマートカードかを
                        // SELECT MF で追加確認する (詳細は detect_car_inspection_a
                        // のコメント参照)。tap のたびに ISO-DEP セッション1回分の
                        // コストが乗るが、非対応カードは RATS 非対応で即座に弾かれる
                        // ため実害は小さい
                        let event = if detect_car_inspection_a() {
                            // 車検証と判定できた回だけ、カードの構造を実機で測る
                            // (issue #110)。管理番号のパースと通知は実測が出てからの別 PR
                            probe_carins_if_due(&uid, &mut last_carins, &status);
                            NfcEvent::CarInspection { uid: uid.clone() }
                        } else {
                            NfcEvent::NfcaUid { uid: uid.clone() }
                        };
                        tap_gate.observe(&uid, event, started, now_ms());
                        got = true;
                    }
                    // issue #103: 空振りで状態をクリアしない (上の FeliCa と同じ理由)
                    Ok(None) => {}
                    Err(e) => log::warn!("nfc: NFC-A poll error: {e:#}"),
                }
            }

            // 確定窓の経過チェック (A の後)。いちばん重い B に入る前に発火できる
            deliver(tap_gate.poll(now_ms()), &status, &mut sink);

            if !got && !license_first {
                if poll_license(&mut tap_gate, &mut sink, &mut last_license_rc) == 0 {
                    got = true;
                }
            }
            if license_first {
                // 周末に B モードへ戻す (PollOrder の doc 参照)。F/A のどちらで
                // 終わっていても、待機と次周先頭の B を切替ゼロにする
                unsafe { nfc_shim_prepare_mode_b() };
            }
        }

        if got {
            triggered_since = None;
            // 読めた = 固着ではない。巻き戻しの回数を数え直す
            stuck_rollbacks = 0;
            // F/A で読めた周も RF が応答している (B は rc で判定済み)
            rf_present = true;
        }

        if license_first {
            // step 4b: 「まだ載っている」は RF の応答で決める (振幅/位相は**ゲートを開く**
            // 役だけに使う。AlwaysPoll ではその役も無く、heartbeat の計器に残るだけ)。位相のベースラインはトリガ中に追従しないので、位相が動いた後は
            // 振幅/位相由来の `present` が張り付き、カードが離れている間も touch が続いて
            // 次のタップが「同じタップ」扱いで debounce に飲まれる (step 4 実機: 読めた
            // 9 タップのうち 7 が発火せず)。LicenseFirst では粘着中の B が毎周 (84ms)
            // 応答するので RF が「載っている」の確実な根拠になる。**FelicaFirst では
            // 免許証が数周に 1 回しか応答しない** (FeliCa / Type-A は毎周応答する) ので
            // 同じ手は使えない → LicenseFirst 限定。
            // 二重打刻の担保: 載ったままの -2 は実測 0/131 周、途中死は最大 1 周 (85ms) で
            // debounce の cooldown (1000ms) に吸収される
            present = rf_present;
            // 離れたことが RF で確定した周 = B (粘着なら -2 ×2 で解けた後) → F → A の
            // 1 周すべて無応答。cooldown を待たずタップを区切り、離した直後の再タップを
            // 別の打刻として受ける (#155、`TapGate::release` の doc)。B だけの周 (~180ms)
            // の 1 回の -2 では解かない — 電界の縁で -2 → 0 と揺れる免許証が新タップになる
            // AlwaysPoll では待機中もこの周が毎回来るが、`release` は区切るものが無ければ
            // no-op で false を返す。計器行の `released=gate` は true の周だけ
            let released_gate = !rf_present && run_fa && tap_gate.release();
            // 待機中 (AlwaysPoll) に流さないための正規化: -1 (未初期化 / バッファ不足) は
            // -2 と同じ「無応答」に寄せ、fa の run/idle の交互 (待機周で毎周入れ替わる) は
            // 「B が周を取ったか」に置き換える。#169 でタップが区切られた周は必ず出す
            let rc_norm = if b_rc == nfc_sticky::RC_NOT_READY {
                nfc_sticky::RC_NO_CARD
            } else {
                b_rc
            };
            let inst = (rc_norm, sticky.on, got || sticky.on, released_gate);
            if last_inst != Some(inst) {
                last_inst = Some(inst);
                log::info!(
                    "nfc cycle={cycle} order=B rc={b_rc} sticky={sticky_word} cycles={} misses={} fa={fa} present=rf:{}{}",
                    sticky.cycles,
                    sticky.misses,
                    u8::from(rf_present),
                    if released_gate { " released=gate" } else { "" }
                );
            }
        }

        // カードが載っている間は「まだ同じタップ」。**読み取り (observe) の後に
        // 置くこと** — 前に置くと、離れていた時間が touch で消えて再タップが
        // 抑止される。確定窓は touch では延びない (延ばすと載せっぱなしで
        // 窓が永久に閉じず 1 回も発火しなくなる — nfc_tap のモジュール doc)
        if present {
            tap_gate.touch(now_ms());
        }

        // 確定窓の経過チェック (B の後)。読めた周のうちに窓が閉じていれば
        // ここで発火し、次の周 (448ms 先) まで待たされない
        deliver(tap_gate.poll(now_ms()), &status, &mut sink);

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

// ===== 電子車検証 (DESFire) の構造 probe (issue #110) =====
//
// PIN 不要で読める「電子車検証管理番号」を取るのが最終目的だが、カードの構造を
// まだ誰も実機で測っていない。測りたい未知数は 3 つだけ:
//   1. AID は F33011 (登録車) / F33018 (軽) か、GetApplicationIDs が返す別の値か
//   2. File 03 が実在し、通信モードが平文・アクセス権の Read が free か
//   3. ReadData が返す中身の形 (`車両ID / 管理番号` の UTF-8 `/` 区切りか)
//
// **測定結果の出口**は 3 つに分ける。実機はオフラインで、戻ってきたら OTA して
// 遠隔ログで読む段取りなので、EVT 行が主の出口になる:
//   - evtlog::emit  … 識別子を含まない行だけ (crashlog のリングに残る = 遠隔で読める)
//   - log::info!    … 生の応答 hex (シリアルのみ。リングには載らない)
//   - push_event    … 1 行だけの要約 (値は出さない)
//
// ⚠ log::info! の生 hex には**実在する車両の管理番号が入る**。この repo は public
// なので、その出力を issue / PR の本文やコメントに貼らないこと。実機の話を書く
// 必要があるなら EVT 行 (識別子を含まない側) だけを引用する。

/// 同じ車検証に probe を打ち直す間隔。置きっぱなしのカードで毎周 12 往復させない
const CARINS_PROBE_INTERVAL_MS: u64 = 10_000;
/// probe 全体の上限。1 往復の上限 (shim 側 1500ms) は 1 回ぶんの値でしかなく、
/// 手順は最大 12 往復あるので全体にも上限が要る — 無いと最悪 18 秒かかり、
/// その間 TapGate の touch/poll が止まって watchdog にも効く
const CARINS_PROBE_DEADLINE_MS: u64 = 2_000;
/// GetFileSettings を掛けるファイル数の上限
const CARINS_MAX_FILES: usize = 8;
/// 1 フレームの受信バッファ。NFC スレッドは 8KB なので大きくしない
const CARINS_RX_CAP: usize = 264;

/// Type-A ISO-DEP セッションの RAII ガード。`Drop` で必ず閉じる —
/// DESFire の「選択中アプリ」は活性化セッションに紐づくので、probe の途中で
/// 抜けてもカードを ACTIVE のまま残さない
struct IsoDepA;

impl IsoDepA {
    /// セッションを開く。`Ok((guard, ATS のバイト数))` / `Err(shim の rc)`
    fn open(ats: &mut [u8]) -> Result<(Self, usize), i32> {
        let n = unsafe { nfc_shim_isodep_a_open(ats.as_mut_ptr(), ats.len() as i32) };
        if n < 0 {
            return Err(n);
        }
        Ok((IsoDepA, n as usize))
    }

    /// APDU を 1 往復。`Ok(受信バイト数)` / `Err(shim の rc)`
    fn transceive(&self, cmd: &[u8], out: &mut [u8]) -> Result<usize, i32> {
        let n = unsafe {
            nfc_shim_isodep_a_transceive(
                cmd.as_ptr(),
                cmd.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if n < 0 {
            return Err(n);
        }
        Ok(n as usize)
    }
}

impl Drop for IsoDepA {
    fn drop(&mut self) {
        unsafe { nfc_shim_isodep_a_close() };
    }
}

/// EVT 行に出す rc トークン (DESFire の `91 xx` の xx、または shim の rc)
fn carins_rc(st: desfire::Status) -> String {
    match st {
        desfire::Status::Ok => "00".to_string(),
        desfire::Status::MoreFrames => "AF".to_string(),
        desfire::Status::Error(x) => format!("{x:02X}"),
        desfire::Status::NotDesfire([a, b]) => format!("nd{a:02X}{b:02X}"),
        desfire::Status::TooShort => "short".to_string(),
    }
}

/// 1 コマンドを送り、`91 AF` で切れていれば `90 AF` を送って継ぎ足す
/// (`IsoDEP::transceiveAPDU` は `61xx`/`6Cxx` しか追従しない)。
/// `Ok((データ部, フレーム数))` / `Err(rc トークン)`
fn carins_exchange(
    session: &IsoDepA,
    cmd: &[u8],
    deadline: u64,
    step: &str,
) -> Result<(Vec<u8>, u32), String> {
    let mut out = [0u8; CARINS_RX_CAP];
    let mut acc: Vec<u8> = Vec::new();
    let mut frames = 0u32;
    let mut next = cmd.to_vec();
    loop {
        if now_ms() > deadline {
            evtlog::emit(&format!("EVT CARINS_DEADLINE step={step}"));
            return Err("deadline".to_string());
        }
        let n = match session.transceive(&next, &mut out) {
            Ok(n) => n,
            Err(rc) => return Err(format!("s{rc}")),
        };
        frames += 1;
        // 生ダンプはシリアルのみ (リングには載らない)。⚠ 本文に貼らないこと
        log::info!("carins probe: {step} rx={}", desfire::hex_upper(&out[..n]));
        match desfire::accumulate(&mut acc, &out[..n]) {
            desfire::ReadStep::Done(data) => return Ok((data, frames)),
            desfire::ReadStep::NeedMore => next = desfire::additional_frame(),
            desfire::ReadStep::Failed(st) => return Err(carins_rc(st)),
        }
    }
}

/// `ReadData` の中身を**値を出さずに**形だけ書く。UTF-8 として読めたら
/// `/` 区切りの各フィールドを「文字クラス (d=数字 / a=英数 / x=その他) + 長さ」で、
/// 読めなければ非 0 バイト数だけを返す (先頭バイトも出さない)
fn carins_shape(data: &[u8]) -> String {
    let end = data
        .iter()
        .rposition(|&b| b != 0x00 && b != 0xFF)
        .map_or(0, |i| i + 1);
    match core::str::from_utf8(&data[..end]) {
        Ok(s) => {
            let fields: Vec<String> = s
                .split('/')
                .map(|f| {
                    let f = f.trim();
                    let class = if f.chars().all(|c| c.is_ascii_digit()) {
                        'd'
                    } else if f.chars().all(|c| c.is_ascii_alphanumeric()) {
                        'a'
                    } else {
                        'x'
                    };
                    format!("{class}{}", f.chars().count())
                })
                .collect();
            format!("utf8=1 fields={}", fields.join("/"))
        }
        Err(_) => format!(
            "utf8=0 nonzero={}",
            data.iter().filter(|&&b| b != 0).count()
        ),
    }
}

/// 同じ UID には [`CARINS_PROBE_INTERVAL_MS`] に 1 回だけ probe を打つ
fn probe_carins_if_due(uid: &str, last: &mut Option<(String, u64)>, status: &SharedStatus) {
    let now = now_ms();
    if let Some((prev_uid, at)) = last.as_ref() {
        if prev_uid == uid && now.saturating_sub(*at) < CARINS_PROBE_INTERVAL_MS {
            return;
        }
    }
    *last = Some((uid.to_string(), now));
    probe_carins(status);
}

/// 構造 probe 本体。手順の分岐 (AID の候補 / ファイルの探索) は実測が出れば
/// 定数に畳まれて消えるものなので、純粋関数へ切り出さず直線で書く
fn probe_carins(status: &SharedStatus) {
    let deadline = now_ms() + CARINS_PROBE_DEADLINE_MS;

    let mut ats = [0u8; 64];
    let (session, ats_len) = match IsoDepA::open(&mut ats) {
        Ok(v) => v,
        Err(rc) => {
            evtlog::emit(&format!("EVT CARINS_PROBE rc=s{rc}"));
            return;
        }
    };
    evtlog::emit(&format!(
        "EVT CARINS_PROBE ats={}",
        desfire::hex_upper(&ats[..ats_len])
    ));
    push_event(status, "電子車検証 構造 probe");

    // 1) AID 一覧。取れなければ候補 2 つを順に試す
    let aids = match carins_exchange(&session, &desfire::get_application_ids(), deadline, "apps") {
        Ok((data, _)) => match desfire::parse_application_ids(&data) {
            Ok(v) => {
                let list: Vec<String> = v.iter().map(|a| desfire::hex_upper(a)).collect();
                evtlog::emit(&format!("EVT CARINS_APPS rc=00 aids={}", list.join(",")));
                v
            }
            Err(e) => {
                evtlog::emit(&format!("EVT CARINS_APPS rc=00 aids=err{e:?}"));
                Vec::new()
            }
        },
        Err(rc) => {
            evtlog::emit(&format!("EVT CARINS_APPS rc={rc}"));
            Vec::new()
        }
    };
    if now_ms() > deadline {
        return;
    }

    // 2) 選ぶ AID。一覧に既知の候補があればそれ、無ければ一覧の先頭、
    //    一覧が取れていなければ F33011 → F33018 の順に試す
    let candidates: Vec<[u8; 3]> = if aids.contains(&desfire::AID_REGISTERED) {
        vec![desfire::AID_REGISTERED]
    } else if aids.contains(&desfire::AID_KEI) {
        vec![desfire::AID_KEI]
    } else if let Some(first) = aids.first() {
        vec![*first]
    } else {
        vec![desfire::AID_REGISTERED, desfire::AID_KEI]
    };

    let mut selected = false;
    for aid in candidates {
        let hex = desfire::hex_upper(&aid);
        match carins_exchange(&session, &desfire::select_application(aid), deadline, "select") {
            Ok(_) => {
                evtlog::emit(&format!("EVT CARINS_SELECT aid={hex} rc=00"));
                selected = true;
                break;
            }
            Err(rc) => evtlog::emit(&format!("EVT CARINS_SELECT aid={hex} rc={rc}")),
        }
        if now_ms() > deadline {
            return;
        }
    }
    if !selected {
        return;
    }

    // 3) ファイル一覧
    let files = match carins_exchange(&session, &desfire::get_file_ids(), deadline, "files") {
        Ok((data, _)) => {
            let ids = desfire::parse_file_ids(&data);
            let list: Vec<String> = ids.iter().map(|f| format!("{f:02X}")).collect();
            evtlog::emit(&format!("EVT CARINS_FILES rc=00 ids={}", list.join(",")));
            ids
        }
        Err(rc) => {
            evtlog::emit(&format!("EVT CARINS_FILES rc={rc}"));
            return;
        }
    };

    // 4) 各ファイルの設定。平文かつ free read の最初のものを控えておく
    let mut plain_free: Option<u8> = None;
    for &f in files.iter().take(CARINS_MAX_FILES) {
        if now_ms() > deadline {
            evtlog::emit("EVT CARINS_DEADLINE step=settings");
            return;
        }
        match carins_exchange(&session, &desfire::get_file_settings(f), deadline, "settings") {
            Ok((data, _)) => match desfire::parse_file_settings(&data) {
                Ok(fs) => {
                    evtlog::emit(&format!(
                        "EVT CARINS_FILE file={f:02X} rc=00 type={:02X} comm={:02X} rights={:04X} size={}",
                        fs.file_type,
                        fs.comm_mode(),
                        fs.access_rights,
                        fs.file_size
                    ));
                    if plain_free.is_none() && fs.is_plain() && fs.is_free_read() {
                        plain_free = Some(f);
                    }
                }
                Err(e) => evtlog::emit(&format!("EVT CARINS_FILE file={f:02X} rc=00 parse={e:?}")),
            },
            Err(rc) => evtlog::emit(&format!("EVT CARINS_FILE file={f:02X} rc={rc}")),
        }
    }

    // 5) 03 があればそれ、無ければ平文 free read の最初のファイルを読む
    let target = if files.contains(&desfire::FILE_NO_MGMT) {
        Some(desfire::FILE_NO_MGMT)
    } else {
        plain_free
    };
    let Some(file_no) = target else {
        return;
    };
    if now_ms() > deadline {
        evtlog::emit("EVT CARINS_DEADLINE step=read");
        return;
    }
    match carins_exchange(&session, &desfire::read_data(file_no, 0, 0), deadline, "read") {
        Ok((data, frames)) => {
            // ⚠ この 1 行に実在する車両の管理番号が入る。シリアルのみ・本文に貼らない
            log::info!(
                "carins probe: read file={file_no:02X} hex={}",
                desfire::hex_upper(&data)
            );
            evtlog::emit(&format!(
                "EVT CARINS_READ file={file_no:02X} rc=00 len={} frames={frames} {}",
                data.len(),
                carins_shape(&data)
            ));
        }
        Err(rc) => evtlog::emit(&format!("EVT CARINS_READ file={file_no:02X} rc={rc}")),
    }
}

/// B (免許証) を 1 回読み、読めたら gate に載せる。戻り値は shim の rc (0 = 読了)。
/// F/A/B の順序に依らず B の後始末は同じなので、[`PollOrder`] の両方から呼ぶ
fn poll_license(
    tap_gate: &mut TapGate<NfcEvent>,
    sink: &mut impl NfcSink,
    last_license_rc: &mut i32,
) -> i32 {
    // 読み始めの時刻。S(WTX) で読了が 2 秒先になっても、載っていた証拠はここ (issue #171)
    let started = now_ms();
    let (rc, issue, expiry) = read_license_expiry();
    if rc == 0 {
        // 免許証も同じ gate に載せる (issue #103)。key は交付日 +
        // 有効期限 = alc-app タブレットが使う employees.nfc_id と同じ 16 桁
        let key = format!("{issue}{expiry}");
        tap_gate.observe(&key, NfcEvent::License { issue, expiry }, started, now_ms());
    } else if rc != nfc_sticky::RC_NO_CARD {
        if rc != nfc_sticky::RC_NOT_READY {
            // カードは応答したが読了しなかった (途中死 / 免許証以外の Type-B)。読み取りの
            // あいだ (S(WTX) で最大 2 秒) は poll も touch も入らないので、この直後の
            // `poll` の expire が「cooldown のあいだ何も見ていない」と last を消す前に
            // 「まだ載っている」を記録する (issue #171)。読了なら observe が since で同じことをする
            tap_gate.touch(now_ms());
        }
        if rc != *last_license_rc {
            // 途中死はカード引き抜き等でも出る
            log::warn!("nfc: 免許証 読み取り失敗 rc={rc} ({})", license_rc_reason(rc));
            sink.on_event(&NfcEvent::ReadFailed { rc });
        }
    }
    *last_license_rc = rc;
    rc
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
        -8 => "免許証以外の Type-B (ATQB の FWI)",
        _ => "不明なエラーコード",
    }
}

/// 確定した [`TapOutcome`] を外へ出す **唯一の口** (issue #143)。
///
/// 読み取り 4 経路それぞれが `sink.on_event` を呼んでいた形をここへ集約した。
/// 分散していると、遅延確定 (確定窓の経過待ち) を挟んだときに「どの経路が
/// 発火済みか」が経路の数だけ増えて追えなくなる。
///
/// `push_event` はイベントログ (UI/WS) 行のみで serial には出ないので、
/// `log::info!` を並置して scripts/nfc_serial_beep.py (COM 監視、既定
/// `--match "NFC|免許|IDm"`) で検知音を鳴らせるようにする (issue #101)。
fn deliver(outcome: TapOutcome<NfcEvent>, status: &SharedStatus, sink: &mut impl NfcSink) {
    let event = match outcome {
        TapOutcome::Idle => return,
        TapOutcome::MultipleCards => {
            // 端末内で完結させる (サーバへは送らない)。受け手が LED/ブザーで出す
            log::warn!(
                "nfc: 確定窓 {DEFAULT_COMMIT_WINDOW_MS}ms に 2 枚 — どちらも登録しない (issue #143)"
            );
            push_event(status, "カードが 2 枚 — 1 枚だけかざしてください");
            sink.on_event(&NfcEvent::MultipleCards);
            return;
        }
        TapOutcome::Fire(event) => event,
    };
    match &event {
        NfcEvent::Felica { idm } => {
            log::info!("NFC IDm={idm}");
            push_event(status, &format!("NFC IDm={idm}"));
        }
        NfcEvent::CarInspection { uid } => {
            log::info!("電子車検証 検知 (UID={uid})");
            push_event(status, "電子車検証 検知");
        }
        NfcEvent::NfcaUid { uid } => {
            log::info!("NFC-A UID={uid}");
            push_event(status, &format!("NFC-A UID={uid}"));
        }
        NfcEvent::License { issue, expiry } => {
            log::info!("免許証 交付 {issue} 期限 {expiry}");
            push_event(status, &format!("免許証 交付 {issue} 期限 {expiry}"));
            // ホストにも通知 (画面遷移は UI が判断する)
            println!("EVT NFC_LICENSE issue={issue} expiry={expiry}");
        }
        // gate を通らないので Fire では来ない (ReadFailed は読み取りループが直接出す)
        NfcEvent::ReadFailed { .. } | NfcEvent::MultipleCards => {}
    }
    sink.on_event(&event);
}

fn push_event(status: &SharedStatus, line: &str) {
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), line);
    }
}
