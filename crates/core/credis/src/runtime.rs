//! Business logic for the Credis contract.
//!
//! - positions carry **10 monthly** anadosis installments, due dates spaced by
//!   `SECONDS_PER_MONTH` from `created_at`;

use alloy_primitives::{Address, U256};

use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::units::CREDIS_INTEREST_RATE_SCALE;

use crate::errors::CredisError;
use crate::precompile::ICredis;
use crate::schema::{Anadosis, CredisContract, Position, NUMBER_OF_ANADOSIS, SECONDS_PER_MONTH};

/// Result bundle returned by [`CredisContract::pay_anadosis`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnadosisPayment {
    /// Stablecoin actually applied — `min(requested_amount, outstanding)`.
    pub total_paid_amount: U256,
    /// Collateral freed by this payment, summed over the installments it touched.
    pub gratis_released: U256,
    pub asset: Address,
    pub smart_account: Address,
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

    /// Total repayable debt for a position: `principal × (1 + rate × TERM / 12)`,
    /// where `rate` is the annualized currency rate with scale `1e6` and `TERM`
    /// is [`NUMBER_OF_ANADOSIS`] months. A zero rate yields `total == principal`
    /// (the pre-currency-rate behavior).
    fn total_debt(principal: U256, currency_rate: U256) -> Result<U256> {
        let term = U256::from(NUMBER_OF_ANADOSIS);
        // Preserve the existing staged rounding: floor the term rate first,
        // then floor the scaled principal multiplication.
        let term_rate = currency_rate
            .checked_mul(term)
            .ok_or_else(|| -> PrecompileError { CredisError::InvalidAmount.into() })?
            / U256::from(12u64);
        let multiplier = CREDIS_INTEREST_RATE_SCALE
            .checked_add(term_rate)
            .ok_or_else(|| -> PrecompileError { CredisError::InvalidAmount.into() })?;
        let scaled = principal
            .checked_mul(multiplier)
            .ok_or_else(|| -> PrecompileError { CredisError::InvalidAmount.into() })?;
        Ok(scaled / CREDIS_INTEREST_RATE_SCALE)
    }

    /// Creates a position and returns the derived
    /// `position_id = keccak256(commitment || smart_account)`.
    ///
    /// `credis_principal` is the disbursed loan amount; the repayment schedule is
    /// sized to `total_debt(principal, currency_rate)` and split across the
    /// [`NUMBER_OF_ANADOSIS`] monthly installments. `currency_rate` (scale
    /// `1e6`) and `issuance_currency` (ISO 4217) are pinned at issuance.
    #[allow(clippy::too_many_arguments)]
    pub fn create_position(
        &mut self,
        handle_id: U256,
        smart_account: Address,
        eoa_ct: Vec<u8>,
        asset: Address,
        issuance_currency: u16,
        currency_rate: U256,
        credis_principal: U256,
        rate: U256,
        gratis_amount: U256,
        current_time: u64,
    ) -> Result<U256> {
        if credis_principal.is_zero() {
            return Err(CredisError::InvalidAmount.into());
        }

        let position_id = CredisContract::position_id(handle_id, smart_account);
        if self.position_exists(position_id)? {
            return Err(CredisError::PositionAlreadyExists.into());
        }

        let total_debt = Self::total_debt(credis_principal, currency_rate)?;

        self.create_position_record(&Position {
            position_id,
            smart_account,
            asset,
            total_anadosis_amount: total_debt,
            outstanding_anadosis_amount: total_debt,
            total_gratis_amount: gratis_amount,
            outstanding_gratis_amount: gratis_amount,
            next_anadosis_number: 1,
            created_at: current_time,
            credis_principal,
            entry_price_minor: rate,
            currency_rate,
            issuance_currency,
            eoa_ct,
        })?;

        for n in 1..=NUMBER_OF_ANADOSIS {
            let anadosis_amount = Self::split_amount(total_debt, n);
            self.create_anadosis_record(&Anadosis {
                anadosis_key: CredisContract::anadosis_key(position_id, n),
                anadosis_number: n,
                due_date: current_time + (n as u64) * SECONDS_PER_MONTH,
                anadosis_amount,
                gratis_amount: Self::split_amount(gratis_amount, n),
                paid_at: 0,
                unpaid_amount: anadosis_amount,
            })?;
        }

        self.append_to_address_index(smart_account, position_id)?;
        self.append_to_global_index(position_id)?;

        self.emit(ICredis::PositionCreated {
            positionId: position_id,
            smartAccount: smart_account,
            anadosisAmount: total_debt,
        })?;

        Ok(position_id)
    }

    /// Collateral of `anadosis` still locked, given its current `unpaid_amount`.
    /// Floor division: it reaches exactly zero when the installment is settled, so
    /// the deltas taken across successive partial payments sum to `gratis_amount`
    /// with no dust left behind and nothing over-released.
    fn locked_gratis(anadosis: &Anadosis) -> U256 {
        if anadosis.anadosis_amount.is_zero() {
            return U256::ZERO;
        }
        anadosis.gratis_amount * anadosis.unpaid_amount / anadosis.anadosis_amount
    }

    /// Applies `amount` to the position's unpaid anadosis schedule, oldest first.
    ///
    /// Each installment is covered in full while the budget lasts; the last one
    /// reached may be covered partially, keeping the remainder on its
    /// `unpaid_amount` with `paid_at` still 0 (so it stays overdue-eligible). Only
    /// what the schedule actually needs is consumed — the returned
    /// `total_paid_amount` is `min(amount, outstanding)` and is what the caller
    /// should pull from the payer.
    ///
    /// - Rejects when the position is missing.
    /// - Rejects when the position has already paid all 10 installments.
    pub fn pay_anadosis(
        &mut self,
        position_id: U256,
        amount: U256,
        current_time: u64,
    ) -> Result<AnadosisPayment> {
        let mut position = self.load_position(position_id)?;
        if position.next_anadosis_number > NUMBER_OF_ANADOSIS {
            return Err(CredisError::PositionCompleted.into());
        }

        let mut remaining = amount;
        let mut total_paid = U256::ZERO;
        let mut gratis_released = U256::ZERO;

        while position.next_anadosis_number <= NUMBER_OF_ANADOSIS {
            let n = position.next_anadosis_number;
            let mut anadosis = self.load_anadosis(position_id, n)?;

            let pay = remaining.min(anadosis.unpaid_amount);
            // Budget exhausted before this installment could be touched at all.
            // (A zero-amount installment still settles here — it needs nothing.)
            if pay.is_zero() && !anadosis.unpaid_amount.is_zero() {
                break;
            }

            let locked_before = Self::locked_gratis(&anadosis);
            anadosis.unpaid_amount -= pay;
            let released = locked_before - Self::locked_gratis(&anadosis);

            let settled = anadosis.unpaid_amount.is_zero();
            if settled {
                anadosis.paid_at = current_time;
                position.next_anadosis_number = n + 1;
            }
            self.update_anadosis_record(&anadosis)?;

            remaining -= pay;
            total_paid += pay;
            gratis_released += released;

            self.emit(ICredis::AnadosisPaid {
                positionId: position_id,
                anadosisNumber: n,
                anadosisAmount: pay,
            })?;

            if !settled {
                break;
            }
        }

        position.outstanding_anadosis_amount = position
            .outstanding_anadosis_amount
            .saturating_sub(total_paid);
        position.outstanding_gratis_amount = position
            .outstanding_gratis_amount
            .saturating_sub(gratis_released);
        self.update_position_record(&position)?;

        Ok(AnadosisPayment {
            total_paid_amount: total_paid,
            gratis_released,
            asset: position.asset,
            smart_account: position.smart_account,
        })
    }

    /// Timestamp at which `position`'s credis period expires — the due date of the
    /// final anadosis installment (`created_at + NUMBER_OF_ANADOSIS × SECONDS_PER_MONTH`).
    pub fn expires_at(position: &Position) -> u64 {
        position
            .created_at
            .saturating_add((NUMBER_OF_ANADOSIS as u64).saturating_mul(SECONDS_PER_MONTH))
    }

    /// Closes an expired position after its collateral has been burned by the caller:
    /// zeroes the outstanding balances, marks the schedule complete (so it is skipped
    /// by future sweeps and overdue checks), and emits `CollateralBurned`. Returns the
    /// pre-close snapshot so the caller can read `outstanding_gratis_amount` /
    /// `eoa_ct` / `smart_account`.
    pub fn expire_position(&mut self, position_id: U256) -> Result<Position> {
        let mut position = self.load_position(position_id)?;
        let snapshot = position.clone();
        position.outstanding_anadosis_amount = U256::ZERO;
        position.outstanding_gratis_amount = U256::ZERO;
        position.next_anadosis_number = NUMBER_OF_ANADOSIS + 1;
        self.update_position_record(&position)?;
        self.emit(ICredis::CollateralBurned {
            positionId: position_id,
            smartAccount: snapshot.smart_account,
            gratisBurned: snapshot.outstanding_gratis_amount,
        })?;
        Ok(snapshot)
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

    /// Total positions ever created (length of the global dense index). Used by the
    /// begin-block expiry sweep to bound its cursor.
    pub fn total_positions(&self) -> Result<u64> {
        self.read_total_positions()
    }

    /// Position id at global dense-index `index` (`index < total_positions()`).
    pub fn position_id_at(&self, index: u64) -> Result<U256> {
        self.read_position_id_at(index)
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
