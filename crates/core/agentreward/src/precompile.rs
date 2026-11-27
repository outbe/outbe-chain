use alloy_primitives::{Address, U256};
#[allow(unused_imports)]
use outbe_macros::{contract_dispatch, contract_public, contract_view};
use outbe_primitives::error::Result;

use crate::schema::{AgentRewardContract, RewardPool};

alloy_sol_types::sol!(
    #[sol(all_derives)]
    "../../../contracts/precompiles/src/IAgentReward.sol"
);

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

impl AgentRewardContract<'_> {
    fn abi_redeem_agent_reward(
        &mut self,
        sender: Address,
        pool: u8,
        amount: U256,
        currencies: (u16, u16),
    ) -> Result<U256> {
        let pool = RewardPool::from_abi(pool)?;
        // A zero amount claims the whole pool balance. Empty WAA, SRA and validator pools
        // permit no-op claims.
        // CCA claims still require registration history and revert when the balance is zero.
        if pool != RewardPool::Cca
            && amount.is_zero()
            && self.get_agent_reward(pool, sender)?.is_zero()
        {
            return Ok(U256::ZERO);
        }
        self.redeem_agent_reward(pool, sender, amount, currencies)
    }
}

/// ABI surface for the AgentReward precompile.
///
/// Each method is annotated with `#[contract_public("<Solidity signature>")]`.
/// `#[contract_dispatch]` collects them, emits a private `sol!` interface,
/// and synthesizes `pub fn dispatch(storage, data, caller, value) -> Result<Bytes>`.
/// The dispatcher reads and writes the Gem through EVM state.
#[contract_dispatch]
impl AgentRewardContract<'_> {
    #[contract_public("agentRewardTotal(address) view returns (uint256)")]
    #[contract_view]
    fn _abi_agent_reward_total(&mut self, account: Address) -> Result<U256> {
        self.agent_reward_total(account)
    }

    #[contract_public("getAgentReward(address,uint8) view returns (uint256)")]
    #[contract_view]
    fn _abi_get_agent_reward(&mut self, account: Address, pool: u8) -> Result<U256> {
        self.get_agent_reward(RewardPool::from_abi(pool)?, account)
    }

    #[contract_public("agentRewardDeadline(address,uint8) view returns (uint64)")]
    #[contract_view]
    fn _abi_agent_reward_deadline(&mut self, account: Address, pool: u8) -> Result<u64> {
        self.agent_reward_deadline(RewardPool::from_abi(pool)?, account)
    }

    #[contract_public("poolAgentRewardTotal(uint8) view returns (uint256)")]
    #[contract_view]
    fn _abi_pool_agent_reward_total(&mut self, pool: u8) -> Result<U256> {
        self.pool_agent_reward_total(RewardPool::from_abi(pool)?)
    }

    #[contract_public("poolPendingAgentRewardTotal(uint8) view returns (uint256)")]
    #[contract_view]
    fn _abi_pool_pending_agent_reward_total(&mut self, pool: u8) -> Result<U256> {
        self.pool_pending_agent_reward_total(RewardPool::from_abi(pool)?)
    }

    #[contract_public("expireAgentReward(address,uint8) returns (uint256)")]
    fn _abi_expire_agent_reward(
        &mut self,
        _sender: Address,
        account: Address,
        pool: u8,
    ) -> Result<U256> {
        self.expire_agent_reward(RewardPool::from_abi(pool)?, account)
    }

    #[contract_public("redeemAgentReward(uint8,uint256,uint16,uint16) returns (uint256)")]
    fn _abi_redeem_agent_reward(
        &mut self,
        sender: Address,
        pool: u8,
        amount: U256,
        issuance_currency: u16,
        reference_currency: u16,
    ) -> Result<U256> {
        self.abi_redeem_agent_reward(
            sender,
            pool,
            amount,
            (issuance_currency, reference_currency),
        )
    }
}
