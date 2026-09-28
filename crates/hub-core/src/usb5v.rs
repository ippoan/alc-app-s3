//! M-Bus 5V 出力を USB ホストの有無に追随させる判定 (純粋部分、#202)。
//!
//! CoreS3 は M-Bus へ 5V を出すか (AW9523 BUS_EN) を選べるが、電池なしの個体では
//! 「USB だけ」と「PoE だけ」を 1 つの値で両立できない — 常に出せば PoE 単独給電で
//! ブラウンアウトし、出さなければ USB 給電のベンチでスタックモジュールが
//! 無電源になる。既定は **USB ホスト (PC) が列挙されている間だけ出す** 自動動作に
//! した。PC が居るなら VBUS がレールを支え、PC が落ちれば Core は手を引いて
//! ベース側 (PoE) に任せられる。
//!
//! ただし**自動では足りない現場がある**ので、上書きの設定
//! ([`Bus5vMode`]、`BUS5V AUTO|ON|OFF`) を残してある — PoE のベースを履いた
//! 常設機は `Off` で「絶対に出さない」に固定する。#203 でこの設定を廃止したとき、
//! 現場が `OFF` にしていた NVS の値が無視されて PoE 単独給電で起動しなくなった
//! (#254)。設定と自動判定を 1 本の純関数 [`bus5v_sample`] に畳んである。
//!
//! 実際の i2c 書き込みは hub-ui の i2c ループが行う。ここは「1 秒ごとに読んだ
//! USB の有無」を受け取り、**切り替えるべきときだけ**新しい出力値を返す。
//! 書き込みが成功したら呼び出し側が [`Latch::commit`] で確定させる — 失敗した
//! ままなら次のサンプルでも同じ値を返し続けるので、それが再試行になる。
//!
//! **M-Bus が入力 (PoE) のあいだ Core は出さない** — この判定規則そのものは
//! 変えていない。呼び手 (hub-ui) は [`bus5v_sample`] が `None` を返す間
//! [`Latch::update`] にサンプルを渡すこと自体を止め、切り替えの過渡を
//! 起こさない (Refs #211)。
//!
//! その `bus_in` を**いつ確定してよいか**の猶予もここに置く
//! ([`BUS_IN_GRACE_MS`] / [`bus_in_absent_confirmed`]、Refs #254) と、判定そのもの
//! ([`BusInJudge`]、材料は AXP2101 の TS = M-Bus 5V の分圧) もここに置く。
//! W5500 の probe 周期 (hub-drivers) も同じ猶予を見るので、**規則は 1 つ**に
//! しておかないとビルドごとに挙動が分かれる。

use crate::protocol::Bus5vMode;

/// USB ホストの有無から M-Bus 5V 出力を決めるラッチ。
///
/// 同じ値が **2 回連続** したときだけ出力を変える。USB の列挙状態は抜き差しの
/// 前後や driver の初期化直後にばたつくので、1 サンプルで i2c を叩くと 5V が
/// チャタリングする。出力の初期値は `false` (起動時は出さない)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Latch {
    /// 直前のサンプル。まだ 1 度も読んでいなければ `None`
    last: Option<bool>,
    /// いま出力している値 (i2c 書き込みの成功を [`Latch::commit`] で確定したもの)
    out: bool,
}

impl Latch {
    /// USB ホストの有無を 1 サンプル与える。
    ///
    /// 直前のサンプルと同じ値が続き、かつ現在の出力と異なるときだけ
    /// `Some(新しい出力値)` を返す。それ以外は `None` (= i2c を叩かない)。
    /// 出力値そのものは変えない — i2c 書き込みが成功したら [`Latch::commit`]
    /// を呼ぶこと。呼ばなければ次のサンプルでも同じ値を返す (= 再試行)。
    pub fn update(&mut self, usb: bool) -> Option<bool> {
        let stable = self.last == Some(usb);
        self.last = Some(usb);
        if stable && self.out != usb {
            Some(usb)
        } else {
            None
        }
    }

    /// `update` が返した値の i2c 書き込みが成功したことを伝え、出力値を確定する。
    pub fn commit(&mut self, out: bool) {
        self.out = out;
    }

    /// いま出力している値
    pub fn out(&self) -> bool {
        self.out
    }
}

