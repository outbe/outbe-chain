//! Store account ciphertext and replay counters. Keep pledged supply separate.

use crate::schema::Gratis;
use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

pub(crate) struct GratisAccount<'ledger, 'storage> {
    ledger: &'ledger Gratis<'storage>,
    owner: Address,
}

pub(crate) fn account<'ledger, 'storage>(
    ledger: &'ledger Gratis<'storage>,
    owner: Address,
) -> GratisAccount<'ledger, 'storage> {
    GratisAccount { ledger, owner }
}

impl GratisAccount<'_, '_> {
    /// Return the stored ciphertext. An unused account has an empty blob.
    pub(crate) fn balance_ct(&self) -> Result<Vec<u8>> {
        self.ledger.balance_ct.get_bytes(&self.owner).read()
    }

    pub(crate) fn op_nonce(&self) -> Result<u64> {
        self.ledger.op_nonce.read(&self.owner)
    }

    pub(crate) fn write_balance_ct(&self, blob: &[u8]) -> Result<()> {
        self.ledger.balance_ct.get_bytes(&self.owner).write(blob)
    }

    pub(crate) fn set_op_nonce(&self, nonce: u64) -> Result<()> {
        self.ledger.op_nonce.write(&self.owner, nonce)
    }
}

pub(crate) fn pledged_total_supply(ledger: &Gratis<'_>) -> Result<U256> {
    ledger.pledged_total_supply.read()
}

pub(crate) fn set_pledged_total_supply(ledger: &Gratis<'_>, value: U256) -> Result<()> {
    ledger.pledged_total_supply.write(value)
}
