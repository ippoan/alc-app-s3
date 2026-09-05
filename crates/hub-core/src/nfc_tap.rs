//! NFC タップの重複抑止 (issue #103) と 2 枚検出 (issue #143)。
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
//! 打刻が `hub_measurements` に 2 行入り、front は打刻の並びから出勤/退勤を
//! 判定するため **出勤と退勤を同時に打った**ことになる。賃金計算に入る誤データ
//! なので、端末側で塞ぐ (サーバ側で握るとブラウザ版キオスクの同じ穴が残る)。
//!
//! # 方式 1: 「離れて N ミリ秒経つまで、まだ同じタップ」(debounce)
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
//! # 方式 2: 2 枚見えたらどちらも登録しない (issue #143)
//!
//! 1 つの財布に FeliCa が 2 枚入っていると、A と B が交互に読まれる。
//! キーごとに独立したクールダウンを持たせれば「A も B も 1 回ずつ」で
//! 収まるが、それは **どちらの人の打刻か決められないまま 2 人ぶん記録する**
//! ということで、賃金データとしては黙って壊れる方に倒れている。
//! **曖昧なら記録しない**を採り、2 枚見えたら**どちらも発火させずエラー**にする。
//!
//! そのために発火を**遅延確定**にした。カードを読んでも即発火せず、
//! **確定窓 [`DEFAULT_COMMIT_WINDOW_MS`] のあいだに別キーが現れなければ**
//! 発火する ([`TapGate::poll`] が返す)。現れたら [`TapOutcome::MultipleCards`]
//! を 1 回だけ返し、そのタップは以後カードが離れるまで何も発火しない。
//!
//! **確定窓の起点は「最初の読み」に固定で、同じキーの再読では延ばさない。**
//! 延ばすと、カードを載せっぱなしにしたとき窓が永久に閉じず **1 回も
//! 発火しなくなる** — #103 で固定した「載せっぱなしは 1 回」が逆方向に壊れる。
//!
//! ## 検出できない条件 (承知のうえの限界)
//!
//! 確定窓で捕まえられるのは **「窓の中で 2 枚目が読めた」場合だけ**。
//!
//! - **免許証が絡む 2 枚は取りこぼしうる。** 上記のとおり免許証は再読が
//!   3.4〜4.1 秒に 1 回しか成功しないので、確定窓 250ms のあいだに
//!   2 枚目として現れないことがある。この場合は 1 枚として発火する
//! - もっと一般に、**交互読みの周期が確定窓より長い組み合わせ**は
//!   「2 枚」と判定できない
//! - 逆に、**確定窓より短い間隔でのかざし替え (A を離して 250ms 未満で B)
//!   は「2 枚」と区別できないのでエラーになる**。かざし直しになるだけで
//!   誤記録にはならないので、この向きの取り違えは許容する
//!
//! 窓を秒単位に伸ばせば取りこぼしは減るが、**全打刻がその秒数だけ遅れる**ので
//! 伸ばさない。#143 が想定する実害 (財布の中の FeliCa 2 枚) は 20ms 周期で
//! 両方が応答するため、この窓で足りるという判断。

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

/// 発火を保留して「2 枚目が来ないこと」を確かめる確定窓 [ms] (issue #143)。
///
/// **全打刻がこの時間だけ遅れる**ので、伸ばすときは打刻の体感速度との
/// トレードオフになる。250ms は「20ms ポーリングで交互に読める 2 枚
/// (財布の中の FeliCa 2 枚) なら十分捕まる」かつ「かざしてから反応するまでの
/// 遅れとして知覚されにくい」ところ。取りこぼす条件はモジュール doc 参照。
pub const DEFAULT_COMMIT_WINDOW_MS: u64 = 250;

/// [`TapGate::poll`] が返す確定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapOutcome<T> {
    /// まだ何も確定していない (保留中 / タップが無い)
    Idle,
    /// 1 枚だけで確定窓を抜けた。発火してよい
    Fire(T),
    /// 確定窓の中で 2 枚目のキーを見た。**どちらも登録しない** (issue #143)。
    /// 同じタップのあいだ (カードが離れるまで) 2 度は返さない
    MultipleCards,
}

