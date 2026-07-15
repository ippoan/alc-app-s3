//! RS232M Module 13.2 → DB9 → FC-1200 の UART 受信スレッド。
//!
//! 受信バイト列は現状そのままホスト (USB CDC) へ `FC1200 <hex>` 行として
//! 転送する (パススルー)。FC-1200 プロトコル解釈 (fc1200-wasm の移植) は
//! 本リポジトリでは扱わない。
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

use anyhow::Result;
use esp_idf_svc::hal::{
    gpio::{AnyIOPin, Gpio17, Gpio18},
    uart::{config::Config as UartConfig, UartDriver, UART1},
    units::Hertz,
};

use alc_hub_common::{
    config,
    status::{now_ms, SharedStatus},
};

pub fn start(
    uart: UART1<'static>,
    tx_pin: Gpio17<'static>,
    rx_pin: Gpio18<'static>,
    status: SharedStatus,
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
        .stack_size(4096)
        .spawn(move || {
            let mut buf = [0u8; 128];
            loop {
                // 100ms タイムアウトのブロッキング読み出し
                match driver.read(&mut buf, 100) {
                    Ok(n) if n > 0 => {
                        let now = now_ms();
                        if let Ok(mut st) = status.lock() {
                            st.rs232_last_rx_ms = Some(now);
                            st.push_event(now, &format!("FC1200 受信 {n}B"));
                        }
                        let hex = buf[..n]
                            .iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        println!("FC1200 {hex}");
                    }
                    _ => {}
                }
            }
        })?;
    Ok(())
}
