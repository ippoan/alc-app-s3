//! VoiceS3R (警告デバイス) の管理者認証用 ed25519 鍵対 (Refs #205)。
//!
//! 機体が鍵対を作り、秘密鍵 (32 B の seed) は呼び出し側 (hub-drivers) が
//! NVS `alarm_sk` に保存する。ここでは NVS にも USB にも触れず、鍵導出・
//! 署名・nonce の形式検査・応答行の組み立てだけを行う純粋関数の集まり
//! (ホストでテスト可能、副作用なし)。
//!
//! 秘密鍵を返す関数はここにも他のどこにも作らない — `AUTH PUBKEY` /
//! `AUTH SIG` の応答は常に公開鍵と署名だけを含む。

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

/// nonce (小文字 hex 32 文字の ASCII そのもの、hex デコードしない) に署名し、
/// 署名 (64 B) を base64url (padding 無し) で返す。
pub fn sign_nonce(seed: &[u8; 32], nonce: &str) -> Result<String, BadNonce> {
    check_nonce(nonce)?;
    let signing_key = SigningKey::from_bytes(seed);
    let sig = signing_key.sign(nonce.as_bytes());
    Ok(b64url_encode(&sig.to_bytes()))
}

/// `AUTH PUBKEY <base64url>` 応答行。
#[must_use]
pub fn auth_pubkey_line(seed: &[u8; 32]) -> String {
    format!("AUTH PUBKEY {}", pubkey_b64(seed))
}

/// `AUTH SIG <pubkey base64url> <sig base64url>` 応答行、または nonce 不正。
pub fn auth_sig_line(seed: &[u8; 32], nonce: &str) -> Result<String, BadNonce> {
    let sig = sign_nonce(seed, nonce)?;
    Ok(format!("AUTH SIG {} {}", pubkey_b64(seed), sig))
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

    #[test]
    fn sign_nonce_roundtrips_through_verify() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let nonce = "0123456789abcdef0123456789abcdef";
        let sig_b64 = sign_nonce(&seed, nonce).expect("valid nonce");
        let sig_bytes = b64url_decode_for_test(&sig_b64);
        let sig = ed25519_dalek::Signature::from_slice(&sig_bytes).unwrap();
        let vk = VerifyingKey::from_bytes(&hex_to_bytes32(RFC8032_PUBKEY)).unwrap();
        assert!(vk.verify(nonce.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn auth_pubkey_line_format() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let line = auth_pubkey_line(&seed);
        assert!(line.starts_with("AUTH PUBKEY "));
        assert_eq!(line.split(' ').count(), 3);
    }

    #[test]
    fn auth_sig_line_format() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        let line = auth_sig_line(&seed, "0123456789abcdef0123456789abcdef").unwrap();
        let parts: Vec<&str> = line.split(' ').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "AUTH");
        assert_eq!(parts[1], "SIG");
    }

    #[test]
    fn auth_sig_line_rejects_bad_nonce() {
        let seed = hex_to_bytes32(RFC8032_SEED);
        assert_eq!(auth_sig_line(&seed, "short"), Err(BadNonce));
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
