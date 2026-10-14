//! Genesis-selectable profile for the call terms a Nod bucket seals: `PROD` (real
//! timings) and `DEV` (short timings). An unset `config_profile` byte resolves by
//! network, so only mainnet runs PROD. The floor rate stays a constant: every floor
//! derives from its entry price.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW, SECS_PER_DAY,
};
use crate::schema::NodContract;

pub use outbe_primitives::profile::{PROFILE_AUTO, PROFILE_DEV, PROFILE_PROD};

/// Resolved Nod call terms. All periods are in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodParams {
    /// Percentage points over the entry price. See `crate::constants`.
    pub call_rate: u16,
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
}

impl NodParams {
    /// Real protocol terms. The default on mainnet.
    pub const PROD: Self = Self {
        call_rate: CALL_RATE_PCT,
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
    };

    /// Short terms for dev/test, as Gem and Intex run them. The scan is day-granular,
    /// so window and threshold stay whole days. The notice is a real wait.
    pub const DEV: Self = Self {
        call_rate: 10,
        call_window_seconds: 3 * SECS_PER_DAY,
        call_threshold_seconds: 2 * SECS_PER_DAY,
        #[cfg(not(feature = "e2e-test"))]
        call_notice_period_seconds: 3 * SECS_PER_DAY,
        #[cfg(feature = "e2e-test")]
        call_notice_period_seconds: 600,
    };

    pub fn from_selector(selector: u8, chain_id: u64) -> Result<Self> {
        outbe_primitives::profile::select(selector, chain_id, Self::PROD, Self::DEV)
            .ok_or_else(|| crate::errors::NodError::UnknownProfileSelector { selector }.into())
    }
}

/// Resolve the profile a chain was seeded with.
pub fn read(storage: &StorageHandle<'_>) -> Result<NodParams> {
    read_from(&NodContract::new(storage.clone()), storage.chain_id()?)
}

pub(crate) fn read_from(nod: &NodContract<'_>, chain_id: u64) -> Result<NodParams> {
    NodParams::from_selector(nod.config_profile.read()?, chain_id)
}
