use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::{errors::PromisLimitError, schema::PromisLimitContract};

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

    pub(crate) fn set_promis_limit(&mut self, total: U256) -> Result<()> {
        self.promis_limit.write(total)
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
