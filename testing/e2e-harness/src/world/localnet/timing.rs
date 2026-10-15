//! Scenario start options and UTC/worldwide-day clock calculations.

use std::time::{SystemTime, UNIX_EPOCH};

/// Test-provided knobs for a localnet start. The **enclave mode** is NOT here.
/// It is an environment decision read from [`Config::tee_mode`](crate::internal::config::Config::tee_mode).
/// Only per-scenario parameters live on this struct.
#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    /// Expected governance voting window in the already materialized genesis.
    /// Startup rejects a mismatch. This option never changes chain parameters.
    pub voting_window: Option<u64>,
    /// Signed wall-clock offset used only by debug-node day-boundary E2E.
    pub unix_time_offset_secs: Option<i64>,
    /// The scenario already shifted `genesis.json` before deriving another
    /// immutable manifest binding from it. Nodes still receive the clock
    /// offset, but the common start path must not shift genesis a second time.
    pub genesis_timestamp_pre_shifted: bool,
    /// Short-lived pool policy used only by the dedicated eviction scenario.
    pub is_txpool_eviction_profile: bool,
}

impl StartOpts {
    /// A start expecting the given genesis governance voting window.
    pub fn with_voting_window(window: u64) -> Self {
        Self {
            voting_window: Some(window),
            unix_time_offset_secs: None,
            genesis_timestamp_pre_shifted: false,
            is_txpool_eviction_profile: false,
        }
    }

    pub fn with_txpool_eviction_profile(window: u64) -> Self {
        Self {
            voting_window: Some(window),
            is_txpool_eviction_profile: true,
            ..Self::default()
        }
    }

    pub fn near_next_utc_day(window: u64, now_secs: u64) -> Self {
        const BOUNDARY_LEAD_SECS: u64 = 120;
        Self::near_next_utc_day_with_lead(window, now_secs, BOUNDARY_LEAD_SECS)
    }

    /// Position the debug-node clock an exact number of seconds before the next
    /// UTC day boundary. Capacity scenarios need a wider lead than the ordinary
    /// one-item lifecycle because every input must first be finalized through
    /// the public transaction path.
    pub fn near_next_utc_day_with_lead(
        window: u64,
        now_secs: u64,
        boundary_lead_secs: u64,
    ) -> Self {
        use outbe_primitives::time::SECONDS_PER_DAY;
        let next_day = now_secs - (now_secs % SECONDS_PER_DAY) + SECONDS_PER_DAY;
        let target = next_day.saturating_sub(boundary_lead_secs);
        Self {
            voting_window: Some(window),
            unix_time_offset_secs: Some(target as i64 - now_secs as i64),
            genesis_timestamp_pre_shifted: false,
            is_txpool_eviction_profile: false,
        }
    }
}

/// The chain's current worldwide-day key (`YYYYMMDD`), matching how
/// `bootstrap-testnet.sh` seeds genesis: `date_key(now + UTC_PLUS_14_OFFSET)`
/// (lib.sh:33-34). Pure-Rust civil-date conversion so no `date(1)` shell-out.
pub fn worldwide_day() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ymd_utc(now + 50_400)
}

/// `YYYYMMDD` for a UTC epoch second (Howard Hinnant's `civil_from_days`).
pub(crate) fn ymd_utc(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}
