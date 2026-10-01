//! 運行管理者席の警告デバイスが、NFC で読んだ IC カードをホスト (USB シリアルで
//! 繋がったブラウザ) へ渡す 1 行 (`EVT NFC_LOGIN`、Refs ippoan/alc-app#387)。
//!
//! **打刻ではない。** 席の PC のブラウザが、かざした人を「この席の運行管理者」に
//! 登録するための合図で、端末はサーバへ何も送らない (警告デバイスは uplink を
//! 持たない)。打刻の行 [`crate::timecard::evt_line`] (`EVT TIMECARD`) と**同じ行名に
//! しないこと** — alc-app は `EVT TIMECARD` を打刻として拾う。
//!
//! # 免許証はここで行にしない
//!
//! 従来 IC 運転免許証は、読み取りの正本 (`alc_hub_drivers::nfc` の `deliver`) が
//! `EVT NFC_LICENSE issue=… expiry=…` を既に出している。ここでも出すと同じ 1 タップが
//! 2 行になるので、[`CardKind::License`] は `None` を返す。
//!
//! # `println!` で出すこと — `evtlog::emit` にしない
//!
//! `card_id` は人を特定できる値。理由は [`crate::timecard::evt_line`] の doc と同じ
//! (リングは `LOG DUMP` で読み出せる)。

use crate::timecard::CardKind;

/// IC カード (FeliCa IDm / NFC-A UID) を読めたときにホストへ出す 1 行。
/// 行にしない種別 (免許証) は `None`。
///
/// **`card_id` は端末が読んだ生値のまま** (接頭辞を付けない — 理由は
/// [`crate::timecard`] のモジュール doc)。種別の綴りは [`CardKind::label`] が正本
pub fn evt_line(card_id: &str, kind: CardKind) -> Option<String> {
    match kind {
        CardKind::FelicaIdm | CardKind::NfcaUid => Some(format!(
            "EVT NFC_LOGIN card_id={card_id} card_kind={}",
            kind.label()
        )),
        CardKind::License => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn felica_line_carries_the_raw_idm() {
        assert_eq!(
            evt_line("0123456789ABCDEF", CardKind::FelicaIdm).as_deref(),
            Some("EVT NFC_LOGIN card_id=0123456789ABCDEF card_kind=felica_idm")
        );
    }

    #[test]
    fn nfca_line_uses_its_own_kind() {
        assert_eq!(
            evt_line("04AABBCC", CardKind::NfcaUid).as_deref(),
            Some("EVT NFC_LOGIN card_id=04AABBCC card_kind=nfca_uid")
        );
    }

    /// 免許証は正本が `EVT NFC_LICENSE` を出すので、ここでは行にしない
    #[test]
    fn license_is_not_a_login_line() {
        assert_eq!(evt_line("2000010120300101", CardKind::License), None);
    }

    /// 打刻の行と取り違えられない (alc-app は `EVT TIMECARD` を打刻として拾う)
    #[test]
    fn line_name_differs_from_the_punch_line() {
        let login = evt_line("04AABBCC", CardKind::NfcaUid).unwrap();
        let punch = crate::timecard::evt_line("04AABBCC", CardKind::NfcaUid);
        assert!(login.starts_with("EVT NFC_LOGIN "));
        assert!(!punch.starts_with("EVT NFC_LOGIN"));
    }
}
