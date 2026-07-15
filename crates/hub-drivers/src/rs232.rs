//! RS232M Module 13.2 → DB9 → FC-1200 の UART 送受信スレッド。
//!
//! 受信バイト列を hub-core の FC-1200 プロトコル (fc1200-wasm 移植) で解釈し、
//! CNOK / RSOK の応答送信と測定フローの進行を行う。測定結果は Measurement
//! チャネルで recorder へ送り、BLE 測定と同じ経路 (ホスト JSON / WS uplink /
//! 画面 / NVS ログ) に fan-out される。生バイト列は従来どおり `FC1200 <hex>`
//! 行でもホストへ流す (診断用パススルー)。
//!
//! 実機設定 (plan/cores3-hub-consolidation.md、FC-1200B の ConnectionRequest
//! 受信まで実機確認済み):
//! - RXD DIP: 2 番 (シルク 16 = バスpin 16 = CoreS3 G17) を ON
//! - TXD DIP: 3 番 (シルク 15 = バスpin 15 = CoreS3 G18) を ON
//! - ホスト側 TX=G17 / RX=G18 (下記の Gpio17/Gpio18)。他スイッチ全 OFF。
//! - DB9 の 2/3 ピンは**線序トグルスイッチ**でストレート/クロスが入れ替わる。
//!   FC-1200 と疎通しない場合はまずトグルを反対側へ (実機でここが原因だった)。
//! - ボーレートは FC-1200 (タニタ ALBLO) 仕様の 9600bps 8N1 (config::RS232_BAUD)。
//! - シルク番号はバスpin (無印 Core 基準) であり CoreS3 GPIO ではない (Community #5581)。

use std::sync::mpsc::Sender;

use anyhow::Result;
use esp_idf_svc::hal::{
    gpio::{AnyIOPin, Gpio17, Gpio18},
    uart::{config::Config as UartConfig, UartDriver, UART1},
    units::Hertz,
};

use alc_hub_common::{
    config,
    measurement::Measurement,
    status::{now_ms, SharedStatus},
};
use alc_hub_core::fc1200::{Event, IncomingCommand, LineParser, StateMachine};

pub fn start(
    uart: UART1<'static>,
    tx_pin: Gpio17<'static>,
    rx_pin: Gpio18<'static>,
    status: SharedStatus,
    meas_tx: Sender<Measurement>,
) -> Result<()> {
    let cfg = UartConfig::new().baudrate(Hertz(config::RS232_BAUD));
    let driver = UartDriver::new(
        uart,
        tx_pin,
        rx_pin,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &cfg,
    )?;

    std::thread::Builder::new()
        .name("rs232".into())
        // プロトコル処理 (String 組み立て) が乗るため passthrough 時代の 4096 から増量
        .stack_size(8 * 1024)
        .spawn(move || {
            let mut buf = [0u8; 128];
            let mut parser = LineParser::new();
            let mut sm = StateMachine::new();
            loop {
                // 100ms タイムアウトのブロッキング読み出し
                let n = match driver.read(&mut buf, 100) {
                    Ok(n) if n > 0 => n,
                    _ => continue,
                };
                let now = now_ms();
                if let Ok(mut st) = status.lock() {
                    st.rs232_last_rx_ms = Some(now);
                }
                let hex = buf[..n]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("FC1200 {hex}");

                for line in parser.feed(&buf[..n]) {
                    let Some(cmd) = IncomingCommand::parse(&line) else {
                        log::warn!("rs232: 不明な行を無視: {line}");
                        continue;
                    };
                    for event in sm.process(&cmd) {
                        handle_event(event, &status, &meas_tx);
                    }
                    // 応答 (CNOK/RSOK) を送る。FC-1200 は応答が無いとリトライを続ける
                    while let Some(resp) = sm.take_response() {
                        if let Err(e) = driver.write(resp.to_bytes()) {
                            log::warn!("rs232: 応答送信失敗 {resp:?}: {e:?}");
                        }
                    }
                }
            }
        })?;
    Ok(())
}

/// 状態機械イベントをホスト行・イベントログ・recorder へ振り分ける。
/// 測定結果の重い処理 (JSON/WS/NVS/画面) は recorder スレッド側で行う
fn handle_event(event: Event, status: &SharedStatus, meas_tx: &Sender<Measurement>) {
    // ホストは既知プレフィックス行のみ解釈する (README) — 状態遷移は EVT で流す
    match event {
        Event::Connected { model, variant } => {
            println!("EVT FC1200 CONNECTED {model}{variant}");
            push_event(status, &format!("FC-1200 接続 ({model}{variant})"));
        }
        Event::WarmingUp {
            total_seconds,
            elapsed_days,
        } => {
            println!("EVT FC1200 WARMING {total_seconds} {elapsed_days}");
        }
        Event::BlowWaiting => {
            println!("EVT FC1200 BLOW_WAITING");
            push_event(status, "FC-1200 吹込待ち");
        }
        Event::BlowTimeout => {
            println!("EVT FC1200 BLOW_TIMEOUT");
            push_event(status, "FC-1200 吹込タイムアウト");
        }
        Event::Measuring => {
            println!("EVT FC1200 MEASURING");
        }
        Event::Result {
            result,
            centi_mg_per_l,
            use_count,
        } => {
            let _ = meas_tx.send(Measurement::Alcohol {
                result,
                centi_mg_per_l,
                use_count,
                at_ms: now_ms(),
            });
        }
        Event::Unexpected { detail } => {
            log::warn!("rs232: 想定外コマンド: {detail}");
        }
    }
}

fn push_event(status: &SharedStatus, line: &str) {
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), line);
    }
}
