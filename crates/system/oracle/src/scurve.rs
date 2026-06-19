//! S-curve pricing algorithm.
//!
//! Ported from Cosmos SDK `x/oracle/algorithm/scurve.go`.
//! Uses 128 precomputed decay coefficients at 1e18 scale.
//! Formula: `value[i] = peak_price * coefficient[i] / 1e18`

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;

use crate::contract::{OracleContract, SCALE_1E18};
use crate::precompile::IOracle;

/// S-curve period in days.
pub const PERIOD: usize = 128;

/// Seconds in one day.
pub const DAY_SECONDS: u64 = 86400;

/// Truncates a timestamp to the start of the UTC day (00:00 UTC).
pub fn truncate_to_day(timestamp: u64) -> u64 {
    (timestamp / DAY_SECONDS) * DAY_SECONDS
}

/// Precomputed S-curve coefficients at 1e18 scale (128 entries).
///
/// Formula: `coef_i = 1 - alpha * (sigmoid(x_i) - sigmoid_min) / (sigmoid_0 - sigmoid_min)`
/// Parameters: alpha=0.08, beta=0.08, center=64
///
/// Source: `x/oracle/algorithm/scurve.go:precomputedCoefficients`
///
/// Each string value `"0.XXXX..."` is converted to U256 by removing the decimal
/// point and reading the 18 digits after it as an integer.
pub static COEFFICIENTS: [U256; PERIOD] = [
    U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]), // [0] 1.000000000000000000
    U256::from_limbs([999_960_180_425_626_732, 0, 0, 0]),   // [1]
    U256::from_limbs([999_917_088_812_140_809, 0, 0, 0]),   // [2]
    U256::from_limbs([999_870_460_263_885_930, 0, 0, 0]),   // [3]
    U256::from_limbs([999_820_009_122_969_866, 0, 0, 0]),   // [4]
    U256::from_limbs([999_765_427_459_612_166, 0, 0, 0]),   // [5]
    U256::from_limbs([999_706_383_473_257_243, 0, 0, 0]),   // [6]
    U256::from_limbs([999_642_519_802_931_395, 0, 0, 0]),   // [7]
    U256::from_limbs([999_573_451_746_113_562, 0, 0, 0]),   // [8]
    U256::from_limbs([999_498_765_386_374_277, 0, 0, 0]),   // [9]
    U256::from_limbs([999_418_015_631_251_544, 0, 0, 0]),   // [10]
    U256::from_limbs([999_330_724_163_311_368, 0, 0, 0]),   // [11]
    U256::from_limbs([999_236_377_309_123_402, 0, 0, 0]),   // [12]
    U256::from_limbs([999_134_423_833_009_877, 0, 0, 0]),   // [13]
    U256::from_limbs([999_024_272_664_953_417, 0, 0, 0]),   // [14]
    U256::from_limbs([998_905_290_575_010_651, 0, 0, 0]),   // [15]
    U256::from_limbs([998_776_799_810_049_853, 0, 0, 0]),   // [16]
    U256::from_limbs([998_638_075_712_641_404, 0, 0, 0]),   // [17]
    U256::from_limbs([998_488_344_346_554_979, 0, 0, 0]),   // [18]
    U256::from_limbs([998_326_780_158_598_548, 0, 0, 0]),   // [19]
    U256::from_limbs([998_152_503_712_523_620, 0, 0, 0]),   // [20]
    U256::from_limbs([997_964_579_537_462_315, 0, 0, 0]),   // [21]
    U256::from_limbs([997_762_014_140_879_616, 0, 0, 0]),   // [22]
    U256::from_limbs([997_543_754_244_337_388, 0, 0, 0]),   // [23]
    U256::from_limbs([997_308_685_309_456_044, 0, 0, 0]),   // [24]
    U256::from_limbs([997_055_630_431_288_864, 0, 0, 0]),   // [25]
    U256::from_limbs([996_783_349_686_807_263, 0, 0, 0]),   // [26]
    U256::from_limbs([996_490_540_037_190_731, 0, 0, 0]),   // [27]
    U256::from_limbs([996_175_835_893_934_436, 0, 0, 0]),   // [28]
    U256::from_limbs([995_837_810_470_146_079, 0, 0, 0]),   // [29]
    U256::from_limbs([995_474_978_049_433_634, 0, 0, 0]),   // [30]
    U256::from_limbs([995_085_797_315_024_556, 0, 0, 0]),   // [31]
    U256::from_limbs([994_668_675_890_604_814, 0, 0, 0]),   // [32]
    U256::from_limbs([994_221_976_251_114_414, 0, 0, 0]),   // [33]
    U256::from_limbs([993_744_023_165_530_121, 0, 0, 0]),   // [34]
    U256::from_limbs([993_233_112_833_517_895, 0, 0, 0]),   // [35]
    U256::from_limbs([992_687_523_872_639_477, 0, 0, 0]),   // [36]
    U256::from_limbs([992_105_530_301_327_532, 0, 0, 0]),   // [37]
    U256::from_limbs([991_485_416_643_820_061, 0, 0, 0]),   // [38]
    U256::from_limbs([990_825_495_255_355_682, 0, 0, 0]),   // [39]
    U256::from_limbs([990_124_125_927_950_627, 0, 0, 0]),   // [40]
    U256::from_limbs([989_379_737_787_912_106, 0, 0, 0]),   // [41]
    U256::from_limbs([988_590_853_435_103_334, 0, 0, 0]),   // [42]
    U256::from_limbs([987_756_115_200_498_774, 0, 0, 0]),   // [43]
    U256::from_limbs([986_874_313_312_974_705, 0, 0, 0]),   // [44]
    U256::from_limbs([985_944_415_669_576_357, 0, 0, 0]),   // [45]
    U256::from_limbs([984_965_598_797_599_151, 0, 0, 0]),   // [46]
    U256::from_limbs([983_937_279_484_725_091, 0, 0, 0]),   // [47]
    U256::from_limbs([982_859_146_439_322_706, 0, 0, 0]),   // [48]
    U256::from_limbs([981_731_191_232_210_532, 0, 0, 0]),   // [49]
    U256::from_limbs([980_553_737_670_156_189, 0, 0, 0]),   // [50]
    U256::from_limbs([979_327_468_667_514_300, 0, 0, 0]),   // [51]
    U256::from_limbs([978_053_449_623_635_163, 0, 0, 0]),   // [52]
    U256::from_limbs([976_733_147_288_004_089, 0, 0, 0]),   // [53]
    U256::from_limbs([975_368_443_109_965_511, 0, 0, 0]),   // [54]
    U256::from_limbs([973_961_640_131_506_101, 0, 0, 0]),   // [55]
    U256::from_limbs([972_515_462_594_043_312, 0, 0, 0]),   // [56]
    U256::from_limbs([971_033_047_594_784_704, 0, 0, 0]),   // [57]
    U256::from_limbs([969_517_928_342_881_641, 0, 0, 0]),   // [58]
    U256::from_limbs([967_974_008_824_358_356, 0, 0, 0]),   // [59]
    U256::from_limbs([966_405_529_977_787_170, 0, 0, 0]),   // [60]
    U256::from_limbs([964_817_027_796_462_168, 0, 0, 0]),   // [61]
    U256::from_limbs([963_213_284_090_982_391, 0, 0, 0]),   // [62]
    U256::from_limbs([961_599_270_950_507_679, 0, 0, 0]),   // [63]
    U256::from_limbs([959_980_090_212_813_386, 0, 0, 0]),   // [64] center
    U256::from_limbs([958_360_909_475_119_094, 0, 0, 0]),   // [65]
    U256::from_limbs([956_746_896_334_644_381, 0, 0, 0]),   // [66]
    U256::from_limbs([955_143_152_629_164_494, 0, 0, 0]),   // [67]
    U256::from_limbs([953_554_650_447_839_491, 0, 0, 0]),   // [68]
    U256::from_limbs([951_986_171_601_268_416, 0, 0, 0]),   // [69]
    U256::from_limbs([950_442_252_082_745_132, 0, 0, 0]),   // [70]
    U256::from_limbs([948_927_132_830_842_068, 0, 0, 0]),   // [71]
    U256::from_limbs([947_444_717_831_583_350, 0, 0, 0]),   // [72]
    U256::from_limbs([945_998_540_294_120_671, 0, 0, 0]),   // [73]
    U256::from_limbs([944_591_737_315_661_262, 0, 0, 0]),   // [74]
    U256::from_limbs([943_227_033_137_622_684, 0, 0, 0]),   // [75]
    U256::from_limbs([941_906_730_801_991_610, 0, 0, 0]),   // [76]
    U256::from_limbs([940_632_711_758_112_472, 0, 0, 0]),   // [77]
    U256::from_limbs([939_406_442_755_470_583, 0, 0, 0]),   // [78]
    U256::from_limbs([938_228_989_193_416_241, 0, 0, 0]),   // [79]
    U256::from_limbs([937_101_033_986_304_066, 0, 0, 0]),   // [80]
    U256::from_limbs([936_022_900_940_901_681, 0, 0, 0]),   // [81]
    U256::from_limbs([934_994_581_628_027_510, 0, 0, 0]),   // [82]
    U256::from_limbs([934_015_764_756_050_415, 0, 0, 0]),   // [83]
    U256::from_limbs([933_085_867_112_652_068, 0, 0, 0]),   // [84]
    U256::from_limbs([932_204_065_225_127_998, 0, 0, 0]),   // [85]
    U256::from_limbs([931_369_326_990_523_438, 0, 0, 0]),   // [86]
    U256::from_limbs([930_580_442_637_714_667, 0, 0, 0]),   // [87]
    U256::from_limbs([929_836_054_497_676_145, 0, 0, 0]),   // [88]
    U256::from_limbs([929_134_685_170_271_091, 0, 0, 0]),   // [89]
    U256::from_limbs([928_474_763_781_806_711, 0, 0, 0]),   // [90]
    U256::from_limbs([927_854_650_124_299_241, 0, 0, 0]),   // [91]
    U256::from_limbs([927_272_656_552_987_296, 0, 0, 0]),   // [92]
    U256::from_limbs([926_727_067_592_108_766, 0, 0, 0]),   // [93]
    U256::from_limbs([926_216_157_260_096_651, 0, 0, 0]),   // [94]
    U256::from_limbs([925_738_204_174_512_358, 0, 0, 0]),   // [95]
    U256::from_limbs([925_291_504_535_021_958, 0, 0, 0]),   // [96]
    U256::from_limbs([924_874_383_110_602_216, 0, 0, 0]),   // [97]
    U256::from_limbs([924_485_202_376_193_138, 0, 0, 0]),   // [98]
    U256::from_limbs([924_122_369_955_480_694, 0, 0, 0]),   // [99]
    U256::from_limbs([923_784_344_531_692_336, 0, 0, 0]),   // [100]
    U256::from_limbs([923_469_640_388_436_042, 0, 0, 0]),   // [101]
    U256::from_limbs([923_176_830_738_819_509, 0, 0, 0]),   // [102]
    U256::from_limbs([922_904_549_994_337_908, 0, 0, 0]),   // [103]
    U256::from_limbs([922_651_495_116_170_728, 0, 0, 0]),   // [104]
    U256::from_limbs([922_416_426_181_289_273, 0, 0, 0]),   // [105]
    U256::from_limbs([922_198_166_284_747_156, 0, 0, 0]),   // [106]
    U256::from_limbs([921_995_600_888_164_457, 0, 0, 0]),   // [107]
    U256::from_limbs([921_807_676_713_103_152, 0, 0, 0]),   // [108]
    U256::from_limbs([921_633_400_267_028_224, 0, 0, 0]),   // [109]
    U256::from_limbs([921_471_836_079_071_793, 0, 0, 0]),   // [110]
    U256::from_limbs([921_322_104_712_985_368, 0, 0, 0]),   // [111]
    U256::from_limbs([921_183_380_615_576_919, 0, 0, 0]),   // [112]
    U256::from_limbs([921_054_889_850_616_121, 0, 0, 0]),   // [113]
    U256::from_limbs([920_935_907_760_673_356, 0, 0, 0]),   // [114]
    U256::from_limbs([920_825_756_592_616_895, 0, 0, 0]),   // [115]
    U256::from_limbs([920_723_803_116_503_370, 0, 0, 0]),   // [116]
    U256::from_limbs([920_629_456_262_315_404, 0, 0, 0]),   // [117]
    U256::from_limbs([920_542_164_794_375_228, 0, 0, 0]),   // [118]
    U256::from_limbs([920_461_415_039_252_495, 0, 0, 0]),   // [119]
    U256::from_limbs([920_386_728_679_513_211, 0, 0, 0]),   // [120]
    U256::from_limbs([920_317_660_622_695_377, 0, 0, 0]),   // [121]
    U256::from_limbs([920_253_796_952_369_418, 0, 0, 0]),   // [122]
    U256::from_limbs([920_194_752_966_014_606, 0, 0, 0]),   // [123]
    U256::from_limbs([920_140_171_302_656_906, 0, 0, 0]),   // [124]
    U256::from_limbs([920_089_720_161_740_843, 0, 0, 0]),   // [125]
    U256::from_limbs([920_043_091_613_485_853, 0, 0, 0]),   // [126]
    U256::from_limbs([920_000_000_000_000_000, 0, 0, 0]),   // [127]
];

