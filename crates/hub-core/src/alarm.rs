//! 警告デバイス (Atom VoiceS3R) の鳴動判定 — **沈黙を異常とみなす** (issue #135)。
//!
//! 点呼キオスク (ブラウザ) が USB CDC で 3 秒ごとに「正常」を送り、**途切れたら
//! 端末が自分の判断で鳴る**。ブラウザが「鳴れ」と命令する形にしないのが設計の要:
//! 命令駆動だとブラウザ / PC が落ちたときに命令が来ず沈黙する = **一番危ない
//! ケースで鳴らない**。沈黙を異常とみなせば、ブラウザのクラッシュ・タブを閉じた・
//! PC のフリーズを**同じ形で**拾える (plan/standing-devices.md §4.1)。
//!
//! ここは純粋ロジックだけを持ち、音を出す・行を書き出す副作用は firmware 側が
//! [`Action`] を受けて行う。時刻は呼び出し側が単調増加のミリ秒 (ESP-IDF の
//! `esp_timer_get_time() / 1000` 相当) で渡す。
//!
//! ```text
//!            HB OK が届き続ける                 10 秒 無音 / HB NG / call=1
//!   ┌──────────────────────────┐        ┌──────────────────────────────┐
//!   │                          ▼        │                              ▼
//!  Idle ──abnormal──▶ Alarming ──ボタン──▶ Muted ──!abnormal──▶ Idle (PlayResolved)
//!         (即 1 回)   call / ng:  1.8 秒ごとに   ▲          │  5 秒ごとに PlayMutedTick
//!                                 PlayAlert (3 連) └──ボタン──┘  (短い単発)
//!                     silence:    5 秒ごとに
//!                                 PlaySilenceTick (短い 2 連)
//! ```
//!
//! **鳴動中の音は理由で変える** — 沈黙 (`Cause::Silence` = USB が抜けた / ブラウザを
//! 閉じた / 運行管理者タブから離れた) は [`SILENCE_TICK_MS`] ごとの短い 2 連
//! ([`Action::PlaySilenceTick`])、着信 (`Cause::Call`) と NG は [`ALERT_PERIOD_MS`]
//! ごとの 3 連 ([`Action::PlayAlert`]) のまま。人を呼ぶ音は強いままにし、
//! 「繋がっていない」は 5 秒に 1 回程度でよいというユーザー要望 (2026-09-09、
//! 運行管理者 PC での実機確認後)。音の種類が変わった瞬間 (沈黙中に着信が来た等) は
//! 周期を待たずに新しい音を 1 回出し、周期をそこから引き直す。
//!
//! **ボタンはトグル** — 鳴動中に押すと黙り、黙っているあいだにもう一度押すと
//! 鳴動へ戻る。黙らせているあいだも [`MUTED_TICK_MS`] ごとに
//! [`Action::PlayMutedTick`] を出す。**完全な無音にはしない** — 異常が続いて
//! いることを忘れられるため (2026-09-09 の実機確認でのユーザー要望)。

/// 最後の heartbeat からこれだけ間が空いたら沈黙 = 異常とみなす。
/// キオスク側の送信間隔は 3 秒 (背面タブの `setInterval` スロットルは 1 秒までなので
/// 3 秒間隔なら間に合う) で、その 3 回ぶんの余裕を見た初期値 (plan §4.1)
pub const SILENCE_MS: u64 = 10_000;

/// 起動直後の猶予。**一度も heartbeat を受け取っていない**あいだはこの時間まで
/// 鳴らさない。USB を挿してブラウザを開くまでの間にいきなり鳴るのを避ける
pub const BOOT_GRACE_MS: u64 = 30_000;

/// 鳴動中に警告音を出し直す周期。**繰り返しはこのモニタが刻む** — 再生スレッド側で
/// ループを作ると再生中にボタン停止を受け取れず、止めたのに鳴り続ける
pub const ALERT_PERIOD_MS: u64 = 1_800;

/// 状態が変わらなくてもホストへ `EVT ALARM` を出し直す周期 (ブラウザのバナー用)。
/// ブラウザは遷移イベントを取りこぼしても、次のこれで現在値に追いつける
pub const BANNER_MS: u64 = 5_000;

