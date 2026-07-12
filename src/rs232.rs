//! RS232M Module 13.2 → DB9 → FC-1200 の UART 受信スレッド。
//!
//! 受信バイト列は現状そのままホスト (USB CDC) へ `FC1200 <hex>` 行として
//! 転送する (パススルー)。FC-1200 プロトコル解釈 (fc1200-wasm の移植) は
//! 本リポジトリでは扱わない — plan/cores3-hub-consolidation.md
//! 「fc1200-wasm の移植」節を参照。

use anyhow::Result;
use esp_idf_svc::hal::{
    gpio::{AnyIOPin, Gpio17, Gpio18},
    uart::{config::Config as UartConfig, UartDriver, UART1},
    units::Hertz,
};

use crate::{
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
                        if let Ok(mut st) = status.lock() {
                            st.rs232_last_rx_ms = Some(now_ms());
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