/// Computes the S-curve value for a given peak price and day index.
///
/// `value = peak_price * COEFFICIENTS[day_index] / 1e18`
///
/// Returns zero if day_index >= PERIOD.
pub fn compute_scurve_value(peak_price: U256, day_index: usize) -> U256 {
    if day_index >= PERIOD {
        return U256::ZERO;
    }
    peak_price
        .checked_mul(COEFFICIENTS[day_index])
        .unwrap_or(U256::ZERO)
        / SCALE_1E18
}

/// Returns the maximum active S-curve value for a pair at a given timestamp.
///
/// Iterates all active S-curve entries for the given pair_id and returns
/// the highest value that applies to the given timestamp.
pub fn get_max_active_scurve_value(
    oracle: &OracleContract,
    pair_id: u32,
    timestamp: u64,
) -> Result<U256> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;
    let target_day = truncate_to_day(timestamp);

    let mut max_value = U256::ZERO;

    for idx in oldest..count {
        let entry_pair = oracle.scurve_pair_id.read(&idx)?;
        if entry_pair != pair_id {
            continue;
        }

        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;

        if target_day < peak_day {
            continue;
        }

        let days_since = ((target_day - peak_day) / DAY_SECONDS) as usize;
        if days_since >= PERIOD {
            continue;
        }

        let value = compute_scurve_value(peak_price, days_since);
        if value > max_value {
            max_value = value;
        }
    }

    Ok(max_value)
}

