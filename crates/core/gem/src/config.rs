//! Genesis-selectable parameter profile for the Gem protocol: `PROD` (real
//! timings) and `DEV` (short timings) are fixed here. A chain picks one with the
//! `config_profile` selector byte seeded from genesis. An unset byte resolves by
//! network, so only mainnet runs PROD.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW, POSITION_VALIDITY_SECONDS,
};
use crate::errors::GemError;
use crate::schema::GemContract;

pub use outbe_primitives::profile::{PROFILE_AUTO, PROFILE_DEV, PROFILE_PROD};

/// Resolved Gem protocol parameters. All periods are in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GemParams {
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
    /// Percentage points over the entry price. See `crate::constants`.
    pub call_rate: u16,
    /// How long a Gem Factory position may still issue gems.
    pub position_validity: u64,
}

impl GemParams {
    /// Real protocol timings. This is the default on mainnet.
    pub const PROD: Self = Self {
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
        call_rate: CALL_RATE_PCT,
        position_validity: POSITION_VALIDITY_SECONDS,
    };

    /// Short timings for dev/test. `called` is day-granular, so window and
    /// threshold stay whole days. The notice and validity are real waits.
    pub const DEV: Self = Self {
        call_window_seconds: 3 * SECONDS_PER_DAY_U32,
        call_threshold_seconds: 2 * SECONDS_PER_DAY_U32,
        #[cfg(not(feature = "e2e-test"))]
        call_notice_period_seconds: 3 * SECONDS_PER_DAY_U32,
        #[cfg(feature = "e2e-test")]
        call_notice_period_seconds: 600,
        call_rate: 10,
        #[cfg(not(feature = "e2e-test"))]
        position_validity: 7 * outbe_primitives::time::SECONDS_PER_DAY,
        #[cfg(feature = "e2e-test")]
        position_validity: 900,
    };

    pub fn from_selector(selector: u8, chain_id: u64) -> Result<Self> {
        outbe_primitives::profile::select(selector, chain_id, Self::PROD, Self::DEV)
            .ok_or_else(|| GemError::UnknownProfileSelector(selector).into())
    }
}

/// Resolve the profile a chain was seeded with. Callers outside the gem crate
/// read the terms through here rather than from the constants.
pub fn read(storage: &StorageHandle<'_>) -> Result<GemParams> {
    read_from(&GemContract::new(storage.clone()), storage.chain_id()?)
}

pub(crate) fn read_from(gem: &GemContract<'_>, chain_id: u64) -> Result<GemParams> {
    GemParams::from_selector(gem.config_profile.read()?, chain_id)
}
