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
//! USB の有無」を受け取り、**切り替えるべき瞬間だけ**新しい出力値を返す。

/// USB ホストの有無から M-Bus 5V 出力を決めるラッチ。
///
/// 同じ値が **2 回連続** したときだけ出力を変える。USB の列挙状態は抜き差しの
/// 前後や driver の初期化直後にばたつくので、1 サンプルで i2c を叩くと 5V が
/// チャタリングする。出力の初期値は `false` (起動時は出さない)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Latch {
    /// 直前のサンプル。まだ 1 度も読んでいなければ `None`
    last: Option<bool>,
    /// いま出力している値
    out: bool,
}

impl Latch {
    /// USB ホストの有無を 1 サンプル与える。
    ///
    /// 直前のサンプルと同じ値が続き、かつ現在の出力と異なるときだけ
    /// `Some(新しい出力値)` を返して内部状態を更新する。それ以外は `None`
    /// (= i2c を叩かない)。
    pub fn update(&mut self, usb: bool) -> Option<bool> {
        let stable = self.last == Some(usb);
        self.last = Some(usb);
        if stable && self.out != usb {
            self.out = usb;
            Some(usb)
        } else {
            None
        }
    }

    /// いま出力している値
    pub fn out(&self) -> bool {
        self.out
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
        // 1 回きりの false では戻さない (ばたつき)
        assert_eq!(l.update(false), None);
        assert_eq!(l.update(true), None);
        assert!(l.out());
        // 2 回連続で初めて止める
        assert_eq!(l.update(false), None);
        assert_eq!(l.update(false), Some(false));
        assert!(!l.out());
        assert_eq!(l.update(false), None);
    }
}
