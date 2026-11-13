//! Execution timestamp conversion and ISO-8601 UTC formatting helpers.

use alloy_primitives::U256;

/// Convert an execution timestamp in Unix seconds without consulting the wall clock.
///
/// Values through `u64::MAX` are preserved exactly. Wider values are rejected,
/// never saturated, so malformed execution contexts cannot silently change time.
pub fn timestamp_secs(timestamp: U256) -> Result<u64, crate::errors::TimestampOverflow> {
    u64::try_from(timestamp).map_err(|_| crate::errors::TimestampOverflow)
}

/// Seconds in one calendar day.
pub const SECONDS_PER_DAY: u64 = 86_400;

/// The local wall-clock time as `YYYY-MM-DDTHH:MM:SS.mmmZ`, for off-chain journals only.
pub fn iso8601_now() -> String {
    iso8601(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default(),
    )
}

fn iso8601(since_epoch: std::time::Duration) -> String {
    let secs = since_epoch.as_secs();
    let (year, month, day) = utc_ymd_from_days(secs / SECONDS_PER_DAY);
    let rem = secs % SECONDS_PER_DAY;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        since_epoch.subsec_millis()
    )
}

fn utc_ymd_from_days(days_since_epoch: u64) -> (u64, u64, u64) {
    let z = days_since_epoch.saturating_add(719_468);
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_timestamp_boundaries() {
        for secs in [0, 1_734_706_800, u64::MAX] {
            assert_eq!(timestamp_secs(U256::from(secs)), Ok(secs));
        }
        for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
            assert_eq!(
                timestamp_secs(timestamp),
                Err(crate::errors::TimestampOverflow)
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn execution_timestamp_preserves_u64(secs: u64) {
            proptest::prop_assert_eq!(timestamp_secs(U256::from(secs)), Ok(secs));
        }
    }

    #[test]
    fn iso8601_renders_utc_with_milliseconds() {
        // 2024-12-31 00:00:00 UTC = 1_735_603_200.
        let at =
            std::time::Duration::from_millis((1_735_603_200 + 23 * 3600 + 59 * 60 + 58) * 1000 + 7);
        assert_eq!(iso8601(at), "2024-12-31T23:59:58.007Z");
        assert_eq!(
            iso8601(std::time::Duration::ZERO),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(iso8601_now().len(), 24);
    }
}
