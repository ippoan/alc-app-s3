//! 指静脈モジュール (Waveshare Finger Vein Scanner Module) の UART ドライバ
//! (Vein Station、ippoan/vein-match#20)。
//!
//! 手順 (接続 → 作業用 ID に 1 回登録 → 登録データの分割読み → 削除) とパケットの検査は
//! `alc_hub_core::vein` (host test 付き) が持ち、ここは次の 4 つだけ:
//!
//! - UART を開き、[`alc_hub_core::vein::Port`] として渡す
//! - `VEIN CAPTURE` を受けて読み取る専用スレッド。結果は 1 行
//!   (`VEIN CHARA <hex>` / `ERR VEIN <reason>`) でホストへ出す
//! - `VEIN TEST ENROLL|VERIFY|CLEAR <id>` / `VEIN TEST CAPACITY` / `VEIN TEST SYSINFO` / `VEIN TEST IMAGE` (試験用): モジュール自身の登録と
//!   照合で撮像を確かめる診断。同じ読み取りスレッドで直列に捌く (結果は `VEIN TEST …` / `ERR VEIN <reason>`)
//! - 本番の口 `VEIN ENROLL <id>` / `VEIN IDENTIFY` / `VEIN SYNC WRITE|CLEAR …` (ippoan/alc-app#436 の
//!   M-b): 手順と行は `alc_hub_core::vein::run`。同じスレッドで直列に捌き、返った行をそのまま出す。
//!   `SYNC WRITE` の組み立て中のデータはこのスレッドが持つ。**記録を出すのは `VEIN RECORD` 行だけ**
//!   (受けたデータ・読んだ記録はログに出さない)
//! - `VEIN SAY <x>` を再生スレッド (speaker) へ取り次ぐ
//!
//! **指の見張り** (ippoan/alc-app#436): 読み取りスレッドは依頼が無い回だけ指の有無を
//! 問い合わせ ([`proto::FingerWatch`])、「無い → 有る」に変わったときに `EVT VEIN FINGER` を
//! 1 行出す。照合 (`VEIN CAPTURE`) を始めるのは画面。UART に触るのはこのスレッドだけなので、
//! 読み取り中は問い合わせない (直列)。非対応 (`01 10`) なら `EVT VEIN FINGER UNSUPPORTED`
//! を 1 回出して見張りをやめる
//!
//! **案内音声は読み取りと切り離してある** — 何をいつ鳴らすか (登録の 2 回読み・照合の
//! 失敗時など) はホスト (alc-app) が決める。`VEIN SAY` は読み取り中でも
//! すぐ鳴る (読み取りスレッドの列に並ばない)。例外は「離して」のビープと撮り直しの
//! 「もう一度置いてください」: 撮れた瞬間と撮り直しはモジュールの途中経過 (0x21 /
//! `01 11`・`01 09`) でしか分からないので、端末が鳴らす (2026-10-07 実機)。
//!
//! FC-1200 の [`crate::rs232`] とは別物 (あちらは UART1 固定で FC-1200 の
//! 行解析を持つ)。UART を開く十数行は共通化していない — rs232 の経路を
//! 触らないため。

use std::io::Write;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use esp_idf_svc::hal::{
    delay::TickType,
    gpio::{AnyIOPin, InputPin, OutputPin},
    uart::{config::Config as UartConfig, Uart, UartDriver},
    units::Hertz,
};

use alc_hub_core::vein::{self as proto, Port, Timeouts, VeinError, VeinVoice};

use crate::speaker::Sound;

/// モジュールの UART 速度 (57600bps 8N1。Waveshare Wiki / vein-base README)
pub const BAUD: u32 = 57_600;

/// console から読み取りスレッドへの依頼
enum Request {
    /// `VEIN CAPTURE`
    Capture,
    /// `VEIN FINGER?`
    Finger,
    /// `VEIN TEST …` (試験用。指の見張りと同じ UART なので同じ列で直列に捌く)
    Test(proto::TestCommand),
    /// 本番の口 (同じく直列。`SYNC WRITE` のデータは Debug に出ない型)
    Run(proto::Command),
}

