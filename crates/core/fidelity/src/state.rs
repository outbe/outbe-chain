//! Low-level storage access for the Fidelity module.
//!
//! CRUD over the encrypted cohort blob and the plaintext
//! `first_qualified_start` anchor. This layer reads and writes ciphertext
//! verbatim. It never decrypts. Building enclave requests, applying the returned
//! receipt, and RCFI/league orchestration live in [`crate::runtime`]. The
//! cross-crate surface is [`crate::api`].

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::schema::FidelityContract;

impl FidelityContract<'_> {
    /// Encrypted cohort blob for `account` (`version(8) || FID2 || binding(32) || AEAD-ct`); empty if the
    /// account has never held a cohort.
    pub fn cohorts_ct_of(&self, account: Address) -> Result<Vec<u8>> {
        self.cohorts_ct.get_bytes(&account).read()
    }

    pub(crate) fn write_cohorts_ct(&self, account: Address, blob: &[u8]) -> Result<()> {
        self.cohorts_ct.get_bytes(&account).write(blob)
    }

    /// Earliest qualified_start across all accounts (0 = none qualified yet).
    pub fn first_qualified_start(&self) -> Result<u64> {
        self.first_qualified_start.read()
    }

    /// Set the global anchor to `timestamp` only if it is still unset (set-once,
    /// preserving the chain-wide-minimum invariant under monotonic timestamps).
    pub(crate) fn init_first_qualified_start(&self, timestamp: u64) -> Result<()> {
        if self.first_qualified_start.read()? == 0 {
            self.first_qualified_start.write(timestamp)?;
        }
        Ok(())
    }

    /// Finalized `max_fidelity` from the previous WWD (`WWD - 1`).
    pub fn max_fidelity_prev(&self) -> Result<U256> {
        self.max_fidelity_prev.read()
    }

    /// Explicitly set `max_fidelity_prev`.
    pub fn set_max_fidelity_prev(&self, value: U256) -> Result<()> {
        self.max_fidelity_prev.write(value)
    }

    /// Fixed `max_fidelity` locked for `wwd`, or `None` if not yet locked.
    pub fn wwd_max_fidelity_of(&self, wwd: WorldwideDay) -> Result<Option<U256>> {
        let raw = self.wwd_max_fidelity.get(&wwd).read()?;
        if raw.is_zero() {
            return Ok(None);
        }
        Ok(Some(raw - U256::from(1u8)))
    }

    pub(crate) fn lock_wwd_max_fidelity(&self, wwd: WorldwideDay, value: U256) -> Result<()> {
        self.wwd_max_fidelity
            .get(&wwd)
            .write(value.saturating_add(U256::from(1u8)))
    }

    /// Running maximum of observed user RCFI during `wwd`.
    pub fn wwd_running_max_fidelity_of(&self, wwd: WorldwideDay) -> Result<U256> {
        self.wwd_running_max_fidelity.get(&wwd).read()
    }

    /// Record an observed user RCFI during `wwd` into `wwd_running_max_fidelity`.
    pub fn record_wwd_fidelity(&self, wwd: WorldwideDay, user_rcfi: U256) -> Result<U256> {
        let current = self.wwd_running_max_fidelity.get(&wwd).read()?;
        let updated = current.max(user_rcfi);
        if updated != current {
            self.wwd_running_max_fidelity.get(&wwd).write(updated)?;
        }
        Ok(updated)
    }
}
