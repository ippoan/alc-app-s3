//! 免許証タップ → 点呼確認画面の純粋ロジック。
//!
//! 待機画面で運転免許証 (IC、NFC-B) がかざされたら、メニューを経由せずに
//! 「点呼を行いますか」の確認画面へ直行させる。かざしたのはドライバー本人で
//! 点呼が目的、という前提が立つので、毎回メニューから「点呼」を選ばせる 1 タップを
//! 省く。あわせて **かざした直後の一定時間はログ確認へ入れなくする**
//! (`LogLock`)。確認画面の下側 (キャンセル) や待機画面の下半分を続けてタップした
//! ときに、メニューの「ログ確認」に流れ込んで点呼が始まらない、という誤操作を
//! 防ぐため。
//!
//! 画面座標のヒット判定 (`menu_hit` / `confirm_hit`) もここに置く。描画側
//! (hub-ui/screens.rs) と同じ幾何で判定させ、描いたボタンと押せる領域のずれを
//! 一箇所で管理する。

/// 免許証をかざしてからログ確認を封じる時間 [ms]
pub const LOG_LOCK_MS: u64 = 15_000;

/// 点呼確認画面を放置したときに待機画面へ戻るまでの時間 [ms]。
/// かざしただけで立ち去ったケースの後始末。ログ確認ロックと同じ長さにして、
/// 「点呼を開始」の下に出す残り秒数がそのままロックの残りにもなるようにする
pub const CONFIRM_TIMEOUT_MS: u64 = LOG_LOCK_MS;

/// 点呼確認画面の残り秒数 (切り上げ、0 で自動クローズ)
pub fn confirm_remaining_secs(entered_ms: u64, now_ms: u64) -> u64 {
    CONFIRM_TIMEOUT_MS
        .saturating_sub(now_ms.saturating_sub(entered_ms))
        .div_ceil(1000)
}

/// 読み取れた運転免許証の情報 (PIN なしで読める EF 2F01 の共通データ要素)。
/// 日付は nfc_shim が返す "YYYYMMDD" のまま持つ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseCard {
    /// 交付日 "YYYYMMDD"
    pub issue: String,
    /// 有効期限 "YYYYMMDD"
    pub expiry: String,
}

impl LicenseCard {
    /// alc-app タブレットと同じ乗務員キー: 交付日 8 桁 + 有効期限 8 桁 = 16 桁
    /// (rust-alc-api `employees.nfc_id`、`GET /api/employees/by-nfc/{nfc_id}`)。
    /// どちらかが 8 桁数字でなければ None (キーにできない)
    pub fn nfc_id(&self) -> Option<String> {
        if is_yyyymmdd(&self.issue) && is_yyyymmdd(&self.expiry) {
            Some(format!("{}{}", self.issue, self.expiry))
        } else {
            None
        }
    }

    /// WS 送信 (kind = "license") の payload。点呼開始時に測定と同じ session_id で
    /// 送り、サーバ側で nfc_id → 乗務員に結合する (ippoan/alc-app-s3#125)
    pub fn payload_json(&self) -> String {
        let nfc = self
            .nfc_id()
            .map_or("null".to_string(), |v| format!("\"{v}\""));
        format!(
            "{{\"type\":\"license\",\"nfc_id\":{nfc},\"issue\":\"{}\",\"expiry\":\"{}\"}}",
            self.issue, self.expiry
        )
    }
}

/// 有効期限の判定結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryState {
    /// 時刻未同期 (NTP 前) または日付の形式が想定外で判定できない
    Unknown,
    /// 有効期限内 (期限日当日を含む)
    Valid,
    /// 期限切れ
    Expired,
}

/// "YYYYMMDD" → "YYYY/MM/DD"。8 桁数字でなければそのまま返す (壊れた値を
/// 隠さず画面に出す)
pub fn fmt_yyyymmdd(s: &str) -> String {
    if is_yyyymmdd(s) {
        format!("{}/{}/{}", &s[0..4], &s[4..6], &s[6..8])
    } else {
        s.to_string()
    }
}

