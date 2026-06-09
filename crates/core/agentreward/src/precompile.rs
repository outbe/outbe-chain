use alloy_primitives::{Address, U256};
#[allow(unused_imports)]
use outbe_macros::{contract_dispatch, contract_public, contract_view};
use outbe_primitives::error::Result;

use crate::schema::AgentRewardContract;

/// ABI surface for the AgentReward precompile.
///
/// Each method is annotated with `#[contract_public("<Solidity signature>")]`.
/// `#[contract_dispatch]` collects them, emits a private `sol!` interface,
/// and synthesizes `pub fn dispatch(storage, data, caller, value) -> Result<Bytes>`.
#[contract_dispatch]
impl AgentRewardContract<'_> {
    #[contract_public("getClaimableBalance(address) view returns (uint256)")]
    #[contract_view]
    fn _abi_get_claimable_balance(&mut self, account: Address) -> Result<U256> {
        self.get_claimable_reward(account)
    }

    #[contract_public("claimReward(uint256) returns (uint256)")]
    fn _abi_claim_reward(&mut self, sender: Address, amount: U256) -> Result<U256> {
        // amount = 0 means claim all (matching Cosmos behavior).
        let amount = if amount.is_zero() {
            self.get_claimable_reward(sender)?
        } else {
            amount
        };
        if amount.is_zero() {
            return Ok(U256::ZERO);
        }
        self.claim_reward(sender, amount)
    }
}