/// ボタンで黙らせているあいだ、短い合図 ([`Action::PlayMutedTick`]) を出す周期。
/// **完全な無音にすると異常が続いていることを忘れられる** — 実機で鳴らした
/// ユーザーの要望 (2026-09-09)。鳴動の周期 (`ALERT_PERIOD_MS`) より長く取り、
/// 「鳴っている」ではなく「まだ直っていない」と伝わる間隔にしてある
pub const MUTED_TICK_MS: u64 = 5_000;

/// 沈黙 (heartbeat が来ない = USB 抜け / ブラウザを閉じた / 運行管理者タブから離れた)
/// で鳴動中に短い 2 連 ([`Action::PlaySilenceTick`]) を出す周期。
/// 「S3 と Windows が繋がっていない」ときの警告は **5 秒に 1 回程度でいい**という
/// ユーザー要望 (2026-09-09、運行管理者 PC での実機確認後)。着信 / NG の 3 連
/// (`ALERT_PERIOD_MS`) はそのまま — 人を呼ぶ音は強いままにする
pub const SILENCE_TICK_MS: u64 = 5_000;

/// `HB NG` に理由ラベルが付いていなかったときに使う既定の理由。
/// `cause=ng:` のように空で出すと行の文法 (`ng:<reason>`) が崩れるため
pub const DEFAULT_NG_REASON: &str = "unspecified";

/// firmware 側が実行する副作用
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 警告音を 1 回鳴らす (`Sound::Alert` = 3000Hz 200ms ×3)
    PlayAlert,
    /// 「直った」合図を鳴らす (`Sound::AlertResolved` = 1200Hz 150ms ×1)。
    /// **ボタンで止めたときには鳴らさない** — ボタンなら異常は続いているので
    /// 人が見に行く必要があり、状態解消なら放置でよい。この区別を現場で音だけで
    /// つけるための合図 (plan/standing-devices.md §2.1)
    PlayResolved,
    /// 黙らせているあいだの短い合図 (`Sound::MutedTick` = 3000Hz 60ms ×1)。
    /// [`MUTED_TICK_MS`] ごとに出る。**3 連の警告音と紛れないよう 1 発だけ** —
    /// 鳴動へ戻すのはボタンだけで、これは「まだ直っていない」ことを思い出させる
    /// ための最小限の合図 (plan/standing-devices.md §4.4)
    PlayMutedTick,
    /// 沈黙で鳴動中の短い 2 連 (`Sound::SilenceTick` = 3000Hz 60ms ×2)。
    /// [`SILENCE_TICK_MS`] ごとに出る。**Muted の単発 ([`Self::PlayMutedTick`]) と
    /// 聞き分けられるよう 2 連**、着信 / NG の 3 連 ([`Self::PlayAlert`]) より弱い音
    PlaySilenceTick,
    /// この行をホスト (キオスク) へ書き出す (`EVT ALARM state=... cause=...`)
    Emit(String),
}

/// 鳴っている理由。**優先は silence > call > ng** — 沈黙はキオスク自身の申告が
/// 当てにならない状態なので、同時に成立していたら沈黙を表に出す
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    /// 異常なし
    None,
    /// heartbeat が途切れた (ブラウザのクラッシュ・タブを閉じた・PC のフリーズ・USB 抜け)
    Silence,
    /// 点呼の呼び出しが来ている (`HB ... call=1`。plan §4.2 の案 B)
    Call,
    /// キオスクが異常を自覚している (`HB NG <reason>`)
    Ng(String),
}

