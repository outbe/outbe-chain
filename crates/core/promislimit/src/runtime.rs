use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::error::Result;
use outbe_system_common::addresses::{PROMIS_LIMIT_ADDRESS, SYSTEM_ADDRESS};
use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

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

impl BurnSource {
    /// Returns `true` when the EVM already burned the `COEN` units before
    /// `PromisLimitContract::burn_to_promis_limit` is called.
    pub const fn is_already_burned(self) -> bool {
        matches!(self, Self::BaseFee | Self::BlobFee)
    }
}

/// The cause of a limit or a load that goes back to the Promis Limit.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitSource {
    DayEmissionLimit = 11,
    MetadosisLimit = 12,
    LysisLimit = 13,
    DesisLimit = 14,
    RewardLimit = 15,
    Nod = 21,
    Intex = 22,
    Gem = 23,
    Credis = 24,
    GemFactoryPosition = 31,
    AgentReward = 32,
}

/// The cause of a take from the Promis Limit.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitTarget {
    DesisLimit = 1,
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

    /// Routes `amount_minor` `COEN` units into the Promis Limit, credits whole Promis Limit
    /// minors to `promis_limit`, and stores the remainder in `rest_minor`.
    ///
    /// Already-burned fee sources (`BurnSource::BaseFee | BurnSource::BlobFee`) may only be
    /// passed by the block executor (`escrow == SYSTEM_ADDRESS`) and do not decrease any
    /// account balance. All other sources must supply a non-system `escrow` account from
    /// which `amount_minor` is burned.
    pub fn burn_to_promis_limit(
        &mut self,
        escrow: Address,
        amount_minor: U256,
        source: BurnSource,
    ) -> Result<U256> {
        if source.is_already_burned() != (escrow == SYSTEM_ADDRESS) {
            return Err(PromisLimitError::UnauthorizedBurnCaller {
                caller: escrow,
                burn_source: source as u8,
            }
            .into());
        }
        if amount_minor.is_zero() {
            return Ok(U256::ZERO);
        }

        let guard = self.storage.checkpoint_guard()?;
        let prev_rest = self.rest_minor.read()?;
        let total_minor = prev_rest
            .checked_add(amount_minor)
            .ok_or(PromisLimitError::Overflow)?;
        let credited_minor = total_minor / UNITS_PER_PROTOCOL_MINOR;
        let new_rest = total_minor % UNITS_PER_PROTOCOL_MINOR;

        if !credited_minor.is_zero() {
            self.checked_credit(credited_minor)?;
        }
        if !source.is_already_burned() {
            self.storage.decrease_balance(escrow, amount_minor)?;
        }
        self.rest_minor.write(new_rest)?;
        // End-of-block already-burned fee settlement runs outside any transaction receipt;
        // its event schema and transport carrier are owned by the Events review (§3.1, §5.2).
        if !source.is_already_burned() {
            self.storage.emit_event(
                PROMIS_LIMIT_ADDRESS,
                IPromisLimit::NativeBurnedToPromisLimit {
                    source: source as u8,
                    burnedUnit: amount_minor,
                }
                .encode_log_data(),
            )?;
        }
        guard.commit()?;

        Ok(credited_minor)
    }

    pub fn add(&mut self, source: LimitSource, amount_minor: U256) -> Result<()> {
        self.add_with_code(source as u8, amount_minor)
    }

    pub(crate) fn add_with_code(&mut self, source: u8, amount_minor: U256) -> Result<()> {
        if amount_minor.is_zero() {
            return Ok(());
        }
        let after = self
            .promis_limit()?
            .checked_add(amount_minor)
            .ok_or(PromisLimitError::Overflow)?;
        self.set_promis_limit(after)?;
        self.storage.emit_event(
            PROMIS_LIMIT_ADDRESS,
            IPromisLimit::PromisLimitAdded {
                source,
                amountMinor: amount_minor,
            }
            .encode_log_data(),
        )
    }

    /// Takes exactly `amount_minor`. Returns `false` and changes nothing when the Promis Limit
    /// holds less.
    pub fn subtract(&mut self, target: LimitTarget, amount_minor: U256) -> Result<bool> {
        if amount_minor.is_zero() {
            return Ok(true);
        }
        let Some(after) = self.promis_limit()?.checked_sub(amount_minor) else {
            return Ok(false);
        };
        self.set_promis_limit(after)?;
        self.storage.emit_event(
            PROMIS_LIMIT_ADDRESS,
            IPromisLimit::PromisLimitSubtracted {
                target: target as u8,
                amountMinor: amount_minor,
            }
            .encode_log_data(),
        )?;
        Ok(true)
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
