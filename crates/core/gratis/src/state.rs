//! Low-level storage access for the confidential Gratis token.
//!
//! CRUD over the encrypted blob slots, the plaintext aggregates, and the
//! modify-auth replay counter. This layer reads and writes ciphertext verbatim. It
//! never decrypts. Business orchestration (building enclave requests, applying
//! the returned receipt, emitting events) lives in [`crate::runtime`]. The
//! cross-crate surface is [`crate::api`].

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use crate::schema::Gratis;

impl Gratis<'_> {
    // --- Metadata ---

    pub fn name(&self) -> &str {
        "gratis"
    }

    pub fn symbol(&self) -> &str {
        "GRATIS"
    }

    pub fn decimals(&self) -> u8 {
        6
    }

    // --- Plaintext aggregates (non-attributable) ---

    pub fn total_supply(&self) -> Result<U256> {
        self.total_supply.read()
    }

    pub fn pledged_total_supply(&self) -> Result<U256> {
        self.pledged_total_supply.read()
    }

    // --- Ciphertext reads (returned verbatim for the view-key holder to decrypt) ---

    /// Encrypted balance blob for `account` (`version(8) || AEAD-ct`). Empty if
    /// the account never held a balance.
    pub fn balance_ct_of(&self, account: Address) -> Result<Vec<u8>> {
        self.balance_ct.get_bytes(&account).read()
    }

    /// Encrypted pledged-collateral blob for `account`, in the balance blob format.
    pub fn pledged_ct_of(&self, account: Address) -> Result<Vec<u8>> {
        self.pledged_ct.get_bytes(&account).read()
    }

    /// The account's current modify-auth replay counter (the value a client must
    /// bind into its next write authorization).
    pub fn op_nonce_of(&self, account: Address) -> Result<u64> {
        self.op_nonce.read(&account)
    }

    // --- Writers (all take `&self`. Storage mutates through interior mutability) ---

    pub(crate) fn write_balance_ct(&self, account: Address, blob: &[u8]) -> Result<()> {
        self.balance_ct.get_bytes(&account).write(blob)
    }

    pub(crate) fn write_pledged_ct(&self, account: Address, blob: &[u8]) -> Result<()> {
        self.pledged_ct.get_bytes(&account).write(blob)
    }

    pub(crate) fn set_op_nonce(&self, account: Address, nonce: u64) -> Result<()> {
        self.op_nonce.write(&account, nonce)
    }

    pub(crate) fn set_total_supply(&self, value: U256) -> Result<()> {
        self.total_supply.write(value)
    }

    pub(crate) fn set_pledged_total_supply(&self, value: U256) -> Result<()> {
        self.pledged_total_supply.write(value)
    }
}