/// Returns all active S-curve entries for a specific pair.
///
/// Returns `(peak_days, peak_prices, current_values)` as parallel arrays.
pub fn get_scurve_entries(
    oracle: &OracleContract,
    pair_id: u32,
    current_timestamp: u64,
) -> Result<(Vec<u64>, Vec<U256>, Vec<U256>)> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;
    let target_day = truncate_to_day(current_timestamp);

    let mut peak_days = Vec::new();
    let mut peak_prices = Vec::new();
    let mut current_values = Vec::new();

    for idx in oldest..count {
        let entry_pair = oracle.scurve_pair_id.read(&idx)?;
        if entry_pair != pair_id {
            continue;
        }

        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;

        let days_since = if target_day >= peak_day {
            ((target_day - peak_day) / DAY_SECONDS) as usize
        } else {
            continue;
        };

        if days_since >= PERIOD {
            continue;
        }

        peak_days.push(peak_day);
        peak_prices.push(peak_price);
        current_values.push(compute_scurve_value(peak_price, days_since));
    }

    Ok((peak_days, peak_prices, current_values))
}

/// Returns all S-curve data across all pairs.
///
/// Returns `(pair_ids, peak_days, peak_prices)` as parallel arrays.
pub fn get_all_scurve_data(oracle: &OracleContract) -> Result<(Vec<u32>, Vec<u64>, Vec<U256>)> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;

    let mut pair_ids = Vec::new();
    let mut peak_days = Vec::new();
    let mut peak_prices = Vec::new();

    for idx in oldest..count {
        let pair_id = oracle.scurve_pair_id.read(&idx)?;
        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;

        pair_ids.push(pair_id);
        peak_days.push(peak_day);
        peak_prices.push(peak_price);
    }

    Ok((pair_ids, peak_days, peak_prices))
}

