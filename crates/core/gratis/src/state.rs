//! Public aggregate CRUD and root-bound account views. Account lookup happens
//! inside the enclave; no source account keys are used for journal storage.

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

    fn view(&self, account: Address, field: u8) -> Result<(Vec<u8>, u64)> {
        let result = crate::enclave_client::execute(
            &self.storage,
            outbe_tee::confidential::Call::GratisView { account, field },
        )?;
        match result.value {
            outbe_tee::confidential::Value::View { blob, nonce } => Ok((blob, nonce)),
            _ => Err(outbe_primitives::error::PrecompileError::Fatal(
                "invalid confidential view".into(),
            )),
        }
    }
    pub fn balance_ct_of(&self, account: Address) -> Result<Vec<u8>> {
        self.view(account, 0).map(|v| v.0)
    }
    pub fn pledged_ct_of(&self, account: Address) -> Result<Vec<u8>> {
        self.view(account, 1).map(|v| v.0)
    }
    pub fn op_nonce_of(&self, account: Address) -> Result<u64> {
        self.view(account, 2).map(|v| v.1)
    }
    pub(crate) fn set_total_supply(&self, value: U256) -> Result<()> {
        self.total_supply.write(value)
    }
    pub(crate) fn set_pledged_total_supply(&self, value: U256) -> Result<()> {
        self.pledged_total_supply.write(value)
    }
}
