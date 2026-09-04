//! NFC タップの重複抑止 (issue #103)。
//!
//! # なぜエッジ判定では足りないか
//!
//! 元の実装は「直前に読んだ値と違えば発火、読めなかったら直前値をクリア」という
//! **エッジ判定**だった。モバイル FeliCa (おサイフケータイ / モバイル Suica) は
//! セキュアエレメントの起床や かざし方の揺れで応答が断続的になり、20ms の
//! ポーリングが **1 回空振りしただけで直前値が消える**。その直後に同じカードを
//! 再検知すると、**1 タップで 2 回発火**する (2026-07-21 実機確認、issue #103)。
//!
//! # なぜ「ビープ 2 回」で済まなくなったか
//!
//! 打刻イベント (`kind="timecard"`、Refs #134) を送るようになると、1 タップが
//! **別々の seq を持つ 2 行**になる。サーバ側の冪等は
//! `UNIQUE (tenant_id, device_id, seq)` なので「同じイベントの再送」しか防げず、
//! **「別イベントが 2 つ生まれた」これは素通りする**。結果、1 回かざした人の
//! `time_punches` に 2 行入り、front は打刻の並びから出勤/退勤を判定するため
//! **出勤と退勤を同時に打った**ことになる。賃金計算に入る誤データなので、
//! 端末側で塞ぐ (サーバ側で握るとブラウザ版キオスクの同じ穴が残る)。
//!
//! # 方式: 「離れて N ミリ秒経つまで、まだ同じタップ」(debounce)
//!
//! **「同じ値は N ms に 1 回まで」(rate limiter) ではない。** そう作ると、
//! カードがリーダーに載りっぱなしのときに **N 秒ごとに発火し続ける** —
//! 壁付けの常設打刻機で財布やスマホを置き忘れると打刻が延々と入る。
//! 存在検知ゲートも助けにならない (`triggered` は立ちっぱなし、`TRIGGER_STUCK`
//! の再較正は「何も読めない」ときだけ、ベースライン追従は非トリガ時だけ)。
//! **旧エッジ判定はこのケースは正しく 1 回で済ませていた**ので、ここを取り違えると
//! #103 を直した代わりに動いていたケースを壊す。
//!
//! 正しい要件は **「カードが載っている間は、まだ同じ 1 タップ」**。したがって
//! 保持するのは「最後に**発火**した時刻」ではなく **「最後に**見た**時刻」**で、
//! 発火の有無にかかわらず毎回更新する。載っている間は毎ポーリング更新されるので
//! 経過がクールダウンに達せず、1 回の滞在は 1 回しか発火しない。
//! 空振り (20〜40ms) も同じ仕組みで吸収される。
//!
//! 読めなかったことを理由に状態をクリアはしない — **空振りで状態が消えることが
//! #103 の原因**なので、ロストの検出そのものをやめて時間だけで判断する。
//! ポーリングは F→A→B の掃引で 1 周期が可変なので、回数ベースのヒステリシスより
//! 時間ベースの方が素直 (issue #103 の「対処案」どおり)。
//!
//! # 読み取り失敗を「離れた」と数えない
//!
//! 検知の成否だけで判断すると足りない。**カードが載ったままでも読み取りは
//! 頻繁に失敗する** (2026-09-04 実機ログ: `Failed to RequestResponse` /
//! `SELECT EF 2F01 失敗` / `deselect failed` が成功の前後に出る)。とくに
//! 免許証 (Type-B の APDU セッション) は**再読が 3.4〜4.1 秒に 1 回しか
//! 成功しない**ため、1 秒のクールダウンを毎回越えて再発火していた
//! (実機で 1 枚の免許証が 7.5 秒で 3 打刻)。
//!
//! そこで [`TapGate::touch`] を用意し、**アンテナの存在検知が立っている間は
//! 毎周期「まだ見えている」ことにする**。読めたかどうかではなく
//! **カードが物理的に載っているか**で「同じタップか」を決める。
//!
//! # 残課題: 覚えているのは直近 1 枚だけ
//!
//! 1 つの財布に FeliCa が 2 枚入っているなど、**A と B が交互に読まれると
//! 毎回発火する** (key が変わるたびに「別のカード」と判定されるため)。
//! 実機で起きるかは未確認。起きるなら直近数枚を覚える形に広げる。

