//! 指静脈モジュール (Waveshare Finger Vein Scanner Module) の UART ドライバ
//! (Vein Station、ippoan/vein-match#20)。
//!
//! 手順 (接続 → GET_CHARA → READ_DATA の分割読み) とパケットの検査は
//! `alc_hub_core::vein` (host test 付き) が持ち、ここは次の 3 つだけ:
//!
//! - UART を開き、[`alc_hub_core::vein::Port`] として渡す
//! - `VEIN CAPTURE` を受けて読み取る専用スレッド。結果は 1 行
//!   (`VEIN CHARA <hex>` / `ERR VEIN <reason>`) でホストへ出す
//! - `VEIN SAY <x>` を再生スレッド (speaker) へ取り次ぐ
//!
//! **音は読み取りと切り離してある** — 何をいつ鳴らすか (登録の 2 回読み・照合の
//! 失敗時など) はホスト (alc-app) が決める。`VEIN SAY` は読み取り中でも
//! すぐ鳴る (読み取りスレッドの列に並ばない)。
//!
//! FC-1200 の [`crate::rs232`] とは別物 (あちらは UART1 固定で FC-1200 の
//! 行解析を持つ)。UART を開く十数行は共通化していない — rs232 の経路を
//! 触らないため。

use std::io::Write;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};

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

/// console から読み取りスレッドと再生スレッドへの口。clone して配る
#[derive(Clone)]
pub struct Link {
    capture_tx: Sender<()>,
    /// 再生スレッドの送信口。音は UART より後に立つので、立ったら
    /// [`Link::set_speaker`] で入れる (初期化に失敗したら空のまま)
    speaker: Arc<OnceLock<Sender<Sound>>>,
}

impl Link {
    /// `VEIN CAPTURE`。結果は読み取りスレッドが後から 1 行で出す。
    /// 続けて届いた分は順番に読む (同時には読まない)
    pub fn capture(&self) {
        if self.capture_tx.send(()).is_err() {
            // 読み取りスレッドが居ない = モジュールへ届かない
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
    let (capture_tx, capture_rx) = mpsc::channel::<()>();

    // NVS も flash も触らないので PSRAM スタックでよい。特徴量 (最大 2KB) と
    // 16 進の行 (最大 4KB) はヒープに置く
    crate::task::name_next_psram(c"vein", 8 * 1024);
    std::thread::Builder::new()
        .name("vein".into())
        .stack_size(8 * 1024)
        .spawn(move || {
            let mut port = UartPort(driver);
            while capture_rx.recv().is_ok() {
                let line = match proto::capture(&mut port, &Timeouts::DEFAULT) {
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
            }
        })?;
    Ok(Link {
        capture_tx,
        speaker: Arc::new(OnceLock::new()),
    })
}

/// 行頭から 1 行を **1 回の write で**出す。
///
/// `VEIN CHARA` は 2 千文字を超える。`println!` だと Rust の stdout
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
