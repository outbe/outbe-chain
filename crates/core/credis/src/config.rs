//! Profile for the call terms a Credis seals: `PROD` (real timings) and `DEV`
//! (short timings), picked by the `config_profile` byte. Genesis leaves it unset,
//! which resolves by network, so only mainnet runs PROD. The call price stays a
//! constant: the requirements fix it at 1.64 times the anchor.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

use crate::constants::{CALL_NOTICE_PERIOD, CALL_THRESHOLD, CALL_WINDOW};
use crate::errors::CredisError;
use crate::schema::CredisContract;

pub use outbe_primitives::profile::{PROFILE_AUTO, PROFILE_DEV, PROFILE_PROD};

/// Resolved Credis call terms, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredisParams {
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
}

impl CredisParams {
    /// Real protocol terms. The default on mainnet.
    pub const PROD: Self = Self {
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
    };

    /// Short terms for dev and test networks, as the other rights run them.
    pub const DEV: Self = Self {
        call_window_seconds: 3 * SECONDS_PER_DAY_U32,
        call_threshold_seconds: 2 * SECONDS_PER_DAY_U32,
        #[cfg(not(feature = "e2e-test"))]
        call_notice_period_seconds: 3 * SECONDS_PER_DAY_U32,
        #[cfg(feature = "e2e-test")]
        call_notice_period_seconds: 600,
    };

    pub fn from_selector(selector: u8, chain_id: u64) -> Result<Self> {
        outbe_primitives::profile::select(selector, chain_id, Self::PROD, Self::DEV)
            .ok_or_else(|| CredisError::UnknownProfileSelector(selector).into())
    }
}

/// The profile this chain runs.
pub fn read(storage: &StorageHandle<'_>) -> Result<CredisParams> {
    read_from(&CredisContract::new(storage.clone()), storage.chain_id()?)
}

pub(crate) fn read_from(credis: &CredisContract<'_>, chain_id: u64) -> Result<CredisParams> {
    CredisParams::from_selector(credis.config_profile.read()?, chain_id)
}
