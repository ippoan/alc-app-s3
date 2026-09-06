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
//! # 読み取りに cooldown より長くかかっても「離れた」と数えない (issue #171)
//!
//! touch も observe も**周の区切りにしか入らない**ので、1 回の読み取りが
//! cooldown (1 秒) を超えると、その間 gate は何も観測しない。免許証 (Type-B) の
//! APDU は S(WTX) で**最大 2 秒まで**延びるため、載せたままでも読了時刻で
//! 連続性を判定すると「1 秒以上見ていない = 新しいタップ」になり**二重打刻**する
//! (実機 2026-09-06: cooldown 500ms の実験で、B 読み 0.87 秒 + -4 → F/A の
//! 寄り道で観測間隔 0.89 秒 → 二重打刻)。cooldown を伸ばしても WTX の上限に
//! 追いつくだけで、かざし直しの待ちが伸びる。
//!
//! **読み取りが成功したなら、カードはその読み取りを始めた時点で載っていた**
//! (WUPB / ポーリングに応答したのがその時刻)。そこで [`TapGate::observe`] は
//! **読み始めた時刻 (`since_ms`) と読了時刻 (`now_ms`) の両方**を受け、
//! 「同じタップか」は `since_ms` で、「最後に見た時刻」と確定窓の起点は
//! `now_ms` で決める。読み取りが何秒かかっても、始めたときに前回の観測から
//! cooldown 以内なら同じタップ。**確定窓 (#143) の意味は変えない** — 起点は
//! 読了時刻のままなので、2 枚検知の窓が読み取り時間ぶん縮むことはない。
//!
//! 「読み取りの前に `touch` を置く」では塞げない — 差は縮まるが、存在検知由来の
//! touch を observe の前に置くと**離れていた時間が消えて再タップが抑止される**
//! (hub-drivers nfc.rs の touch 位置のコメント)。読めた事実だけを根拠にする。
//!
//! ## 副作用 (承知のうえ)
//!
//! 「離れてからの再タップ」を新タップと数える境界が、**その読みの所要時間ぶん**
//! 手前に動く (B は 0.2 s、WTX で 0.87〜2.0 s)。前回の観測から cooldown 以内に
//! **読み始めた**タップは、読了が cooldown を超えていても同じタップになる。
//!
//! - LicenseFirst (タイムカード端末) は #169 の [`TapGate::release`] (RF が
//!   1 周無応答の周で last を消す) が補償するので、離した直後の再タップは変わらない
//! - **FelicaFirst (CoreS3 / atoms3-nfc) には補償が無い**。点呼に再タップの要件は
//!   無いが、退行の境界はテストで固定する (読み 0.2 s のタップを 1.5 s 間隔で
//!   2 回 → 2 回とも発火)
//! - 2 枚エラー ([`Phase::Rejected`]) の解除も同じ判定なので、エラー後の再タップの
//!   受付が読み時間ぶん遅れる
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
            self.clear_last();
        }
    }

    /// タップの区切り: 直前のカードを忘れ、エラー確定済みなら解除する。
    /// **保留 (`Pending`) は落とさない** ([`TapGate::expire`] と同じ理由)
    fn clear_last(&mut self) {
        self.last = None;
        if matches!(self.phase, Phase::Rejected) {
            self.phase = Phase::Idle;
        }
    }

    /// カードが離れたことが**外から確定した**ときに、cooldown を待たずタップを区切る
    /// (issue #155)。
    ///
    /// 呼び出し側が RF の無応答 (B → F → A の 1 周すべて無応答) で「離れた」と
    /// 判定できるとき用。cooldown (既定 1000ms) は「載ったまま読み取りが空振りした
    /// 時間」を吸収するためのもので、離れたことが分かっているなら待つ必要が無い —
    /// 離した直後 (0.5 秒程度) の再タップを別の打刻として受けたい運用要望に応える。
    /// cooldown を短くする案は、B の読み取りが S(WTX) で 0.87 秒かかった周や
    /// -4 → F/A の寄り道で観測の間隔が 500ms を超え、**載ったまま二重打刻**になった
    /// (実機 2026-09-06) ので採らない。
    ///
    /// **確定窓 (`Pending`) は落とさない** — 窓より短くかざして離した打刻を消さない。
    pub fn release(&mut self) {
        self.clear_last();
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
    /// - `since_ms`: **この読み取りを始めた時刻**。読めたということはカードは
    ///   この時点で載っていたので、「同じタップか」(cooldown 内か) はこちらで
    ///   判定する (issue #171、モジュール doc)。読み取りが同期で数秒かかっても
    ///   「離れた」と数えない。呼び出し側はポーリングを打つ**直前**に取ること
    /// - `now_ms`: 読了時刻 = 「最後に見た時刻」と確定窓の起点。**単調増加**の
    ///   時刻 (稼働時間) を渡し、壁時計を渡さないこと — NTP 同期で時刻が飛ぶと
    ///   クールダウンが飛ぶ
    ///
    /// **契約: `since_ms <= now_ms`** (同じ時計で、読み始め ≤ 読了)。呼び出し側は
    /// `since_ms` を**各 poll の直前**で取ること — 周の先頭で 1 つ取り回すと
    /// 抑止の窓が 1 周期ぶん (~0.5 s) 余計に膨らむ
    pub fn observe(&mut self, key: &str, payload: T, since_ms: u64, now_ms: u64) {
        debug_assert!(since_ms <= now_ms, "observe: since_ms > now_ms");
        self.expire(since_ms);
        // 同じカードを cooldown 内にまた見た = まだ同じタップ (issue #103)。
        // 経過は読み始めから数える — 読了時刻から数えると、WTX で 1 秒を超えた
        // 読みが載せたまま新タップになる (issue #171)
        let same_tap = matches!(&self.last, Some((k, seen))
            if k == key && since_ms.saturating_sub(*seen) < self.cooldown_ms);
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

    /// テスト用: `key` をペイロードにして観測する。読み取りは一瞬 (開始 = 読了)
    fn observe(g: &mut TapGate<&'static str>, key: &'static str, now: u64) {
        g.observe(key, key, now, now);
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

    // ---- 実機の呼び出しパターンを固定する (issue #155) ----
    //
    // `hub-drivers/src/nfc.rs` は **1 周 448ms** かけて F → A → B を順に試し、
    // **各 poll のあいだにも `poll()` を挟む** (#155 で追加。挟まないと窓が閉じても
    // 次の周まで最大 448ms 発火が遅れる)。**`TapGate` 自体は無改造**なので、
    // ここで固定するのは「その呼び出し順で仕様が保たれるか」だけ。
    //
    // 実測 (Atom VoiceS3R、#155): F=91ms / A=140ms / B=200ms、周の末尾に 20ms の sleep。

    /// F / A / B の poll 所要 [ms] (実機実測)
    const T_F: u64 = 91;
    const T_A: u64 = 140;
    const T_B: u64 = 200;
    /// 周末の `FreeRtos::delay_ms(POLL_INTERVAL_MS)`
    const T_SLEEP: u64 = 20;

    /// 1 周ぶんを実機と同じ順序で回す。
    ///
    /// `read_f` / `read_a` / `read_b` は「その poll でカードが読めたか」。
    /// 実機と同じく **読めた時点で以降の poll は飛ばす** (`got` による短絡)。
    /// 戻り値は、その周で確定したもの。
    fn one_loop(
        g: &mut TapGate<&'static str>,
        t0: u64,
        read_f: Option<&'static str>,
        read_a: Option<&'static str>,
        read_b: Option<&'static str>,
    ) -> (u64, Vec<TapOutcome<&'static str>>) {
        let mut out = Vec::new();
        let mut push = |o: TapOutcome<&'static str>, out: &mut Vec<_>| {
            if !matches!(o, TapOutcome::Idle) {
                out.push(o);
            }
        };
        let mut t = t0;
        // ループ先頭の poll (元からある)
        push(g.poll(t), &mut out);

        // 各 poll は「打つ直前の時刻」を since に渡す (#171。実機の呼び出しと同じ形)
        // --- F ---
        let since = t;
        t += T_F;
        let mut got = false;
        if let Some(k) = read_f {
            g.observe(k, k, since, t);
            got = true;
        }
        push(g.poll(t), &mut out); // #155 で追加

        // --- A ---
        if !got {
            let since = t;
            t += T_A;
            if let Some(k) = read_a {
                g.observe(k, k, since, t);
                got = true;
            }
        }
        push(g.poll(t), &mut out); // #155 で追加

        // --- B ---
        if !got {
            let since = t;
            t += T_B;
            if let Some(k) = read_b {
                g.observe(k, k, since, t);
            }
        }
        push(g.poll(t), &mut out); // #155 で追加

        t += T_SLEEP;
        (t, out)
    }

    /// **#143 の本命が保たれる**: 財布の中の FeliCa 2 枚は**どちらも F で読める**ので、
    /// #155 で挟んだ `poll()` がすべて F の**後ろ**にある限り、2 枚目の `observe` が
    /// 先に走る。つまり 2 枚検出は壊れない。
    #[test]
    fn issue155_call_pattern_still_detects_two_felica_cards() {
        let mut g = TapGate::default();
        let mut fired = Vec::new();
        let mut t = 0;
        // 1 周目: 1 枚目が F で読める
        let (next, o) = one_loop(&mut g, t, Some("FELICA_1"), None, None);
        fired.extend(o);
        t = next;
        // 2 周目: 2 枚目が F で読める (交互読み)。1 周 111ms < 確定窓 250ms
        let (next, o) = one_loop(&mut g, t, Some("FELICA_2"), None, None);
        fired.extend(o);
        t = next;
        // 以降は窓が閉じるまで回す
        for _ in 0..6 {
            let (next, o) = one_loop(&mut g, t, Some("FELICA_1"), None, None);
            fired.extend(o);
            t = next;
        }
        assert_eq!(
            fired,
            vec![TapOutcome::MultipleCards],
            "2 枚とも F で読めるなら、#155 の呼び出し順でも 2 枚と判定できること"
        );
    }

    /// **A (NFC-A) で読めるカードも、#155 の呼び出し順で 1 回だけ発火する。**
    /// F が空振りしてから A が読む経路 (HCE / UID タグ) の回帰。
    #[test]
    fn issue155_single_nfca_card_fires_through_the_new_call_pattern() {
        let mut g = TapGate::default();
        let mut fired = Vec::new();
        let mut t = 0;
        // 1 周目: F は空振り、A で読める
        let (next, o) = one_loop(&mut g, t, None, Some("NFCA_1"), None);
        fired.extend(o);
        t = next;
        // 載せっぱなしで数周。確定窓が閉じたところで 1 回だけ発火する
        for _ in 0..4 {
            let (next, o) = one_loop(&mut g, t, None, Some("NFCA_1"), None);
            fired.extend(o);
            t = next;
        }
        assert_eq!(fired, vec![TapOutcome::Fire("NFCA_1")]);
    }

    /// **1 枚目が F、2 枚目が A でも 2 枚検出は保たれる** (#155 で親から名指しされたケース)。
    ///
    /// 経過時間だけ見ると 2 枚目の観測は 1 枚目から 251ms 後で**確定窓 250ms を超えて**
    /// いるが、**窓は `poll()` が呼ばれて初めて閉じる**。F と A のあいだの `poll()` は
    /// まだ 111ms の時点なので閉じておらず、A が読んだ瞬間はまだ保留中 —
    /// つまり **2 枚目として拾える**。
    ///
    /// **#155 で `poll()` を増やしても、増やした位置がすべて F の後ろなので、
    /// 「F で 1 枚目 → 同じ周の A で 2 枚目」の並びは壊れない。**
    #[test]
    fn issue155_felica_then_nfca_still_detects_two_cards() {
        let mut g = TapGate::default();
        let mut fired = Vec::new();
        let mut t = 0;
        // 1 周目: F で 1 枚目 (got により A/B は短絡)
        let (next, o) = one_loop(&mut g, t, Some("FELICA_1"), None, None);
        fired.extend(o);
        t = next;
        // 2 周目: F は空振り、A で 2 枚目
        let (_next, o) = one_loop(&mut g, t, None, Some("NFCA_2"), None);
        fired.extend(o);
        assert_eq!(fired, vec![TapOutcome::MultipleCards]);
    }

    /// 2 枚とも A で読める場合も同様に 2 枚と判定できる (上と同じ理由)。
    #[test]
    fn issue155_two_nfca_cards_still_detected() {
        let mut g = TapGate::default();
        let mut fired = Vec::new();
        let mut t = 0;
        let (next, o) = one_loop(&mut g, t, None, Some("NFCA_1"), None);
        fired.extend(o);
        t = next;
        let (_next, o) = one_loop(&mut g, t, None, Some("NFCA_2"), None);
        fired.extend(o);
        assert_eq!(fired, vec![TapOutcome::MultipleCards]);
    }

    /// **既知の限界を仕様として固定する**: 「1 枚目が F、2 枚目が B でしか読めない」
    /// 組み合わせは **2 枚と判定できない**。
    ///
    /// **これは #155 が壊したのではない。**1 周 448ms が確定窓 250ms より長いので、
    /// **窓が F/A/B の一巡をまたげない**ため元から取りこぼしていた
    /// (モジュール doc の「交互読みの周期が確定窓より長い組み合わせは 2 枚と
    /// 判定できない」がこれ)。**doc と一致していることをテストで示す。**
    #[test]
    fn issue155_two_cards_across_slow_poll_cycle_are_not_detected_by_design() {
        let mut g = TapGate::default();
        let mut fired = Vec::new();
        let mut t = 0;
        // 1 周目: F で 1 枚目。この周の残り (A/B) は短絡で走らない
        let (next, o) = one_loop(&mut g, t, Some("FELICA_1"), None, None);
        fired.extend(o);
        t = next;
        // 2 周目: F も A も空振りし、B (免許証) でようやく 2 枚目。
        // ここに着くのは 1 周目の観測から 111 + 91 + 140 + 200 = 542ms 後で、
        // **確定窓 250ms はとうに閉じている**
        let (_next, o) = one_loop(&mut g, t, None, None, Some("LICENSE_2"));
        fired.extend(o);
        assert_eq!(
            fired,
            vec![TapOutcome::Fire("FELICA_1")],
            "窓より遅い交互読みは 2 枚と判定できない (既知の限界。doc と一致)"
        );
    }

    /// **#155 で挟んだ `poll()` は発火を早める**: 読めた周のうちに窓が閉じていれば、
    /// **次の周 (448ms 先) を待たずに**発火する。
    #[test]
    fn issue155_fires_within_the_same_loop_when_window_already_closed() {
        let mut g = TapGate::default();
        // 0ms に観測 (前の周で読めた想定)
        observe(&mut g, "A", 0);
        // 窓が閉じた後の周: 先頭の poll は t0=W-50 でまだ閉じていない。
        // F の poll (+91ms) を過ぎた時点で窓が閉じ、**その周のうちに**発火する
        let (_t, out) = one_loop(&mut g, W - 50, None, None, None);
        assert_eq!(
            out,
            vec![TapOutcome::Fire("A")],
            "窓が閉じたら、その周のうちに発火すること (次の周まで待たない)"
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

    /// release (RF で離れたと確定) の後は、cooldown 内の同じカードでも新しいタップ (#155)
    #[test]
    fn release_lets_same_card_fire_again_within_cooldown() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 0);
        g.touch(0);
        assert_eq!(poll_until(&mut g, 0, 300), vec![TapOutcome::Fire("A")]);
        // 離れたと確定 (300ms) → 700ms で再タップ = cooldown 1000ms の内側でも別の打刻
        g.release();
        observe(&mut g, "A", 700);
        assert_eq!(poll_until(&mut g, 700, 1_000), vec![TapOutcome::Fire("A")]);
    }

    /// release 無しなら同じ再タップは cooldown 内で抑止される (上の対照)
    #[test]
    fn without_release_same_card_within_cooldown_is_suppressed() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 0);
        assert_eq!(poll_until(&mut g, 0, 300), vec![TapOutcome::Fire("A")]);
        observe(&mut g, "A", 700);
        assert_eq!(poll_until(&mut g, 700, 1_000), vec![]);
    }

    /// 確定窓の途中で release されても保留は落ちない (窓より短くかざした打刻を消さない)
    #[test]
    fn release_while_pending_keeps_the_pending_tap() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 0);
        g.release();
        assert_eq!(poll_until(&mut g, 100, 400), vec![TapOutcome::Fire("A")]);
    }

    /// エラー確定済み (2 枚) は release で解除され、次のカードをすぐ受ける
    #[test]
    fn release_clears_rejected_error() {
        let mut g = TapGate::new(1_000);
        observe(&mut g, "A", 0);
        observe(&mut g, "B", 100);
        assert_eq!(poll_until(&mut g, 100, 400), vec![TapOutcome::MultipleCards]);
        g.release();
        observe(&mut g, "A", 500);
        assert_eq!(poll_until(&mut g, 500, 800), vec![TapOutcome::Fire("A")]);
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

    // ---- 読み取りが cooldown より長い (S(WTX)) でも載せたままは 1 タップ (issue #171) ----
    //
    // LicenseFirst (タイムカード端末) の周を模す: B を先頭で読み、読了で observe、
    // RF が応答した周の末尾で touch、各所で poll。読み取りは**同期**なので、
    // その間 touch も poll も入らない (実機と同じ)。

    /// 免許証を載せたまま 1 周の B 読みが S(WTX) で 2 秒かかっても、二重打刻しない。
    /// **読了時刻で連続性を見ると 2 秒 > cooldown 1 秒で新タップになる**穴 (#171)
    #[test]
    fn issue171_held_license_read_spanning_wtx_fires_only_once() {
        let mut g = TapGate::new(1_000);
        let mut fired = Vec::new();
        let mut push = |o: TapOutcome<&'static str>, out: &mut Vec<_>| {
            if !matches!(o, TapOutcome::Idle) {
                out.push(o);
            }
        };
        // 1 周目: 通常の読み (200ms)
        let mut t = T_B;
        g.observe("LICENSE", "LICENSE", 0, t);
        // 以降 6 周、毎周の B 読みが WTX 上限の 2 秒かかる
        for _ in 0..6 {
            push(g.poll(t), &mut fired);
            g.touch(t);
            t += T_SLEEP;
            let since = t;
            t += 2_000; // read_license_expiry が S(WTX) で 2 秒ブロック
            g.observe("LICENSE", "LICENSE", since, t);
            push(g.poll(t), &mut fired);
        }
        assert_eq!(
            fired,
            vec![TapOutcome::Fire("LICENSE")],
            "載せたまま WTX 2 秒の周で再発火した"
        );
    }

    /// 実測型 (2026-09-06、cooldown 500ms の実験で顕在化): B が -4 で応答して粘着を解き
    /// F → A の寄り道で周が伸び、次周の B 読みが S(WTX) で 0.87 秒。周末の touch から
    /// 読了まで 0.89 秒でも同じタップ。**cooldown 500ms でも塞がる**ことを、穴が
    /// cooldown の長さの問題ではないことの証拠として固定する。
    /// B が応答したが読了しなかった周は、`poll_license` が読み取り直後 (次の poll の前) に
    /// touch する (nfc.rs) — 呼び出し順もここで模す
    #[test]
    fn issue171_measured_detour_then_wtx_read_stays_one_tap_even_with_short_cooldown() {
        let mut g = TapGate::new(500);
        let mut fired = Vec::new();
        let mut push = |o: TapOutcome<&'static str>, out: &mut Vec<_>| {
            if !matches!(o, TapOutcome::Idle) {
                out.push(o);
            }
        };
        // 周 1: B 読了 (200ms)。RF 応答ありなので周末に touch
        g.observe("LICENSE", "LICENSE", 0, 200);
        push(g.poll(200), &mut fired);
        g.touch(200);
        // 周 2 (220〜): B が -4 (SELECT MF 失敗、応答あり) を 100ms で返す →
        // 読み取り直後の touch → poll → F (91ms) → poll → A (140ms) → poll → 周末 touch
        g.touch(320);
        push(g.poll(320), &mut fired);
        push(g.poll(411), &mut fired);
        push(g.poll(551), &mut fired);
        g.touch(551);
        // 周 3 (571〜): B 読みが S(WTX) で 0.87 秒。読了 1_441 = 周末 touch から 0.89 秒
        g.observe("LICENSE", "LICENSE", 571, 1_441);
        push(g.poll(1_441), &mut fired);
        g.touch(1_441);
        // 次の確定窓が閉じるところまで回す (二重打刻ならここで 2 つ目の Fire が出る)
        push(g.poll(1_461), &mut fired);
        push(g.poll(1_700), &mut fired);
        assert_eq!(
            fired,
            vec![TapOutcome::Fire("LICENSE")],
            "寄り道 + WTX 読みで観測間隔が cooldown を超えても、載せたままなら 1 タップ"
        );
    }

    /// **読了しなかった長い読みでも同じタップ**: B が S(WTX) で 2 秒待った末に途中死 (-6) した周。
    /// 読み取り直後の touch が次の `poll` の expire より**前**にあれば last は消えず、
    /// 次周の読了は同じタップ。逆順 (poll → touch) だと expire が last を消し、touch は
    /// 空振り (last=None は何もしない) → 次周が新タップ = 二重打刻になる。
    /// nfc.rs の `poll_license` がこの順で呼ぶことを、ここで呼び出しパターンとして固定する
    #[test]
    fn issue171_failed_long_read_touch_before_poll_keeps_the_tap() {
        let mut g = TapGate::new(1_000);
        let mut fired = Vec::new();
        let mut push = |o: TapOutcome<&'static str>, out: &mut Vec<_>| {
            if !matches!(o, TapOutcome::Idle) {
                out.push(o);
            }
        };
        g.observe("LICENSE", "LICENSE", 0, 200);
        push(g.poll(200), &mut fired);
        g.touch(200);
        // 周 2 (220〜): B が 2 秒ブロックして -6。poll_license が touch してから poll
        g.touch(2_220);
        push(g.poll(2_220), &mut fired);
        g.touch(2_220);
        // 周 3 (2_240〜): B 読了 (200ms)
        g.observe("LICENSE", "LICENSE", 2_240, 2_440);
        push(g.poll(2_440), &mut fired);
        g.touch(2_440);
        push(g.poll(2_700), &mut fired);
        assert_eq!(fired, vec![TapOutcome::Fire("LICENSE")]);
    }

    /// **退行の境界 (FelicaFirst 相当 = `release` が無い)**: 読み 0.2 s のタップを
    /// 1.5 s 間隔で 2 回 → 2 回とも発火する。since で判定するぶん再タップの境界は
    /// 読み時間 (0.2 s) だけ手前に動くが、1.5 s 間隔なら影響しない (モジュール doc の副作用)
    #[test]
    fn issue171_felicafirst_retaps_1500ms_apart_both_fire_without_release() {
        let mut g = TapGate::new(1_000);
        let mut fired = Vec::new();
        for tap in 0..2u64 {
            // F/A 空振り後の B 読み: 開始 t、読了 t + 200。周末に touch (存在検知)
            let t = tap * 1_500;
            g.observe("LICENSE", "LICENSE", t, t + T_B);
            g.touch(t + T_B);
            // 離れた。release は呼ばれない (FelicaFirst)。次のタップまで poll だけ回る
            fired.extend(poll_until(&mut g, t + T_B, t + 1_480));
        }
        assert_eq!(
            fired,
            vec![TapOutcome::Fire("LICENSE"), TapOutcome::Fire("LICENSE")],
            "release 無しでも 1.5 s 間隔の再タップは 2 回とも別打刻"
        );
    }

    /// **無応答 (-2 / -1) の周では touch しない**ので、載っていない時間は cooldown どおり
    /// 数えられる。nfc.rs の `poll_license` は「応答あり・未読了」(rc ∉ {0, -2, -1}) でだけ
    /// touch する — 無応答でも touch すると離れたカードの last が生き続け、再タップが
    /// 抑止される。ここでは「touch が無ければ cooldown で区切れる」側を固定する
    #[test]
    fn issue171_no_response_cycle_does_not_touch_so_cooldown_still_splits() {
        let mut g = TapGate::new(1_000);
        g.observe("LICENSE", "LICENSE", 0, 200);
        g.touch(200);
        assert_eq!(
            poll_until(&mut g, 200, 600),
            vec![TapOutcome::Fire("LICENSE")]
        );
        // 以降の周は B が -2 (無応答、~180ms): touch は無く poll だけ
        assert_eq!(poll_until(&mut g, 780, 1_320), vec![]);
        // 前回の観測 (200) から 1 秒以上 → 読み始め 1_340 の再タップは新タップ
        g.observe("LICENSE", "LICENSE", 1_340, 1_540);
        assert_eq!(
            poll_until(&mut g, 1_540, 1_900),
            vec![TapOutcome::Fire("LICENSE")]
        );
    }

    /// release (#169) の後に始めた読みは、読み始めが cooldown 内でも新タップ
    /// (release と since の判定は矛盾しない: release が last を消すので since は比較されない)
    #[test]
    fn issue171_read_started_after_release_is_a_new_tap() {
        let mut g = TapGate::new(1_000);
        g.observe("LICENSE", "LICENSE", 0, 200);
        assert_eq!(
            poll_until(&mut g, 200, 600),
            vec![TapOutcome::Fire("LICENSE")]
        );
        g.release();
        // 離れてから 300ms で再タップ。読みは 2 秒かかっても 1 回だけ発火
        g.observe("LICENSE", "LICENSE", 900, 2_900);
        assert_eq!(
            poll_until(&mut g, 2_900, 3_300),
            vec![TapOutcome::Fire("LICENSE")]
        );
    }

    /// **確定窓の起点は読了時刻のまま** (2 枚検知 #143 の窓が since で縮まない)。
    /// 2 秒かかった B の読了直後 (窓の中) に別キーが読めたら、従来どおり 2 枚
    #[test]
    fn issue171_commit_window_still_starts_at_read_end() {
        let mut g = TapGate::default();
        // 読み始め 0、読了 2_000。since で窓を測ると 2_000 の時点で閉じているはず
        g.observe("LICENSE", "LICENSE", 0, 2_000);
        assert_eq!(g.poll(2_000), TapOutcome::Idle, "読了直後はまだ確定窓の中");
        // 次周 F (111ms 後) で 2 枚目
        g.observe("FELICA_2", "FELICA_2", 2_020, 2_111);
        assert_eq!(
            poll_until(&mut g, 2_111, 2_111 + W + 500),
            vec![TapOutcome::MultipleCards]
        );
    }

    /// 確定窓の途中で始めた 2 枚目の読みが窓の外で読了しても 2 枚。
    /// キー違いは時刻を見ず、窓は `poll()` が呼ばれて初めて閉じる (#155 と同じ。従来どおり)
    #[test]
    fn issue171_second_key_read_finishing_after_window_is_still_two_cards() {
        let mut g = TapGate::default();
        g.observe("FELICA_1", "FELICA_1", 0, 91);
        // A の読みは 111 に始まり (窓 91 + 250 = 341 の中)、車検証の ISO-DEP が
        // 延びて 400 に読了 (窓の外)。あいだに poll は入らない
        g.observe("NFCA_2", "NFCA_2", 111, 400);
        assert_eq!(
            poll_until(&mut g, 400, 800),
            vec![TapOutcome::MultipleCards]
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
