//! Type-A (NFC-A) の UID の判定 (issue #155)。
//!
//! スマホ (モバイル Suica 等) は近づけたとき FeliCa、離すとき HCE (Type-A) で応答し、
//! タイムカード端末では別の key として **1 タップで 2 回発火**する (実機 2026-09-06、1.6 秒差)。
//! HCE の UID は **ISO/IEC 14443-3 §6.4.4 のランダム UID** (single size = 4 バイト、
//! UID0 = 0x08) で毎回変わるので、打刻 ID としても無意味。一方 NTAG / MIFARE 等の
//! 実タグは 7 バイト (先頭は製造者コード、NXP = 0x04) で、社員証としての運用がある。
//! ⇒ **4 バイトかつ UID0 = 0x08 だけ**を打刻から外す。

/// `uid` (16 進文字列、大小文字不問) が ISO/IEC 14443-3 のランダム UID (4 バイト、UID0 = 0x08) か
pub fn is_random_nfca_uid(uid: &str) -> bool {
    uid.len() == 8 && uid.is_char_boundary(2) && uid[..2].eq_ignore_ascii_case("08")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_byte_uid0_08_is_random() {
        assert!(is_random_nfca_uid("08A1B2C3"));
        assert!(is_random_nfca_uid("08a1b2c3"));
    }

    #[test]
    fn four_byte_other_uid0_is_not() {
        assert!(!is_random_nfca_uid("04A1B2C3"));
    }

    #[test]
    fn seven_byte_is_not_even_if_08() {
        assert!(!is_random_nfca_uid("04A1B2C3D4E5F6"));
        assert!(!is_random_nfca_uid("08A1B2C3D4E5F6"));
    }

    #[test]
    fn malformed_is_not() {
        assert!(!is_random_nfca_uid(""));
        assert!(!is_random_nfca_uid("08A1B2C"));
        assert!(!is_random_nfca_uid("あ8A1B2C3"));
    }
}