/// 設定 ([`Bus5vMode`]) と M-Bus の外部給電判定・USB ホストの有無から、
/// [`Latch::update`] へ渡すサンプルを決める (Refs #254)。
///
/// 戻り値の `None` は「**[`Latch`] に触らない**」= その周回では
/// `power::set_ext_5v_out` を呼ばない、の意味。`Some(v)` は「出力を `v` に
/// したい」で、実際に i2c を叩くかは [`Latch`] の debounce が決める。
///
/// - `Off`: 常に `Some(false)`。**絶対に出さない**。起動時の出力は `false`
///   なので [`Latch`] は何も返さず i2c は 1 度も叩かれないが、**稼働中に
///   `BUS5V OFF` へ変えたときだけ 1 回落としに行く** — 設定した現場が
///   再起動を待たずに「出ていない」を確かめられる
/// - `On`: 常に `Some(true)`。USB ホストの有無も `bus_in` も見ない
///   (PoE のベースでは使わないこと。[`Bus5vMode`] の doc 参照)
/// - `Auto` (既定): `bus_in` が `Some(false)` (= M-Bus は外部給電でないと
///   確定済み) のときだけ USB ホストの有無を渡す。`None` (未判定) と
///   `Some(true)` (PoE 等で外部給電中) は `None` を返し、切り替えの過渡
///   そのものを起こさない (Refs #211) — 未判定の間は fail-closed
pub fn bus5v_sample(mode: Bus5vMode, bus_in: Option<bool>, usb_host: bool) -> Option<bool> {
    match mode {
        Bus5vMode::Off => Some(false),
        Bus5vMode::On => Some(true),
        Bus5vMode::Auto => (bus_in == Some(false)).then_some(usb_host),
    }
}

/// 「M-Bus は外部給電ではない」(`HubStatus::bus_in = Some(false)`) と確定する
/// までの猶予 [ms] (Refs #254)。
///
/// 起動直後に「5V が来ていない」と 1 回読んだだけで確定すると、**PoE 単独給電で
/// 起動できなくなる** — PoE スプリッタ → ベースの順に電気が回るので Core の boot に
/// 間に合わないことがあり、そこで誤確定すると Core が同じ 5V レールを両側から
/// 駆動してブラウンアウトする (#254 の現場症状。当時の判定材料は W5500 probe)。
/// だから**猶予の間は `None` (まだ分からない) のまま置く**。
///
/// 値の決め方 (5 秒→15 秒、実機ログから改定、Refs #254) — `#256` (猶予 5 秒)
/// を焼いた実機で症状が変わらず、猶予切れ後に `EVT ETH_PROBE_OK n=11` が出た。
/// `EVT ETH_PROBE_OK n=11` は 10 秒間隔の 1 回目なので、W5500 が応答できる
/// ようになった真値は **4.5〜14.5 秒の間**。15 秒はその上限側を取った値。
/// 次の実機ログでは 500 ms グリッド 30 回ぶんの分解能で `n` が読めるので、
/// そこで詰め直せる。
///
/// 猶予の間は Core が M-Bus 5V を出さないので、USB 給電のベンチ (ベースが
/// 自前電源を持たず RS232M / LAN 13.2 を積む構成、Refs #76) ではスタック
/// モジュールがこの秒数だけ無電源になる。壊れはしないが疎通が遅れる。
pub const BUS_IN_GRACE_MS: u64 = 15_000;

/// 起動からの経過時間 `uptime_ms` [ms] から、`HubStatus::bus_in` を
/// `Some(false)` (= M-Bus は外部給電でない) と確定してよいかを返す。
///
/// `false` のあいだは `None` (まだ分からない) のまま置くこと — `None` の間は
/// 呼び手 ([`Latch`] へサンプルを渡す hub-ui) が切り替え自体を起こさないので
/// fail-closed (5V を出さない側) に倒れる。
pub fn bus_in_absent_confirmed(uptime_ms: u64) -> bool {
    uptime_ms >= BUS_IN_GRACE_MS
}

/// AXP2101 の TS ADC (0x36/0x37、14 bit、0.5 mV/LSB) の生値のうち、測れていない
/// (ADC 無効) とみなす下限。M5Unified `_core_s3_ext_output_unsafe` と同じ
/// `0x3FFF - 32`
pub const TS_RAW_INVALID: u16 = 0x3FFF - 32;

/// TS がこれを超えたら M-Bus に 5V が来ている (生値。4000 = 2.0 V)。
/// CoreS3 の TS には BUS_OUT (M-Bus 5V) の 1:1 分圧が入っている
/// (m5stack/M5Unified#352)。5 V なら約 2.5 V、無ければ 0 V 近く
pub const TS_RAW_BUS_5V: u16 = 4000;

