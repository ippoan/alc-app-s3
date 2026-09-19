//! VoiceS3R (警告デバイス) の管理者認証用 ed25519 鍵対 (Refs #205)。
//!
//! 機体が鍵対を作り、秘密鍵 (32 B の seed) は呼び出し側 (hub-drivers) が
//! NVS `alarm_sk` に保存する。ここでは NVS にも USB にも触れず、鍵導出・
//! 署名・nonce の形式検査・応答行の組み立てだけを行う純粋関数の集まり
//! (ホストでテスト可能、副作用なし)。
//!
//! 秘密鍵を返す関数はここにも他のどこにも作らない — `AUTH PUBKEY` /
//! `AUTH SIG` / `AUTH SIGBP` の応答は公開鍵・署名・血圧計のボンド状態だけを含む。
//!
//! 署名の口は**用途ごとに 2 本**ある (Refs #249)。混ぜないこと:
//!
//! | コマンド | 応答 | 署名対象 | 使うのは |
//! |---|---|---|---|
//! | `AUTH SIGN <nonce>` | `AUTH SIG <pubkey> <sig>` | `<nonce>` | 管理者ログイン (`/auth/device-login`) と旧ホスト |
//! | `AUTH SIGNBP <nonce>` | `AUTH SIGBP <pubkey> <sig> BP=<1\|0>` | `<nonce>\|bp=<1\|0>` | キオスク端末の認証 (`/device/alarm-token`) |
//!
//! ★ **`AUTH SIGN` の署名対象にボンド状態を混ぜないこと。** ホスト側の
//! `signAlarmDeviceNonce` は 1 本の関数を管理者ログインとキオスクで共有しており、
//! ファームは行からどちらの用途か判別できない。混ぜると**管理者ログインが 401 になる**。

use crate::device::BpReport;
use ed25519_dalek::{Signer, SigningKey};

/// `AUTH SIGN <nonce>` の nonce が形式不正 (長さ != 32 / 大文字を含む /
/// 非 hex 文字を含む) だったときのエラー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadNonce;

/// nonce が「小文字 hex 32 文字の ASCII」であることを検査する。
pub fn check_nonce(nonce: &str) -> Result<(), BadNonce> {
    if nonce.len() == 32
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(BadNonce)
    }
}

/// 32 B の seed から公開鍵を導出し、base64url (padding 無し) にエンコードする。
#[must_use]
pub fn pubkey_b64(seed: &[u8; 32]) -> String {
    let signing_key = SigningKey::from_bytes(seed);
    b64url_encode(signing_key.verifying_key().as_bytes())
}

/// `AUTH SIGNBP` の署名対象の文字列 (ASCII)。nonce に血圧計のボンド状態を
/// 束縛する (Refs #249)。
///
/// ```text
/// <nonce>|bp=1   血圧計がボンドされている
/// <nonce>|bp=0   ボンドされていない
/// ```
///
/// ★ **この形は `ippoan/auth-worker` の検証側と 1 文字も違わずに揃っている。**
/// 区切りは半角パイプ 1 文字、`bp=` の後は `1` か `0` のみ、空白を入れない。
/// **片側だけ変えないこと** — 変えると署名検証が全滅する。
///
/// **`AUTH SIGN` (管理者ログイン) はこれを使わない** — あちらは `<nonce>` その
/// ものに署名する ([`sign_nonce`])。古いファームは `AUTH SIGNBP` を知らず
/// エラーを返すので、ホスト側が `AUTH SIGN` へフォールバックする。こちら側に
/// バージョン交渉は要らない。
#[must_use]
pub fn sign_payload(nonce: &str, bp_bonded: bool) -> String {
    format!("{nonce}|bp={}", u8::from(bp_bonded))
}

/// nonce (小文字 hex 32 文字の ASCII そのもの、hex デコードしない) に署名し、
/// 署名 (64 B) を base64url (padding 無し) で返す。
///
/// ★ **署名対象は nonce だけ。ここに何かを足さないこと** — 管理者ログインが
/// この署名を使っており、足すと auth-worker の検証と合わず 401 になる (Refs #249)。
pub fn sign_nonce(seed: &[u8; 32], nonce: &str) -> Result<String, BadNonce> {
    check_nonce(nonce)?;
    let signing_key = SigningKey::from_bytes(seed);
    let sig = signing_key.sign(nonce.as_bytes());
    Ok(b64url_encode(&sig.to_bytes()))
}