/// console から読み取りスレッドと再生スレッドへの口。clone して配る
#[derive(Clone)]
pub struct Link {
    capture_tx: Sender<Request>,
    /// 再生スレッドの送信口。音は UART より後に立つので、立ったら
    /// [`Link::set_speaker`] で入れる (初期化に失敗したら空のまま)
    speaker: Arc<OnceLock<Sender<Sound>>>,
}

impl Link {
    /// `VEIN CAPTURE`。結果は読み取りスレッドが後から 1 行で出す。
    /// 続けて届いた分は順番に読む (同時には読まない)
    pub fn capture(&self) {
        if self.capture_tx.send(Request::Capture).is_err() {
            // 読み取りスレッドが居ない = モジュールへ届かない
            println!("{}", proto::err_line(VeinError::NoModule));
        }
    }

    /// `VEIN FINGER?`。指の有無を 1 回問い合わせる。結果は読み取りスレッドが後から 1 行で出す
    pub fn finger_query(&self) {
        if self.capture_tx.send(Request::Finger).is_err() {
            println!("{}", proto::err_line(VeinError::NoModule));
        }
    }

    /// `VEIN TEST …`。結果は読み取りスレッドが後から 1 行で出す
    pub fn test(&self, cmd: proto::TestCommand) {
        if self.capture_tx.send(Request::Test(cmd)).is_err() {
            println!("{}", proto::err_line(VeinError::NoModule));
        }
    }

    /// 本番の口 (`VEIN ENROLL` / `VEIN IDENTIFY` / `VEIN SYNC …`)。結果は読み取りスレッドが後から出す
    pub fn run(&self, cmd: proto::Command) {
        if self.capture_tx.send(Request::Run(cmd)).is_err() {
            println!("{}", proto::err_line(VeinError::NoModule));
        }
    }

    /// `VEIN SAY <x>`。登録完了 (`ENROLLED`) は既存の「登録完了しました」
    pub fn say(&self, voice: VeinVoice) {
        let sound = match voice {
            VeinVoice::Place => Sound::VeinPlace,
            VeinVoice::Again => Sound::VeinAgain,
            VeinVoice::Enrolled => Sound::Registered,
            VeinVoice::Failed => Sound::VeinFailed,
        };
        match self.speaker.get().map(|tx| tx.send(sound)) {
            Some(Ok(())) => println!("{}", proto::say_ok_line(voice)),
            // 起動時にスピーカーの初期化に失敗した (`EVT SPEAKER_NG` が出ている)
            _ => println!("ERR VEIN NO_SPEAKER"),
        }
    }

    /// 再生スレッドが立ったら入れる (2 回目以降は無視)
    pub fn set_speaker(&self, tx: Sender<Sound>) {
        let _ = self.speaker.set(tx);
    }
}