/// タップの内部状態。
#[derive(Debug, Clone)]
enum Phase<T> {
    /// 保留なし
    Idle,
    /// 最初の 1 枚を見た。`first_seen` から確定窓が経つのを待っている。
    /// **同じキーの再読で `first_seen` を更新しないこと** (載せっぱなしで
    /// 窓が永久に閉じなくなる)
    Pending {
        key: String,
        payload: T,
        first_seen: u64,
    },
    /// 2 枚目を見た。次の [`TapGate::poll`] で [`TapOutcome::MultipleCards`] を返す
    Rejecting,
    /// エラーを返し終えた。カードが離れる (無検知が cooldown 続く) まで何もしない
    Rejected,
}

/// タップの重複抑止 + 2 枚検出。
///
/// **全系統 (FeliCa IDm / NFC-A UID / 免許証 / 車検証) で 1 つを共有する。**
/// 分けると「別系統で読めた 2 枚目」を 2 枚と数えられなくなるうえ、同じカードが
/// F と B の両方で読めるようなケース (読み取りが揺れて別経路に落ちる) でも
/// 1 タップ 1 回に収まらなくなる。
///
/// `T` は発火まで持ち越すペイロード (呼び出し側のイベント型)。**発火は
/// [`TapGate::poll`] だけが返す** — 読んだ瞬間には発火しないので、
/// 読み取り側で `on_event` を直接呼ばないこと。
#[derive(Debug, Clone)]
pub struct TapGate<T> {
    /// 最後に見たキーと、最後に見た時刻。`touch` でも更新される
    last: Option<(String, u64)>,
    phase: Phase<T>,
    cooldown_ms: u64,
    window_ms: u64,
}

impl<T> TapGate<T> {
    pub fn new(cooldown_ms: u64) -> Self {
        Self::with_window(cooldown_ms, DEFAULT_COMMIT_WINDOW_MS)
    }

    pub fn with_window(cooldown_ms: u64, window_ms: u64) -> Self {
        Self {
            last: None,
            phase: Phase::Idle,
            cooldown_ms,
            window_ms,
        }
    }

    /// 無検知が cooldown 続いたらタップの区切り。
    /// **保留 (`Pending`) は落とさない** — カードを確定窓より短くかざして
    /// 離したときに打刻が消えてしまう。`Rejected` だけ解除する
    fn expire(&mut self, now_ms: u64) {
        // saturating_sub: now_ms が巻き戻っても「経過 0」に倒して抑止側に寄せる
        let quiet = match &self.last {
            Some((_, last_seen)) => now_ms.saturating_sub(*last_seen) >= self.cooldown_ms,
            None => false,
        };
        if quiet {
            self.last = None;
            if matches!(self.phase, Phase::Rejected) {
                self.phase = Phase::Idle;
            }
        }
    }

    /// カードが載っていることだけを伝える (読めたかは問わない)。
    ///
    /// **読み取りの成否ではなくカードの存在で判断するための口。**
    /// アンテナの存在検知が立っている間これを毎周期呼べば、読み取りが
    /// 失敗し続けても「離れた」と数えられない。まだ何も読んでいない
    /// (`last` が None) ときは何もしない — 触っても抑止する対象が無い。
    ///
    /// **確定窓は延ばさない** — 延ばすと載せっぱなしで一度も発火しなくなる。
    pub fn touch(&mut self, now_ms: u64) {
        if let Some((_, last_seen)) = &mut self.last {
            *last_seen = now_ms;
        }
    }

    /// カードを 1 枚読めたことを記録する。**ここでは発火しない** (遅延確定)。
    ///
    /// - `key`: カードを一意に表す文字列 (IDm / UID / 免許証の 16 桁)
    /// - `payload`: 発火が確定したときに [`TapOutcome::Fire`] で返す値
    /// - `now_ms`: **単調増加**の時刻 (稼働時間)。壁時計を渡さないこと —
    ///   NTP 同期で時刻が飛ぶとクールダウンが飛ぶ
    pub fn observe(&mut self, key: &str, payload: T, now_ms: u64) {
        self.expire(now_ms);
        // 同じカードを cooldown 内にまた見た = まだ同じタップ (issue #103)
        let same_tap = matches!(&self.last, Some((k, seen))
            if k == key && now_ms.saturating_sub(*seen) < self.cooldown_ms);
        self.last = Some((key.to_string(), now_ms));

        match &self.phase {
            // 確定窓の中に別キー → 2 枚。どちらも登録しない (issue #143)
            Phase::Pending { key: pending, .. } if pending != key => {
                self.phase = Phase::Rejecting;
            }
            // 同じキーの再読。窓の起点は動かさない
            Phase::Pending { .. } => {}
            // エラー確定済み。カードが離れるまで何も始めない
            Phase::Rejecting | Phase::Rejected => {}
            Phase::Idle if !same_tap => {
                self.phase = Phase::Pending {
                    key: key.to_string(),
                    payload,
                    first_seen: now_ms,
                };
            }
            Phase::Idle => {}
        }
    }

