use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::error::Result;
use outbe_system_common::addresses::{
    GRATIS_FACTORY_ADDRESS, INTEX_FACTORY_ADDRESS, PROMIS_LIMIT_ADDRESS, STAKING_ADDRESS,
    VOTE_ADDRESS,
};
use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

use crate::{errors::PromisLimitError, precompile::IPromisLimit, schema::PromisLimitContract};

/// The cause of a limit or a load that goes back to the Promis Limit.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitSource {
    DayEmissionLimit = 11,
    MetadosisLimit = 12,
    DesisLimit = 13,
    RewardLimit = 14,
    Nod = 21,
    Intex = 22,
    Gem = 23,
    Credis = 24,
    GemFactoryPosition = 31,
    AgentReward = 32,
}

/// The cause of a burn of COEN from the one account of its module.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BurnSource {
    PriorityFees = 42,
    IntexAuctionProceeds = 43,
    ValidatorStake = 44,
    ProposalBond = 45,
    GratisMint = 47,
}

impl BurnSource {
    pub const fn account(self) -> Address {
        match self {
            Self::PriorityFees => REWARDS_ADDRESS,
            Self::IntexAuctionProceeds => INTEX_FACTORY_ADDRESS,
            Self::ValidatorStake => STAKING_ADDRESS,
            Self::ProposalBond => VOTE_ADDRESS,
            Self::GratisMint => GRATIS_FACTORY_ADDRESS,
        }
    }
}

const BASE_FEES: u8 = 41;
const BURN_ADDRESSES: u8 = 46;

pub const DEAD_ADDRESS: Address = address!("0x000000000000000000000000000000000000dEaD");

/// The cause of a take from the Promis Limit.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitTarget {
    DesisLimit = 1,
}

impl PromisLimitContract<'_> {
    pub fn promis_limit(&self) -> Result<U256> {
        self.promis_limit.read()
    }

    /// COEN units below one minor that stay on the account of `source` for its next burn.
    pub fn burn_rest(&self, source: BurnSource) -> Result<U256> {
        self.burn_rests.read(&(source as u8))
    }

    /// Base and blob fees that the EVM burned and the daily add has not taken yet.
    pub fn base_fee_count(&self) -> Result<U256> {
        self.base_fee_count.read()
    }

    pub(crate) fn set_promis_limit(&mut self, total: U256) -> Result<()> {
        self.promis_limit.write(total)
    }

    /// Burns `amount` COEN units from the account of `source` with its rest, and adds the whole
    /// minors. The part below one minor stays on the account as the new rest.
    pub fn burn(&mut self, source: BurnSource, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let guard = self.storage.checkpoint_guard()?;
        let code = source as u8;
        let total = self
            .burn_rests
            .read(&code)?
            .checked_add(amount)
            .ok_or(PromisLimitError::Overflow)?;
        self.burn_rests
            .write(&code, total % UNITS_PER_PROTOCOL_MINOR)?;
        self.burn_whole_minors(code, source.account(), total)?;
        guard.commit()
    }

    /// Counts base and blob fees that the EVM burned in a block.
    pub fn count_base_fees(&mut self, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let count = self
            .base_fee_count
            .read()?
            .checked_add(amount)
            .ok_or(PromisLimitError::Overflow)?;
        self.base_fee_count.write(count)
    }

    /// The daily step: adds the whole minors of the base fee count, and burns the whole minors
    /// on each burn address.
    pub fn settle_day_burns(&mut self) -> Result<()> {
        let count = self.base_fee_count.read()?;
        self.base_fee_count
            .write(count % UNITS_PER_PROTOCOL_MINOR)?;
        self.add_with_code(BASE_FEES, count / UNITS_PER_PROTOCOL_MINOR)?;

        let mut burned = U256::ZERO;
        for account in [Address::ZERO, DEAD_ADDRESS] {
            let minors = self.storage.balance(account)? / UNITS_PER_PROTOCOL_MINOR;
            self.storage
                .decrease_balance(account, minors * UNITS_PER_PROTOCOL_MINOR)?;
            burned += minors;
        }
        self.add_with_code(BURN_ADDRESSES, burned)
    }

    fn burn_whole_minors(&mut self, code: u8, from: Address, units: U256) -> Result<()> {
        let minors = units / UNITS_PER_PROTOCOL_MINOR;
        if minors.is_zero() {
            return Ok(());
        }
        self.storage
            .decrease_balance(from, minors * UNITS_PER_PROTOCOL_MINOR)?;
        self.add_with_code(code, minors)
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
}