/// UART を [`Port`] に見せる
struct UartPort(UartDriver<'static>);

impl Port for UartPort {
    fn send(&mut self, bytes: &[u8]) -> bool {
        // uart_write_bytes は TX バッファへ全部入れるまで戻らない
        matches!(self.0.write(bytes), Ok(n) if n == bytes.len())
    }

    fn discard_input(&mut self) {
        let _ = self.0.clear_rx();
    }

    fn recv(&mut self, buf: &mut [u8], timeout_ms: u32) -> usize {
        self.0
            .read(buf, TickType::new_millis(u64::from(timeout_ms)).ticks())
            .unwrap_or(0)
    }

    fn pause(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
}

/// UART を開いて読み取りスレッドを立てる。ピンは呼び出し側 (main.rs) が
/// 1 か所で決める (基盤が未発注で変わりうるため)
pub fn start(
    uart: impl Uart + 'static,
    tx_pin: impl OutputPin + 'static,
    rx_pin: impl InputPin + 'static,
) -> Result<Link> {
    let cfg = UartConfig::new().baudrate(Hertz(BAUD));
    let driver = UartDriver::new(
        uart,
        tx_pin,
        rx_pin,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &cfg,
    )?;
    let (capture_tx, capture_rx) = mpsc::channel::<Request>();
    // 再生スレッドの送信口。読み取りスレッドも「離して」のビープに使う
    let speaker: Arc<OnceLock<Sender<Sound>>> = Arc::new(OnceLock::new());
    let beeper = Arc::clone(&speaker);

    // NVS も flash も触らないので PSRAM スタックでよい。登録データ (最大 8KB) と
    // 復号した記録・16 進の行はヒープに置く。スタックは 32KB: 取り出し
    // (extract_chara) が AES (ソフト実装) の鍵展開と復号の作業領域、LZMA の層が
    // 付いていればその復号器、取り出した特徴量 (0x448 × 2 の写し) をスタックに置く。
    // 8KB では UART の手順の分と合わせて余裕が無い。PSRAM なので内部 RAM は減らない
    crate::task::name_next_psram(c"vein", 32 * 1024);
    std::thread::Builder::new()
        .name("vein".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let mut port = UartPort(driver);
            let mut watch = proto::FingerWatch::new();
            // 非対応と分かったら見張りをやめる (`VEIN FINGER?` と読み取りは受け続ける)
            let mut watching = true;
            let mut was_failing = false;
            // `VEIN SYNC WRITE` で組み立て中の登録データ (ヒープ。最大 8KB)
            let mut assembling: Option<proto::TemplateAssembler> = None;
            loop {
                let req = match capture_rx
                    .recv_timeout(Duration::from_millis(u64::from(watch.interval_ms())))
                {
                    Ok(req) => req,
                    Err(RecvTimeoutError::Timeout) => {
                        // 依頼が無い回だけ問い合わせる
                        if watching {
                            match watch.poll(&mut port, &Timeouts::DEFAULT) {
                                proto::WatchPoll::Idle => {}
                                proto::WatchPoll::Finger => {
                                    alc_hub_common::evtlog::emit("EVT VEIN FINGER")
                                }
                                proto::WatchPoll::Unsupported => {
                                    watching = false;
                                    log::warn!(
                                        "vein: 指の状態コマンドが非対応 (01 10)。見張りをやめる"
                                    );
                                    alc_hub_common::evtlog::emit("EVT VEIN FINGER UNSUPPORTED");
                                }
                            }
                            // ログは状態が変わったときだけ
                            if watch.is_failing() != was_failing {
                                was_failing = watch.is_failing();
                                if was_failing {
                                    log::warn!("vein: 指の問い合わせが続けて失敗。間隔を延ばす");
                                } else {
                                    log::info!("vein: 指の問い合わせが戻った");
                                }
                            }
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                if let Request::Finger = req {
                    let line = match proto::connect(&mut port, &Timeouts::DEFAULT) {
                        Ok(()) => {
                            proto::finger_line(proto::finger_state(&mut port, &Timeouts::DEFAULT))
                        }
                        Err(e) => proto::err_line(e),
                    };
                    emit_whole_line(&line);
                    continue;
                }
                let mut on_prompt = |p: proto::Prompt| {
                    // 「離して」はビープ、撮り直しと登録の置き直しは「もう一度置いてください」。
                    // どれもモジュールの途中経過 (置き直しは指が離れたこと) でしか分からないので端末が鳴らす
                    let sound = match p {
                        proto::Prompt::Release => Some(Sound::BeepOk),
                        proto::Prompt::Retry(code) => {
                            // 画面が「もう一度置いてください」を出し、次の指を待つ時間を延ばすのに使う。
                            // 音だけだと、撮り直しの指待ち (最長 15 秒) の間に画面が何も言えない。
                            // 理由コード (11 = 静脈が写らない / 09 = 同じ指でない) も載せる — 毎回撮り直しに
                            // なるときの切り分けに要る (生体データではない)
                            alc_hub_common::evtlog::emit(&format!("EVT VEIN AGAIN {code:02X}"));
                            Some(Sound::VeinAgain)
                        }
                        proto::Prompt::Replace(n) => {
                            // 画面が文言と進み具合に使う (生体データなし)
                            alc_hub_common::evtlog::emit(&format!("EVT VEIN PLACE {n}"));
                            Some(Sound::VeinAgain)
                        }
                        proto::Prompt::Place => None,
                    };
                    if let (Some(s), Some(tx)) = (sound, beeper.get()) {
                        let _ = tx.send(s);
                    }
                };
                if let Request::Test(cmd) = req {
                    let t = &Timeouts::DEFAULT;
                    // 生画像 (IMAGE) は何十行にもなるので、どの試験も行の列で返す
                    let one = |line: String| vec![line];
                    let lines = match cmd {
                        proto::TestCommand::Enroll(id) => proto::test_enroll(&mut port, t, id, &mut on_prompt)
                            .map(|()| one(proto::test_enrolled_line(id))),
                        proto::TestCommand::Verify(id) => proto::test_verify(&mut port, t, id, &mut on_prompt)
                            .map(|data| one(proto::test_verify_line(id, &data))),
                        proto::TestCommand::Clear(id) => {
                            proto::clear(&mut port, t, id).map(|()| one(proto::test_cleared_line(id)))
                        }
                        proto::TestCommand::Capacity => proto::test_capacity(&mut port, t)
                            .map(|(users, max)| one(proto::test_capacity_line(users, max))),
                        proto::TestCommand::SysInfo => {
                            proto::test_sysinfo(&mut port, t).map(|data| one(proto::test_sysinfo_line(&data)))
                        }
                        proto::TestCommand::Image => proto::test_image(&mut port, t, &mut on_prompt)
                            .map(|(w, h, img)| proto::image_lines(w, h, &img)),
                    }
                    .unwrap_or_else(|e| one(proto::err_line(e)));
                    for line in &lines {
                        emit_whole_line(line);
                    }
                    // モジュール側の状態が変わったので、接続し直して「無い」を見るまで見張りは出さない
                    watch.after_capture();
                    continue;
                }
                if let Request::Run(cmd) = req {
                    // SYNC WRITE の途中の塊だけは UART に触れない。ほかはモジュールの状態が変わりうる
                    let touches = !matches!(cmd, proto::Command::SyncWrite(proto::WriteCommand::Chunk { .. }));
                    // 行は 1 行ずつ 1 回の write で出す (VEIN RECORD は 1 行 2 千文字を超える)。
                    // 受けたデータ・読んだ記録はここから先ログに出さない
                    for line in proto::run(cmd, &mut assembling, &mut port, &Timeouts::DEFAULT, &mut on_prompt) {
                        emit_whole_line(&line);
                    }
                    if touches {
                        watch.after_capture();
                    }
                    continue;
                }
                // 登録データ (1:N のテンプレート) から 0xBDBD の特徴量 1 件を取り出して送る
                let got = proto::capture(&mut port, &Timeouts::DEFAULT, &mut on_prompt)
                    .and_then(|enroll| {
                        log::info!("vein: 登録データ {} バイト", enroll.len());
                        proto::extract_chara(&enroll)
                    });
                let line = match got {
                    Ok(chara) => {
                        log::info!("vein: 特徴量 {} バイト", chara.len());
                        proto::chara_line(&chara)
                    }
                    Err(e) => {
                        log::warn!("vein: 読み取り失敗 {e:?}");
                        proto::err_line(e)
                    }
                };
                emit_whole_line(&line);
                // 撮った直後の同じ指で次の合図が出ないよう、離れるまで待つ (接続も張り直す)
                watch.after_capture();
            }
        })?;
    Ok(Link {
        capture_tx,
        speaker,
    })
}

/// 行頭から 1 行を **1 回の write で**出す。
///
/// `VEIN CHARA` / `VEIN RECORD` は 2 千文字を超える (0x448 バイト = 2192 文字)。`println!` だと Rust の stdout
/// (LineWriter、1KB) が「前置き」「本文」「改行」を別々の write に分けることが
/// あり、その間に ESP-IDF のログ (C 側、同じ USB Serial/JTAG) が割り込むと
/// 行が割れる。VFS の write は 1 回の呼び出しの間ロックを持つので、1 本に
/// まとめれば割り込まれない。行頭の改行は [`alc_hub_common::hostout`] と同じ理由
fn emit_whole_line(line: &str) {
    let mut buf = String::with_capacity(line.len() + 2);
    buf.push('\n');
    buf.push_str(line);
    buf.push('\n');
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}
