//! Genesis-selectable parameter profile for IntexFactory: `PROD` (real timings)
//! and `DEV` (short timings) are fixed here. A chain picks one via the
//! `config_profile` selector byte seeded from genesis. An unset byte resolves by
//! network, so only mainnet runs PROD.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18_U128;

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_RATE, CALL_THRESHOLD, CALL_WINDOW, COMMIT_BOND_MINOR, FLOOR_RATE,
};
use crate::schema::IntexFactoryContract;

pub use outbe_primitives::profile::{PROFILE_AUTO, PROFILE_DEV, PROFILE_PROD};

/// Resolved IntexFactory protocol parameters. Periods are seconds. Rates are
/// percentage points over [`outbe_primitives::markup::PRICE_RATE_DEN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntexParams {
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
    pub call_rate: u16,
    pub floor_rate: u16,
    /// Commit-entry bond on the target-chain auction in 18-decimal WCOEN units.
    pub commit_bond_minor: u128,
}

impl IntexParams {
    /// Real protocol timings. This is the default on mainnet.
    pub const PROD: Self = Self {
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
        call_rate: CALL_RATE,
        floor_rate: FLOOR_RATE,
        commit_bond_minor: COMMIT_BOND_MINOR,
    };

    /// Short timings for dev/test. `called` is day-granular (daily VWAP scan),
    /// so window/threshold stay whole multiples of a day. The bond drops to
    /// 100 wCOEN so test bidders do not have to mint 100M per commit.
    pub const DEV: Self = Self {
        call_window_seconds: 3 * 24 * 3600,
        call_threshold_seconds: 2 * 24 * 3600,
        #[cfg(not(feature = "e2e-test"))]
        call_notice_period_seconds: 3 * 24 * 3600,
        #[cfg(feature = "e2e-test")]
        call_notice_period_seconds: 600,
        call_rate: 10,
        floor_rate: 5,
        commit_bond_minor: 100 * SCALE_1E18_U128,
    };

    pub fn from_selector(selector: u8, chain_id: u64) -> Result<Self> {
        outbe_primitives::profile::select(selector, chain_id, "intex", Self::PROD, Self::DEV)
    }
}

pub fn read(storage: &StorageHandle<'_>) -> Result<IntexParams> {
    read_from(
        &IntexFactoryContract::new(storage.clone()),
        storage.chain_id()?,
    )
}

pub(crate) fn read_from(factory: &IntexFactoryContract<'_>, chain_id: u64) -> Result<IntexParams> {
    IntexParams::from_selector(factory.config_profile.read()?, chain_id)
}
