//! A seed from a noise source that is not a TRNG (the CH32V203 has none): the raw samples' bits are checked
//! with SP 800-90B §4.4's two health tests and conditioned with SHA-256 (§3.1.5.1.1, a vetted function).
//!
//! The source (the kind's: the ADC's least significant bit, ...) is one bit per sample, assumed to carry at least
//! [`H_PER_SAMPLE`] bits of min-entropy. [`SAMPLES`] samples are taken, so the 256-bit output is fed
//! `SAMPLES * H_PER_SAMPLE` = 512 bits, twice its size (§3.1.5.1.2's `n_in >= n_out + 64` with room).
//!
//! Fail closed: a failed test gives [`Unhealthy`] and no seed (the kind answers `ERR AUTH: entropy` and makes no
//! key). The raw samples never leave this function — there is no way to read them out (a debug word included).

use sha2::{Digest, Sha256};

/// The assumed min-entropy per 1-bit sample (conservative: the ADC's LSB is close to a fair coin on the bench,
/// but nothing here measures it on each board).
pub const H_PER_SAMPLE: f64 = 0.25;
/// Samples per seed: SAMPLES * H_PER_SAMPLE = 512 bits into SHA-256.
pub const SAMPLES: usize = 2048;
/// Repetition Count Test cutoff (§4.4.1): C = 1 + ceil(-log2(alpha) / H), alpha = 2^-20.
pub const RCT_CUTOFF: u32 = 81;
/// Adaptive Proportion Test window for binary samples (§4.4.2).
pub const APT_WINDOW: u32 = 1024;
/// APT cutoff: C = 1 + CRITBINOM(W, 2^-H, 1 - alpha), alpha = 2^-20 (tests::cutoffs recomputes both).
pub const APT_CUTOFF: u32 = 915;

/// A health test failed: the source is stuck or biased beyond what H_PER_SAMPLE allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unhealthy {
    RepetitionCount,
    AdaptiveProportion,
}

/// Pulls [`SAMPLES`] bits from `next_bit`, runs the RCT and the APT on every one of them, and returns
/// SHA-256(the bits packed LSB first). None of the bits are kept apart from the hash state.
pub fn seed_from_bits(mut next_bit: impl FnMut() -> bool) -> Result<[u8; 32], Unhealthy> {
    let mut h = Sha256::new();
    let mut rct_last = false;
    let mut rct_run = 0u32;
    let mut apt_ref = false;
    let mut apt_seen = 0u32; // samples in the current window
    let mut apt_hits = 0u32;
    let mut byte = 0u8;
    for i in 0..SAMPLES {
        let b = next_bit();
        // RCT
        if i > 0 && b == rct_last {
            rct_run += 1;
        } else {
            rct_last = b;
            rct_run = 1;
        }
        if rct_run >= RCT_CUTOFF {
            return Err(Unhealthy::RepetitionCount);
        }
        // APT: the window's first sample is the reference; count how often it comes back
        if apt_seen == 0 {
            apt_ref = b;
            apt_hits = 1;
        } else if b == apt_ref {
            apt_hits += 1;
            if apt_hits >= APT_CUTOFF {
                return Err(Unhealthy::AdaptiveProportion);
            }
        }
        apt_seen += 1;
        if apt_seen == APT_WINDOW {
            apt_seen = 0;
        }
        // conditioning
        byte |= u8::from(b) << (i % 8);
        if i % 8 == 7 {
            h.update([byte]);
            byte = 0;
        }
    }
    Ok(h.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift: a stand-in for a healthy source
    fn prng(mut s: u32) -> impl FnMut() -> bool {
        move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s & 1 == 1
        }
    }

    #[test]
    fn cutoffs() {
        let alpha = (2.0f64).powi(-20);
        assert_eq!(RCT_CUTOFF, 1 + (-(alpha.log2()) / H_PER_SAMPLE).ceil() as u32);
        // CRITBINOM(W, p, 1 - alpha): the smallest k with P(X <= k) >= 1 - alpha, X ~ Binomial(W, p)
        let (w, p) = (APT_WINDOW as usize, (2.0f64).powf(-H_PER_SAMPLE));
        let mut ln_c = 0.0f64; // ln C(w, k)
        let mut cdf = 0.0f64;
        let mut crit = None;
        for k in 0..=w {
            if k > 0 {
                ln_c += ((w - k + 1) as f64).ln() - (k as f64).ln();
            }
            cdf += (ln_c + k as f64 * p.ln() + (w - k) as f64 * (1.0 - p).ln()).exp();
            if cdf >= 1.0 - alpha {
                crit = Some(k as u32);
                break;
            }
        }
        assert_eq!(APT_CUTOFF, 1 + crit.unwrap());
        assert!(SAMPLES as f64 * H_PER_SAMPLE >= 2.0 * 256.0);
    }

    #[test]
    fn healthy_source_gives_the_hash_of_its_bits() {
        let mut bits = prng(0x1234_5678);
        let mut packed = [0u8; SAMPLES / 8];
        for i in 0..SAMPLES {
            packed[i / 8] |= u8::from(bits()) << (i % 8);
        }
        let want: [u8; 32] = Sha256::digest(packed).into();
        assert_eq!(seed_from_bits(prng(0x1234_5678)), Ok(want));
        assert_ne!(seed_from_bits(prng(0x1234_5678)), seed_from_bits(prng(0x8765_4321)));
    }

    #[test]
    fn alternating_passes() {
        let mut b = false;
        assert!(seed_from_bits(|| {
            b = !b;
            b
        })
        .is_ok());
    }

    #[test]
    fn stuck_fails_rct() {
        assert_eq!(seed_from_bits(|| true), Err(Unhealthy::RepetitionCount));
        assert_eq!(seed_from_bits(|| false), Err(Unhealthy::RepetitionCount));
        // stuck after a while
        let mut n = 0;
        let mut p = prng(42);
        assert_eq!(
            seed_from_bits(|| {
                n += 1;
                n > 1500 || p()
            }),
            Err(Unhealthy::RepetitionCount)
        );
    }

    #[test]
    fn run_just_under_the_cutoff_passes_rct() {
        // 80 ones then 40 zeros, repeated: runs of 80 (< 81) and 67% ones (under the APT's cutoff)
        let mut i = 0u32;
        assert!(seed_from_bits(|| {
            i += 1;
            (i - 1) % 120 < 80
        })
        .is_ok());
    }

    #[test]
    fn biased_fails_apt() {
        // 15 ones, 1 zero: never a long run, but 94% ones
        let mut i = 0u32;
        assert_eq!(
            seed_from_bits(|| {
                i += 1;
                i % 16 != 0
            }),
            Err(Unhealthy::AdaptiveProportion)
        );
    }
}