impl Cause {
    /// 行プロトコルに載せる形 (`none` / `silence` / `call` / `ng:<reason>`)
    pub fn label(&self) -> String {
        match self {
            Self::None => "none".to_string(),
            Self::Silence => "silence".to_string(),
            Self::Call => "call".to_string(),
            Self::Ng(reason) => format!("ng:{reason}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum State {
    Idle,
    Alarming { next_beep_at: u64 },
    /// `next_muted_tick_at` = 次に短い合図を出す時刻。バナー (`BANNER_MS`) とは
    /// 独立に刻む — バナーは emit のたびに引き直されるので合図の間隔に使えない
    Muted { next_muted_tick_at: u64 },
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Alarming { .. } => "alarming",
            Self::Muted { .. } => "muted",
        }
    }
}

/// heartbeat の受信と鳴動状態の管理
#[derive(Debug, Clone)]
pub struct AlarmMonitor {
    state: State,
    /// 最後に heartbeat を受けた時刻。`None` = 起動後まだ一度も受けていない
    last_hb_at: Option<u64>,
    hb_ok: bool,
    hb_reason: Option<String>,
    hb_call: bool,
    /// 直近に `Emit` した cause (変化の検出用)
    last_cause: Cause,
    next_banner_at: u64,
}

impl Default for AlarmMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl AlarmMonitor {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            last_hb_at: None,
            // 未受信のあいだは「正常」に倒しておく。この間の異常判定は
            // BOOT_GRACE_MS の沈黙だけが担う
            hb_ok: true,
            hb_reason: None,
            hb_call: false,
            last_cause: Cause::None,
            next_banner_at: BANNER_MS,
        }
    }

    /// heartbeat を 1 行受け取る (`HB OK` / `HB NG <reason>` / 末尾 `call=0|1`)。
    /// **状態遷移はここでは起こさない** — 次の [`Self::tick`] でまとめて判定する
    pub fn on_heartbeat(&mut self, now_ms: u64, ok: bool, reason: Option<&str>, call: bool) {
        self.last_hb_at = Some(now_ms);
        self.hb_ok = ok;
        self.hb_reason = reason.map(|s| s.to_string());
        self.hb_call = call;
    }

    /// 本体ボタン (VoiceS3R は G41) が押された。**トグル** — 鳴動中なら黙らせ、
    /// 黙らせているあいだに押されたら鳴動へ戻す。
    ///
    /// 黙らせるときは**音を鳴らさない** (「直った」合図と紛れさせないため)。
    /// 鳴動へ戻すときは押した手応えを兼ねて即 1 回鳴らし (音と周期は理由で選ぶ —
    /// `alarm_sound`)、次の周期を `now + 周期` に置く (Idle → Alarming と同じ入り方)。
    /// Idle での押下は何も起こさない
    pub fn on_button(&mut self, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        match self.state {
            State::Alarming { .. } => {
                self.state = State::Muted {
                    next_muted_tick_at: now_ms + MUTED_TICK_MS,
                };
                let cause = self.cause_at(now_ms);
                self.emit(now_ms, &cause, &mut out);
            }
            State::Muted { .. } => {
                let cause = self.cause_at(now_ms);
                let (sound, period) = alarm_sound(&cause);
                self.state = State::Alarming {
                    next_beep_at: now_ms + period,
                };
                out.push(sound);
                self.emit(now_ms, &cause, &mut out);
            }
            State::Idle => {}
        }
        out
    }

    /// 時間を進めて副作用を取り出す。firmware 側から短い周期で呼ぶ
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        let cause = self.cause_at(now_ms);
        let abnormal = !matches!(cause, Cause::None);
        match self.state {
            State::Idle => {
                if abnormal {
                    let (sound, period) = alarm_sound(&cause);
                    self.state = State::Alarming {
                        next_beep_at: now_ms + period,
                    };
                    out.push(sound);
                    self.emit(now_ms, &cause, &mut out);
                }
            }
            State::Alarming { next_beep_at } => {
                if abnormal {
                    let (sound, period) = alarm_sound(&cause);
                    if sound != alarm_sound(&self.last_cause).0 {
                        // 音の種類が変わった (沈黙 ⇄ 着信 / NG)。周期を待たずに
                        // 新しい音を 1 回出し、周期をここから引き直す — 沈黙中に
                        // 着信が来たら 5 秒待たずに 3 連へ切り替わる
                        out.push(sound);
                        self.state = State::Alarming {
                            next_beep_at: now_ms + period,
                        };
                    } else if now_ms >= next_beep_at {
                        out.push(sound);
                        self.state = State::Alarming {
                            next_beep_at: next_beep_at + period,
                        };
                    }
                    // 同じ種類の音のまま理由だけ変わった (ng の reason 等) なら
                    // 鳴らし直さないが、ホストへは伝える
                    if cause != self.last_cause {
                        self.emit(now_ms, &cause, &mut out);
                    }
                } else {
                    self.resolve(now_ms, &cause, &mut out);
                }
            }
            // ボタンで黙らせた後。3 連の鳴動には戻さない (人が認識済みなので
            // cause が変わっても鳴らし直さない) が、**完全な無音にもしない** —
            // MUTED_TICK_MS ごとに短い合図を出し、異常が続いていることを
            // 思い出させる (2026-09-09 の実機確認での要望。plan §4.4)。
            // 鳴動へ戻すのはボタンだけ (on_button のトグル)
            State::Muted { next_muted_tick_at } => {
                if abnormal {
                    if now_ms >= next_muted_tick_at {
                        out.push(Action::PlayMutedTick);
                        self.state = State::Muted {
                            next_muted_tick_at: next_muted_tick_at + MUTED_TICK_MS,
                        };
                    }
                } else {
                    self.resolve(now_ms, &cause, &mut out);
                }
            }
        }
        if now_ms >= self.next_banner_at {
            let cause = self.cause_at(now_ms);
            self.emit(now_ms, &cause, &mut out);
        }
        out
    }

    /// `STATUS` への応答行。**呼び出し側が末尾に ` VER=<firmware_version_full()>` を
    /// 足して返すこと** (バージョンは hub-common 側にあり、このクレートからは見えない)。
    /// 先頭 2 トークン `STATUS alarm` は**ブラウザ側が機種を識別する目印**なので変えない
    /// — CoreS3 と VoiceS3R は USB の VID/PID が同一で記述子では見分けられない
    pub fn status_line(&self, now_ms: u64) -> String {
        let age = match self.last_hb_at {
            Some(t) => now_ms.saturating_sub(t).to_string(),
            None => "-".to_string(),
        };
        format!(
            "STATUS alarm state={} cause={} hb_age_ms={}",
            self.state.label(),
            self.cause_at(now_ms).label(),
            age,
        )
    }

    /// 異常が解消したときの共通処理 (鳴動中でもボタンで黙らせた後でも同じ)
    fn resolve(&mut self, now_ms: u64, cause: &Cause, out: &mut Vec<Action>) {
        self.state = State::Idle;
        out.push(Action::PlayResolved);
        self.emit(now_ms, cause, out);
    }

    fn emit(&mut self, now_ms: u64, cause: &Cause, out: &mut Vec<Action>) {
        self.last_cause = cause.clone();
        self.next_banner_at = now_ms + BANNER_MS;
        out.push(Action::Emit(format!(
            "EVT ALARM state={} cause={}",
            self.state.label(),
            cause.label(),
        )));
    }

    /// 今の時刻での異常の有無と理由
    fn cause_at(&self, now_ms: u64) -> Cause {
        let silence = match self.last_hb_at {
            // 一度も受けていないうちは起動猶予いっぱいまで待つ
            None => now_ms >= BOOT_GRACE_MS,
            Some(t) => now_ms.saturating_sub(t) >= SILENCE_MS,
        };
        if silence {
            return Cause::Silence;
        }
        if self.hb_call {
            return Cause::Call;
        }
        if !self.hb_ok {
            let reason = self
                .hb_reason
                .clone()
                .unwrap_or_else(|| DEFAULT_NG_REASON.to_string());
            return Cause::Ng(reason);
        }
        Cause::None
    }
}

