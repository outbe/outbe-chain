//! Read-only queries over Credis positions.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;

use crate::errors::CredisError;
use crate::schema::{CredisContract, Position};

impl CredisContract<'_> {
    /// Loads the position record. Reverts on missing.
    pub fn get_position(&self, position_id: U256) -> Result<Position> {
        self.load_position(position_id)
    }

    /// True if any of `account`'s positions is CALLED. Informational only: a
    /// pending call does not gate origination of further positions.
    pub fn has_called_position(&self, account: Address) -> Result<bool> {
        Ok(self.called_position_counts.read(&account)? > 0)
    }

    /// Sum of `principal_minor` and of `outstanding_principal_minor` across all positions for
    /// `account`, in one walk of the owner index.
    pub fn principal_and_outstanding_of(&self, account: Address) -> Result<(U256, U256)> {
        let mut principal = U256::ZERO;
        let mut outstanding = U256::ZERO;
        for position in self.get_positions_by_address(account)? {
            principal = principal
                .checked_add(position.principal_minor)
                .ok_or(CredisError::ArithmeticOverflow)?;
            outstanding = outstanding
                .checked_add(position.outstanding_principal_minor)
                .ok_or(CredisError::ArithmeticOverflow)?;
        }
        Ok((principal, outstanding))
    }

    /// How many positions `account` has ever been issued.
    pub fn position_count_of(&self, account: Address) -> Result<u32> {
        self.read_address_position_count(account)
    }

    /// `account`'s `index`-th position, in insertion order.
    pub fn position_of_address_at(&self, account: Address, index: u32) -> Result<Position> {
        if index >= self.read_address_position_count(account)? {
            return Err(CredisError::IndexOutOfBounds.into());
        }
        self.load_position(self.read_address_position_id(account, index)?)
    }

    /// The `index`-th position ever created, in creation order.
    pub fn position_at(&self, index: u64) -> Result<Position> {
        if index >= self.read_total_positions()? {
            return Err(CredisError::IndexOutOfBounds.into());
        }
        self.load_position(self.read_position_id_at(index)?)
    }

    /// All positions for `account`, in insertion order. Unbounded, so it stays
    /// internal: the ABI enumerates through `position_of_address_at` instead.
    pub(crate) fn get_positions_by_address(&self, account: Address) -> Result<Vec<Position>> {
        let count = self.read_address_position_count(account)?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let position_id = self.read_address_position_id(account, i)?;
            if let Some(position) = self.positions.get(position_id)? {
                out.push(position);
            }
        }
        Ok(out)
    }

    /// Total positions ever created, including closed positions. This value backs `totalSupply`.
    pub fn total_positions(&self) -> Result<u64> {
        self.read_total_positions()
    }

    /// Position id at global dense-index `index` (`index < total_positions()`).
    pub fn position_id_at(&self, index: u64) -> Result<U256> {
        self.read_position_id_at(index)
    }
}
