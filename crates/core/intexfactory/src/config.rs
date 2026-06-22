//! Genesis-selectable parameter profile for IntexFactory: `PROD` (real timings)
//! and `DEV` (short timings) are fixed here; a chain picks one via the
//! `config_profile` selector byte seeded from genesis (unset reads 0 = prod).

use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::{
    CALL_PRICE_NUM, CALL_THRESHOLD_DAYS, CALL_WINDOW_DAYS, FLOOR_PRICE_NUM,
    INTEX_CALL_PERIOD_SECONDS, MATURITY_PERIOD_SECONDS,
};
use crate::schema::IntexFactoryContract;

pub const PROFILE_PROD: u8 = 0;
pub const PROFILE_DEV: u8 = 1;

/// Resolved IntexFactory protocol parameters. Price numerators are over the
/// fixed `*_PRICE_DEN` denominators in [`crate::constants`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntexParams {
    pub maturity_period_secs: u64,
    pub call_window_days: u16,
    pub call_threshold_days: u16,
    pub intex_call_period_secs: u32,
    pub call_price_num: u64,
    pub floor_price_num: u64,
}

impl IntexParams {
    /// Real protocol timings; also the default when no profile is selected.
    pub const PROD: Self = Self {
        maturity_period_secs: MATURITY_PERIOD_SECONDS,
        call_window_days: CALL_WINDOW_DAYS,
        call_threshold_days: CALL_THRESHOLD_DAYS,
        intex_call_period_secs: INTEX_CALL_PERIOD_SECONDS,
        call_price_num: CALL_PRICE_NUM,
        floor_price_num: FLOOR_PRICE_NUM,
    };

    /// Short timings for dev/test. `called` is day-granular (daily VWAP scan),
    /// so window/threshold stay in whole days.
    pub const DEV: Self = Self {
        maturity_period_secs: 24 * 3600,
        call_window_days: 3,
        call_threshold_days: 2,
        intex_call_period_secs: 3 * 24 * 3600,
        call_price_num: 110,
        floor_price_num: 105,
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