/// 「カードが離れた」とみなすまでの無検知時間 [ms]。
///
/// **タップの間隔ではなく「最後に見てからの経過」の閾値。** これより長く
/// 検知が途切れて初めて次のタップとして扱う。短すぎると空振りを吸収できず
/// #103 が再発し、長すぎると「打刻し直し」までの待ち時間になる。
///
/// **1 秒はユーザーの指定** (issue #103 の当初の目安は 2〜3 秒だったが、
/// かざし直しの待ちを短くしたいとの判断)。モバイル FeliCa の空振りは
/// 20〜40ms 程度なので 1 秒でも十分吸収でき、実運用のかざし直しは 1 秒より
/// 長くかかるため打刻し直しも妨げない。
pub const DEFAULT_COOLDOWN_MS: u64 = 1_000;

/// タップの重複抑止。
///
/// **全系統 (FeliCa IDm / NFC-A UID / 免許証 / 車検証) で 1 つを共有する。**
/// 別のカードに持ち替えれば key が変わって即発火するので分ける必要がない。
/// むしろ共有した方が、同じカードが F と B の両方で読めるようなケース
/// (読み取りが揺れて別経路に落ちる) でも 1 タップ 1 回に収まる。
#[derive(Debug, Clone)]
pub struct TapGate {
    last: Option<(String, u64)>,
    cooldown_ms: u64,
}

impl TapGate {
    pub fn new(cooldown_ms: u64) -> Self {
        Self {
            last: None,
            cooldown_ms,
        }
    }

    /// 発火してよいか。**呼ぶたびに「最後に見た時刻」を更新する**
    /// (発火したときだけ更新すると、載せっぱなしで N 秒ごとに発火する —
    /// モジュール doc 参照)。
    ///
    /// - `key`: カードを一意に表す文字列 (IDm / UID / 免許証の 16 桁)
    /// - `now_ms`: **単調増加**の時刻 (稼働時間)。壁時計を渡さないこと —
    ///   NTP 同期で時刻が飛ぶとクールダウンが飛ぶ
    ///
    /// 別のカードに変わったときは即座に `true` (かざし替えは待たせない)。
    /// 「まだ見えている」ことにする (発火はしない)。
    ///
    /// **読み取りの成否ではなくカードの存在で判断するための口。**
    /// アンテナの存在検知が立っている間これを毎周期呼べば、読み取りが
    /// 失敗し続けても「離れた」と数えられない。まだ何も読んでいない
    /// (`last` が None) ときは何もしない — 触っても抑止する対象が無い。
    pub fn touch(&mut self, now_ms: u64) {
        if let Some((_, last_seen)) = &mut self.last {
            *last_seen = now_ms;
        }
    }

    pub fn should_fire(&mut self, key: &str, now_ms: u64) -> bool {
        let fire = match &self.last {
            // 同じカードを最後に見てから cooldown 経っていなければ「まだ同じタップ」
            Some((last_key, last_seen)) if last_key == key => {
                // saturating_sub: now_ms が巻き戻っても「経過 0」に倒して抑止側に寄せる
                now_ms.saturating_sub(*last_seen) >= self.cooldown_ms
            }
            _ => true,
        };
        self.last = Some((key.to_string(), now_ms));
        fire
    }
}

impl Default for TapGate {
    fn default() -> Self {
        Self::new(DEFAULT_COOLDOWN_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_tap_fires() {
        let mut g = TapGate::default();
        assert!(g.should_fire("A", 0));
    }

    /// **issue #103 の本体**: ポーリングが空振りして再検知しても、
    /// クールダウン内なら 2 回目は発火しない
    #[test]
    fn same_key_within_cooldown_is_suppressed() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("01401D0B1D37B660", 1_000));
        // モバイル FeliCa の空振り → 直後の再検知 (40ms 後)
        assert!(!g.should_fire("01401D0B1D37B660", 1_040));
        // 20ms 間隔で読め続けている限り、何秒経っても「まだ同じタップ」
        assert!(!g.should_fire("01401D0B1D37B660", 1_060));
    }

