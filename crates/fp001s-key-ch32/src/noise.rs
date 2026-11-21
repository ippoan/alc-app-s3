//! The seed's entropy (the CH32V203 has no TRNG): the ADC's least significant bit, read from the internal
//! temperature sensor and Vrefint in turn with the shortest sample time, checked and conditioned by
//! alarm_key_nostd::entropy (SP 800-90B's Repetition Count / Adaptive Proportion tests, then SHA-256).
//!
//! Nothing here hands the raw samples out: they go straight into the health tests and the hash.

use alarm_key_nostd::entropy::{self, Unhealthy};
use ch32_hal::adc::{Adc, Pga, SampleTime, Temperature, VrefInt};
use ch32_hal::peripherals::ADC1;

pub fn seed(adc: &mut Adc<'static, ADC1>) -> Result<[u8; 32], Unhealthy> {
    let mut temp = Temperature;
    let mut vref = VrefInt;
    let mut i = 0u32;
    entropy::seed_from_bits(|| {
        i = i.wrapping_add(1);
        let v = if i & 1 == 0 {
            adc.convert(&mut temp, SampleTime::CYCLES1_5, Pga::X1)
        } else {
            adc.convert(&mut vref, SampleTime::CYCLES1_5, Pga::X1)
        };
        v & 1 == 1
    })
}
