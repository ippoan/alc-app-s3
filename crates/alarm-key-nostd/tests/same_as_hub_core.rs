//! The no_std lines against the 正本 (alc_hub_core::alarm_key): the same seed and nonce must give the same
//! `AUTH PUBKEY` / `AUTH SIG` line, character for character (auth-worker and the browser read both).

use alc_hub_core::alarm_key;

fn seeds() -> Vec<[u8; 32]> {
    let mut v = vec![[0u8; 32], [0xFFu8; 32]];
    let mut s = 0x9E37_79B9u32;
    for _ in 0..30 {
        let mut seed = [0u8; 32];
        for b in &mut seed {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *b = s as u8;
        }
        v.push(seed);
    }
    v
}

const NONCES: &[&str] = &[
    "00000000000000000000000000000000",
    "0123456789abcdef0123456789abcdef",
    "ffffffffffffffffffffffffffffffff",
    "a1b2c3d4e5f60718293a4b5c6d7e8f90",
];

/// malformed: alarm_key refuses them, so must the no_std side
const BAD: &[&str] = &[
    "",
    "0123456789abcdef0123456789abcde",
    "0123456789abcdef0123456789abcdef0",
    "0123456789ABCDEF0123456789abcdef",
    "0123456789abcdeg0123456789abcdef",
    "0123456789abcdef 123456789abcdef",
];

#[test]
fn pubkey_line_is_the_same() {
    for seed in seeds() {
        assert_eq!(alarm_key_nostd::pubkey_line(&seed).as_str(), alarm_key::auth_pubkey_line(&seed));
    }
}

#[test]
fn sig_line_is_the_same() {
    for seed in seeds() {
        for nonce in NONCES {
            let want = alarm_key::auth_sig_line(&seed, nonce).unwrap();
            assert_eq!(alarm_key_nostd::sig_line(&seed, nonce).unwrap().as_str(), want);
        }
    }
}

#[test]
fn sig_line_is_the_same_for_random_nonces() {
    let mut s = 0x0BAD_5EEDu32;
    for seed in seeds().iter().take(4) {
        for _ in 0..64 {
            let nonce: String = (0..32)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    char::from(b"0123456789abcdef"[(s & 15) as usize])
                })
                .collect();
            let want = alarm_key::auth_sig_line(seed, &nonce).unwrap();
            assert_eq!(alarm_key_nostd::sig_line(seed, &nonce).unwrap().as_str(), want);
        }
    }
}

#[test]
fn bad_nonce_is_refused_by_both() {
    let seed = [3u8; 32];
    for nonce in BAD {
        assert!(alarm_key::auth_sig_line(&seed, nonce).is_err(), "{nonce:?}");
        assert!(alarm_key_nostd::sig_line(&seed, nonce).is_none(), "{nonce:?}");
        assert!(!alarm_key_nostd::nonce_ok(nonce));
    }
    assert_eq!(alarm_key_nostd::ERR_BAD_NONCE, alarm_key::ERR_BAD_NONCE);
}
