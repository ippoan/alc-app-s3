//! M-Bus 5V 出力を USB ホストの有無に追随させる判定 (純粋部分、#202)。
//!
//! CoreS3 は M-Bus へ 5V を出すか (AW9523 BUS_EN) を選べるが、電池なしの個体では
//! 「USB だけ」と「PoE だけ」を設定で両立できない — 常に出せば PoE 単独給電で
//! ブラウンアウトし、出さなければ USB 給電のベンチでスタックモジュールが
//! 無電源になる。そこで設定を持たず、**USB ホスト (PC) が列挙されている間だけ
//! 出す**を唯一の動作にした。PC が居るなら VBUS がレールを支え、PC が落ちれば
//! Core は手を引いてベース側 (PoE) に任せられる。
//!
//! 実際の i2c 書き込みは hub-ui の i2c ループが行う。ここは「1 秒ごとに読んだ
//! USB の有無」を受け取り、**切り替えるべきときだけ**新しい出力値を返す。
//! 書き込みが成功したら呼び出し側が [`Latch::commit`] で確定させる — 失敗した
//! ままなら次のサンプルでも同じ値を返し続けるので、それが再試行になる。
//!
//! **M-Bus が入力 (PoE) のあいだ Core は出さない** — この判定規則そのものは
//! 変えていない。呼び手 (hub-ui) が `HubStatus::bus_in` を見て、M-Bus が外部
//! 給電中と分かっている間は [`Latch::update`] にサンプルを渡すこと自体を止め、
//! 切り替えの過渡を起こさない (Refs #211)。
//!
//! その `bus_in` を**いつ確定してよいか**の猶予もここに置く
//! ([`BUS_IN_GRACE_MS`] / [`bus_in_absent_confirmed`]、Refs #254)。判定の入力は
//! W5500 の probe (hub-drivers) と起動からの経過時間 (hub-ui) で持ち主が違うが、
//! **規則は 1 つ**にしておかないとビルドごとに挙動が分かれる。

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

/// 「M-Bus は外部給電ではない」(`HubStatus::bus_in = Some(false)`) と確定する
/// までの猶予 [ms] (Refs #254)。
///
/// 起動直後の W5500 probe が 1 回失敗しただけで確定すると、**PoE 単独給電で
/// 起動できなくなる** — PoE スプリッタ → ベース → W5500 の順に電気が回るので
/// Core の boot に W5500 が間に合わないことがあり、そこで誤確定すると Core が
/// 同じ 5V レールを両側から駆動してブラウンアウトする (#254 の現場症状)。
/// だから**猶予の間は `None` (まだ分からない) のまま置く**。
///
/// 値の決め方 — 猶予の間は Core が M-Bus 5V を出さないので、USB 給電のベンチ
/// (ベースが自前電源を持たず RS232M / LAN 13.2 を積む構成、Refs #76) では
/// スタックモジュールがこの秒数だけ無電源になる。壊れはしないが疎通が遅れる。
/// 一方 hub-ui は起動 3 秒後まで USB を読まず、同じ値が 2 サンプル続いて初めて
/// 出すので **元々 4 秒より早くは出ない**。5 秒ならベンチの実害は 1 秒程度に
/// 収まり、その間に W5500 へ 500 ms 間隔で 10 回の probe を与えられる。
/// 現場で足りなければ上げてよい — 代償はベンチの疎通が数秒遅れることだけ。
pub const BUS_IN_GRACE_MS: u64 = 5_000;

/// 起動からの経過時間 `uptime_ms` [ms] から、`HubStatus::bus_in` を
/// `Some(false)` (= M-Bus は外部給電でない) と確定してよいかを返す。
///
/// `false` のあいだは `None` (まだ分からない) のまま置くこと — `None` の間は
/// 呼び手 ([`Latch`] へサンプルを渡す hub-ui) が切り替え自体を起こさないので
/// fail-closed (5V を出さない側) に倒れる。
pub fn bus_in_absent_confirmed(uptime_ms: u64) -> bool {
    uptime_ms >= BUS_IN_GRACE_MS
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
    fn grace_outlasts_the_earliest_possible_5v_out() {
        // hub-ui は起動 3 秒後に USB を読み始め、同じ値が 2 サンプル (1 秒間隔)
        // 続いて初めて出す = 最短 4 秒。そこまでは未判定のままでなければ、
        // 5V を出す門が開いたまま誤確定する隙が残る
        assert!(!bus_in_absent_confirmed(4_000));
    }
}
