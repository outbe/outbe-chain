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

/// ABI surface for the AgentReward precompile.
///
/// Each method is annotated with `#[contract_public("<Solidity signature>")]`.
/// `#[contract_dispatch]` collects them, emits a private `sol!` interface,
/// and synthesizes `pub fn dispatch(storage, data, caller, value) -> Result<Bytes>`.
/// The dispatcher reads and writes the Gem through EVM state.
#[contract_dispatch]
impl AgentRewardContract<'_> {
    #[contract_public("getAgentRewardRate(uint8,uint32) view returns (uint256,uint64,uint256)")]
    #[contract_view]
    fn _abi_get_agent_reward_rate(
        &mut self,
        pool: u8,
        emission_day: u32,
    ) -> Result<__AgentRewardContractAbi::getAgentRewardRateReturn> {
        let (rate, deadline, unclaimed) =
            self.agent_reward_rate(RewardPool::from_abi(pool)?, emission_day)?;
        Ok(__AgentRewardContractAbi::getAgentRewardRateReturn {
            _0: rate,
            _1: deadline,
            _2: unclaimed,
        })
    }

    #[contract_public("getAgentReward(address,uint8,uint32) view returns (uint256,uint256)")]
    #[contract_view]
    fn _abi_get_agent_reward(
        &mut self,
        account: Address,
        pool: u8,
        emission_day: u32,
    ) -> Result<__AgentRewardContractAbi::getAgentRewardReturn> {
        let (weight, amount) =
            self.agent_reward(RewardPool::from_abi(pool)?, emission_day, account)?;
        Ok(__AgentRewardContractAbi::getAgentRewardReturn {
            _0: weight,
            _1: amount,
        })
    }

    #[contract_public("expireAgentReward(address,uint8,uint32[]) returns (uint256)")]
    fn _abi_expire_agent_reward(
        &mut self,
        _sender: Address,
        account: Address,
        pool: u8,
        emission_days: Vec<u32>,
    ) -> Result<U256> {
        self.expire_agent_reward(RewardPool::from_abi(pool)?, account, &emission_days)
    }

    #[contract_public("redeemAgentReward(uint8,uint32[],uint16,uint16) returns (uint256)")]
    fn _abi_redeem_agent_reward(
        &mut self,
        sender: Address,
        pool: u8,
        emission_days: Vec<u32>,
        issuance_currency: u16,
        reference_currency: u16,
    ) -> Result<U256> {
        self.redeem_agent_reward(
            RewardPool::from_abi(pool)?,
            sender,
            &emission_days,
            (issuance_currency, reference_currency),
        )
    }
}
