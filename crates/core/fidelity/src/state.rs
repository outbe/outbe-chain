//! Low-level storage access for the Fidelity module.
//!
//! CRUD over the encrypted cohort blob and the plaintext
//! `first_qualified_start` anchor. Ciphertext is read/written verbatim - this
//! layer never decrypts. Building enclave requests, applying the returned
//! receipt, and RCFI/league orchestration live in [`crate::runtime`]; the
//! cross-crate surface is [`crate::api`].

use outbe_primitives::error::Result;

use crate::schema::FidelityContract;

impl FidelityContract<'_> {
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
}