fn is_yyyymmdd(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit())
}

/// 有効期限 (YYYYMMDD) を今日 (YYYYMMDD、`clock::jst_yyyymmdd`) と比べる。
/// 免許証は有効期限日当日まで有効なので、`expiry < today` のときだけ期限切れ
pub fn expiry_state(expiry: &str, today: Option<&str>) -> ExpiryState {
    match today {
        Some(t) if is_yyyymmdd(expiry) && is_yyyymmdd(t) => {
            // 桁が揃った数字列なので辞書順比較 = 日付順比較
            if expiry < t {
                ExpiryState::Expired
            } else {
                ExpiryState::Valid
            }
        }
        _ => ExpiryState::Unknown,
    }
}

/// 免許証タップ後のログ確認ロック。UI ループが 1 つ持ち、免許証コマンドの
/// 到着で `arm`、メニュー描画/ヒット判定で `remaining_ms` を見る
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogLock {
    until: Option<u64>,
}

impl LogLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 今から LOG_LOCK_MS の間ロックする (連続タップは延長)
    pub fn arm(&mut self, now_ms: u64) {
        self.until = Some(now_ms + LOG_LOCK_MS);
    }

    /// 残りロック時間 [ms]。0 = ロックなし
    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        self.until
            .map_or(0, |u| u.saturating_sub(now_ms))
    }

    pub fn is_locked(&self, now_ms: u64) -> bool {
        self.remaining_ms(now_ms) > 0
    }

    /// 表示用の残り秒 (切り上げ。0.2 秒残っていても "1" と出す)
    pub fn remaining_secs(&self, now_ms: u64) -> u64 {
        self.remaining_ms(now_ms).div_ceil(1000)
    }
}

/// メニュー画面 (上: 点呼 / 下: ログ確認) のタップ先
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    Tenko,
    Log,
}

/// メニューのヒット判定。`top` はメニュー領域の上端 (ステータスバー直下)、
/// `bottom` は画面下端。上下 2 等分で、ログ確認はロック中は押せない (None)
pub fn menu_hit(y: i32, top: i32, bottom: i32, log_locked: bool) -> Option<MenuChoice> {
    let mid = top + (bottom - top) / 2;
    if y < mid {
        Some(MenuChoice::Tenko)
    } else if log_locked {
        None
    } else {
        Some(MenuChoice::Log)
    }
}

/// 点呼確認画面 (免許証タップ後) のタップ先
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmChoice {
    /// 点呼を開始する (上のボタン)
    Start,
    /// やめて待機画面へ (下のボタン)
    Cancel,
}

