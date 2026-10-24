//! M5Stack Unit GPS (AT6558 / AT6668) の NMEA 0183 を読む純粋部分。
//!
//! Unit GPS は UART に NMEA の文を 1 秒ごとに流すだけで、こちらからの設定は
//! 要らない。ここでは「バイト列を行に切る ([`LineBuf`])」「行の checksum を
//! 確かめて RMC / GGA を読む ([`parse`])」「読めた文を 1 つの測位結果に畳む
//! ([`Tracker`])」「結果を `EVT GPS …` の 1 行にする ([`Fix::evt_line`])」を持つ。
//! UART の読み書き (ピン・ボーレート) は呼び手 (crates/atom-gps) の仕事。
//!
//! 文の送り手 (talker) は `GP` (GPS) / `GN` (複合) / `BD` / `GB` (北斗) /
//! `GL` (GLONASS) のどれでも同じに扱う — AT6558 は既定で `GN` を出す。
//! RMC と GGA 以外 (GSV / GSA / VTG / TXT 等) は読まずに捨てる。

/// 1 行の上限 [byte]。NMEA 0183 の規格上の最大は `$` から LF まで 82 byte。
/// これを超えたら壊れた行として捨てる (ボーレートの食い違いで改行が来ない
/// ときにバッファが伸び続けないため)
pub const MAX_LINE: usize = 82;

/// UART から読んだバイトを行に切る。
///
/// CR / LF のどちらでも行が終わる。空行は返さない。[`MAX_LINE`] を超えた行は
/// 次の改行まで丸ごと捨てる (途中から切り出すと別の文に見えるため)。
#[derive(Debug, Default)]
pub struct LineBuf {
    buf: Vec<u8>,
    overflow: bool,
}

impl LineBuf {
    /// 1 バイト足す。行が終わったら `Some(行)` を返す (CR / LF は含まない)
    pub fn push(&mut self, b: u8) -> Option<String> {
        if b == b'\r' || b == b'\n' {
            let line = std::mem::take(&mut self.buf);
            let overflow = std::mem::replace(&mut self.overflow, false);
            if overflow || line.is_empty() {
                return None;
            }
            return Some(String::from_utf8_lossy(&line).into_owned());
        }
        if self.buf.len() >= MAX_LINE {
            self.overflow = true;
            self.buf.clear();
        }
        if !self.overflow {
            self.buf.push(b);
        }
        None
    }
}

/// `$…*hh` の checksum を確かめ、`$` と `*hh` を外した本体を返す。
/// 形が違う・checksum が合わない行は `None`
pub fn verify(line: &str) -> Option<&str> {
    let body_star = line.trim().strip_prefix('$')?;
    let (body, hex) = body_star.rsplit_once('*')?;
    let want = u8::from_str_radix(hex, 16).ok()?;
    let got = body.bytes().fold(0u8, |acc, b| acc ^ b);
    (got == want).then_some(body)
}

/// UTC の日付と時刻。日付は RMC にしか無いので別々に持つ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hms {
    pub h: u8,
    pub m: u8,
    pub s: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ymd {
    pub y: u16,
    pub m: u8,
    pub d: u8,
}

/// 読めた文
#[derive(Debug, Clone, PartialEq)]
pub enum Sentence {
    /// RMC (推奨最小データ)。`valid` は status 欄の `A`
    Rmc {
        time: Option<Hms>,
        date: Option<Ymd>,
        valid: bool,
        pos: Option<(f64, f64)>,
    },
    /// GGA (測位情報)。`quality` 0 は測位なし
    Gga {
        time: Option<Hms>,
        pos: Option<(f64, f64)>,
        quality: u8,
        sats: u8,
        hdop: Option<f32>,
        alt_m: Option<f32>,
    },
}

/// checksum を確かめて RMC / GGA を読む。それ以外の文・壊れた行は `None`
pub fn parse(line: &str) -> Option<Sentence> {
    let body = verify(line)?;
    let f: Vec<&str> = body.split(',').collect();
    let kind = f[0].get(2..)?;
    match kind {
        "RMC" if f.len() >= 10 => Some(Sentence::Rmc {
            time: hms(f[1]),
            valid: f[2] == "A",
            pos: latlon(f[3], f[4], f[5], f[6]),
            date: ymd(f[9]),
        }),
        "GGA" if f.len() >= 10 => Some(Sentence::Gga {
            time: hms(f[1]),
            pos: latlon(f[2], f[3], f[4], f[5]),
            quality: f[6].parse().unwrap_or(0),
            sats: f[7].parse().unwrap_or(0),
            hdop: f[8].parse().ok(),
            alt_m: f[9].parse().ok(),
        }),
        _ => None,
    }
}

/// `hhmmss(.sss)` → 時刻
fn hms(s: &str) -> Option<Hms> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<u8>().ok();
    Some(Hms {
        h: n(0..2)?,
        m: n(2..4)?,
        s: n(4..6)?,
    })
}

