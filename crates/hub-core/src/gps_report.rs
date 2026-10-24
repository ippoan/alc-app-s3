//! Unit GPS を Vein Base の Grove PORT.A につなぐ版 (`vein-gps`) の、`EVT GPS` の出し方と
//! RX ピンの当て方の純粋部分 (ippoan/alc-app#514)。
//!
//! NMEA の解釈は [`crate::gps`] が正本。ここは UART の読み出し (hub-drivers の `gps`) から
//! 呼ばれる 2 つの小さな状態機械だけを持つ:
//!
//! - [`Reporter`]: バイト列を受け、いつ `EVT GPS …` の 1 行を出すかを決める。
//!   **最初の正しい NMEA を受けるまで何も出さない** (GPS を挿していない機体を web が
//!   「GPS 無し」と見分けるため)。その後は FIX / NOFIX が変わったときと
//!   [`REPORT_EVERY_MS`] ごとに出す
//! - [`RxProbe`]: RX を G1 から試し、[`SWITCH_AFTER_MS`] のあいだ正しい NMEA が来なければ
//!   G2 に替える (以後、交互)。読めたらそのピンで固定する

use crate::gps::{verify, LineBuf, Tracker};

/// 状態が変わらないときに 1 行出す間隔 (ms)
pub const REPORT_EVERY_MS: u64 = 30_000;
/// 正しい NMEA が来ないまま RX のピンを替えるまでの時間 (ms)
pub const SWITCH_AFTER_MS: u64 = 3_000;

/// バイト列から `EVT GPS` の行を作る
#[derive(Debug, Default)]
pub struct Reporter {
    lines: LineBuf,
    tracker: Tracker,
    /// 正しい NMEA を 1 本でも受けたか
    seen: bool,
    /// 最後に出した行の FIX / NOFIX
    last_valid: Option<bool>,
    last_emit_ms: u64,
}

impl Reporter {
    /// 1 バイト食わせる。`now_ms` は単調増加の時刻。戻りは 2 つ:
    /// 行の終わりで checksum の合う NMEA だったか (`RxProbe` に渡す) と、出す行
    pub fn push(&mut self, b: u8, now_ms: u64) -> (bool, Option<String>) {
        let Some(line) = self.lines.push(b) else {
            return (false, None);
        };
        if verify(&line).is_none() {
            return (false, None);
        }
        self.seen = true;
        self.tracker.feed(&line);
        let valid = self.tracker.fix().valid;
        let due = self.last_valid != Some(valid)
            || now_ms.saturating_sub(self.last_emit_ms) >= REPORT_EVERY_MS;
        if !due {
            return (true, None);
        }
        self.last_valid = Some(valid);
        self.last_emit_ms = now_ms;
        (true, Some(self.tracker.fix().evt_line()))
    }

    /// 正しい NMEA を 1 本でも受けたか
    pub fn seen(&self) -> bool {
        self.seen
    }
}

/// RX に使うピン
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxPin {
    G1,
    G2,
}

/// RX ピンの当て方
#[derive(Debug)]
pub struct RxProbe {
    pin: RxPin,
    since_ms: u64,
    locked: bool,
}

impl RxProbe {
    pub fn new(now_ms: u64) -> Self {
        Self {
            pin: RxPin::G1,
            since_ms: now_ms,
            locked: false,
        }
    }

    /// いま使うピン
    pub fn pin(&self) -> RxPin {
        self.pin
    }

    /// 回るたびに呼ぶ。`verified` はそのときに正しい NMEA を受けたか。
    /// ピンを替えるべきなら替えた先を返す (呼び手は UART のピンを差し替える)
    pub fn observe(&mut self, now_ms: u64, verified: bool) -> Option<RxPin> {
        if self.locked {
            return None;
        }
        if verified {
            self.locked = true;
            return None;
        }
        if now_ms.saturating_sub(self.since_ms) < SWITCH_AFTER_MS {
            return None;
        }
        self.since_ms = now_ms;
        self.pin = match self.pin {
            RxPin::G1 => RxPin::G2,
            RxPin::G2 => RxPin::G1,
        };
        Some(self.pin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nmea(body: &str) -> String {
        let cs = body.bytes().fold(0u8, |a, b| a ^ b);
        format!("${body}*{cs:02X}\r\n")
    }

    const RMC_A: &str = "GNRMC,063000.000,A,3540.8742,N,13946.0275,E,0.00,0.00,241026,,,A";
    const RMC_V: &str = "GNRMC,063000.000,V,,,,,,,241026,,,N";
    const GGA_1: &str = "GNGGA,063000.000,3540.8742,N,13946.0275,E,1,08,0.9,40.1,M,39.4,M,,";
    const GGA_0: &str = "GNGGA,063000.000,,,,,0,03,,,M,,M,,";

    fn feed(r: &mut Reporter, text: &str, now: u64) -> (bool, Vec<String>) {
        let mut ok = false;
        let mut out = Vec::new();
        for b in text.bytes() {
            let (v, l) = r.push(b, now);
            ok |= v;
            out.extend(l);
        }
        (ok, out)
    }

    #[test]
    fn silent_until_first_valid_nmea() {
        let mut r = Reporter::default();
        let (ok, out) = feed(&mut r, "garbage\r\n$GNRMC,bad*00\r\n", 0);
        assert!(!ok && out.is_empty() && !r.seen());
        let (ok, out) = feed(&mut r, &nmea(RMC_V), 100);
        assert!(ok && r.seen());
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("EVT GPS NOFIX"));
    }

    #[test]
    fn emits_on_change_and_every_30s() {
        let mut r = Reporter::default();
        feed(&mut r, &nmea(RMC_V), 0);
        // 変わらない間は黙る
        let (_, out) = feed(&mut r, &nmea(GGA_0), 1_000);
        assert!(out.is_empty());
        let (_, out) = feed(&mut r, &nmea(RMC_V), 29_999);
        assert!(out.is_empty());
        // 30 秒で 1 行
        let (_, out) = feed(&mut r, &nmea(RMC_V), 30_000);
        assert_eq!(out.len(), 1);
        // NOFIX → FIX は即出す
        let (_, out) = feed(&mut r, &nmea(RMC_A), 31_000);
        assert!(out.is_empty(), "GGA がまだなので valid は偽のまま");
        let (_, out) = feed(&mut r, &nmea(GGA_1), 31_100);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("EVT GPS FIX lat=35.681237"));
        // FIX → NOFIX も即出す
        let (_, out) = feed(&mut r, &nmea(RMC_V), 32_000);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("EVT GPS NOFIX"));
    }

    #[test]
    fn probe_switches_after_3s_and_locks_on_data() {
        let mut p = RxProbe::new(1_000);
        assert_eq!(p.pin(), RxPin::G1);
        assert_eq!(p.observe(3_999, false), None);
        assert_eq!(p.observe(4_000, false), Some(RxPin::G2));
        assert_eq!(p.pin(), RxPin::G2);
        // 来なければ交互に戻る (後から挿した GPS も見つかる)
        assert_eq!(p.observe(6_999, false), None);
        assert_eq!(p.observe(7_000, false), Some(RxPin::G1));
        // 読めたら固定
        assert_eq!(p.observe(7_100, true), None);
        assert_eq!(p.observe(60_000, false), None);
        assert_eq!(p.pin(), RxPin::G1);
    }
}
