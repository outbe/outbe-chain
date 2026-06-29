//! UTC date and time helpers.
//!
//! All functions are pure integer arithmetic — no `chrono`, no float, no
//! locale, no DST. The yyyymmdd "date key" is a `u32` like `20251205`. UTC
//! is the only calendar; `worldwide_day_from_timestamp` shifts by +14h
//! (UTC+14) for Metadosis-internal "Worldwide Day" semantics.

/// Seconds in one calendar day. Public so consumers can do day-aligned
/// timestamp arithmetic without redefining the constant.
pub const SECONDS_PER_DAY: u64 = 86_400;

/// UTC+14 offset used by `worldwide_day_from_timestamp`. Public so
/// callers that need to align Metadosis WWD with arbitrary timestamps can
/// use the same constant.
pub const UTC_PLUS_14_OFFSET: u64 = 50_400;

/// Errors returned by the time helpers. Currently only one variant; the
/// enum is `non_exhaustive` so additional variants can be introduced
/// without breaking callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeError {
    /// `utc_day` is strictly before `genesis_utc_day`. Caller is expected
    /// to translate this into a fatal protocol error — a finalized block
    /// predating genesis must not be processed.
    PreGenesis { utc_day: u32, genesis_utc_day: u32 },
}

impl core::fmt::Display for TimeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TimeError::PreGenesis {
                utc_day,
                genesis_utc_day,
            } => write!(
                f,
                "utc_day {utc_day} predates genesis_utc_day {genesis_utc_day}"
            ),
        }
    }
}

/// Converts a unix timestamp to a yyyymmdd date key in UTC.
pub fn timestamp_to_date_key(timestamp: u64) -> u32 {
    // `timestamp / SECONDS_PER_DAY` is at most `u64::MAX / 86_400` ≈ 2.1e14, far
    // below `i64::MAX`, so this never saturates; `try_from` (not `as`) keeps the
    // conversion non-narrowing and deterministic on the consensus date path.
    let days = i64::try_from(timestamp / SECONDS_PER_DAY).unwrap_or(i64::MAX);
    civil_date_from_days(days)
}

/// Returns the worldwide day key for a unix timestamp (UTC+14).
pub fn worldwide_day_from_timestamp(timestamp: u64) -> u32 {
    timestamp_to_date_key(timestamp + UTC_PLUS_14_OFFSET)
}

/// Returns the unix timestamp for midnight UTC of a yyyymmdd date key.
pub fn date_key_to_utc_timestamp(date_key: u32) -> u64 {
    let year = (date_key / 10_000) as i64;
    let month = ((date_key / 100) % 100) as i64;
    let day = (date_key % 100) as i64;
    let days = days_from_civil(year, month, day);
    (days as u64) * SECONDS_PER_DAY
}

/// Returns the previous calendar day key for a yyyymmdd date key.
pub fn previous_date_key(date_key: u32) -> u32 {
    let ts = date_key_to_utc_timestamp(date_key).saturating_sub(SECONDS_PER_DAY);
    timestamp_to_date_key(ts)
}

/// Returns the next calendar day key for a yyyymmdd date key.
///
/// Walks forward 24h via integer timestamp arithmetic; this is the only
/// correct way to advance across month/year boundaries — direct `u32`
/// arithmetic on `yyyymmdd` is wrong (e.g., `20251231 + 1 != 20260101`).
pub fn next_date_key(date_key: u32) -> u32 {
    let ts = date_key_to_utc_timestamp(date_key).saturating_add(SECONDS_PER_DAY);
    timestamp_to_date_key(ts)
}

/// Computes the integer number of UTC days between two date keys.
///
/// Returns `Ok(0)` when `utc_day == genesis_utc_day`,
/// `Ok(n)` when `utc_day > genesis_utc_day`, and
/// `Err(TimeError::PreGenesis)` when `utc_day < genesis_utc_day`.
///
/// Computed via `(date_key_to_timestamp(utc_day) -
/// date_key_to_timestamp(genesis_utc_day)) / SECONDS_PER_DAY`. Direct
/// `u32` subtraction of `yyyymmdd` keys is wrong across month/year
/// boundaries and must not be used.
pub fn day_number_between(genesis_utc_day: u32, utc_day: u32) -> Result<u32, TimeError> {
    let g_ts = date_key_to_utc_timestamp(genesis_utc_day);
    let u_ts = date_key_to_utc_timestamp(utc_day);
    let delta = u_ts.checked_sub(g_ts).ok_or(TimeError::PreGenesis {
        utc_day,
        genesis_utc_day,
    })?;
    // Day count since genesis. `u32` covers ~11.7M years of days; saturating
    // `try_from` (not `as`) keeps it non-narrowing and deterministic — an
    // unreachable overflow clamps rather than silently wrapping.
    Ok(u32::try_from(delta / SECONDS_PER_DAY).unwrap_or(u32::MAX))
}

fn civil_date_from_days(days_since_epoch: i64) -> u32 {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32) * 10000 + m * 100 + d
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_to_date_key_uses_utc() {
        assert_eq!(timestamp_to_date_key(0), 19700101);
        assert_eq!(timestamp_to_date_key(1_704_067_200), 20240101);
        assert_eq!(timestamp_to_date_key(1_734_706_800), 20241220);
    }

    #[test]
    fn worldwide_day_uses_utc_plus_14_boundary() {
        assert_eq!(worldwide_day_from_timestamp(1_734_706_800), 20241221);
    }

    #[test]
    fn date_key_to_timestamp_roundtrip_at_midnight() {
        // Midnight UTC of 2024-01-01 = 1_704_067_200.
        assert_eq!(date_key_to_utc_timestamp(20240101), 1_704_067_200);
        assert_eq!(date_key_to_utc_timestamp(19700101), 0);
    }

    #[test]
    fn date_key_roundtrip_through_timestamp() {
        for k in [19700101u32, 20240101, 20240229, 20241231, 20251205] {
            let ts = date_key_to_utc_timestamp(k);
            assert_eq!(timestamp_to_date_key(ts), k, "roundtrip failed for {k}");
        }
    }

    #[test]
    fn previous_date_key_crosses_month_and_year() {
        assert_eq!(previous_date_key(20240101), 20231231);
        assert_eq!(previous_date_key(20240301), 20240229); // leap year
        assert_eq!(previous_date_key(20230301), 20230228);
    }

    #[test]
    fn next_date_key_crosses_month_and_year() {
        assert_eq!(next_date_key(20231231), 20240101);
        assert_eq!(next_date_key(20240229), 20240301); // leap year
        assert_eq!(next_date_key(20230228), 20230301);
        assert_eq!(next_date_key(20240630), 20240701);
    }

    #[test]
    fn day_number_between_same_day_is_zero() {
        assert_eq!(day_number_between(20240101, 20240101), Ok(0));
    }

    #[test]
    fn day_number_between_walks_forward_across_year() {
        // 2024 is leap → 366 days.
        assert_eq!(day_number_between(20240101, 20250101), Ok(366));
        // 2023 is non-leap → 365 days.
        assert_eq!(day_number_between(20230101, 20240101), Ok(365));
    }

    #[test]
    fn day_number_between_walks_forward_within_year() {
        assert_eq!(day_number_between(20240101, 20240131), Ok(30));
        assert_eq!(day_number_between(20240101, 20240301), Ok(60)); // 31 + 29 (leap)
    }

    #[test]
    fn day_number_between_pre_genesis_is_error() {
        assert_eq!(
            day_number_between(20240101, 20231231),
            Err(TimeError::PreGenesis {
                utc_day: 20231231,
                genesis_utc_day: 20240101,
            })
        );
    }
}
