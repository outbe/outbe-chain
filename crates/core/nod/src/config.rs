//! Genesis-selectable profile for the call terms a Nod group seals: `MAINNET` (real
//! timings), `TESTNET` (short timings) and `E2E` (a short notice). An unset
//! `config_profile` byte resolves by network, so only mainnet runs MAINNET.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::SECONDS_PER_DAY_U32;

use crate::constants::{CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW};
use crate::schema::NodContract;

pub use outbe_core_common::profile::{PROFILE_AUTO, PROFILE_E2E, PROFILE_MAINNET, PROFILE_TESTNET};

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
    pub const MAINNET: Self = Self {
        call_rate: CALL_RATE_PCT,
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
    };

    /// Short terms for dev/test. Only the timings are the same as Gem and Intex use.
    /// The scan is day-granular, so window and threshold stay whole days. The notice
    /// is a real wait.
    pub const TESTNET: Self = Self {
        call_rate: 8,
        call_window_seconds: 3 * SECONDS_PER_DAY_U32,
        call_threshold_seconds: 2 * SECONDS_PER_DAY_U32,
        call_notice_period_seconds: 3 * SECONDS_PER_DAY_U32,
    };

    /// TESTNET terms with a notice the end-to-end tests wait out in real time.
    pub const E2E: Self = Self {
        call_notice_period_seconds: 600,
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
