//! Genesis-selectable parameter profile for the Gem protocol: `PROD` (real
//! timings) and `DEV` (short timings) are fixed here; a chain picks one via the
//! `config_profile` selector byte seeded from genesis (unset reads 0 = prod).

use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::constants::{
    CALL_NOTICE_PERIOD, CALL_RATE, CALL_THRESHOLD, CALL_WINDOW, FLOOR_RATE,
    POSITION_VALIDITY_SECONDS,
};
use crate::schema::GemContract;

pub const PROFILE_PROD: u8 = 0;
pub const PROFILE_DEV: u8 = 1;

/// Resolved Gem protocol parameters; all periods are seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GemParams {
    pub call_window: u32,
    pub call_threshold: u32,
    pub call_notice_period: u32,
    /// Percentage points over the entry price; see `crate::constants`.
    pub call_rate: u16,
    pub floor_rate: u16,
    /// How long a parked-Intex position may still issue gems.
    pub position_validity: u64,
}

impl GemParams {
    /// Real protocol timings; also the default when no profile is selected.
    pub const PROD: Self = Self {
        call_window: CALL_WINDOW,
        call_threshold: CALL_THRESHOLD,
        call_notice_period: CALL_NOTICE_PERIOD,
        call_rate: CALL_RATE,
        floor_rate: FLOOR_RATE,
        position_validity: POSITION_VALIDITY_SECONDS,
    };

    /// Short timings for dev/test. `called` is day-granular, so window and
    /// threshold stay whole days; the notice and validity are real waits.
    pub const DEV: Self = Self {
        call_window: 3 * 24 * 3600,
        call_threshold: 2 * 24 * 3600,
        #[cfg(not(feature = "e2e-test"))]
        call_notice_period: 3 * 24 * 3600,
        #[cfg(feature = "e2e-test")]
        call_notice_period: 600,
        call_rate: 10,
        floor_rate: 5,
        #[cfg(not(feature = "e2e-test"))]
        position_validity: 7 * 24 * 3600,
        #[cfg(feature = "e2e-test")]
        position_validity: 900,
    };

    pub fn from_selector(selector: u8) -> Result<Self> {
        match selector {
            PROFILE_PROD => Ok(Self::PROD),
            PROFILE_DEV => Ok(Self::DEV),
            other => Err(PrecompileError::Revert(format!(
                "unknown gem profile selector: {other}"
            ))),
        }
    }
}

/// Resolve the profile a chain was seeded with. Callers outside the gem crate
/// read the terms through here rather than from the constants.
pub fn read(storage: &StorageHandle<'_>) -> Result<GemParams> {
    read_from(&GemContract::new(storage.clone()))
}

pub(crate) fn read_from(gem: &GemContract<'_>) -> Result<GemParams> {
    GemParams::from_selector(gem.config_profile.read()?)
}
