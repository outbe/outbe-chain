//! Genesis-selectable profile for the call terms a Credis seals: `PROD` (real
//! timings) and `DEV` (short timings). An unset `config_profile` byte resolves by
//! network, so only mainnet runs PROD. The call price stays a constant: the
//! requirements fix it at 1.64 times the anchor.

use outbe_primitives::chain::is_mainnet;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::constants::{CALL_NOTICE_PERIOD, CALL_THRESHOLD, CALL_WINDOW, SECS_PER_DAY};
use crate::schema::CredisContract;

/// Resolve by network: PROD on mainnet, DEV everywhere else.
pub const PROFILE_AUTO: u8 = 0;
pub const PROFILE_DEV: u8 = 1;
pub const PROFILE_PROD: u8 = 2;

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
        call_window_seconds: 3 * SECS_PER_DAY,
        call_threshold_seconds: 2 * SECS_PER_DAY,
        #[cfg(not(feature = "e2e-test"))]
        call_notice_period_seconds: 3 * SECS_PER_DAY,
        #[cfg(feature = "e2e-test")]
        call_notice_period_seconds: 600,
    };

    pub fn from_selector(selector: u8, chain_id: u64) -> Result<Self> {
        match selector {
            PROFILE_AUTO if is_mainnet(chain_id) => Ok(Self::PROD),
            PROFILE_AUTO | PROFILE_DEV => Ok(Self::DEV),
            PROFILE_PROD => Ok(Self::PROD),
            other => Err(PrecompileError::Revert(format!(
                "unknown credis profile selector: {other}"
            ))),
        }
    }
}

/// The profile this chain runs.
pub fn read(storage: &StorageHandle<'_>) -> Result<CredisParams> {
    read_from(&CredisContract::new(storage.clone()), storage.chain_id()?)
}

pub(crate) fn read_from(credis: &CredisContract<'_>, chain_id: u64) -> Result<CredisParams> {
    CredisParams::from_selector(credis.config_profile.read()?, chain_id)
}