/// 鳴動中に出す音とその周期を理由で選ぶ。沈黙は短い 2 連を [`SILENCE_TICK_MS`]
/// ごと、着信 / NG は 3 連を [`ALERT_PERIOD_MS`] ごと。`Cause::None` は鳴動中には
/// 来ない (先に `resolve` へ落ちる) が、来ても 3 連側に倒す
fn alarm_sound(cause: &Cause) -> (Action, u64) {
    match cause {
        Cause::Silence => (Action::PlaySilenceTick, SILENCE_TICK_MS),
        Cause::Call | Cause::Ng(_) | Cause::None => (Action::PlayAlert, ALERT_PERIOD_MS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(state: &str, cause: &str) -> Action {
        Action::Emit(format!("EVT ALARM state={state} cause={cause}"))
    }

    #[test]
    fn boot_grace_delays_the_first_silence_alarm() {
        let mut m = AlarmMonitor::new();
        // 起動直後は一度も heartbeat が無くても鳴らない
        assert_eq!(m.tick(0), vec![]);
        // 猶予中でもバナーは出る (ブラウザ側が現在値に追いつけるように)
        assert_eq!(m.tick(BOOT_GRACE_MS - 1), vec![emit("idle", "none")]);
        assert_eq!(
            m.status_line(BOOT_GRACE_MS - 1),
            "STATUS alarm state=idle cause=none hb_age_ms=-"
        );
        // 猶予を過ぎても届かなければ沈黙 = 異常 (沈黙は短い 2 連から)
        assert_eq!(
            m.tick(BOOT_GRACE_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        assert_eq!(
            m.status_line(BOOT_GRACE_MS),
            "STATUS alarm state=alarming cause=silence hb_age_ms=-"
        );
    }

    #[test]
    fn silence_alarms_repeats_and_resolves() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(1_000, true, None, false);
        assert_eq!(m.tick(1_000), vec![]);
        assert_eq!(
            m.status_line(4_000),
            "STATUS alarm state=idle cause=none hb_age_ms=3000"
        );
        // 10 秒に 1ms 足りないうちは鳴らない (出るのはバナーだけ)
        assert_eq!(m.tick(1_000 + SILENCE_MS - 1), vec![emit("idle", "none")]);
        assert_eq!(
            m.tick(1_000 + SILENCE_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        // 沈黙は 5 秒周期。3 連の周期 (1.8 秒) では鳴らし直さない
        assert_eq!(m.tick(1_000 + SILENCE_MS + ALERT_PERIOD_MS), vec![]);
        // 1ms 足りないうちは出ない (バナーは emit の 5 秒後なので同時に来る)
        assert_eq!(m.tick(1_000 + SILENCE_MS + SILENCE_TICK_MS - 1), vec![]);
        // 周期が来たら短い 2 連 (cause は変わっていないので EVT はバナーぶんだけ)
        assert_eq!(
            m.tick(1_000 + SILENCE_MS + SILENCE_TICK_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        // heartbeat が戻ったら「直った」合図
        m.on_heartbeat(17_000, true, None, false);
        assert_eq!(
            m.tick(17_000),
            vec![Action::PlayResolved, emit("idle", "none")]
        );
    }

    #[test]
    fn ng_reason_change_is_reported_without_re_ringing() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, false, Some("serial"), false);
        assert_eq!(
            m.tick(0),
            vec![Action::PlayAlert, emit("alarming", "ng:serial")]
        );
        m.on_heartbeat(500, false, Some("nfc_bridge"), false);
        assert_eq!(m.tick(500), vec![emit("alarming", "ng:nfc_bridge")]);
    }

    #[test]
    fn ng_without_reason_falls_back_to_a_grammar_safe_label() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, false, None, false);
        assert_eq!(
            m.status_line(0),
            "STATUS alarm state=idle cause=ng:unspecified hb_age_ms=0"
        );
    }

    #[test]
    fn cause_priority_is_silence_then_call_then_ng() {
        let mut m = AlarmMonitor::new();
        // NG と call が同時なら call
        m.on_heartbeat(0, false, Some("serial"), true);
        assert_eq!(
            m.status_line(0),
            "STATUS alarm state=idle cause=call hb_age_ms=0"
        );
        // 沈黙が成立したら silence が勝つ (キオスクの自己申告は当てにならない)
        assert_eq!(
            m.status_line(SILENCE_MS),
            "STATUS alarm state=idle cause=silence hb_age_ms=10000"
        );
    }

    #[test]
    fn button_mutes_until_the_cause_is_gone() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, true);
        assert_eq!(m.tick(0), vec![Action::PlayAlert, emit("alarming", "call")]);
        // ボタンは黙らせるだけ (音は鳴らさない)
        assert_eq!(m.on_button(100), vec![emit("muted", "call")]);
        assert_eq!(
            m.status_line(200),
            "STATUS alarm state=muted cause=call hb_age_ms=200"
        );
        // 鳴動周期が来ても 3 連は鳴らない
        assert_eq!(m.tick(ALERT_PERIOD_MS + 200), vec![]);
        // 人が認識済みなので、理由が変わっても鳴らし直さない
        m.on_heartbeat(2_500, false, Some("serial"), false);
        assert_eq!(m.tick(2_500), vec![]);
        // 解消したときだけ「直った」合図が鳴る
        m.on_heartbeat(3_000, true, None, false);
        assert_eq!(
            m.tick(3_000),
            vec![Action::PlayResolved, emit("idle", "none")]
        );
        // 解消したら短い合図も止まる (Muted のカウンタは持ち越さない)
        m.on_heartbeat(3_000 + MUTED_TICK_MS, true, None, false);
        assert_eq!(m.tick(3_000 + MUTED_TICK_MS), vec![emit("idle", "none")]);
    }

    #[test]
    fn muted_beeps_a_short_reminder_every_five_seconds() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, true);
        assert_eq!(m.tick(0), vec![Action::PlayAlert, emit("alarming", "call")]);
        assert_eq!(m.on_button(0), vec![emit("muted", "call")]);
        // 1ms 足りないうちは出ない
        m.on_heartbeat(MUTED_TICK_MS - 1, true, None, true);
        assert_eq!(m.tick(MUTED_TICK_MS - 1), vec![]);
        // 5 秒でバナーと重なる。Action の順は PlayMutedTick → Emit
        m.on_heartbeat(MUTED_TICK_MS, true, None, true);
        assert_eq!(
            m.tick(MUTED_TICK_MS),
            vec![Action::PlayMutedTick, emit("muted", "call")]
        );
        // 2 回目もその 5 秒後
        m.on_heartbeat(2 * MUTED_TICK_MS - 1, true, None, true);
        assert_eq!(m.tick(2 * MUTED_TICK_MS - 1), vec![]);
        m.on_heartbeat(2 * MUTED_TICK_MS, true, None, true);
        assert_eq!(
            m.tick(2 * MUTED_TICK_MS),
            vec![Action::PlayMutedTick, emit("muted", "call")]
        );
    }

    #[test]
    fn pressing_the_button_again_restores_the_alarm() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, true);
        assert_eq!(m.tick(0), vec![Action::PlayAlert, emit("alarming", "call")]);
        assert_eq!(m.on_button(0), vec![emit("muted", "call")]);
        // もう一度押すと鳴動へ戻る (即 PlayAlert)
        assert_eq!(
            m.on_button(1_000),
            vec![Action::PlayAlert, emit("alarming", "call")]
        );
        assert_eq!(
            m.status_line(1_000),
            "STATUS alarm state=alarming cause=call hb_age_ms=1000"
        );
        // 戻った後は、黙らせていたときのカウンタが来ても短い合図は出ない
        assert_eq!(m.tick(MUTED_TICK_MS), vec![Action::PlayAlert]);
    }

    #[test]
    fn button_while_idle_does_nothing() {
        let mut m = AlarmMonitor::new();
        assert_eq!(m.on_button(0), vec![]);
    }

    #[test]
    fn banner_repeats_on_its_own_period() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, false);
        assert_eq!(m.tick(BANNER_MS), vec![emit("idle", "none")]);
        assert_eq!(m.tick(BANNER_MS + 1), vec![]);
        m.on_heartbeat(6_000, true, None, false);
        assert_eq!(m.tick(2 * BANNER_MS), vec![emit("idle", "none")]);
    }

    #[test]
    fn call_keeps_the_triple_beep_on_its_own_period() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, true);
        assert_eq!(m.tick(0), vec![Action::PlayAlert, emit("alarming", "call")]);
        // 着信は 1.8 秒周期の 3 連のまま (人を呼ぶ音は強いまま)
        assert_eq!(m.tick(ALERT_PERIOD_MS - 1), vec![]);
        assert_eq!(m.tick(ALERT_PERIOD_MS), vec![Action::PlayAlert]);
        assert_eq!(m.tick(2 * ALERT_PERIOD_MS), vec![Action::PlayAlert]);
    }

    #[test]
    fn a_call_during_silence_switches_to_the_alert_immediately() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, false);
        assert_eq!(
            m.tick(SILENCE_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        // 沈黙中に着信 (HB call=1) が来たら、5 秒待たずに即 3 連へ切り替わる
        let t = SILENCE_MS + 100;
        m.on_heartbeat(t, true, None, true);
        assert_eq!(m.tick(t), vec![Action::PlayAlert, emit("alarming", "call")]);
        // 周期は切り替えた時刻から 1.8 秒で引き直す
        assert_eq!(m.tick(t + ALERT_PERIOD_MS - 1), vec![]);
        assert_eq!(m.tick(t + ALERT_PERIOD_MS), vec![Action::PlayAlert]);
        // 元の沈黙の周期 (SILENCE_MS + 5 秒) が来ても 2 連は出ない (3 連の続きだけ)
        assert_eq!(
            m.tick(SILENCE_MS + SILENCE_TICK_MS),
            vec![Action::PlayAlert]
        );
    }

    #[test]
    fn losing_the_heartbeat_during_a_call_falls_back_to_the_silence_tick() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, true);
        assert_eq!(m.tick(0), vec![Action::PlayAlert, emit("alarming", "call")]);
        assert_eq!(m.tick(ALERT_PERIOD_MS), vec![Action::PlayAlert]);
        // 3 連の周期 (3.6 秒) とバナー (5 秒) が同時に来る
        assert_eq!(
            m.tick(BANNER_MS),
            vec![Action::PlayAlert, emit("alarming", "call")]
        );
        // HB が止まって沈黙が成立したら、次の 3 連を待たずに短い 2 連へ落ちる
        assert_eq!(
            m.tick(SILENCE_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        // 以後は 5 秒周期。1.8 秒では鳴らない
        assert_eq!(m.tick(SILENCE_MS + ALERT_PERIOD_MS), vec![]);
        assert_eq!(m.tick(SILENCE_MS + SILENCE_TICK_MS - 1), vec![]);
        assert_eq!(
            m.tick(SILENCE_MS + SILENCE_TICK_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
    }

    #[test]
    fn button_toggle_uses_the_silence_tick_while_silent() {
        let mut m = AlarmMonitor::new();
        m.on_heartbeat(0, true, None, false);
        assert_eq!(
            m.tick(SILENCE_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        // 黙らせるときは音を鳴らさない
        assert_eq!(
            m.on_button(SILENCE_MS + 100),
            vec![emit("muted", "silence")]
        );
        // 黙らせているあいだの合図は単発 (2 連とは別)
        assert_eq!(
            m.tick(SILENCE_MS + 100 + MUTED_TICK_MS),
            vec![Action::PlayMutedTick, emit("muted", "silence")]
        );
        // 鳴動へ戻すときは理由が沈黙なので 2 連から
        let t = SILENCE_MS + 100 + MUTED_TICK_MS + 100;
        assert_eq!(
            m.on_button(t),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
        assert_eq!(m.tick(t + SILENCE_TICK_MS - 1), vec![]);
        assert_eq!(
            m.tick(t + SILENCE_TICK_MS),
            vec![Action::PlaySilenceTick, emit("alarming", "silence")]
        );
    }

    #[test]
    fn default_is_idle_and_the_types_are_printable() {
        let m = AlarmMonitor::default();
        assert_eq!(
            m.status_line(0),
            "STATUS alarm state=idle cause=none hb_age_ms=-"
        );
        assert!(format!("{:?}", m.clone()).contains("AlarmMonitor"));
        for a in [
            Action::PlayAlert,
            Action::PlayResolved,
            Action::PlayMutedTick,
            Action::PlaySilenceTick,
            Action::Emit("x".into()),
        ] {
            assert!(!format!("{:?}", a.clone()).is_empty());
        }
        for c in [Cause::None, Cause::Silence, Cause::Call] {
            assert!(!format!("{:?}", c.clone()).is_empty());
        }
        assert_eq!(Cause::None.label(), "none");
        assert_eq!(Cause::Silence.label(), "silence");
        assert_eq!(Cause::Call.label(), "call");
        assert_eq!(Cause::Ng("serial".into()).label(), "ng:serial");
    }
}
