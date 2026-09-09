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
//!         PlayAlert   1.8 秒ごとに        音を止めるだけ
//!                     PlayAlert           (異常が続く限り維持)
//! ```

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
    Muted,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Alarming { .. } => "alarming",
            Self::Muted => "muted",
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

    /// 本体ボタン (VoiceS3R は G41) が押された。鳴動中なら黙らせる。
    /// **音は鳴らさない** — 「直った」合図と紛れさせないため
    pub fn on_button(&mut self, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if matches!(self.state, State::Alarming { .. }) {
            self.state = State::Muted;
            let cause = self.cause_at(now_ms);
            self.emit(now_ms, &cause, &mut out);
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
                    self.state = State::Alarming {
                        next_beep_at: now_ms + ALERT_PERIOD_MS,
                    };
                    out.push(Action::PlayAlert);
                    self.emit(now_ms, &cause, &mut out);
                }
            }
            State::Alarming { next_beep_at } => {
                if abnormal {
                    if now_ms >= next_beep_at {
                        out.push(Action::PlayAlert);
                        self.state = State::Alarming {
                            next_beep_at: next_beep_at + ALERT_PERIOD_MS,
                        };
                    }
                    // 鳴らし直しはしないが、理由が変わったらホストへは伝える
                    if cause != self.last_cause {
                        self.emit(now_ms, &cause, &mut out);
                    }
                } else {
                    self.resolve(now_ms, &cause, &mut out);
                }
            }
            // ボタンで黙らせた後は、異常が完全に解消するまで維持する。
            // 人が認識済みなので cause が変わっても鳴らし直さない (plan §4.4)
            State::Muted => {
                if !abnormal {
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
        // 猶予を過ぎても届かなければ沈黙 = 異常
        assert_eq!(
            m.tick(BOOT_GRACE_MS),
            vec![Action::PlayAlert, emit("alarming", "silence")]
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
            vec![Action::PlayAlert, emit("alarming", "silence")]
        );
        // 周期が来るまでは鳴らし直さない
        assert_eq!(m.tick(1_000 + SILENCE_MS + ALERT_PERIOD_MS - 1), vec![]);
        // 周期が来たら音だけ (cause は変わっていないので EVT は出さない)
        assert_eq!(
            m.tick(1_000 + SILENCE_MS + ALERT_PERIOD_MS),
            vec![Action::PlayAlert]
        );
        // heartbeat が戻ったら「直った」合図
        m.on_heartbeat(13_000, true, None, false);
        assert_eq!(
            m.tick(13_000),
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
        // 鳴動周期が来ても鳴らない
        assert_eq!(m.tick(ALERT_PERIOD_MS + 200), vec![]);
        // 人が認識済みなので、理由が変わっても鳴らし直さない
        m.on_heartbeat(2_500, false, Some("serial"), false);
        assert_eq!(m.tick(2_500), vec![]);
        // 二度押しは何も起こさない
        assert_eq!(m.on_button(2_600), vec![]);
        // 解消したときだけ「直った」合図が鳴る
        m.on_heartbeat(3_000, true, None, false);
        assert_eq!(
            m.tick(3_000),
            vec![Action::PlayResolved, emit("idle", "none")]
        );
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
