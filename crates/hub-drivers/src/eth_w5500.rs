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
//! | `EVT ETH_PROBE_OK n=<回数>` | W5500 が SPI に応答した (n = probe 回数) |
//! | `EVT ETH_CONNECTED <ip>` | リンクアップ + IP 取得 |
//! | `EVT ETH_DISCONNECTED` | リンクダウン |
//! | `EVT ETH NG <理由>` | 初期化失敗 (機能無効のまま稼働継続) |

use std::time::Duration;

use anyhow::{Context, Result};
use esp_idf_svc::eth::{EspEth, EthDriver, SpiEthChipset, SpiEventSource};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyOutputPin, Pin, PinDriver, PinId};
use esp_idf_svc::hal::spi::{
    config::Config as SpiConfig, config::MODE_0, SpiDeviceDriver, SpiDriver,
};
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
/// probe (VERSIONR 読み) の SPI クロック。存在確認だけなので、電源が来た直後や
/// スタック接続の信号品質が悪い状態でも読めるよう本転送より十分低くする
const PROBE_BAUDRATE_HZ: u32 = 2_000_000;
/// W5500 が応答するまでの probe 間隔。上限は設けない — 据置機なので
/// PoE (= ベースの 5V) が来るまで待ち続ける
const ETH_PROBE_INTERVAL: Duration = Duration::from_secs(10);
/// VERSIONR (共通レジスタ 0x0039) の読み出しフレーム。アドレス上位 / 下位 /
/// コントロールバイト (BSB=0 共通レジスタ, RWB=0 read, OM=00 可変長データモード)
/// に、値を受け取るためのダミー 1 バイトを足した 4 バイトを全二重で往復する
const W5500_VERSIONR_FRAME: [u8; 4] = [0x00, 0x39, 0x00, 0x00];
/// VERSIONR の固定値 (W5500 データシート)。これが返れば W5500 に電源が来ている
const W5500_VERSION: u8 = 0x04;
/// ハードリセットの L 幅と、その後 PLL が安定するまでの待ち
/// (データシートの最小値 500us / 1ms に対して余裕を取る)
const RST_LOW_MS: u32 = 5;
const RST_SETTLE_MS: u32 = 10;

/// W5500 を初期化しリンク監視スレッドを起動する。
/// 初期化失敗はイベント出力のみで呼び出し元へはエラーを返さない
/// (LAN 無しでも USB 経由の診断は生かす — lan.rs スタブと同方針)。
///
/// `spi` は leak 済みの 'static 参照で受け取る。EthDriver に所有で渡すと
/// esp_eth_driver_install 失敗時 (基板未接続等) の drop 連鎖で
/// esp-idf-hal の SpiDriver::drop が ESP_ERR_INVALID_STATE を unwrap
/// panic し再起動ループになる (実機で確認)。また CoreS3 では LCD と
/// バスを共有するため、そもそも所有で渡せない。
/// `rst` は W5500 のハードリセット線 (未配線基板は None → ソフトリセットのみ)
pub fn start(
    spi: &'static SpiDriver<'static>,
    cs: AnyOutputPin<'static>,
    rst: Option<AnyOutputPin<'static>>,
    sysloop: EspSystemEventLoop,
    status: SharedStatus,
) -> Result<()> {
    crate::task::name_next_psram(c"eth_w5500", 8 * 1024);
    std::thread::Builder::new()
        .name("eth_w5500".into())
        // TCP/IP イベント + ドライバ初期化を考慮して余裕を持たせる
        .stack_size(8 * 1024)
        .spawn(move || {
            // pin 番号だけ控えておく。probe 用の一時デバイスは steal した複製を
            // 使い、init には元の cs / rst をそのまま渡す (所有権は動かさない)
            let cs_num = cs.pin();
            let rst_num = rst.as_ref().map(|p| p.pin());

            // 電源が後から来た W5500 のためにハードリセットを 1 回だけ打つ。
            // probe の間は H に保ち、init の直前に手放す — init 側の driver が
            // 同じ番号を reset_gpio_num で取り直すので、二重取得を避ける
            let rst_drv = rst_num.and_then(pulse_reset);

            let n = wait_for_w5500(spi, cs_num, &status);
            drop(rst_drv);
            println!("EVT ETH_PROBE_OK n={n}");

            match init(spi, cs, rst, sysloop) {
                Ok(eth) => monitor_loop(eth, status),
                Err(e) => println!("EVT ETH NG {e:#}"),
            }
        })
        .context("eth_w5500 スレッド起動失敗")?;
    Ok(())
}

