//! Profile for the call terms a Credis seals and the oldest policy rate it accepts:
//! `MAINNET` (real timings), `TESTNET` (short timings) and `E2E` (a short notice), picked
//! by the `config_profile` byte.
//! Genesis leaves it unset, which resolves by network, so only mainnet runs MAINNET.
//! The call rate is part of the profile. MAINNET keeps the call price at 1.64 times
//! the anchor.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::SECONDS_PER_DAY_U32;

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW, MAX_POLICY_RATE_AGE,
};
use crate::errors::CredisError;
use crate::schema::CredisContract;

pub use outbe_core_common::profile::{PROFILE_AUTO, PROFILE_E2E, PROFILE_MAINNET, PROFILE_TESTNET};

/// Resolved Credis call terms. All periods are in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredisParams {
    /// Percentage points over the call anchor. See `crate::constants`.
    pub call_rate: u16,
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
    pub max_policy_rate_age_seconds: u32,
}

impl CredisParams {
    /// Real protocol terms. The default on mainnet.
    pub const MAINNET: Self = Self {
        call_rate: CALL_RATE_PCT,
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
        max_policy_rate_age_seconds: MAX_POLICY_RATE_AGE,
    };

    /// Short terms for dev and test networks, as the other rights run them.
    pub const TESTNET: Self = Self {
        call_rate: 2,
        call_window_seconds: 3 * SECONDS_PER_DAY_U32,
        call_threshold_seconds: 2 * SECONDS_PER_DAY_U32,
        call_notice_period_seconds: 3 * SECONDS_PER_DAY_U32,
        max_policy_rate_age_seconds: MAX_POLICY_RATE_AGE,
    };

    /// TESTNET terms with a notice the end-to-end tests wait out in real time.
    pub const E2E: Self = Self {
        call_notice_period_seconds: 1_800,
        ..Self::TESTNET
    };

    pub fn from_selector(selector: u8, chain_id: u64) -> Result<Self> {
        outbe_core_common::profile::select(
            selector,
            chain_id,
            Self::MAINNET,
            Self::TESTNET,
            Self::E2E,
        )
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
