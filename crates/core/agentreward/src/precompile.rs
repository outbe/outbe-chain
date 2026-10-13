use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::{ExecutionReaders, ParentBodySource};
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

alloy_sol_types::sol! {
    /// The one selector whose handler mints a Gem body and therefore needs the
    /// block's compressed-entity readers. Same signature as the macro surface below.
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IAgentRewardClaim {
        function claimReward(uint8 pool, uint256 amount) external returns (uint256);
    }
}

/// Dispatches AgentReward calls. `claimReward` issues a Gem, so it routes through the
/// block-scoped compressed-body lifecycle; every other selector is the generated
/// storage-only dispatcher.
pub fn dispatch_with_readers(
    storage: outbe_primitives::storage::StorageHandle,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    if let Ok(call) = IAgentRewardClaim::claimRewardCall::abi_decode(data) {
        outbe_primitives::dispatch::reject_value(&value)?;
        outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;
        let mut contract = AgentRewardContract::new(storage.clone());
        let claimed = contract.abi_claim_reward(
            readers.scope,
            readers.parent,
            caller,
            call.pool,
            call.amount,
        )?;
        return Ok(IAgentRewardClaim::claimRewardCall::abi_encode_returns(&claimed).into());
    }
    dispatch(storage, data, caller, value)
}

impl AgentRewardContract<'_> {
    fn abi_claim_reward(
        &mut self,
        scope: &outbe_compressed_entities::ExecutionScope,
        parent: &impl ParentBodySource,
        sender: Address,
        pool: u8,
        amount: U256,
    ) -> Result<U256> {
        let pool = RewardPool::from_abi(pool)?;
        // A zero amount claims the whole pool balance. Empty WAA and SRA pools permit no-op
        // claims.
        // CCA claims still require registration history and revert when the balance is zero.
        if pool != RewardPool::Cca
            && amount.is_zero()
            && self.get_pool_claimable_reward(pool, sender)?.is_zero()
        {
            return Ok(U256::ZERO);
        }
        self.claim_reward(scope, parent, pool, sender, amount)
    }
}

/// ABI surface for the AgentReward precompile.
///
/// Each method is annotated with `#[contract_public("<Solidity signature>")]`.
/// `#[contract_dispatch]` collects them, emits a private `sol!` interface,
/// and synthesizes `pub fn dispatch(storage, data, caller, value) -> Result<Bytes>`.
/// `claimReward` stays declared here so the ABI export is complete; its live route is
/// [`dispatch_with_readers`], which handles it before this dispatcher sees it.
#[contract_dispatch]
impl AgentRewardContract<'_> {
    #[contract_public("getClaimableBalance(address) view returns (uint256)")]
    #[contract_view]
    fn _abi_get_claimable_balance(&mut self, account: Address) -> Result<U256> {
        self.get_claimable_reward(account)
    }

    #[contract_public("getPoolClaimableBalance(address,uint8) view returns (uint256)")]
    #[contract_view]
    fn _abi_get_pool_claimable_balance(&mut self, account: Address, pool: u8) -> Result<U256> {
        self.get_pool_claimable_reward(RewardPool::from_abi(pool)?, account)
    }

    #[contract_public("claimReward(uint8,uint256) returns (uint256)")]
    fn _abi_claim_reward(&mut self, _sender: Address, _pool: u8, _amount: U256) -> Result<U256> {
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "claimReward requires the compressed-entity readers route".into(),
        ))
    }
}