/// W5500 の RST 線を L (数 ms) → H に打ち、開いたままの `PinDriver` を返す。
/// 取得に失敗しても probe と init は続行する (RST 未配線の基板もあるため)
fn pulse_reset(num: PinId) -> Option<PinDriver<'static, esp_idf_svc::hal::gpio::Output>> {
    // Safety: この番号のピンは呼び出し元が所有する rst と同一で、この関数が
    // 返す PinDriver を drop するまで他の driver へは渡らない
    let pin = unsafe { AnyOutputPin::steal(num) };
    let mut drv = match PinDriver::output(pin) {
        Ok(d) => d,
        Err(e) => {
            println!("EVT ETH NG w5500 rst pin の取得に失敗 ({e})");
            return None;
        }
    };
    if let Err(e) = drv.set_low() {
        println!("EVT ETH NG w5500 rst の L 出力に失敗 ({e})");
        return None;
    }
    FreeRtos::delay_ms(RST_LOW_MS);
    if let Err(e) = drv.set_high() {
        println!("EVT ETH NG w5500 rst の H 出力に失敗 ({e})");
        return None;
    }
    FreeRtos::delay_ms(RST_SETTLE_MS);
    Some(drv)
}

/// 生 SPI で VERSIONR を 1 バイト読む。一時デバイスはこの関数を抜けるときに
/// Drop され (`spi_bus_remove_device`)、バスのデバイス枠が戻る
fn probe_versionr(spi: &'static SpiDriver<'static>, cs_num: PinId) -> Result<u8> {
    // Safety: cs は init に渡すまでこのスレッドしか使わず、一時デバイスは
    // この関数を抜けるときに必ず外れる
    let cs = unsafe { AnyOutputPin::steal(cs_num) };
    let config = SpiConfig::new()
        .baudrate(Hertz(PROBE_BAUDRATE_HZ))
        .data_mode(MODE_0);
    let mut dev = SpiDeviceDriver::new(spi, Some(cs), &config)
        .context("probe 用 SPI デバイスの追加に失敗")?;
    let mut rx = [0u8; 4];
    dev.transfer(&mut rx, &W5500_VERSIONR_FRAME)
        .context("VERSIONR の読み出しに失敗")?;
    Ok(rx[3])
}

/// W5500 が応答する (VERSIONR = 0x04) まで `ETH_PROBE_INTERVAL` ごとに待ち、
/// かかった probe 回数を返す。driver の install は「応答してから 1 回だけ」に
/// したいのでここで待つ — 無電源の W5500 に対して install を繰り返すと
/// esp-idf-svc が失敗時に MAC/PHY を解放せず、SPI ホストのデバイス枠
/// (LCD と共有) が埋まって別の理由で永久に失敗するため。
/// 失敗ログは理由が変わったときだけ出す (10 秒ごとに同じ行を吐き続けない)
///
/// **1 回目の probe の結果で `HubStatus::bus_in` を確定する** (Refs #211)。
/// 起動時の BUS_EN は L (`power::init`) なので、1 回目で応答があれば M-Bus に
/// 外から 5V が来ている (PoE)。失敗した 1 回目でも `Some(false)` を入れる —
/// この関数は成功するまで戻らないので、戻ってから入れると USB 単独起動が
/// 判定待ちのまま止まる。2 回目以降の probe では触らない (起動中 sticky)。
fn wait_for_w5500(
    spi: &'static SpiDriver<'static>,
    cs_num: PinId,
    status: &SharedStatus,
) -> u32 {
    let mut n: u32 = 0;
    let mut last: Option<String> = None;
    loop {
        n += 1;
        let reason = match probe_versionr(spi, cs_num) {
            Ok(W5500_VERSION) => {
                if n == 1 {
                    if let Ok(mut st) = status.lock() {
                        st.bus_in = Some(true);
                    }
                }
                return n;
            }
            // 0x00 / 0xFF はベースに 5V が無い (PoE 未接続) か未接続。
            // それ以外の値は配線か SPI モードを疑う
            Ok(v) => format!("versionr=0x{v:02X}"),
            Err(e) => format!("spi_err={e:#}"),
        };
        if n == 1 {
            if let Ok(mut st) = status.lock() {
                st.bus_in = Some(false);
            }
        }
        if last.as_deref() != Some(reason.as_str()) {
            log::warn!(
                "eth_w5500: W5500 が応答しない ({reason}) — {ETH_PROBE_INTERVAL:?} ごとに probe する"
            );
            println!("EVT ETH NG w5500 not responding {reason}");
            last = Some(reason);
        }
        FreeRtos::delay_ms(ETH_PROBE_INTERVAL.as_millis() as u32);
    }
}