/// Returns all S-curve data for a specific pair.
///
/// Returns `(peak_days, peak_prices)` as parallel arrays. The full 128-day
/// value curve is deterministic from the peak price, so callers can use
/// `getScurveValues` for timestamp-specific values.
pub fn get_all_scurve_data_for_pair(
    oracle: &OracleContract,
    pair_id: u32,
) -> Result<(Vec<u64>, Vec<U256>)> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;

    let mut peak_days = Vec::new();
    let mut peak_prices = Vec::new();

    for idx in oldest..count {
        if oracle.scurve_pair_id.read(&idx)? != pair_id {
            continue;
        }
        peak_days.push(oracle.scurve_peak_day.read(&idx)?);
        peak_prices.push(oracle.scurve_peak_price.read(&idx)?);
    }

    Ok((peak_days, peak_prices))
}

/// Stores a new S-curve entry for a detected peak.
pub fn store_scurve_entry(
    oracle: &mut OracleContract,
    pair_id: u32,
    peak_day: u64,
    peak_price: U256,
) -> Result<()> {
    let idx = oracle.scurve_count.read()?;
    oracle.scurve_pair_id.write(&idx, pair_id)?;
    oracle.scurve_peak_day.write(&idx, peak_day)?;
    oracle.scurve_peak_price.write(&idx, peak_price)?;
    oracle.scurve_count.write(idx + 1)?;
    Ok(())
}