    /// 検知が途切れて cooldown 経てば同じカードでも再び打刻できる (打刻し直し)
    #[test]
    fn same_key_after_cooldown_fires_again() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("A", 1_000));
        assert!(g.should_fire("A", 2_000)); // ちょうど境界は発火する
    }

    /// **載せっぱなしでは 1 回しか発火しない。**
    /// 「最後に発火した時刻」で判定すると、壁付けの常設機にカードを置き忘れた
    /// ときに cooldown ごとに打刻が入り続ける (旧エッジ判定はこのケースを
    /// 正しく 1 回で済ませていた)。ここが逆転しないよう固定する
    #[test]
    fn held_card_fires_only_once() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("A", 0));
        // 20ms ポーリングで載り続けている間 — cooldown をまたいでも発火しない
        for t in (20..30_000).step_by(20) {
            assert!(!g.should_fire("A", t), "載せっぱなしで再発火した t={t}");
        }
    }

    /// 載せっぱなし → 外す → cooldown 後に再タップ で発火する
    /// (滞在中は 1 回、離れれば打刻し直せる、を 1 本で押さえる)
    #[test]
    fn held_then_removed_then_tapped_again_fires() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("A", 0));
        for t in (20..5_000).step_by(20) {
            assert!(!g.should_fire("A", t));
        }
        // 最後に見たのは 4_980。そこから 1 秒以上 検知が途切れた後の再タップ
        assert!(g.should_fire("A", 5_980));
    }

    /// 別のカードに変わったら待たせない (かざし替え)
    #[test]
    fn different_key_fires_immediately() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("A", 0));
        assert!(g.should_fire("B", 10));
        // 直前が B になっているので A はまた発火できる
        assert!(g.should_fire("A", 20));
    }

    /// 時刻が巻き戻っても発火し続けない (抑止側に倒す)
    #[test]
    fn clock_going_backwards_suppresses() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("A", 10_000));
        assert!(!g.should_fire("A", 5_000));
    }

    /// **`touch` が「離れた」の判定を止める。**
    /// 免許証のように再読が数秒に 1 回しか成功しない経路でも、載っている間は
    /// 1 回しか発火しない (実機で 7.5 秒に 3 打刻していたケース)
    #[test]
    fn touch_keeps_a_held_card_from_refiring() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("2023060920280513", 180_012));
        // 20ms 周期で存在検知は立ちっぱなし = 毎周期 touch される
        for t in (180_032..187_500).step_by(20) {
            g.touch(t);
        }
        // 読み取りが 3.4 秒ぶり / 4.1 秒ぶりに成功しても「まだ同じタップ」
        assert!(!g.should_fire("2023060920280513", 183_392));
        for t in (183_412..187_460).step_by(20) {
            g.touch(t);
        }
        assert!(!g.should_fire("2023060920280513", 187_464));
    }

    /// 何も読んでいないうちの `touch` は無害 (抑止する対象が無い)
    #[test]
    fn touch_before_any_read_does_nothing() {
        let mut g = TapGate::new(1_000);
        g.touch(5_000);
        assert!(g.should_fire("A", 5_000));
    }

    /// カードが離れれば touch も止まるので、再びかざせば発火する
    #[test]
    fn touch_stops_when_card_leaves_so_next_tap_fires() {
        let mut g = TapGate::new(1_000);
        assert!(g.should_fire("A", 0));
        for t in (20..2_000).step_by(20) {
            g.touch(t); // 載っている間
        }
        // 1_980 で離れた → 存在検知が落ちるので touch されない
        assert!(g.should_fire("A", 3_000)); // 1 秒以上あけて再タップ
    }

    #[test]
    fn zero_cooldown_never_suppresses() {
        let mut g = TapGate::new(0);
        assert!(g.should_fire("A", 0));
        assert!(g.should_fire("A", 0));
    }
}
