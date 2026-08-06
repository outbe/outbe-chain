//! Genesis-selectable parameter profile for IntexFactory: `PROD` (real timings)
//! and `DEV` (short timings) are fixed here; a chain picks one via the
//! `config_profile` selector byte seeded from genesis (unset reads 0 = prod).

use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_PRICE_NUM, CALL_THRESHOLD, CALL_WINDOW, COMMIT_BOND_MINOR,
    FLOOR_PRICE_NUM, QUALIFICATION_PERIOD,
};
use crate::schema::IntexFactoryContract;

pub const PROFILE_PROD: u8 = 0;
pub const PROFILE_DEV: u8 = 1;

/// Resolved IntexFactory protocol parameters. Price numerators are over the
/// fixed `*_PRICE_DEN` denominators in [`crate::constants`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntexParams {
    pub qualification_period: u32,
    pub call_window: u32,
    pub call_threshold: u32,
    pub call_notice_period: u32,
    pub call_price_num: u64,
    pub floor_price_num: u64,
    /// Commit-entry bond on the target-chain auction (payment-token 18-dec minor units).
    pub commit_bond_minor: u128,
}

impl IntexParams {
    /// Real protocol timings; also the default when no profile is selected.
    pub const PROD: Self = Self {
        qualification_period: QUALIFICATION_PERIOD,
        call_window: CALL_WINDOW,
        call_threshold: CALL_THRESHOLD,
        call_notice_period: CALL_NOTICE_PERIOD,
        call_price_num: CALL_PRICE_NUM,
        floor_price_num: FLOOR_PRICE_NUM,
        commit_bond_minor: COMMIT_BOND_MINOR,
    };

    /// Short timings for dev/test. `called` is day-granular (daily VWAP scan),
    /// so window/threshold stay whole multiples of a day. The bond drops to
    /// 100 wCOEN so test bidders are not forced to mint 100M per commit.
    pub const DEV: Self = Self {
        qualification_period: 24 * 3600,
        call_window: 3 * 24 * 3600,
        call_threshold: 2 * 24 * 3600,
        call_notice_period: 3 * 24 * 3600,
        call_price_num: 110,
        floor_price_num: 105,
        commit_bond_minor: 100 * 10u128.pow(18),
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