/// Core が自分の 5V 出力を止めた直後、TS を信じずに待つ時間 [ms]。
/// TS の ADC は実電圧に最大 0.7 s ほど遅れる (M5Unified#352 の実測) ので、
/// 止めた直後に読むと自分の出していた 5V を「外部給電」と読み違える
pub const TS_SETTLE_AFTER_OFF_MS: u64 = 1_500;

/// TS の生値を「M-Bus に 5V が来ているか」に読み替える。
/// **Core 自身が 5V を出していない (BUS_EN = 0) ときだけ意味がある** —
/// 出している間の TS は自分の出力を測っているだけ。
/// 測れていない値 ([`TS_RAW_INVALID`] 以上) は `None`
pub fn ts_bus_5v(raw: u16) -> Option<bool> {
    (raw < TS_RAW_INVALID).then_some(raw > TS_RAW_BUS_5V)
}

/// M-Bus の外部給電判定 (`HubStatus::bus_in`) を TS の読みから決める (Refs #211, #254)。
///
/// 以前は起動時の W5500 probe が通ったら「外部給電あり」としていたが、**W5500 は
/// Core の 3.3 V だけでも SPI に応答する**ことがあり、PoE も外部電源も無い USB
/// 給電の機で `Some(true)` に誤判定 → `AUTO` が 5V を出さず Base LAN のリンクが
/// 張れなかった (2026-09-28 実機)。TS は M-Bus 5V そのものを測るので、
/// 応答の有無に左右されない。
///
/// 規則:
/// - 同じ読みが **2 回連続**したときだけ動かす (1 秒ポーリングのばたつき除け)
/// - 5V あり → `Some(true)`。いつでも (猶予の内でも) 入れる
/// - 5V なし → `Some(false)`。**猶予 ([`BUS_IN_GRACE_MS`]) の内は入れない** —
///   PoE の給電が Core の起動に間に合わないことがあるため (#254)。猶予の後は、
///   稼働中に外部給電が抜けた場合にも `Some(true)` から `Some(false)` へ戻す
/// - 読めない (`ts == None`: Core が出している最中・出し止めた直後・i2c 失敗・
///   ADC 無効) は連続の数え直しにして `bus_in` を触らない。**読めないまま
///   `Some(false)` にはしない** (fail-closed。M5Unified も読めなければ危険側)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusInJudge {
    last: Option<bool>,
}