/// [`sign_payload`] の組み立て結果 (nonce + ボンド状態) に署名し、
/// 署名 (64 B) を base64url (padding 無し) で返す (`AUTH SIGNBP` 用)。
pub fn sign_nonce_bp(seed: &[u8; 32], nonce: &str, bp_bonded: bool) -> Result<String, BadNonce> {
    check_nonce(nonce)?;
    let signing_key = SigningKey::from_bytes(seed);
    let sig = signing_key.sign(sign_payload(nonce, bp_bonded).as_bytes());
    Ok(b64url_encode(&sig.to_bytes()))
}

/// `AUTH PUBKEY <base64url>` 応答行。
#[must_use]
pub fn auth_pubkey_line(seed: &[u8; 32]) -> String {
    format!("AUTH PUBKEY {}", pubkey_b64(seed))
}

/// `AUTH SIG <pubkey base64url> <sig base64url>` 応答行、または nonce 不正。
///
/// ★ **形を変えないこと** — 管理者ログインのホスト側パーサが語数で読む。
/// ボンド状態を返すのは別の口 ([`auth_sigbp_line`])。
pub fn auth_sig_line(seed: &[u8; 32], nonce: &str) -> Result<String, BadNonce> {
    let sig = sign_nonce(seed, nonce)?;
    Ok(format!("AUTH SIG {} {}", pubkey_b64(seed), sig))
}

/// `AUTH SIGBP <pubkey base64url> <sig base64url> BP=<1|0>` 応答行、
/// または nonce 不正 (`AUTH SIGNBP` の応答)。
///
/// prefix を `AUTH SIG` と分けてあるので、管理者ログインの既存パーサは影響を
/// 受けない。`BP=` はホスト (ブラウザ) が署名と一緒に上流へ渡すための**ボンド
/// 状態の値そのもの**で、auth-worker はこの値で [`sign_payload`] を組み立て直して
/// 検証する。**署名した値と必ず同じものを出すこと** (Refs #249)。
///
/// 応答行は大文字の `BP=`、署名対象の中は小文字の `bp=` — 混同しないこと。
pub fn auth_sigbp_line(seed: &[u8; 32], nonce: &str, bp_bonded: bool) -> Result<String, BadNonce> {
    let sig = sign_nonce_bp(seed, nonce, bp_bonded)?;
    Ok(format!(
        "AUTH SIGBP {} {} BP={}",
        pubkey_b64(seed),
        sig,
        u8::from(bp_bonded)
    ))
}

/// nonce の形式が不正だったときの応答行。
pub const ERR_BAD_NONCE: &str = "ERR AUTH: bad nonce";

/// ボンド状態をまだ確認できておらず、`AUTH SIGNBP` に答えられないときの
/// 応答行 (Refs #269)。**`AUTH SIGN` (管理者ログイン) では決して返さない。**
///
/// ホスト (`ippoan/alc-app` の `useDeviceToken`) は `ERR AUTH` を受けると
/// `AUTH SIGN` へフォールバックし、`bp_bonded` を**付けずに**上流へ行く —
/// つまり「未確認」が `bp=0` の顔をして届くことはない。
pub const ERR_BP_NOT_READY: &str = "ERR AUTH: bp not ready";

/// `AUTH SIGNBP` の応答行を決める (読了ゲート込み、Refs #269)。
///
/// [`BpReport::NotReady`] のあいだは**署名しない** — 未確認を `bp=0` として
/// 署名すると、血圧計が繋がっている端末が「無い」と鍵付きで申告してしまう
/// ([`crate::device::bp_report`] の doc 参照)。
///
/// ★ **`AUTH SIGN` (管理者ログイン) はこのゲートを通さない。** 署名対象に
/// ボンド状態を持たない別の口で ([`auth_sig_line`])、血圧計の準備を待たせると
/// 鍵が在るのに管理者がログインできない端末ができる。
#[must_use]
pub fn auth_sigbp_response(seed: &[u8; 32], nonce: &str, report: BpReport) -> String {
    match report {
        BpReport::NotReady => ERR_BP_NOT_READY.to_string(),
        BpReport::Ready { bonded } => {
            auth_sigbp_line(seed, nonce, bonded).unwrap_or_else(|_| ERR_BAD_NONCE.to_string())
        }
    }
}