/// 確認画面のヒット判定。`buttons_top` はボタン領域の上端 (その上はカード情報の
/// ヘッダで、押しても何も起きない)、`bottom` は画面下端。ボタンは上下 2 等分
pub fn confirm_hit(y: i32, buttons_top: i32, bottom: i32) -> Option<ConfirmChoice> {
    if y < buttons_top {
        return None;
    }
    let mid = buttons_top + (bottom - buttons_top) / 2;
    if y < mid {
        Some(ConfirmChoice::Start)
    } else {
        Some(ConfirmChoice::Cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_date_ok_and_passthrough() {
        assert_eq!(fmt_yyyymmdd("20301231"), "2030/12/31");
        assert_eq!(fmt_yyyymmdd(""), "");
        assert_eq!(fmt_yyyymmdd("2030-12"), "2030-12");
        assert_eq!(fmt_yyyymmdd("2030123X"), "2030123X");
    }

    #[test]
    fn license_nfc_id_and_payload() {
        let c = LicenseCard {
            issue: "20231117".into(),
            expiry: "20290107".into(),
        };
        assert_eq!(c.nfc_id().as_deref(), Some("2023111720290107"));
        assert_eq!(
            c.payload_json(),
            r#"{"type":"license","nfc_id":"2023111720290107","issue":"20231117","expiry":"20290107"}"#
        );
        let broken = LicenseCard {
            issue: "".into(),
            expiry: "20290107".into(),
        };
        assert_eq!(broken.nfc_id(), None);
        assert_eq!(
            broken.payload_json(),
            r#"{"type":"license","nfc_id":null,"issue":"","expiry":"20290107"}"#
        );
    }

    #[test]
    fn expiry_valid_including_same_day() {
        assert_eq!(expiry_state("20301231", Some("20260902")), ExpiryState::Valid);
        assert_eq!(expiry_state("20260902", Some("20260902")), ExpiryState::Valid);
    }

    #[test]
    fn expiry_expired() {
        assert_eq!(expiry_state("20260901", Some("20260902")), ExpiryState::Expired);
    }

    #[test]
    fn expiry_unknown_when_unsynced_or_malformed() {
        assert_eq!(expiry_state("20301231", None), ExpiryState::Unknown);
        assert_eq!(expiry_state("", Some("20260902")), ExpiryState::Unknown);
        assert_eq!(expiry_state("20301231", Some("bad")), ExpiryState::Unknown);
    }

    #[test]
    fn confirm_remaining() {
        assert_eq!(confirm_remaining_secs(1_000, 1_000), 15);
        assert_eq!(confirm_remaining_secs(1_000, 1_000 + CONFIRM_TIMEOUT_MS - 200), 1);
        assert_eq!(confirm_remaining_secs(1_000, 1_000 + CONFIRM_TIMEOUT_MS), 0);
        assert_eq!(confirm_remaining_secs(1_000, 1_000 + CONFIRM_TIMEOUT_MS + 5_000), 0);
    }

    #[test]
    fn log_lock_lifecycle() {
        let mut l = LogLock::new();
        assert!(!l.is_locked(0));
        assert_eq!(l.remaining_ms(0), 0);
        assert_eq!(l.remaining_secs(0), 0);

        l.arm(1_000);
        assert!(l.is_locked(1_000));
        assert_eq!(l.remaining_ms(1_000), LOG_LOCK_MS);
        assert_eq!(l.remaining_secs(1_000), 15);
        // 0.2 秒残り → 切り上げで 1 秒表示
        assert_eq!(l.remaining_secs(1_000 + LOG_LOCK_MS - 200), 1);
        assert!(!l.is_locked(1_000 + LOG_LOCK_MS));
        assert_eq!(l.remaining_secs(1_000 + LOG_LOCK_MS), 0);
    }

    #[test]
    fn log_lock_rearm_extends() {
        let mut l = LogLock::new();
        l.arm(0);
        l.arm(10_000);
        assert_eq!(l.remaining_ms(10_000), LOG_LOCK_MS);
    }

    #[test]
    fn menu_hit_halves() {
        // 320x240 横向き: バー 18px、メニュー領域 18..240、境界 129
        assert_eq!(menu_hit(50, 18, 240, false), Some(MenuChoice::Tenko));
        assert_eq!(menu_hit(128, 18, 240, false), Some(MenuChoice::Tenko));
        assert_eq!(menu_hit(129, 18, 240, false), Some(MenuChoice::Log));
        assert_eq!(menu_hit(239, 18, 240, false), Some(MenuChoice::Log));
    }

    #[test]
    fn menu_hit_log_locked() {
        assert_eq!(menu_hit(200, 18, 240, true), None);
        // 点呼はロック中でも押せる
        assert_eq!(menu_hit(50, 18, 240, true), Some(MenuChoice::Tenko));
    }

    #[test]
    fn confirm_hit_header_and_buttons() {
        // ヘッダ 18..70、ボタン 70..240 (境界 155)
        assert_eq!(confirm_hit(30, 70, 240), None);
        assert_eq!(confirm_hit(70, 70, 240), Some(ConfirmChoice::Start));
        assert_eq!(confirm_hit(154, 70, 240), Some(ConfirmChoice::Start));
        assert_eq!(confirm_hit(155, 70, 240), Some(ConfirmChoice::Cancel));
        assert_eq!(confirm_hit(239, 70, 240), Some(ConfirmChoice::Cancel));
    }
}