/// Evicts expired S-curve entries (older than 128 days).
pub fn evict_expired_scurves(oracle: &mut OracleContract, current_timestamp: u64) -> Result<()> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;
    let cutoff = current_timestamp.saturating_sub(PERIOD as u64 * DAY_SECONDS);

    let mut new_oldest = oldest;
    while new_oldest < count {
        let peak_day = oracle.scurve_peak_day.read(&new_oldest)?;
        if peak_day >= cutoff {
            break;
        }
        new_oldest += 1;
    }

    if new_oldest != oldest {
        oracle.scurve_oldest_idx.write(new_oldest)?;
    }

    Ok(())
}

/// Detects peaks from the last 3 *closed* daily close prices for a pair
/// and stores new S-curve entries.
///
/// A peak occurs when: close[D-3] < close[D-2] > close[D-1], i.e. D-2 is the
/// peak. The current (just-started) day is never used as a close, so the peak
/// of a day X is confirmed at the start of X+2.
///
/// Called from the daily hook on the first block of each UTC day.
pub fn process_daily_scurve(
    oracle: &mut OracleContract,
    pair_id: u32,
    timestamp: u64,
) -> Result<()> {
    let current_day = truncate_to_day(timestamp);

    // The daily hook fires on the first block of `current_day`, so
    // `current_day` itself has no close yet. Detect peaks only over fully
    // CLOSED UTC days. At this point the most recent closed day is D-1, so
    // the latest peak we can confirm is D-2 — confirming a peak requires the
    // close of the day that follows it.
    //
    //   day_minus_3 (close before peak) < day_minus_2 (peak) > day_minus_1 (close after peak)
    let day_minus_1 = current_day.saturating_sub(DAY_SECONDS);
    let day_minus_2 = current_day.saturating_sub(2 * DAY_SECONDS);
    let day_minus_3 = current_day.saturating_sub(3 * DAY_SECONDS);

    // Last snapshot rate within each fully-closed UTC day.
    let close_d1 = get_daily_close(oracle, pair_id, day_minus_1)?;
    let close_d2 = get_daily_close(oracle, pair_id, day_minus_2)?;
    let close_d3 = get_daily_close(oracle, pair_id, day_minus_3)?;

    // Need all three closed-day prices to detect a peak.
    if close_d1.is_zero() || close_d2.is_zero() || close_d3.is_zero() {
        return Ok(());
    }

    // Peak detection: D-3 < D-2 > D-1 (i.e., D-2 is the peak).
    if close_d3 < close_d2 && close_d2 > close_d1 {
        store_scurve_entry(oracle, pair_id, day_minus_2, close_d2)?;
        let event = IOracle::ScurvePeakDetected {
            pairId: pair_id,
            peakPrice: close_d2,
            peakDay: day_minus_2,
        };
        let _ = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());
    }

    // Evict expired entries
    evict_expired_scurves(oracle, timestamp)?;

    Ok(())
}

