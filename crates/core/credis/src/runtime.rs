//! Business logic for the Credis contract.
//!
//! - positions carry **10 monthly** anadosis installments, due dates spaced by
//!   `SECONDS_PER_MONTH` from `created_at`;

use alloy_primitives::{Address, U256};

use outbe_primitives::error::{PrecompileError, Result};

use crate::errors::CredisError;
use crate::precompile::ICredis;
use crate::schema::{Anadosis, CredisContract, Position, NUMBER_OF_ANADOSIS, SECONDS_PER_MONTH};

/// Result bundle returned by [`CredisContract::make_next_anadosis`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnadosisResult {
    pub anadosis_number: u32,
    pub due_date: u64,
    pub anadosis_amount: U256,
    pub gratis_amount: U256,
    pub paid_at: u64,
    pub vault_provider: Address,
    pub asset: Address,
    pub bundle_account: Address,
}

impl CredisContract<'_> {
    /// Per-anadosis amount with remainder routed to the final anadosis so
    /// `sum(anadosis_amount[i]) == total` exactly. Anadosis
    /// 1..NUMBER_OF_ANADOSIS-1 each pay `total / NUMBER_OF_ANADOSIS`; anadosis
    /// `NUMBER_OF_ANADOSIS` pays the remainder.
    fn split_amount(total: U256, anadosis_number: u32) -> U256 {
        let n = U256::from(NUMBER_OF_ANADOSIS);
        let per = total / n;
        if anadosis_number == NUMBER_OF_ANADOSIS {
            // saturating_sub: per * (n-1) <= total for any non-negative U256.
            total.saturating_sub(per * U256::from(NUMBER_OF_ANADOSIS - 1))
        } else {
            per
        }
    }

    /// Creates a position and returns the derived
    /// `position_id = keccak256(commitment || bundle_account)`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_position(
        &mut self,
        commitment: U256,
        bundle_account: Address,
        vault_provider: Address,
        asset: Address,
        anadosis_amount: U256,
        gratis_amount: U256,
        current_time: u64,
    ) -> Result<U256> {
        if anadosis_amount.is_zero() {
            return Err(CredisError::InvalidAmount.into());
        }

        let position_id = CredisContract::position_id(commitment, bundle_account);
        if self.position_exists(position_id)? {
            return Err(CredisError::PositionAlreadyExists.into());
        }

        let position = Position {
            position_id,
            bundle_account,
            vault_provider,
            asset,
            total_anadosis_amount: anadosis_amount,
            outstanding_anadosis_amount: anadosis_amount,
            total_gratis_amount: gratis_amount,
            outstanding_gratis_amount: gratis_amount,
            next_anadosis_number: 1,
            created_at: current_time,
        };
        self.create_position_record(&position)?;

        for n in 1..=NUMBER_OF_ANADOSIS {
            let anadosis = Anadosis {
                anadosis_key: CredisContract::anadosis_key(position_id, n),
                anadosis_number: n,
                due_date: current_time + (n as u64) * SECONDS_PER_MONTH,
                anadosis_amount: Self::split_amount(anadosis_amount, n),
                gratis_amount: Self::split_amount(gratis_amount, n),
                paid_at: 0,
            };
            self.create_anadosis_record(&anadosis)?;
        }

        self.append_to_address_index(bundle_account, position_id)?;
        self.append_to_global_index(position_id)?;

        self.emit(ICredis::PositionCreated {
            positionId: position_id,
            bundleAccount: bundle_account,
            anadosisAmount: anadosis_amount,
        })?;

        Ok(position_id)
    }

    /// Advances the position by one anadosis installment.
    ///
    /// - Rejects when the position is missing.
    /// - Rejects when the position has already paid all 10 installments.
    pub fn make_next_anadosis(
        &mut self,
        position_id: U256,
        current_time: u64,
    ) -> Result<AnadosisResult> {
        let mut position = self.load_position(position_id)?;
        if position.next_anadosis_number > NUMBER_OF_ANADOSIS {
            return Err(CredisError::PositionCompleted.into());
        }

        let n = position.next_anadosis_number;
        let mut anadosis = self.load_anadosis(position_id, n)?;

        anadosis.paid_at = current_time;
        self.update_anadosis_record(&anadosis)?;

        position.outstanding_anadosis_amount = position
            .outstanding_anadosis_amount
            .saturating_sub(anadosis.anadosis_amount);
        position.outstanding_gratis_amount = position
            .outstanding_gratis_amount
            .saturating_sub(anadosis.gratis_amount);
        position.next_anadosis_number = n + 1;
        self.update_position_record(&position)?;

        self.emit(ICredis::AnadosisPaid {
            positionId: position_id,
            anadosisNumber: anadosis.anadosis_number,
            anadosisAmount: anadosis.anadosis_amount,
        })?;

        Ok(AnadosisResult {
            anadosis_number: anadosis.anadosis_number,
            due_date: anadosis.due_date,
            anadosis_amount: anadosis.anadosis_amount,
            gratis_amount: anadosis.gratis_amount,
            paid_at: anadosis.paid_at,
            vault_provider: position.vault_provider,
            asset: position.asset,
            bundle_account: position.bundle_account,
        })
    }

    /// Loads the position head record. Reverts on missing.
    pub fn get_position(&self, position_id: U256) -> Result<Position> {
        self.load_position(position_id)
    }

    /// Loads a single anadosis record, validated by both position existence and
    /// anadosis_number range.
    pub fn get_anadosis(&self, position_id: U256, anadosis_number: u32) -> Result<Anadosis> {
        validate_anadosis_number(anadosis_number)?;
        if !self.position_exists(position_id)? {
            return Err(CredisError::PositionNotFound.into());
        }
        self.load_anadosis(position_id, anadosis_number)
    }

    /// Returns the next unpaid anadosis, or `None` when complete.
    pub fn get_next_anadosis(&self, position_id: U256) -> Result<Option<Anadosis>> {
        let position = self.load_position(position_id)?;
        if position.outstanding_anadosis_amount.is_zero()
            || position.next_anadosis_number > NUMBER_OF_ANADOSIS
        {
            return Ok(None);
        }
        Ok(Some(self.load_anadosis(
            position_id,
            position.next_anadosis_number,
        )?))
    }

    /// All 10 anadosis records for a position.
    pub fn get_position_anadosis(&self, position_id: U256) -> Result<Vec<Anadosis>> {
        if !self.position_exists(position_id)? {
            return Err(CredisError::PositionNotFound.into());
        }
        let mut out = Vec::with_capacity(NUMBER_OF_ANADOSIS as usize);
        for n in 1..=NUMBER_OF_ANADOSIS {
            out.push(self.load_anadosis(position_id, n)?);
        }
        Ok(out)
    }

    /// True if any of `account`'s positions has an unpaid anadosis whose
    /// `due_date` is strictly before `current_time`.
    pub fn has_overdue_anadosis(&self, account: Address, current_time: u64) -> Result<bool> {
        let count = self.read_address_position_count(account)?;
        for i in 0..count {
            let position_id = self.read_address_position_id(account, i)?;
            let position = match self.positions.get(position_id)? {
                Some(p) => p,
                None => continue,
            };
            if position.outstanding_anadosis_amount.is_zero() {
                continue;
            }
            if position.next_anadosis_number > NUMBER_OF_ANADOSIS {
                continue;
            }
            let anadosis = self.load_anadosis(position_id, position.next_anadosis_number)?;
            if anadosis.paid_at == 0 && anadosis.due_date < current_time {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Sum of `outstanding_anadosis_amount` across all positions for `account`.
    pub fn get_outstanding_amount(&self, account: Address) -> Result<U256> {
        let count = self.read_address_position_count(account)?;
        let mut total = U256::ZERO;
        for i in 0..count {
            let position_id = self.read_address_position_id(account, i)?;
            let position = match self.positions.get(position_id)? {
                Some(p) => p,
                None => continue,
            };
            total = total
                .checked_add(position.outstanding_anadosis_amount)
                .ok_or_else(|| PrecompileError::Revert("credis outstanding sum overflow".into()))?;
        }
        Ok(total)
    }

    /// All positions for `account`, in insertion order.
    pub fn get_positions_by_address(&self, account: Address) -> Result<Vec<Position>> {
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

    /// All positions ever created, in creation order.
    pub fn get_all_positions(&self) -> Result<Vec<Position>> {
        let total = self.read_total_positions()?;
        let mut out = Vec::with_capacity(total as usize);
        for i in 0..total {
            let position_id = self.read_position_id_at(i)?;
            if let Some(position) = self.positions.get(position_id)? {
                out.push(position);
            }
        }
        Ok(out)
    }
}

/// Validates `anadosis_number` is in `[1, NUMBER_OF_ANADOSIS]`.
fn validate_anadosis_number(anadosis_number: u32) -> Result<()> {
    if anadosis_number == 0 || anadosis_number > NUMBER_OF_ANADOSIS {
        return Err(CredisError::InvalidAnadosisNumber.into());
    }
    Ok(())
}