impl BusInJudge {
    /// TS の読み `ts` ([`ts_bus_5v`] の結果、読めなければ `None`) と今の
    /// `bus_in`、起動からの経過時間を与え、`bus_in` を変えるべきときだけ
    /// `Some(新しい値)` を返す。それ以外は `None` (= そのまま)
    pub fn update(
        &mut self,
        ts: Option<bool>,
        current: Option<bool>,
        uptime_ms: u64,
    ) -> Option<bool> {
        let prev = std::mem::replace(&mut self.last, ts);
        let v = ts?;
        if prev != Some(v) || current == Some(v) {
            return None;
        }
        (v || bus_in_absent_confirmed(uptime_ms)).then_some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_not_out_and_first_sample_never_switches() {
        let mut l = Latch::default();
        assert!(!l.out());
        // 初回は「2 回連続」を満たさない
        assert_eq!(l.update(true), None);
        assert!(!l.out());
    }

    #[test]
    fn alternating_input_never_switches() {
        let mut l = Latch::default();
        for usb in [true, false, true, false, true, false] {
            assert_eq!(l.update(usb), None);
        }
        assert!(!l.out());
    }

    #[test]
    fn two_consecutive_true_switches_on_once() {
        let mut l = Latch::default();
        assert_eq!(l.update(true), None);
        assert_eq!(l.update(true), Some(true));
        l.commit(true);
        assert!(l.out());
        // 以後 true が続いても再通知しない (出力と同じ値なので)
        assert_eq!(l.update(true), None);
        assert_eq!(l.update(true), None);
        assert!(l.out());
    }

    #[test]
    fn two_consecutive_false_switches_off() {
        let mut l = Latch::default();
        l.update(true);
        assert_eq!(l.update(true), Some(true));
        l.commit(true);
        // 1 回きりの false では戻さない (ばたつき)
        assert_eq!(l.update(false), None);
        assert_eq!(l.update(true), None);
        assert!(l.out());
        // 2 回連続で初めて止める
        assert_eq!(l.update(false), None);
        assert_eq!(l.update(false), Some(false));
        l.commit(false);
        assert!(!l.out());
        assert_eq!(l.update(false), None);
    }

    #[test]
    fn update_alone_does_not_change_out() {
        // commit するまでは i2c が成功したとみなさない
        let mut l = Latch::default();
        l.update(true);
        assert_eq!(l.update(true), Some(true));
        assert!(!l.out());
    }

    #[test]
    fn uncommitted_switch_is_retried_on_next_sample() {
        // i2c 書き込みに失敗 (commit なし) → 同じ値が続く限り毎サンプル再提示
        let mut l = Latch::default();
        l.update(true);
        assert_eq!(l.update(true), Some(true));
        assert_eq!(l.update(true), Some(true));
        assert_eq!(l.update(true), Some(true));
        assert!(!l.out());
        // 再試行が成功したら止まる
        l.commit(true);
        assert!(l.out());
        assert_eq!(l.update(true), None);
    }

    #[test]
    fn uncommitted_switch_off_is_retried() {
        let mut l = Latch::default();
        l.update(true);
        l.update(true);
        l.commit(true);
        l.update(false);
        assert_eq!(l.update(false), Some(false));
        // 失敗: 出力は true のまま、次も false を再提示
        assert!(l.out());
        assert_eq!(l.update(false), Some(false));
        l.commit(false);
        assert!(!l.out());
        assert_eq!(l.update(false), None);
    }

    #[test]
    fn uncommitted_switch_is_dropped_when_usb_reverts() {
        // 失敗中に USB が元の値へ戻ったら、もう切り替える必要は無い
        let mut l = Latch::default();
        l.update(true);
        assert_eq!(l.update(true), Some(true));
        // 1 回目の false はばたつき扱い (2 回連続の debounce は維持)
        assert_eq!(l.update(false), None);
        // false が続いても出力 (false) と同じなので叩かない
        assert_eq!(l.update(false), None);
        assert!(!l.out());
        // 再び true が 2 回続けば改めて提示
        assert_eq!(l.update(true), None);
        assert_eq!(l.update(true), Some(true));
    }

    #[test]
    fn bus_in_stays_unknown_during_grace() {
        // 猶予の内は「まだ分からない」— ここで `Some(false)` を入れると #254
        assert!(!bus_in_absent_confirmed(0));
        assert!(!bus_in_absent_confirmed(BUS_IN_GRACE_MS - 1));
    }

    #[test]
    fn bus_in_absent_is_confirmed_after_grace() {
        assert!(bus_in_absent_confirmed(BUS_IN_GRACE_MS));
        assert!(bus_in_absent_confirmed(BUS_IN_GRACE_MS * 10));
    }

    #[test]
    fn auto_only_samples_when_bus_in_is_confirmed_absent() {
        // 未判定 / 外部給電中は Latch に触らない = set_ext_5v_out を呼ばない
        assert_eq!(bus5v_sample(Bus5vMode::Auto, None, true), None);
        assert_eq!(bus5v_sample(Bus5vMode::Auto, None, false), None);
        assert_eq!(bus5v_sample(Bus5vMode::Auto, Some(true), true), None);
        assert_eq!(bus5v_sample(Bus5vMode::Auto, Some(true), false), None);
        // 外部給電でないと確定して初めて USB ホストの有無に追随する (#203 の固定動作)
        assert_eq!(bus5v_sample(Bus5vMode::Auto, Some(false), true), Some(true));
        assert_eq!(
            bus5v_sample(Bus5vMode::Auto, Some(false), false),
            Some(false)
        );
    }

    #[test]
    fn off_never_asks_for_output() {
        // PoE の現場の設定。bus_in / usb_host が何であれ「出さない」
        for bus_in in [None, Some(true), Some(false)] {
            for usb in [true, false] {
                assert_eq!(bus5v_sample(Bus5vMode::Off, bus_in, usb), Some(false));
            }
        }
    }

    #[test]
    fn off_costs_no_i2c_from_boot_but_switches_back_off_when_set_at_runtime() {
        // 起動時の出力は false なので、OFF のまま回しても Latch は何も返さない
        let mut l = Latch::default();
        for _ in 0..5 {
            assert_eq!(
                l.update(bus5v_sample(Bus5vMode::Off, None, true).unwrap()),
                None
            );
        }
        assert!(!l.out());
        // 既に出ている状態 (AUTO で出した後) から OFF にしたら 1 回落としに行く
        let mut l = Latch::default();
        l.update(true);
        assert_eq!(l.update(true), Some(true));
        l.commit(true);
        assert_eq!(
            l.update(bus5v_sample(Bus5vMode::Off, None, true).unwrap()),
            None
        );
        assert_eq!(
            l.update(bus5v_sample(Bus5vMode::Off, None, true).unwrap()),
            Some(false)
        );
    }

    #[test]
    fn update_never_returns_the_opposite_of_its_sample() {
        // [`Latch::update`] は `None` か `Some(渡した値)` しか返さない。
        // これが「`OFF` なら絶対に出さない」の土台 — 呼び手は `update` が返した
        // 値をそのまま `set_ext_5v_out` へ渡すので、**サンプルが `false` である
        // 限り `true` が渡ることは構造上あり得ない**
        for pre_out in [false, true] {
            for a in [false, true] {
                for b in [false, true] {
                    let mut l = Latch::default();
                    if pre_out {
                        l.update(true);
                        l.update(true);
                        l.commit(true);
                    }
                    for sample in [a, b, a, b, b, b] {
                        if let Some(desired) = l.update(sample) {
                            assert_eq!(desired, sample, "update が渡した値と違う値を返した");
                            l.commit(desired);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn off_can_never_reach_set_ext_5v_out_true() {
        // ★ オーナー決定「`OFF` なら絶対に M-Bus 5V を出さない」の機械証明。
        //
        // hub-ui (`crates/hub-ui/src/lib.rs`) の唯一の呼び出し経路と同じ形で回す:
        //
        //     if let Some(sample) = usb5v::bus5v_sample(mode, bus_in, usb) {
        //         if let Some(desired) = usb5v.update(sample) {
        //             power::set_ext_5v_out(&mut i2c, desired)   // ← 呼び出しは 1 か所
        //
        // `set_ext_5v_out` に渡りうる値は `update` の戻り値だけで、その `update`
        // に渡るのは `bus5v_sample` の戻り値だけ (門はこの 1 本)。だから
        // 「`OFF` のとき `desired == true` になる入力が 1 つも無い」ことを、
        // **Latch の事前状態 (出ている / 出ていない) × `bus_in` × USB ホストの
        // 有無 × 連続サンプル**で尽くせば、`set_ext_5v_out(true)` への到達が
        // 無いことの証明になる。USB ホストが列挙されている経路 (`usb = true`)
        // も含む — #254 の現場はそこで 5V が出ていた
        for pre_out in [false, true] {
            let mut l = Latch::default();
            if pre_out {
                // `AUTO` で既に出している状態を作ってから `OFF` に変える
                l.update(true);
                l.update(true);
                l.commit(true);
                assert!(l.out());
            }
            for bus_in in [None, Some(true), Some(false)] {
                for usb in [true, false] {
                    for _ in 0..4 {
                        let sample = bus5v_sample(Bus5vMode::Off, bus_in, usb)
                            .expect("OFF は Latch に必ず『出さない』を渡す");
                        assert!(!sample, "OFF なのに Latch へ true を渡した");
                        if let Some(desired) = l.update(sample) {
                            // ここが `set_ext_5v_out(desired)` へ渡る唯一の値
                            assert!(!desired, "OFF なのに 5V を出そうとした");
                            l.commit(desired);
                        }
                    }
                }
            }
            // 事前に出していた個体も、最後は必ず「出していない」に落ちている
            assert!(!l.out());
        }
    }

    #[test]
    fn on_always_asks_for_output() {
        // USB 電源アダプタのベンチ (USB ホストとして列挙されない) 向け
        for bus_in in [None, Some(true), Some(false)] {
            for usb in [true, false] {
                assert_eq!(bus5v_sample(Bus5vMode::On, bus_in, usb), Some(true));
            }
        }
    }

    #[test]
    fn default_mode_is_the_fixed_behaviour_from_203() {
        // NVS 未設定の端末は #203 以降の固定動作のまま (挙動を変えない)
        let mode = Bus5vMode::default();
        assert_eq!(bus5v_sample(mode, None, true), None);
        assert_eq!(bus5v_sample(mode, Some(false), true), Some(true));
    }

    #[test]
    fn grace_outlasts_the_earliest_possible_5v_out() {
        // hub-ui は起動 3 秒後に USB を読み始め、同じ値が 2 サンプル (1 秒間隔)
        // 続いて初めて出す = 最短 4 秒。そこまでは未判定のままでなければ、
        // 5V を出す門が開いたまま誤確定する隙が残る
        assert!(!bus_in_absent_confirmed(4_000));
    }

    #[test]
    fn ts_reads_bus_5v_above_2v() {
        // 5V の 1:1 分圧 ≒ 2.5V = 5000
        assert_eq!(ts_bus_5v(5000), Some(true));
        assert_eq!(ts_bus_5v(TS_RAW_BUS_5V + 1), Some(true));
        // 閾値ちょうどと 0V 近くは「来ていない」
        assert_eq!(ts_bus_5v(TS_RAW_BUS_5V), Some(false));
        assert_eq!(ts_bus_5v(0), Some(false));
    }

    #[test]
    fn ts_invalid_adc_is_unknown() {
        assert_eq!(ts_bus_5v(TS_RAW_INVALID - 1), Some(true));
        assert_eq!(ts_bus_5v(TS_RAW_INVALID), None);
        assert_eq!(ts_bus_5v(0x3FFF), None);
    }

    const AFTER: u64 = BUS_IN_GRACE_MS;

    #[test]
    fn judge_needs_two_consecutive_reads() {
        let mut j = BusInJudge::default();
        assert_eq!(j.update(Some(true), None, 1_000), None);
        assert_eq!(j.update(Some(true), None, 2_000), Some(true));
    }

    #[test]
    fn judge_alternating_reads_never_decide() {
        let mut j = BusInJudge::default();
        for (i, v) in [true, false, true, false].into_iter().enumerate() {
            assert_eq!(j.update(Some(v), None, AFTER + i as u64), None);
        }
    }

    #[test]
    fn judge_external_5v_is_taken_even_within_grace() {
        // PoE で起動した機は猶予を待たずに「外部給電あり」へ
        let mut j = BusInJudge::default();
        j.update(Some(true), None, 3_000);
        assert_eq!(j.update(Some(true), None, 4_000), Some(true));
    }

    #[test]
    fn judge_absent_waits_for_grace() {
        let mut j = BusInJudge::default();
        j.update(Some(false), None, 3_000);
        assert_eq!(j.update(Some(false), None, 4_000), None);
        assert_eq!(j.update(Some(false), None, AFTER - 1), None);
        assert_eq!(j.update(Some(false), None, AFTER), Some(false));
    }

    #[test]
    fn judge_w5500_answering_on_3v3_no_longer_matters() {
        // 2026-09-28 の現場: USB 給電のみ、TS は 0V 近く。猶予の後に Some(false)
        // へ確定し、AUTO が 5V を出せるようになる
        let mut j = BusInJudge::default();
        let mut got = None;
        for t in (3_000..=AFTER + 1_000).step_by(1_000) {
            if let Some(v) = j.update(ts_bus_5v(20), got, t) {
                got = Some(v);
            }
        }
        assert_eq!(got, Some(false));
    }

    #[test]
    fn judge_unreadable_never_confirms_absent() {
        let mut j = BusInJudge::default();
        for t in (3_000..AFTER * 3).step_by(1_000) {
            assert_eq!(j.update(None, None, t), None);
        }
    }

    #[test]
    fn judge_unreadable_resets_the_streak() {
        let mut j = BusInJudge::default();
        j.update(Some(false), None, AFTER);
        j.update(None, None, AFTER + 1_000);
        assert_eq!(j.update(Some(false), None, AFTER + 2_000), None);
        assert_eq!(j.update(Some(false), None, AFTER + 3_000), Some(false));
    }

    #[test]
    fn judge_does_not_repeat_current_value() {
        let mut j = BusInJudge::default();
        j.update(Some(true), Some(true), 1_000);
        assert_eq!(j.update(Some(true), Some(true), 2_000), None);
        j.update(Some(false), Some(false), AFTER);
        assert_eq!(j.update(Some(false), Some(false), AFTER + 1_000), None);
    }

    #[test]
    fn judge_follows_external_power_removed_and_restored() {
        // 稼働中に Base の外部電源が抜けた → 戻った
        let mut j = BusInJudge::default();
        j.update(Some(false), Some(true), AFTER);
        assert_eq!(
            j.update(Some(false), Some(true), AFTER + 1_000),
            Some(false)
        );
        j.update(Some(true), Some(false), AFTER + 2_000);
        assert_eq!(j.update(Some(true), Some(false), AFTER + 3_000), Some(true));
    }
}
