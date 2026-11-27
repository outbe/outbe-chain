//! Calendar arithmetic over `YYYYMMDD` worldwide days (UTC+14 protocol convention).
//!
//! Ported verbatim from `pso-precompiles::precompile::worldwide_day`.

const MIN_DATE: u64 = 19_700_101;
const MAX_DATE: u64 = 99_991_231;
const UTC_PLUS_14_SECS: u128 = 14 * 3600;
const SECONDS_PER_DAY: u128 = 86_400;

/// Days since 1970-01-01 of the `YYYYMMDD` date, or `None` if not a valid
/// proleptic Gregorian date in `1970-01-01..=9999-12-31`.
pub const fn day_number(yyyymmdd: u64) -> Option<u64> {
    if yyyymmdd < MIN_DATE || yyyymmdd > MAX_DATE {
        return None;
    }
    let y = yyyymmdd / 10_000;
    let m = (yyyymmdd / 100) % 100;
    let d = yyyymmdd % 100;
    if m == 0 || m > 12 || d == 0 || d > days_in_month(y, m) {
        return None;
    }

    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Day number of the calendar date at UTC+14 for Unix timestamp `timestamp_secs`.
pub const fn today_utc_plus_14(timestamp_secs: u64) -> u64 {
    ((timestamp_secs as u128 + UTC_PLUS_14_SECS) / SECONDS_PER_DAY) as u64
}

const fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if is_leap_year(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const fn is_leap_year(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