/// RFC 4648 base64url (padding 無し)。標準 base64 alphabet の `62`/`63` 番目
/// (`+` `/`) を `-` `_` に差し替え、末尾パディングを出さないだけの実装。
/// 外部 crate は足さず、必要なのは encode のみ (decode は不要)。
fn b64url_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Verifier, VerifyingKey};

    fn hex_to_bytes32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    // RFC 8032 §7.1 test vector 1 (seed / pubkey / 空メッセージの署名)。
    // cryptography (Python) の Ed25519PrivateKey.from_private_bytes / .sign(b"")
    // で実際に導出し直して埋め込んでいる (転記ミス防止)。
    const RFC8032_SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const RFC8032_PUBKEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const RFC8032_SIG_OF_EMPTY: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    #[test]
    fn rfc8032_vector1_pubkey_matches() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let signing_key = SigningKey::from_bytes(&seed);
        assert_eq!(
            signing_key.verifying_key().as_bytes(),
            &hex_to_bytes32(RFC8032_PUBKEY)
        );
    }

    #[test]
    fn rfc8032_vector1_signs_empty_message() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let signing_key = SigningKey::from_bytes(&seed);
        let sig = signing_key.sign(b"");
        let mut expected = [0u8; 64];
        for (i, b) in expected.iter_mut().enumerate() {
            *b = u8::from_str_radix(&RFC8032_SIG_OF_EMPTY[i * 2..i * 2 + 2], 16).unwrap();
        }
        assert_eq!(sig.to_bytes(), expected);
    }

    #[test]
    fn pubkey_b64_matches_rfc8032_vector1() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        // d75a9801... を base64url (padding無し) にすると次の文字列になる
        // (python: base64.urlsafe_b64encode(bytes).decode().rstrip("="))
        let pk = pubkey_b64(&seed);
        assert_eq!(pk.len(), 43); // 32 B -> 43 文字 (padding 無し)
                                  // 先頭バイト 0xd7 = base64url の最初の文字 "1w"... 実値は decode roundtrip で検証
        let decoded = b64url_decode_for_test(&pk);
        assert_eq!(decoded, hex_to_bytes32(RFC8032_PUBKEY));
    }

    // encode の正しさをテストで確かめるための decode (本体には不要、テスト専用)
    fn b64url_decode_for_test(s: &str) -> Vec<u8> {
        fn val(c: u8) -> u32 {
            match c {
                b'A'..=b'Z' => u32::from(c - b'A'),
                b'a'..=b'z' => u32::from(c - b'a') + 26,
                b'0'..=b'9' => u32::from(c - b'0') + 52,
                b'-' => 62,
                b'_' => 63,
                _ => panic!("bad base64url char"),
            }
        }
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let n = bytes.len() - i;
            let c0 = val(bytes[i]);
            let c1 = val(bytes[i + 1]);
            out.push(((c0 << 2) | (c1 >> 4)) as u8);
            if n > 2 {
                let c2 = val(bytes[i + 2]);
                out.push((((c1 & 0xf) << 4) | (c2 >> 2)) as u8);
            }
            if n > 3 {
                let c2 = val(bytes[i + 2]);
                let c3 = val(bytes[i + 3]);
                out.push((((c2 & 0x3) << 6) | c3) as u8);
            }
            i += 4;
        }
        out
    }

    /// 署名対象の形。**auth-worker (ippoan/auth-worker#571) と揃える正本**なので、
    /// 文字列リテラルを直に書いて比べる (組み立て式を写すと両方同時に壊れる)
    #[test]
    fn sign_payload_binds_bond_state() {
        let nonce = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            sign_payload(nonce, true),
            "0123456789abcdef0123456789abcdef|bp=1"
        );
        assert_eq!(
            sign_payload(nonce, false),
            "0123456789abcdef0123456789abcdef|bp=0"
        );
    }

    /// ★ **管理者ログインの退行検知。** `AUTH SIGN` の署名対象は `<nonce>` その
    /// もので、ボンド状態を混ぜてはいけない (混ぜると `/auth/device-login` が 401)
    #[test]
    fn sign_nonce_roundtrips_through_verify() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        let sig_b64 = sign_nonce(&seed, nonce).expect("valid nonce");
        let sig_bytes = b64url_decode_for_test(&sig_b64);
        let sig = ed25519_dalek::Signature::from_slice(&sig_bytes).unwrap();
        let vk = VerifyingKey::from_bytes(&hex_to_bytes32(RFC8032_PUBKEY)).unwrap();
        assert!(vk.verify(nonce.as_bytes(), &sig).is_ok());
        // ボンド状態を束縛した形では**ない** (SIGNBP と取り違えていない)
        assert!(vk
            .verify(sign_payload(nonce, true).as_bytes(), &sig)
            .is_err());
        assert!(vk
            .verify(sign_payload(nonce, false).as_bytes(), &sig)
            .is_err());
    }

    #[test]
    fn sign_nonce_bp_roundtrips_through_verify() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        let vk = VerifyingKey::from_bytes(&hex_to_bytes32(RFC8032_PUBKEY)).unwrap();
        for bp in [true, false] {
            let sig_b64 = sign_nonce_bp(&seed, nonce, bp).expect("valid nonce");
            let sig_bytes = b64url_decode_for_test(&sig_b64);
            let sig = ed25519_dalek::Signature::from_slice(&sig_bytes).unwrap();
            assert!(vk.verify(sign_payload(nonce, bp).as_bytes(), &sig).is_ok());
            // nonce だけの署名 (= AUTH SIGN) にはならない
            assert!(vk.verify(nonce.as_bytes(), &sig).is_err());
        }
    }

    /// ボンド状態が違えば署名も違う — 署名が状態を実際に束縛している証拠
    #[test]
    fn sign_nonce_bp_differs_by_bond_state() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        assert_ne!(
            sign_nonce_bp(&seed, nonce, true).unwrap(),
            sign_nonce_bp(&seed, nonce, false).unwrap()
        );
        // AUTH SIGN の署名とも一致しない (口が分かれている)
        let plain = sign_nonce(&seed, nonce).unwrap();
        assert_ne!(sign_nonce_bp(&seed, nonce, true).unwrap(), plain);
        assert_ne!(sign_nonce_bp(&seed, nonce, false).unwrap(), plain);
    }

    #[test]
    fn sign_nonce_bp_rejects_bad_nonce() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        assert_eq!(sign_nonce_bp(&seed, "short", true), Err(BadNonce));
    }

    #[test]
    fn auth_pubkey_line_format() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let line = auth_pubkey_line(&seed);
        assert!(line.starts_with("AUTH PUBKEY "));
        assert_eq!(line.split(' ').count(), 3);
    }

    /// ★ **管理者ログインの退行検知。** 語数も prefix も変えないこと
    #[test]
    fn auth_sig_line_format() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let line = auth_sig_line(&seed, "0123456789abcdef0123456789abcdef").unwrap();
        let parts: Vec<&str> = line.split(' ').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "AUTH");
        assert_eq!(parts[1], "SIG");
        // ボンド状態は載らない
        assert!(!line.contains("BP="));
    }

    #[test]
    fn auth_sig_line_rejects_bad_nonce() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        assert_eq!(auth_sig_line(&seed, "short"), Err(BadNonce));
    }

    /// `AUTH SIGBP` は prefix が `AUTH SIG` と別 — ホスト側の既存パーサ
    /// (`AUTH SIG` を語数で読む) に当たらないことが要点
    #[test]
    fn auth_sigbp_line_format() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        for (bp, field) in [(true, "BP=1"), (false, "BP=0")] {
            let line = auth_sigbp_line(&seed, nonce, bp).unwrap();
            let parts: Vec<&str> = line.split(' ').collect();
            assert_eq!(parts.len(), 5);
            assert_eq!(parts[0], "AUTH");
            assert_eq!(parts[1], "SIGBP");
            assert_eq!(parts[2], pubkey_b64(&seed));
            assert_eq!(parts[3], sign_nonce_bp(&seed, nonce, bp).unwrap());
            // 応答行は大文字の BP=、署名対象の中は小文字の bp=
            assert_eq!(parts[4], field);
        }
    }

    #[test]
    fn auth_sigbp_line_rejects_bad_nonce() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        assert_eq!(auth_sigbp_line(&seed, "short", true), Err(BadNonce));
    }

    /// #269: スキャンが回るまでは署名しない。**`bp=0` の署名を出さない**のが
    /// 要点 — 出すと血圧計が在る端末が「無い」と鍵付きで申告してしまう
    #[test]
    fn auth_sigbp_response_does_not_sign_before_the_scan_ran() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        let line = auth_sigbp_response(&seed, nonce, BpReport::NotReady);
        assert_eq!(line, "ERR AUTH: bp not ready");
        assert!(!line.contains("BP="));
        assert_ne!(line, auth_sigbp_line(&seed, nonce, false).unwrap());
    }

    #[test]
    fn auth_sigbp_response_signs_the_observed_state_once_the_scan_ran() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        for bonded in [false, true] {
            assert_eq!(
                auth_sigbp_response(&seed, nonce, BpReport::Ready { bonded }),
                auth_sigbp_line(&seed, nonce, bonded).unwrap()
            );
        }
        // nonce 不正は読了ゲートより後 (ゲートを通っても形式は見る)
        assert_eq!(
            auth_sigbp_response(&seed, "short", BpReport::Ready { bonded: true }),
            "ERR AUTH: bad nonce"
        );
    }

    /// ★ **管理者ログインの退行検知 (#269)。** 読了ゲートが `AUTH SIGN` 側へ
    /// 滲むと、血圧計を積まない機・スキャン前の窓で管理者がログインできなくなる。
    /// `AUTH SIGN` の応答は seed と nonce だけで決まり、ゲートが返しうる応答
    /// (`ERR AUTH: bp not ready` / `AUTH SIGBP …`) のどれとも一致しない
    #[test]
    fn bp_ready_gate_never_changes_auth_sign() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        let sign = auth_sig_line(&seed, nonce).unwrap();
        for report in [
            BpReport::NotReady,
            BpReport::Ready { bonded: false },
            BpReport::Ready { bonded: true },
        ] {
            assert_ne!(auth_sigbp_response(&seed, nonce, report), sign);
        }
        assert!(sign.starts_with("AUTH SIG "));
        assert!(!sign.contains("BP="));
        assert_eq!(sign, auth_sig_line(&seed, nonce).unwrap());
    }

    #[test]
    fn check_nonce_rejects_wrong_length() {
        assert_eq!(check_nonce(&"a".repeat(31)), Err(BadNonce));
        assert_eq!(check_nonce(&"a".repeat(33)), Err(BadNonce));
    }

    #[test]
    fn check_nonce_rejects_uppercase() {
        assert_eq!(
            check_nonce("0123456789ABCDEF0123456789abcdef"),
            Err(BadNonce)
        );
    }

    #[test]
    fn check_nonce_rejects_non_hex() {
        assert_eq!(
            check_nonce("0123456789abcdef0123456789abcdeg"),
            Err(BadNonce)
        );
        assert_eq!(
            check_nonce("012345678-abcdef0123456789abcdef"),
            Err(BadNonce)
        );
    }

    #[test]
    #[should_panic(expected = "bad base64url char")]
    fn b64url_decode_for_test_rejects_invalid_char() {
        // テスト専用 decode ヘルパーの panic 分岐 (本体の encode は不正文字を
        // 生成しないので実運用では通らない経路)。coverage 100% のために
        // 分岐そのものを踏んでおく。
        b64url_decode_for_test("!!!!");
    }

    #[test]
    fn check_nonce_accepts_valid() {
        assert_eq!(check_nonce("0123456789abcdef0123456789abcdef"), Ok(()));
    }
}
