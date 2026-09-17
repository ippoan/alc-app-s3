//! NFC タイムカード端末の打刻イベント (`kind = "timecard"`) の payload 組み立て
//! (issue #134、設計は plan/standing-devices.md §3.2 / §3.3)。
//!
//! # `card_id` に接頭辞を付けてはいけない
//!
//! サーバ側の打刻 (rust-alc-api の `POST /api/timecard/punch`) は
//! `SELECT ... WHERE tenant_id = $1 AND card_id = $2` の**完全一致**で
//! `timecard_cards` を引き、外れたら `employees.nfc_id` (免許証の 16 桁) へ
//! フォールバックする。`felica:` のような名前空間を足すと**この 2 経路と
//! alc-app の登録 UI が必ず同時に外れる**。
//!
//! したがって **`card_id` は端末が読んだ生値**を送り、種別は別フィールド
//! `card_kind` で運ぶ。`card_kind` は当面**記録と診断のためだけ**に使い、
//! 照合には使わない (使い始めた瞬間、`card_kind` を送らないブラウザ版
//! (alc-app の TimePunchKiosk.vue) と挙動が割れる)。

/// 打刻に使えたカードの種別。`payload.card_kind` に載る
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    /// 交通系 IC 等の FeliCa IDm。8 バイトを `%02X` で並べた**大文字** 16 文字
    FelicaIdm,
    /// NFC-A の UID (HCE / NTAG 等)。同じく大文字 hex
    NfcaUid,
    /// 従来 IC 運転免許証。`card_id` は交付日 8 桁 + 有効期限 8 桁 = 16 桁で、
    /// alc-app タブレットが使う `employees.nfc_id` と同じキー
    /// (`tenko_prompt::LicenseCard::nfc_id`)
    License,
}

impl CardKind {
    pub fn label(self) -> &'static str {
        match self {
            CardKind::FelicaIdm => "felica_idm",
            CardKind::NfcaUid => "nfca_uid",
            CardKind::License => "license",
        }
    }
}

/// `kind = "timecard"` の payload。`session_id` は付けない (点呼のセッションでは
/// ないため — uplink.rs は `None` なら key ごと省く)。
///
/// `card_id` は FFI 越しの文字列なので、JSON を壊さないよう最低限の
/// エスケープを掛ける (通常は hex か数字しか来ない)。
pub fn payload_json(card_id: &str, kind: CardKind) -> String {
    format!(
        "{{\"card_id\":\"{}\",\"card_kind\":\"{}\"}}",
        escape(card_id),
        kind.label(),
    )
}

/// ホスト (USB シリアルで繋がった PC) へ打刻を知らせる 1 行。
///
/// **CoreS3 と VoiceS3R で同一の行**にする (同じ出来事に 2 つの行種を作らない)。
/// 受け側は alc-app の `useCoreS3Serial` で、`EVT <NAME> <k=v>…` の汎用形を
/// 名前で振り分ける。
///
/// # なぜ WS ではなくシリアルに出すのか (Refs ippoan/rust-alc-api#644)
///
/// IC カード (FeliCa IDm / NFC-A UID) には点呼動線の `EVT NFC_LICENSE` /
/// `EVT NFC_CARINS` に相当する行が無く、PC は「保存 → 合図 → 一覧の引き直し」で
/// **クラウドを一周**しないと打刻を知れなかった。WS が切れていると数秒〜十数秒
/// 待たされ、再起動直後は WS が繋がるまで画面が動かない。**端末は PC と USB で
/// 繋がっている**ので、目の前のケーブルに出せば即座に届く。
///
/// # `println!` で出すこと — `evtlog::emit` にしない
///
/// `card_id` は人を特定できる値で、`emit` の行は `.noinit` リングにも残る。
/// リングは `LOG DUMP` / WS 下り `get_log` で**遠隔から読み出せる**ため、
/// そこから issue / PR の本文へ転記されうる経路を作らない
/// (`alc_hub_common::evtlog` の「どの行を emit にするか」。この行は同 doc の
/// 除外例に `EVT TIMECARD` として最初から挙がっている)。
pub fn evt_line(card_id: &str, kind: CardKind) -> String {
    format!("EVT TIMECARD card_id={card_id} card_kind={}", kind.label())
}

/// JSON 文字列リテラルとして安全な形に直す。`"` と `\` は前置エスケープし、
/// 制御文字は捨てる (hex/数字しか来ない前提の保険で、値を作り変えない)
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_the_wire_values() {
        assert_eq!(CardKind::FelicaIdm.label(), "felica_idm");
        assert_eq!(CardKind::NfcaUid.label(), "nfca_uid");
        assert_eq!(CardKind::License.label(), "license");
    }

    /// 交通系 IC の実機実測値 (2026-09-04、AtomS3 Lite + Unit NFC)。
    /// **接頭辞が付いていないこと**がこのテストの主眼
    #[test]
    fn felica_payload_carries_the_raw_idm() {
        assert_eq!(
            payload_json("01401D0B1D37B660", CardKind::FelicaIdm),
            r#"{"card_id":"01401D0B1D37B660","card_kind":"felica_idm"}"#
        );
    }

    /// 免許証は交付日 + 有効期限の 16 桁 (employees.nfc_id と同じキー)
    #[test]
    fn license_payload_uses_the_16_digit_key() {
        assert_eq!(
            payload_json("2023060920280513", CardKind::License),
            r#"{"card_id":"2023060920280513","card_kind":"license"}"#
        );
    }

    #[test]
    fn nfca_payload_uses_its_own_kind() {
        assert_eq!(
            payload_json("04AABBCC", CardKind::NfcaUid),
            r#"{"card_id":"04AABBCC","card_kind":"nfca_uid"}"#
        );
    }

    /// シリアル行の形は alc-app (`useCoreS3Serial`) との取り決めなので、
    /// **キーの綴りと順序ごと**固定する。IC カードの 2 種が出ることが主眼
    /// (Refs ippoan/rust-alc-api#644)
    #[test]
    fn evt_line_is_fixed_for_both_ic_card_kinds() {
        assert_eq!(
            evt_line("01401D0B1D37B660", CardKind::FelicaIdm),
            "EVT TIMECARD card_id=01401D0B1D37B660 card_kind=felica_idm"
        );
        assert_eq!(
            evt_line("04AABBCC", CardKind::NfcaUid),
            "EVT TIMECARD card_id=04AABBCC card_kind=nfca_uid"
        );
    }

    /// 免許証の行も**組み立てられる** (VoiceS3R は出す)。CoreS3 が出さないのは
    /// 呼び出し側の判断で、ここは種別を問わず同じ形を返す
    #[test]
    fn evt_line_covers_the_license_kind_too() {
        assert_eq!(
            evt_line("2023060920280513", CardKind::License),
            "EVT TIMECARD card_id=2023060920280513 card_kind=license"
        );
    }

    /// FFI 越しの文字列が壊れていても JSON を壊さない (uplink の payload_object
    /// がパースに失敗すると送信キューへ積めない)
    #[test]
    fn escapes_quotes_and_backslashes_and_drops_control_chars() {
        let dirty = format!("a\"b\\c{}", char::from(0x01));
        assert_eq!(
            payload_json(&dirty, CardKind::NfcaUid),
            r#"{"card_id":"a\"b\\c","card_kind":"nfca_uid"}"#
        );
    }
}