/// Gets the closest exchange rate snapshot for a pair on a given day.
///
/// Scans snapshots backwards from the day end to find the last rate for that day.
fn get_daily_close(oracle: &OracleContract, pair_id: u32, day_start: u64) -> Result<U256> {
    let day_end = day_start + DAY_SECONDS;
    let write_idx = oracle.snapshot_write_idx.read()?;
    let oldest_idx = oracle.snapshot_oldest_idx.read()?;

    let mut idx = write_idx;
    while idx > oldest_idx {
        idx -= 1;
        let ts = oracle.snapshot_timestamp.read(&idx)?;
        if ts < day_start {
            break;
        }
        if ts >= day_end {
            continue;
        }

        // Found a snapshot in this day — look for our pair
        let pc = oracle.snapshot_pair_count.read(&idx)?;
        let pair_id_map = oracle.snapshot_pair_id.get_nested(&idx);
        let rate_map = oracle.snapshot_rate.get_nested(&idx);

        for p in 0..pc {
            if pair_id_map.read(&p)? == pair_id {
                return rate_map.read(&p);
            }
        }
    }

    Ok(U256::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::units::Units;

    #[test]
    fn test_coefficients_boundary_values() {
        // First coefficient should be exactly 1.0 (1e18)
        assert_eq!(COEFFICIENTS[0], SCALE_1E18);
        // Last coefficient should be exactly 0.92 (920e15)
        assert_eq!(COEFFICIENTS[127], U256::from(920_000_000_000_000_000u64));
        // Center (64) should be ~0.95998
        assert_eq!(COEFFICIENTS[64], U256::from(959_980_090_212_813_386u64));
    }

    #[test]
    fn test_coefficients_monotonically_decreasing() {
        for i in 1..PERIOD {
            assert!(
                COEFFICIENTS[i] <= COEFFICIENTS[i - 1],
                "coefficient[{}] > coefficient[{}]: {} > {}",
                i,
                i - 1,
                COEFFICIENTS[i],
                COEFFICIENTS[i - 1]
            );
        }
    }

    #[test]
    fn test_compute_scurve_value() {
        let peak_price = U256::in_units(100); // 100.0

        // Day 0: value = 100 * 1.0 = 100
        assert_eq!(compute_scurve_value(peak_price, 0), peak_price);

        // Day 127: value = 100 * 0.92 = 92
        let day_127_value = compute_scurve_value(peak_price, 127);
        assert_eq!(day_127_value, U256::in_units(92));

        // Day 128+: returns zero
        assert_eq!(compute_scurve_value(peak_price, 128), U256::ZERO);
        assert_eq!(compute_scurve_value(peak_price, 1000), U256::ZERO);
    }

    #[test]
    fn test_compute_scurve_value_precision() {
        // Test with a non-round peak price
        let peak = U256::from(18_343_660_000_000_000_000_000u128); // 18343.66
        let val_day1 = compute_scurve_value(peak, 1);
        // Expected: 18343.66 * 0.999960180425626732 = ~18342.93 (approx)
        // The exact value depends on integer truncation
        assert!(val_day1 < peak);
        assert!(val_day1 > U256::in_units(18342));
    }

    #[test]
    fn test_truncate_to_day() {
        // 2025-01-01 12:34:56 UTC
        let ts = 1735735696u64;
        let day = truncate_to_day(ts);
        assert_eq!(day % DAY_SECONDS, 0);
        assert!(day <= ts);
        assert!(ts - day < DAY_SECONDS);
    }

    #[test]
    fn test_scurve_storage_and_lookup() {
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);

            // Store an S-curve entry. Use day-aligned timestamps.
            let pair_id = 1u32;
            let peak_day = truncate_to_day(1_000_000);
            let peak_price = U256::in_units(500);

            store_scurve_entry(&mut oracle, pair_id, peak_day, peak_price).unwrap();
            assert_eq!(oracle.scurve_count.read().unwrap(), 1);

            // Query value at peak day (day_index=0) → should equal peak_price
            let val = get_max_active_scurve_value(&oracle, pair_id, peak_day).unwrap();
            assert_eq!(val, peak_price);

            // Query value at peak_day + 127 days → should be ~92% of peak
            let far_future = peak_day + 127 * DAY_SECONDS;
            let val_127 = get_max_active_scurve_value(&oracle, pair_id, far_future).unwrap();
            assert_eq!(val_127, U256::in_units(460)); // 500 * 0.92 = 460

            // Query value at peak_day + 128 days → should be zero (expired)
            let expired = peak_day + 128 * DAY_SECONDS;
            let val_expired = get_max_active_scurve_value(&oracle, pair_id, expired).unwrap();
            assert_eq!(val_expired, U256::ZERO);
        });
    }

    #[test]
    fn test_scurve_max_of_multiple() {
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            let pair_id = 1u32;

            // Two peaks for the same pair, day-aligned
            let peak1_day = truncate_to_day(1_000_000);
            let peak1_price = U256::in_units(100);
            store_scurve_entry(&mut oracle, pair_id, peak1_day, peak1_price).unwrap();

            let peak2_day = peak1_day + 10 * DAY_SECONDS;
            let peak2_price = U256::in_units(200);
            store_scurve_entry(&mut oracle, pair_id, peak2_day, peak2_price).unwrap();

            // At peak2_day, both are active.
            // Peak1 at day_index=10: 100 * coeff[10] ≈ 99.94
            // Peak2 at day_index=0: 200 * coeff[0] = 200
            // Max should be 200
            let val = get_max_active_scurve_value(&oracle, pair_id, peak2_day).unwrap();
            assert_eq!(val, U256::in_units(200));
        });
    }

    // ===================================================================
    // Daily peak-detection window (regression tests).
    //
    // The daily hook fires on the FIRST block of the current UTC day, when
    // that day has no close yet. Detection therefore runs over fully-CLOSED
    // days only: D-3 < D-2 > D-1 confirms a peak on D-2. The previous
    // implementation used the just-started current day as a close, so
    // `close_d0` was zero at fire time and no runtime peak was ever stored.
    // ===================================================================

    /// Writes a single end-of-day snapshot so `get_daily_close` treats `rate`
    /// as that UTC day's close.
    fn write_daily_close(oracle: &mut OracleContract, pair_id: u32, day_start: u64, rate: U256) {
        oracle
            .write_snapshot(day_start + 80_000, &[(pair_id, rate, U256::in_units(1))])
            .unwrap();
    }

    #[test]
    fn test_peak_detected_over_closed_days_without_current_day_data() {
        // Core regression: a peak on D-2 is detected at the start of D0 using
        // only closed days, regardless of whether the current day has data.
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            let pair_id = 1u32;

            let d0 = truncate_to_day(1_700_000_000); // current day — empty at fire time
            let d1 = d0 - DAY_SECONDS;
            let d2 = d0 - 2 * DAY_SECONDS; // peak
            let d3 = d0 - 3 * DAY_SECONDS;

            write_daily_close(&mut oracle, pair_id, d3, U256::in_units(100));
            write_daily_close(&mut oracle, pair_id, d2, U256::in_units(120));
            write_daily_close(&mut oracle, pair_id, d1, U256::in_units(110));
            // intentionally NO D0 data — the fix must not depend on it

            process_daily_scurve(&mut oracle, pair_id, d0).unwrap();

            assert_eq!(oracle.scurve_count.read().unwrap(), 1);
            assert_eq!(oracle.scurve_peak_day.read(&0).unwrap(), d2);
            assert_eq!(
                oracle.scurve_peak_price.read(&0).unwrap(),
                U256::in_units(120)
            );
        });
    }

    #[test]
    fn test_current_day_data_is_irrelevant() {
        // Whatever the current (incomplete) day shows must not change the
        // outcome — detection is over closed days only.
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            let pair_id = 1u32;

            let d0 = truncate_to_day(1_700_000_000);
            let d1 = d0 - DAY_SECONDS;
            let d2 = d0 - 2 * DAY_SECONDS;
            let d3 = d0 - 3 * DAY_SECONDS;

            write_daily_close(&mut oracle, pair_id, d3, U256::in_units(100));
            write_daily_close(&mut oracle, pair_id, d2, U256::in_units(120));
            write_daily_close(&mut oracle, pair_id, d1, U256::in_units(110));
            // A spurious current-day tick that the old code would have consumed.
            write_daily_close(&mut oracle, pair_id, d0, U256::in_units(999));

            process_daily_scurve(&mut oracle, pair_id, d0).unwrap();

            assert_eq!(oracle.scurve_count.read().unwrap(), 1);
            assert_eq!(oracle.scurve_peak_day.read(&0).unwrap(), d2);
            assert_eq!(
                oracle.scurve_peak_price.read(&0).unwrap(),
                U256::in_units(120)
            );
        });
    }

    #[test]
    fn test_no_peak_on_monotonic_closed_days() {
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            let pair_id = 1u32;

            let d0 = truncate_to_day(1_700_000_000);
            let d1 = d0 - DAY_SECONDS;
            let d2 = d0 - 2 * DAY_SECONDS;
            let d3 = d0 - 3 * DAY_SECONDS;

            // Monotonic rising: no local max at D-2.
            write_daily_close(&mut oracle, pair_id, d3, U256::in_units(100));
            write_daily_close(&mut oracle, pair_id, d2, U256::in_units(110));
            write_daily_close(&mut oracle, pair_id, d1, U256::in_units(120));

            process_daily_scurve(&mut oracle, pair_id, d0).unwrap();

            assert_eq!(oracle.scurve_count.read().unwrap(), 0);
        });
    }

    #[test]
    fn test_no_detection_with_insufficient_history() {
        // Only two closed days available (D-3 missing) → no peak, no panic.
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            let pair_id = 1u32;

            let d0 = truncate_to_day(1_700_000_000);
            let d1 = d0 - DAY_SECONDS;
            let d2 = d0 - 2 * DAY_SECONDS;

            write_daily_close(&mut oracle, pair_id, d2, U256::in_units(100));
            write_daily_close(&mut oracle, pair_id, d1, U256::in_units(120));
            // D-3 missing

            process_daily_scurve(&mut oracle, pair_id, d0).unwrap();

            assert_eq!(oracle.scurve_count.read().unwrap(), 0);
        });
    }
}
