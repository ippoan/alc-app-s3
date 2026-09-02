//! CoreS3 統合ハブの周辺デバイス状態。
//!
//! 各 I/O モジュール (rs232 / lan / ble) が更新し、画面処理 (ui) が
//! ステータスバーおよびステータス詳細画面に反映する。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// ログ確認画面に保持する直近イベント数
pub const MAX_EVENTS: usize = 8;

/// イベント/測定ログ 1 行の時刻ラベル。NTP 同期済みなら日本時間
/// "MM/DD HH:MM:SS"、未同期なら稼働時間 "HH:MM:SS"。全ログで共通に使う。
pub fn event_timestamp(now_ms: u64) -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| alc_hub_core::clock::format_jst(d.as_secs() as i64))
        .unwrap_or_else(|| alc_hub_core::layout::fmt_uptime(now_ms))
}

#[derive(Default, Clone)]
pub struct HubStatus {
    /// 直近イベント (新しいものが末尾)。ログ確認画面に表示する
    pub events: VecDeque<String>,

    /// ボード種別 (CoreS3 / CoreS3 SE)。main.rs が起動時に内部 I2C を probe
    /// して確定する (`STATUS BOARD=` / Log 画面に表示)。SE はバッテリーが無い
    pub board: alc_hub_core::board::BoardKind,

    /// OTA (firmware 更新) の実行中フラグ (Refs #116)。
    ///
    /// **OTA は HTTPS の TLS をもう 1 本張る**が、内部RAM の定常空きは約 72KB しか
    /// なく、WS の TLS が張られたままだとハンドシェイクのピーク (約 30KB) で
    /// 枯渇して落ちる (実機で確認: `esp-x509-crt-bundle: Certificate validated`
    /// の直後に panic)。ここが true の間、ws_uplink は接続を切って張り直さず、
    /// BLE は scan を止めてヒープを譲る。
    pub ota_active: bool,

    /// 点呼に血圧を含めるか (Settings::tenko_bp の写し。main.rs が起動時に入れ、
    /// host_link が `TENKO BP` で更新する)。UI は点呼画面に入るたびに読む
    pub tenko_bp: bool,

    /// 進行中の点呼セッションの識別子 (Refs #112)。点呼画面 (Measuring) に
    /// いる間だけ Some で、待機画面へ戻ると None に戻る。**発番と更新は UI
    /// スレッドだけが行い、recorder は読むだけ** — 点呼の開始/終了を知って
    /// いるのは画面状態機械のため。
    pub session_id: Option<String>,

    /// LAN Module 13.2 (W5500) のリンク状態 (lan.rs — 未実装のため常に false)。
    /// AtomS3 印刷ブリッジでは eth_w5500.rs が更新する
    pub lan_link: bool,
    /// LAN 接続時の IP アドレス (eth_w5500.rs が更新。未接続は空)
    pub lan_ip: String,

    /// RS232 (FC-1200) の最終受信時刻 [ms]。None = 起動後受信なし
    pub rs232_last_rx_ms: Option<u64>,

    /// 内蔵 BLE central の接続状態 (ble.rs — 未実装のため常に false)
    pub ble_connected: bool,
    /// 接続中の BLE デバイス名 (NT-100B / NBP-1BLE)
    pub ble_device: String,

    /// Wi-Fi STA の接続状態 (Improv Wi-Fi Serial で設定, wifi.rs が更新)
    pub wifi_connected: bool,
    /// Wi-Fi 接続時の IP アドレス
    pub wifi_ip: String,

    /// cf-alc-recorder への WS 接続状態 (ws_uplink.rs が更新)
    pub ws_connected: bool,
    /// WS 送信キューの未 ack 件数 (`WS STATUS` 応答用)
    pub ws_queue_len: usize,
    /// WS 送信の最終採番 seq
    pub ws_last_seq: u64,

    /// Windows GW (alc-gw) ハブへの WS 接続状態 (gw_link.rs が更新)
    pub gw_connected: bool,
    /// beacon (UDP 9001) 自動発見で見つけた GW の WS URL (未発見は空。
    /// gw_link.rs が更新。`GW STATUS` / 遠隔 gw_status の表示用)
    pub gw_discovered_url: String,

    /// 内部RAM の現在空き [bytes] (heap.rs が定期更新。0 = 未計測)
    pub heap_free_int: usize,
    /// 内部RAM の起動以来の最低空き (low-water mark) [bytes] (Refs #27)
    pub heap_min_int: usize,
    /// PSRAM の現在空き [bytes] (未搭載/無効なら 0)
    pub heap_free_psram: usize,
    /// 内部RAM のヒープ総量 [bytes] (使用率計算用。0 = 未計測)
    pub heap_total_int: usize,
    /// PSRAM のヒープ総量 [bytes] (未搭載/無効なら 0)
    pub heap_total_psram: usize,

    /// AXP2101 を一度でも読めたか (バッテリー系表示のゲート。ui が更新、Refs #50)
    pub power_read: bool,
    /// バッテリーが接続されているか (AXP2101 0x00 bit3)。CoreS3 SE は常に false
    /// なので、残量/充電状態の表示はこれでゲートする
    pub battery_present: bool,
    /// バッテリー残量 [%] (AXP2101 フューエルゲージ 0xA4。255 = 未測定/電池なし)
    pub battery_percent: u8,
    /// バッテリー電圧 [mV] (AXP2101 ADC。0 = 未計測)
    pub battery_mv: u16,
    /// VBUS (外部給電) が来ているか
    pub vbus_present: bool,
    /// 充電状態: 0=待機/満充電 1=充電中 2=放電中 (AXP2101 0x01[6:5])
    pub charge_state: u8,
}

impl HubStatus {
    /// 直近 `window_ms` 以内に RS232 受信があったか
    pub fn rs232_active(&self, now_ms: u64, window_ms: u64) -> bool {
        self.rs232_last_rx_ms
            .map_or(false, |t| now_ms.saturating_sub(t) < window_ms)
    }

    /// イベントログへ 1 行追加 (時刻ラベル付き、直近 MAX_EVENTS 件を保持)。
    /// 時刻は NTP 同期済みなら日本時間、未同期なら稼働時間 (event_timestamp)。
    pub fn push_event(&mut self, now_ms: u64, msg: &str) {
        let line = format!("{} {msg}", event_timestamp(now_ms));
        self.push_line(line);
    }

    /// 整形済みの 1 行をイベントログへ追加 (時刻の付け方を呼び出し側が決める
    /// 場合用。測定値は NTP 同期時に実時刻を付けるため recorder が使う)。
    pub fn push_line(&mut self, line: String) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(line);
    }
}

pub type SharedStatus = Arc<Mutex<HubStatus>>;

/// 現在の壁時計 (epoch ms)。NTP 未同期時は 1970 起点の稼働時間になる
/// (uplink::MIN_SYNCED_MS 未満)。記録側はそのまま保存し、送信側で補正する
pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 起動からの経過ミリ秒
pub fn now_ms() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1000 }
}
