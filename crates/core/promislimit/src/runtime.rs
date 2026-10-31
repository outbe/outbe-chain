use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::PROMIS_LIMIT_ADDRESS, error::Result, units::UNITS_PER_PROTOCOL_MINOR,
};

use crate::{errors::PromisLimitError, precompile::IPromisLimit, schema::PromisLimitContract};

/// Identifies the protocol path that burns `COEN` units into the Promis Limit.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BurnSource {
    BaseFee = 0,
    BlobFee = 1,
    Slash = 2,
    ProposalBond = 3,
    BlockFee = 4,
    Proceeds = 5,
    LysisUnused = 6,
    NodForfeit = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromisLimitCredit {
    pub before: U256,
    pub credited: U256,
    pub after: U256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromisLimitTake {
    pub before: U256,
    pub taken: U256,
    pub after: U256,
}

impl PromisLimitContract<'_> {
    pub fn promis_limit(&self) -> Result<U256> {
        self.promis_limit.read()
    }

    pub fn rest_minor(&self) -> Result<U256> {
        self.rest_minor.read()
    }

    pub(crate) fn set_promis_limit(&mut self, total: U256) -> Result<()> {
        self.promis_limit.write(total)
    }

    /// Burns `amount_minor` `COEN` units from `escrow`, credits whole Promis Limit minors
    /// to `promis_limit`, and stores the remainder in `rest_minor`.
    pub fn burn_to_promis_limit(
        &mut self,
        escrow: Address,
        amount_minor: U256,
        source: BurnSource,
    ) -> Result<U256> {
        if amount_minor.is_zero() {
            return Ok(U256::ZERO);
        }

        let guard = self.storage.checkpoint_guard();
        let prev_rest = self.rest_minor.read()?;
        let total_unit = prev_rest
            .checked_add(amount_minor)
            .ok_or(PromisLimitError::Overflow)?;
        let credited_units = total_unit / UNITS_PER_PROTOCOL_MINOR;
        let new_rest = total_unit % UNITS_PER_PROTOCOL_MINOR;

        if !credited_units.is_zero() {
            self.checked_credit(credited_units)?;
        }
        self.storage.decrease_balance(escrow, amount_minor)?;
        self.rest_minor.write(new_rest)?;
        self.storage.emit_event(
            PROMIS_LIMIT_ADDRESS,
            IPromisLimit::NativeBurnedToPromisLimit {
                source: source as u8,
                burnedUnit: amount_minor,
            }
            .encode_log_data(),
        )?;
        guard.commit();

        Ok(credited_units)
    }

    pub fn checked_credit(&mut self, amount: U256) -> Result<PromisLimitCredit> {
        let before = self.promis_limit()?;
        let after = before
            .checked_add(amount)
            .ok_or(PromisLimitError::Overflow)?;
        self.set_promis_limit(after)?;
        Ok(PromisLimitCredit {
            before,
            credited: amount,
            after,
        })
    }

    /// Take at most `amount`, returning what was actually taken. The Promis Limit serves what it
    /// holds, so a request above the balance is a partial take rather than an error.
    pub fn checked_take_up_to(&mut self, amount: U256) -> Result<PromisLimitTake> {
        let before = self.promis_limit()?;
        let taken = amount.min(before);
        let after = before
            .checked_sub(taken)
            .ok_or(PromisLimitError::Underflow)?;
        self.set_promis_limit(after)?;
        Ok(PromisLimitTake {
            before,
            taken,
            after,
        })
    }

    /// Take exactly `amount`, or nothing when the Promis Limit holds less.
    pub fn checked_take(&mut self, amount: U256) -> Result<Option<PromisLimitTake>> {
        let before = self.promis_limit()?;
        let Some(after) = before.checked_sub(amount) else {
            return Ok(None);
        };
        self.set_promis_limit(after)?;
        Ok(Some(PromisLimitTake {
            before,
            taken: amount,
            after,
        }))
    }
}