/// `ddmmyy` → 日付 (年は 2000 年代とみなす)
fn ymd(s: &str) -> Option<Ymd> {
    if s.len() != 6 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u8>().ok();
    Some(Ymd {
        d: n(0..2)?,
        m: n(2..4)?,
        y: 2000 + u16::from(n(4..6)?),
    })
}

/// `ddmm.mmmm,N,dddmm.mmmm,E` → (緯度, 経度) [度]。南・西は負
fn latlon(lat: &str, ns: &str, lon: &str, ew: &str) -> Option<(f64, f64)> {
    let lat = deg(lat)? * sign(ns, "N", "S")?;
    let lon = deg(lon)? * sign(ew, "E", "W")?;
    Some((lat, lon))
}

/// `(d)ddmm.mmmm` → 度
fn deg(s: &str) -> Option<f64> {
    let v: f64 = s.parse().ok()?;
    let d = (v / 100.0).trunc();
    Some(d + (v - d * 100.0) / 60.0)
}

fn sign(s: &str, pos: &str, neg: &str) -> Option<f64> {
    match s {
        _ if s == pos => Some(1.0),
        _ if s == neg => Some(-1.0),
        _ => None,
    }
}

/// RMC と GGA を畳んだ現在の測位結果
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fix {
    /// 測位できているか (直近の RMC が `A`、かつ GGA の quality が 0 でない)
    pub valid: bool,
    pub lat: f64,
    pub lon: f64,
    pub sats: u8,
    pub hdop: Option<f32>,
    pub alt_m: Option<f32>,
    pub date: Option<Ymd>,
    pub time: Option<Hms>,
}

impl Fix {
    /// シリアルへ出す 1 行。測位できていれば
    /// `EVT GPS FIX lat=35.681236 lon=139.767125 sats=8 hdop=0.9 alt=40.1 utc=2026-10-24T06:30:00Z`、
    /// できていなければ `EVT GPS NOFIX sats=3 utc=…`。分からない欄は `-`
    pub fn evt_line(&self) -> String {
        let utc = match (self.date, self.time) {
            (Some(d), Some(t)) => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                d.y, d.m, d.d, t.h, t.m, t.s
            ),
            _ => "-".to_string(),
        };
        if !self.valid {
            return format!("EVT GPS NOFIX sats={} utc={utc}", self.sats);
        }
        let opt = |v: Option<f32>| v.map_or("-".to_string(), |v| format!("{v:.1}"));
        format!(
            "EVT GPS FIX lat={:.6} lon={:.6} sats={} hdop={} alt={} utc={utc}",
            self.lat,
            self.lon,
            self.sats,
            opt(self.hdop),
            opt(self.alt_m),
        )
    }
}

/// 文を順に食わせて [`Fix`] を更新する
#[derive(Debug, Default)]
pub struct Tracker {
    fix: Fix,
    rmc_valid: bool,
    gga_valid: bool,
}

impl Tracker {
    /// 1 行食わせる。RMC / GGA として読めたら `true`
    pub fn feed(&mut self, line: &str) -> bool {
        let Some(s) = parse(line) else {
            return false;
        };
        match s {
            Sentence::Rmc {
                time,
                date,
                valid,
                pos,
            } => {
                self.rmc_valid = valid;
                self.fix.time = time.or(self.fix.time);
                self.fix.date = date.or(self.fix.date);
                self.set_pos(pos);
            }
            Sentence::Gga {
                time,
                pos,
                quality,
                sats,
                hdop,
                alt_m,
            } => {
                self.gga_valid = quality != 0;
                self.fix.time = time.or(self.fix.time);
                self.fix.sats = sats;
                self.fix.hdop = hdop;
                self.fix.alt_m = alt_m;
                self.set_pos(pos);
            }
        }
        self.fix.valid = self.rmc_valid && self.gga_valid;
        true
    }

    fn set_pos(&mut self, pos: Option<(f64, f64)>) {
        if let Some((lat, lon)) = pos {
            self.fix.lat = lat;
            self.fix.lon = lon;
        }
    }

