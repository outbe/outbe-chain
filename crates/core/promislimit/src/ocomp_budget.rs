use alloy_primitives::U256;
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::PromisLimitContract;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CarryOverCredit {
    pub before: U256,
    pub credited: U256,
    pub after: U256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CarryOverTake {
    pub before: U256,
    pub taken: U256,
    pub after: U256,
}

impl PromisLimitContract<'_> {
    pub fn checked_add_carry_over(&mut self, amount: U256) -> Result<CarryOverCredit> {
        let before = self.get_total_unallocated()?;
        let after = before.checked_add(amount).ok_or_else(|| {
            PrecompileError::Revert("promislimit total_unallocated overflow".into())
        })?;
        self.set_total_unallocated(after)?;
        Ok(CarryOverCredit {
            before,
            credited: amount,
            after,
        })
    }

    pub fn checked_take_carry_over(&mut self) -> Result<CarryOverTake> {
        let before = self.get_total_unallocated()?;
        self.set_total_unallocated(U256::ZERO)?;
        Ok(CarryOverTake {
            before,
            taken: before,
            after: U256::ZERO,
        })
    }
}