    /// 確定窓の経過を進める。**毎周期、存在検知の有無にかかわらず呼ぶこと。**
    ///
    /// 存在検知が立っている周期だけで呼ぶと、**カードを確定窓より短くかざして
    /// 離したときに保留が確定せず打刻が消える** (`observe` した周期の次に
    /// カードはもう居ない)。
    pub fn poll(&mut self, now_ms: u64) -> TapOutcome<T> {
        self.expire(now_ms);
        // take してから戻す形にする (`&self.phase` を見てから取り出すと、
        // 取り出せなかった場合の `unreachable!` が 100% カバレッジの穴になる)
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Rejecting => {
                self.phase = Phase::Rejected;
                TapOutcome::MultipleCards
            }
            Phase::Pending {
                key,
                payload,
                first_seen,
            } => {
                if now_ms.saturating_sub(first_seen) >= self.window_ms {
                    TapOutcome::Fire(payload)
                } else {
                    self.phase = Phase::Pending {
                        key,
                        payload,
                        first_seen,
                    };
                    TapOutcome::Idle
                }
            }
            other => {
                self.phase = other;
                TapOutcome::Idle
            }
        }
    }
}

impl<T> Default for TapGate<T> {
    fn default() -> Self {
        Self::new(DEFAULT_COOLDOWN_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u64 = DEFAULT_COMMIT_WINDOW_MS;

    /// テスト用: `key` をペイロードにして観測する
    fn observe(g: &mut TapGate<&'static str>, key: &'static str, now: u64) {
        g.observe(key, key, now);
    }

    /// `now` まで 20ms 刻みで poll し、確定したものを順に集める
    /// (実機のループが毎周期 `poll` を呼ぶのと同じ形)
    fn poll_until(
        g: &mut TapGate<&'static str>,
        from: u64,
        to: u64,
    ) -> Vec<TapOutcome<&'static str>> {
        let mut out = Vec::new();
        for t in (from..=to).step_by(20) {
            match g.poll(t) {
                TapOutcome::Idle => {}
                o => out.push(o),
            }
        }
        out
    }

    /// 1 枚だけなら確定窓の経過後に 1 回だけ発火する (受け入れ条件 b)
    #[test]
    fn single_card_fires_once_after_window() {
        let mut g = TapGate::default();
        observe(&mut g, "A", 0);
        // 窓が閉じる前は発火しない
        assert_eq!(poll_until(&mut g, 0, W - 20), vec![]);
        assert_eq!(poll_until(&mut g, W, W + 500), vec![TapOutcome::Fire("A")]);
    }

    /// **issue #143 の本体**: 確定窓の中に 2 枚目が現れたら、**どちらも発火せず**
    /// エラーになる (受け入れ条件 a)。どちらの人の打刻か決められないので
    /// 「2 人ぶん記録する」より「記録しない」を採る
    #[test]
    fn second_card_within_window_errors_and_fires_neither() {
        let mut g = TapGate::default();
        observe(&mut g, "01401D0B1D37B660", 0);
        observe(&mut g, "0114B2C4D5E6F708", 20);
        // エラーは 1 回だけ。窓を過ぎても Fire は来ない
        assert_eq!(
            poll_until(&mut g, 40, W + 1_000),
            vec![TapOutcome::MultipleCards]
        );
    }

    /// 2 枚が載りっぱなしで交互に読まれ続けても、エラーは 1 タップ 1 回。
    /// (毎周期エラーを出すとブザーが鳴りっぱなしになる)
    #[test]
    fn two_cards_kept_on_reader_error_only_once() {
        let mut g = TapGate::default();
        let mut errors = 0;
        for t in (0..10_000).step_by(20) {
            if matches!(g.poll(t), TapOutcome::MultipleCards) {
                errors += 1;
            }
            observe(&mut g, if (t / 20) % 2 == 0 { "A" } else { "B" }, t);
            g.touch(t);
        }
        assert_eq!(errors, 1, "載せっぱなしの 2 枚でエラーが繰り返された");
    }

    /// 2 枚を離せば、次のタップは普通に打刻できる (エラーが固着しない)
    #[test]
    fn error_clears_after_cards_leave() {
        let mut g = TapGate::default();
        observe(&mut g, "A", 0);
        observe(&mut g, "B", 20);
        assert_eq!(poll_until(&mut g, 40, 60), vec![TapOutcome::MultipleCards]);
        // 20 で最後に見た。1 秒以上 検知が途切れてから 1 枚だけかざす
        observe(&mut g, "A", 2_000);
        assert_eq!(
            poll_until(&mut g, 2_020, 2_500 + W),
            vec![TapOutcome::Fire("A")]
        );
    }

    /// **確定窓を過ぎてからの別カードは発火する** (受け入れ条件 c)。
    /// 「離れてから別の人がかざす」を殺していないこと
    #[test]
    fn different_card_after_window_still_fires() {
        let mut g = TapGate::default();
        observe(&mut g, "A", 0);
        assert_eq!(poll_until(&mut g, 0, 380), vec![TapOutcome::Fire("A")]);
        // A は離れた (touch されない) → 窓の外なので B は 2 枚目ではない
        observe(&mut g, "B", 400);
        assert_eq!(poll_until(&mut g, 420, 900), vec![TapOutcome::Fire("B")]);
    }

    /// **離れないまま 2 枚目**は上のケースと区別してエラーにする。
    /// (`different_card_after_window_still_fires` と対で、旧
    /// `different_key_fires_immediately` が「かざし替え = 常に正常」として
    /// 固定していた意図を分解したもの)
    #[test]
    fn second_card_without_leaving_errors() {
        let mut g = TapGate::default();
        observe(&mut g, "A", 0);
        // A が載ったまま (毎周期 touch) B も読めた = 財布の中の 2 枚
        for t in (20..W).step_by(20) {
            g.touch(t);
            if t == 100 {
                observe(&mut g, "B", t);
            }
        }
        assert_eq!(
            poll_until(&mut g, W, W + 1_000),
            vec![TapOutcome::MultipleCards]
        );
    }

    /// **1 枚を確定窓より短くかざして離しても打刻は消えない** (受け入れ条件 f)。
    /// 保留を進める `poll` を「存在検知が立っている周期」だけで呼ぶと落ちる —
    /// カードはもう居ないので、実機だけで壊れる形になる
    #[test]
    fn card_removed_before_window_still_fires() {
        let mut g = TapGate::default();
        observe(&mut g, "A", 0);
        // 100ms で離れた。以降 touch も observe も来ない
        assert_eq!(poll_until(&mut g, 20, 100), vec![]);
        assert_eq!(
            poll_until(&mut g, 120, W + 200),
            vec![TapOutcome::Fire("A")]
        );
    }

    /// **issue #103**: ポーリングが空振りして再検知しても 2 回発火しない
    #[test]
    fn same_key_within_cooldown_is_suppressed() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "01401D0B1D37B660", 1_000);
        // モバイル FeliCa の空振り → 直後の再検知 (40ms 後)
        observe(&mut g, "01401D0B1D37B660", 1_040);
        observe(&mut g, "01401D0B1D37B660", 1_060);
        assert_eq!(
            poll_until(&mut g, 1_000, 3_000),
            vec![TapOutcome::Fire("01401D0B1D37B660")]
        );
    }

    /// 検知が途切れて cooldown 経てば同じカードでも再び打刻できる (打刻し直し)
    #[test]
    fn same_key_after_cooldown_fires_again() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 1_000);
        assert_eq!(
            poll_until(&mut g, 1_000, 1_500),
            vec![TapOutcome::Fire("A")]
        );
        // 1_000 で最後に見てから 1 秒 (境界ちょうど)
        observe(&mut g, "A", 2_000);
        assert_eq!(
            poll_until(&mut g, 2_000, 2_500),
            vec![TapOutcome::Fire("A")]
        );
    }

    /// **載せっぱなしでは 1 回しか発火しない** (受け入れ条件 d)。
    /// 「最後に発火した時刻」で判定すると、壁付けの常設機にカードを置き忘れた
    /// ときに cooldown ごとに打刻が入り続ける (旧エッジ判定はこのケースを
    /// 正しく 1 回で済ませていた)。ここが逆転しないよう固定する。
    /// **確定窓を同じキーの再読で延ばしてしまうと、逆に 1 回も発火しなくなる**
    #[test]
    fn held_card_fires_only_once() {
        let mut g = TapGate::new(1_000);
        let mut fires = 0;
        for t in (0..30_000).step_by(20) {
            if matches!(g.poll(t), TapOutcome::Fire(_)) {
                fires += 1;
            }
            observe(&mut g, "A", t);
            g.touch(t);
        }
        assert_eq!(fires, 1, "載せっぱなしの発火回数");
    }

    /// 載せっぱなし → 外す → cooldown 後に再タップ で発火する
    #[test]
    fn held_then_removed_then_tapped_again_fires() {
        let mut g = TapGate::new(1_000);
        let mut fires = 0;
        for t in (0..5_000).step_by(20) {
            if matches!(g.poll(t), TapOutcome::Fire(_)) {
                fires += 1;
            }
            observe(&mut g, "A", t);
            g.touch(t);
        }
        assert_eq!(fires, 1);
        // 最後に見たのは 4_980。そこから 1 秒以上 検知が途切れた後の再タップ
        observe(&mut g, "A", 5_980);
        assert_eq!(
            poll_until(&mut g, 5_980, 6_500),
            vec![TapOutcome::Fire("A")]
        );
    }

    /// 時刻が巻き戻っても発火し続けない (抑止側に倒す)
    #[test]
    fn clock_going_backwards_suppresses() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 10_000);
        assert_eq!(
            poll_until(&mut g, 10_000, 10_500),
            vec![TapOutcome::Fire("A")]
        );
        observe(&mut g, "A", 5_000);
        assert_eq!(poll_until(&mut g, 5_000, 9_000), vec![]);
    }

    /// **`touch` が「離れた」の判定を止める** (受け入れ条件 e)。
    /// 免許証のように再読が数秒に 1 回しか成功しない経路でも、載っている間は
    /// 1 回しか発火しない (実機で 7.5 秒に 3 打刻していたケース)
    #[test]
    fn touch_keeps_a_held_card_from_refiring() {
        let mut g = TapGate::new(1_000);
        let mut fires = 0;
        observe(&mut g, "2023060920280513", 180_012);
        // 20ms 周期で存在検知は立ちっぱなし = 毎周期 touch される。
        // 読み取りが成功するのは 3.4 秒ぶり / 4.1 秒ぶりの 2 回だけ
        for t in (180_012..187_500).step_by(20) {
            if matches!(g.poll(t), TapOutcome::Fire(_)) {
                fires += 1;
            }
            if t == 183_392 || t == 187_452 {
                observe(&mut g, "2023060920280513", t);
            }
            g.touch(t);
        }
        assert_eq!(fires, 1, "免許証の再読で再発火した");
    }

    /// 何も読んでいないうちの `touch` は無害 (抑止する対象が無い)
    #[test]
    fn touch_before_any_read_does_nothing() {
        let mut g = TapGate::new(1_000);
        g.touch(5_000);
        observe(&mut g, "A", 5_000);
        assert_eq!(
            poll_until(&mut g, 5_000, 5_500),
            vec![TapOutcome::Fire("A")]
        );
    }

    /// カードが離れれば touch も止まるので、再びかざせば発火する
    #[test]
    fn touch_stops_when_card_leaves_so_next_tap_fires() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 0);
        assert_eq!(poll_until(&mut g, 0, 500), vec![TapOutcome::Fire("A")]);
        for t in (20..2_000).step_by(20) {
            g.touch(t); // 載っている間
        }
        // 1_980 で離れた → 存在検知が落ちるので touch されない
        observe(&mut g, "A", 3_000);
        assert_eq!(
            poll_until(&mut g, 3_000, 3_500),
            vec![TapOutcome::Fire("A")]
        );
    }

    /// 確定窓 0 なら次の poll で即発火する (窓は延ばせても縮められることの確認)
    #[test]
    fn zero_window_fires_on_next_poll() {
        let mut g: TapGate<&'static str> = TapGate::with_window(1_000, 0);
        observe(&mut g, "A", 0);
        assert_eq!(g.poll(0), TapOutcome::Fire("A"));
        assert_eq!(g.poll(20), TapOutcome::Idle);
    }
}
