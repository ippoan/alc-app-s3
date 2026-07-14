//! W5500 SPI Ethernet (Atomic PoE Base A091、ippoan/alc-app-s3#38)。
//!
//! AtomS3 + Atomic PoE Base の有線 LAN。基板の W5500 は INT ピンが MCU に
//! 配線されていないため、esp-idf の polling モード
//! (`SpiEventSource::polling`、ESP-IDF v5.3+ の poll_period_ms) を使う。
//! ピン割当は M5Stack 公式サンプル (M5AtomS3/AtomicBase/AtomicPoE) 準拠で
//! 呼び出し側 (atoms3-print/main.rs) が SpiDriver を組んで渡す:
//! SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6。
//!
//! W5500 は MAC を持たないため、efuse 由来の ETH 用 MAC を採番して与える。
//! リンク状態は専用スレッドでポーリングし、`HubStatus::lan_link` と
//! ホストイベントに反映する。CoreS3 の LAN Module 13.2 (lan.rs スタブ) とは
//! ピンも基板も別物なので独立モジュールとする。
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT ETH_CONNECTED <ip>` | リンクアップ + IP 取得 |
//! | `EVT ETH_DISCONNECTED` | リンクダウン |
//! | `EVT ETH NG <理由>` | 初期化失敗 (機能無効のまま稼働継続) |

use anyhow::{Context, Result};
use esp_idf_svc::eth::{EspEth, EthDriver, SpiEthChipset, SpiEventSource};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::AnyOutputPin;
use esp_idf_svc::hal::spi::SpiDriver;
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys;

use alc_hub_common::status::{now_ms, SharedStatus};

/// W5500 SPI クロック。公式サンプルは既定 SPI 速度、esp-idf example は
/// 36MHz だが、スタック接続 (pogo ピン) の信号品質を考慮して控えめにする
const SPI_BAUDRATE_HZ: u32 = 20_000_000;
/// INT 未配線のため W5500 レジスタをポーリングする間隔
const POLL_INTERVAL_MS: u64 = 10;
/// リンク状態の監視間隔
const LINK_CHECK_INTERVAL_MS: u32 = 500;

/// W5500 を初期化しリンク監視スレッドを起動する。
/// 初期化失敗はイベント出力のみで呼び出し元へはエラーを返さない
/// (LAN 無しでも USB 経由の診断は生かす — lan.rs スタブと同方針)。
pub fn start(
    spi: SpiDriver<'static>,
    cs: AnyOutputPin<'static>,
    sysloop: EspSystemEventLoop,
    status: SharedStatus,
) -> Result<()> {
    std::thread::Builder::new()
        .name("eth_w5500".into())
        // TCP/IP イベント + ドライバ初期化を考慮して余裕を持たせる
        .stack_size(8 * 1024)
        .spawn(move || match init(spi, cs, sysloop) {
            Ok(eth) => monitor_loop(eth, status),
            Err(e) => println!("EVT ETH NG {e:#}"),
        })
        .context("eth_w5500 スレッド起動失敗")?;
    Ok(())
}

fn init(
    spi: SpiDriver<'static>,
    cs: AnyOutputPin<'static>,
    sysloop: EspSystemEventLoop,
) -> Result<EspEth<'static, esp_idf_svc::eth::SpiEth<SpiDriver<'static>>>> {
    // W5500 は MAC 不揮発領域を持たないため efuse 由来の ETH MAC を使う
    let mut mac = [0u8; 6];
    unsafe {
        sys::esp!(sys::esp_read_mac(
            mac.as_mut_ptr(),
            sys::esp_mac_type_t_ESP_MAC_ETH,
        ))
        .context("ETH MAC の取得に失敗")?;
    }

    let event_source = SpiEventSource::polling(core::time::Duration::from_millis(POLL_INTERVAL_MS))
        .context("polling 間隔が不正")?;

    let driver = EthDriver::new_spi_with_event_source(
        spi,
        event_source,
        Some(cs),
        Option::<AnyOutputPin>::None, // RST 未配線
        SpiEthChipset::W5500,
        Hertz(SPI_BAUDRATE_HZ),
        Some(&mac),
        None,
        sysloop,
    )
    .context("W5500 ドライバ初期化失敗 (PoE Base の接続を確認してください)")?;

    let mut eth = EspEth::wrap(driver).context("Ethernet netif 初期化失敗")?;
    eth.start().context("Ethernet 開始失敗")?;
    Ok(eth)
}

/// リンク状態を監視し、変化時にイベント出力 + HubStatus を更新し続ける。
/// eth ハンドルはこのループが所有し続ける (drop すると停止するため)。
fn monitor_loop(
    eth: EspEth<'static, esp_idf_svc::eth::SpiEth<SpiDriver<'static>>>,
    status: SharedStatus,
) -> ! {
    let mut was_up = false;
    loop {
        let up = eth.is_up().unwrap_or(false);
        if up != was_up {
            if up {
                let ip = eth
                    .netif()
                    .get_ip_info()
                    .map(|i| i.ip.to_string())
                    .unwrap_or_default();
                println!("EVT ETH_CONNECTED {ip}");
                if let Ok(mut st) = status.lock() {
                    st.lan_link = true;
                    st.push_event(now_ms(), &format!("LAN 接続 {ip}"));
                }
            } else {
                println!("EVT ETH_DISCONNECTED");
                if let Ok(mut st) = status.lock() {
                    st.lan_link = false;
                    st.push_event(now_ms(), "LAN 切断");
                }
            }
            was_up = up;
        }
        FreeRtos::delay_ms(LINK_CHECK_INTERVAL_MS);
    }
}