fn init(
    spi: &'static SpiDriver<'static>,
    cs: AnyOutputPin<'static>,
    rst: Option<AnyOutputPin<'static>>,
    sysloop: EspSystemEventLoop,
) -> Result<EspEth<'static, esp_idf_svc::eth::SpiEth<&'static SpiDriver<'static>>>> {
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
        rst,
        SpiEthChipset::W5500,
        Hertz(SPI_BAUDRATE_HZ),
        Some(&mac),
        None,
        sysloop,
    )
    .context("W5500 ドライバ初期化失敗 (基板/モジュールの接続を確認してください)")?;

    let mut eth = EspEth::wrap(driver).context("Ethernet netif 初期化失敗")?;
    eth.start().context("Ethernet 開始失敗")?;
    Ok(eth)
}

/// リンク状態を監視し、変化時にイベント出力 + HubStatus を更新し続ける。
/// eth ハンドルはこのループが所有し続ける (drop すると停止するため)。
fn monitor_loop(
    eth: EspEth<'static, esp_idf_svc::eth::SpiEth<&'static SpiDriver<'static>>>,
    status: SharedStatus,
) -> ! {
    let mut was_up = false;
    loop {
        let up = eth.is_up().unwrap_or(false);
        if up != was_up {
            if up {
                // 診断: ip だけでなく subnet (netmask + gateway) も出す。
                // デバイスとプリンターが同一サブネットか (例 .18.x と .21.x が
                // /24 なら別サブネットで直接到達不可) の切り分けに使う。
                let info = eth.netif().get_ip_info();
                let ip = info
                    .as_ref()
                    .map(|i| i.ip.to_string())
                    .unwrap_or_default();
                match &info {
                    Ok(i) => println!("EVT ETH_CONNECTED {ip} subnet={:?}", i.subnet),
                    Err(e) => println!("EVT ETH_CONNECTED {ip} (ip_info 取得失敗: {e})"),
                }
                // println! は vprintf hook を通らないためリングに残らない。
                // 事後解析 (`LOG DUMP`) で「いつ繋がって いつ切れたか」を
                // 追えるよう明示的に残す (crashlog.rs 参照)
                crate::crashlog::note(&format!("EVT ETH_CONNECTED {ip} up_ms={}", now_ms()));
                if let Ok(mut st) = status.lock() {
                    st.lan_link = true;
                    st.lan_ip = ip.clone();
                    st.push_event(now_ms(), &format!("LAN 接続 {ip}"));
                }
            } else {
                println!("EVT ETH_DISCONNECTED");
                // 切断時は「そのとき何が枯れていたか」まで残す。ヒープ不足と
                // SPI 競合のどちらなのかを、後から `LOG DUMP` だけで切り分ける
                let h = crate::heap::stats();
                crate::crashlog::note(&format!(
                    "EVT ETH_DISCONNECTED up_ms={} free_int={} min_int={} free_psram={}",
                    now_ms(),
                    h.free_int,
                    h.min_int,
                    h.free_psram,
                ));
                if let Ok(mut st) = status.lock() {
                    st.lan_link = false;
                    st.lan_ip.clear();
                    st.push_event(now_ms(), "LAN 切断");
                }
            }
            was_up = up;
        }
        FreeRtos::delay_ms(LINK_CHECK_INTERVAL_MS);
    }
}
