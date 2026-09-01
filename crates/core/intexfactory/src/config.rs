//! Genesis-selectable parameter profile for IntexFactory: `PROD` (real timings)
//! and `DEV` (short timings) are fixed here; a chain picks one via the
//! `config_profile` selector byte seeded from genesis (unset reads 0 = prod).

use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::units::SCALE_1E18_U128;

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_RATE, CALL_THRESHOLD, CALL_WINDOW, COMMIT_BOND_MINOR, FLOOR_RATE,
    QUALIFICATION_PERIOD,
};
use crate::schema::IntexFactoryContract;

pub const PROFILE_PROD: u8 = 0;
pub const PROFILE_DEV: u8 = 1;

/// Resolved IntexFactory protocol parameters. Periods are seconds; rates are
/// percentage points over [`crate::constants::PRICE_RATE_DEN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntexParams {
    pub qualification_period: u32,
    pub call_window: u32,
    pub call_threshold: u32,
    pub call_notice_period: u32,
    pub call_rate: u16,
    pub floor_rate: u16,
    /// Commit-entry bond on the target-chain auction in 18-decimal WCOEN units.
    pub commit_bond_minor: u128,
}

impl IntexParams {
    /// Real protocol timings; also the default when no profile is selected.
    pub const PROD: Self = Self {
        qualification_period: QUALIFICATION_PERIOD,
        call_window: CALL_WINDOW,
        call_threshold: CALL_THRESHOLD,
        call_notice_period: CALL_NOTICE_PERIOD,
        call_rate: CALL_RATE,
        floor_rate: FLOOR_RATE,
        commit_bond_minor: COMMIT_BOND_MINOR,
    };

    /// Short timings for dev/test. `called` is day-granular (daily VWAP scan),
    /// so window/threshold stay whole multiples of a day. The bond drops to
    /// 100 wCOEN so test bidders are not forced to mint 100M per commit.
    pub const DEV: Self = Self {
        #[cfg(not(feature = "e2e-test"))]
        qualification_period: 24 * 3600,
        // An e2e run cannot spend a day waiting, and jumping one crashes the
        // metadosis day machine. The sweep's decision is the subject, not the wait.
        #[cfg(feature = "e2e-test")]
        qualification_period: 120,
        call_window: 3 * 24 * 3600,
        call_threshold: 2 * 24 * 3600,
        call_notice_period: 3 * 24 * 3600,
        call_rate: 10,
        floor_rate: 5,
        commit_bond_minor: 100 * SCALE_1E18_U128,
    };

    pub fn from_selector(selector: u8) -> Result<Self> {
        match selector {
            PROFILE_PROD => Ok(Self::PROD),
            PROFILE_DEV => Ok(Self::DEV),
            other => Err(PrecompileError::Revert(format!(
                "unknown intex profile selector: {other}"
            ))),
        }
    }
}

pub(crate) fn read(factory: &IntexFactoryContract<'_>) -> Result<IntexParams> {
    IntexParams::from_selector(factory.config_profile.read()?)
}