    pub fn fix(&self) -> &Fix {
        &self.fix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本体に checksum を付けて 1 行にする
    fn nmea(body: &str) -> String {
        let cs = body.bytes().fold(0u8, |a, b| a ^ b);
        format!("${body}*{cs:02X}")
    }

    const RMC: &str = "GNRMC,063000.000,A,3540.8742,N,13946.0275,E,0.00,0.00,241026,,,A";
    const GGA: &str = "GNGGA,063000.000,3540.8742,N,13946.0275,E,1,08,0.9,40.1,M,39.4,M,,";

    #[test]
    fn line_buf_splits_on_cr_and_lf_and_skips_empty() {
        let mut b = LineBuf::default();
        let mut out = vec![];
        for &c in b"$A\r\n\n$B\n" {
            out.extend(b.push(c));
        }
        assert_eq!(out, vec!["$A".to_string(), "$B".to_string()]);
    }

    #[test]
    fn line_buf_drops_overlong_line_until_newline() {
        let mut b = LineBuf::default();
        for _ in 0..MAX_LINE + 5 {
            assert_eq!(b.push(b'x'), None);
        }
        assert_eq!(b.push(b'\n'), None);
        b.push(b'o');
        assert_eq!(b.push(b'k'), None);
        assert_eq!(b.push(b'\n'), Some("ok".to_string()));
    }

    #[test]
    fn verify_checks_shape_and_checksum() {
        assert_eq!(verify(&nmea("GNRMC,1")), Some("GNRMC,1"));
        assert_eq!(verify(" $GNRMC,1*00 "), None); // checksum 違い
        assert_eq!(verify("GNRMC,1*00"), None); // $ 無し
        assert_eq!(verify("$GNRMC,1"), None); // * 無し
        assert_eq!(verify("$GNRMC,1*ZZ"), None); // 16 進でない
    }

    #[test]
    fn parse_rmc_and_gga() {
        let pos = latlon("3540.8742", "N", "13946.0275", "E");
        let (lat, lon) = pos.unwrap();
        assert!((lat - 35.681237).abs() < 1e-6 && (lon - 139.767125).abs() < 1e-6);
        assert_eq!(
            parse(&nmea(RMC)),
            Some(Sentence::Rmc {
                time: Some(Hms { h: 6, m: 30, s: 0 }),
                date: Some(Ymd {
                    y: 2026,
                    m: 10,
                    d: 24
                }),
                valid: true,
                pos,
            })
        );
        assert_eq!(
            parse(&nmea(GGA)),
            Some(Sentence::Gga {
                time: Some(Hms { h: 6, m: 30, s: 0 }),
                pos,
                quality: 1,
                sats: 8,
                hdop: Some(0.9),
                alt_m: Some(40.1),
            })
        );
    }

    #[test]
    fn parse_south_west_and_blank_fields() {
        let s = parse(&nmea("GPRMC,,V,3340.0000,S,07030.0000,W,,,,,,N")).unwrap();
        assert_eq!(
            s,
            Sentence::Rmc {
                time: None,
                date: None,
                valid: false,
                pos: Some((-33.0 - 40.0 / 60.0, -70.5)),
            }
        );
        let s = parse(&nmea("GNGGA,,,,,,0,,,,,,,,")).unwrap();
        assert_eq!(
            s,
            Sentence::Gga {
                time: None,
                pos: None,
                quality: 0,
                sats: 0,
                hdop: None,
                alt_m: None,
            }
        );
    }

    #[test]
    fn parse_rejects_other_and_broken() {
        assert_eq!(parse(&nmea("GPGSV,3,1,12")), None); // 読まない文
        assert_eq!(parse(&nmea("GNRMC,1,2")), None); // 欄が足りない
        assert_eq!(parse(&nmea("G")), None); // 種別が取れない
        assert_eq!(parse("$GNRMC*00"), None); // checksum 違い
    }

    #[test]
    fn field_helpers_reject_garbage() {
        assert_eq!(hms("0630"), None);
        assert_eq!(hms("ab3000"), None);
        assert_eq!(ymd("2410"), None);
        assert_eq!(ymd("24ab26"), None);
        assert_eq!(latlon("3540.0", "X", "13946.0", "E"), None);
        assert_eq!(latlon("3540.0", "N", "13946.0", "X"), None);
        assert_eq!(latlon("abc", "N", "13946.0", "E"), None);
    }

    #[test]
    fn tracker_needs_both_rmc_and_gga() {
        let mut t = Tracker::default();
        assert!(!t.feed("garbage"));
        assert!(t.feed(&nmea(RMC)));
        assert!(!t.fix().valid); // GGA がまだ
        assert!(t.feed(&nmea(GGA)));
        assert!(t.fix().valid);
        assert_eq!(
            t.fix().evt_line(),
            "EVT GPS FIX lat=35.681237 lon=139.767125 sats=8 hdop=0.9 alt=40.1 utc=2026-10-24T06:30:00Z"
        );

        // 時刻・位置の無い文が来ても直前の値を残し、測位なしに落ちる
        assert!(t.feed(&nmea("GNGGA,,,,,,0,03,,,,,,,")));
        assert!(!t.fix().valid);
        assert!((t.fix().lat - 35.681237).abs() < 1e-6);
        assert_eq!(
            t.fix().evt_line(),
            "EVT GPS NOFIX sats=3 utc=2026-10-24T06:30:00Z"
        );
    }

    #[test]
    fn evt_line_unknown_fields() {
        let f = Fix {
            valid: true,
            ..Fix::default()
        };
        assert_eq!(
            f.evt_line(),
            "EVT GPS FIX lat=0.000000 lon=0.000000 sats=0 hdop=- alt=- utc=-"
        );
        assert_eq!(Fix::default().evt_line(), "EVT GPS NOFIX sats=0 utc=-");
    }
}
