//! ABI definitions for outbe precompile contracts.
//!
//! Every interface is generated from its canonical Solidity source under
//! `contracts/precompiles/src/`.

use alloy_primitives::{address, Address};
use alloy_sol_types::sol;

pub use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1 as ITeeRegistry;

pub use outbe_primitives::addresses::VOTE_ADDRESS;

// Precompile contract addresses
pub const VALIDATOR_SET_ADDR: Address = address!("0x000000000000000000000000000000000000EE00");
pub const SLASH_INDICATOR_ADDR: Address = address!("0x000000000000000000000000000000000000EE01");
pub const STAKING_ADDR: Address = address!("0x000000000000000000000000000000000000EE02");
// The Rewards precompile (EE03) exposes no callable methods. Validator emission is
// paid in gems. Thus only the address-pin test references it.
#[cfg(test)]
pub const REWARDS_ADDR: Address = address!("0x000000000000000000000000000000000000EE03");
pub const TRIBUTE_ADDR: Address = address!("0x0000000000000000000000000000000000001101");
pub const TRIBUTE_FACTORY_ADDR: Address = address!("0x0000000000000000000000000000000000001100");
pub const TEE_REGISTRY_ADDR: Address = address!("0x000000000000000000000000000000000000EE0A");
#[cfg(test)]
pub const NOD_ADDR: Address = address!("0x0000000000000000000000000000000000001006");
#[cfg(test)]
pub const CYCLE_ADDR: Address = address!("0x0000000000000000000000000000000000001010");
#[cfg(test)]
pub const OUTBE_SYSTEM_TX_ADDR: Address = address!("0xff00000000000000000000000000000000000001");

sol!("../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../contracts/precompiles/src/ISlashIndicator.sol");
sol!("../../contracts/precompiles/src/IStaking.sol");
// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod tribute_abi {
    alloy_sol_types::sol!("../../contracts/precompiles/src/ITribute.sol");
}
pub use tribute_abi::ITribute;
sol!("../../contracts/precompiles/src/ITributeFactory.sol");
sol!("../../contracts/precompiles/src/ICycle.sol");
sol!("../../contracts/precompiles/src/INod.sol");
sol!("../../contracts/precompiles/src/IOracle.sol");
sol!("../../contracts/precompiles/src/IUpdate.sol");
sol!("../../contracts/precompiles/src/IVote.sol");
sol!("../../contracts/precompiles/src/IStablecoinPolicyRegistry.sol");
sol!("../../contracts/precompiles/src/ICodeRegistry.sol");

pub const ORACLE_ADDR: Address = address!("0x000000000000000000000000000000000000EE05");

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    // TC-026: Pin precompile addresses
    #[test]
    fn test_precompile_addresses_pinned() {
        assert_eq!(
            VALIDATOR_SET_ADDR,
            address!("0x000000000000000000000000000000000000EE00")
        );
        assert_eq!(
            SLASH_INDICATOR_ADDR,
            address!("0x000000000000000000000000000000000000EE01")
        );
        assert_eq!(
            STAKING_ADDR,
            address!("0x000000000000000000000000000000000000EE02")
        );
        assert_eq!(
            REWARDS_ADDR,
            address!("0x000000000000000000000000000000000000EE03")
        );
        assert_eq!(
            TRIBUTE_ADDR,
            address!("0x0000000000000000000000000000000000001101")
        );
        assert_eq!(
            NOD_ADDR,
            address!("0x0000000000000000000000000000000000001006")
        );
        assert_eq!(
            CYCLE_ADDR,
            address!("0x0000000000000000000000000000000000001010")
        );
        assert_eq!(
            OUTBE_SYSTEM_TX_ADDR,
            address!("0xff00000000000000000000000000000000000001")
        );
    }

    // TC-026: Pin representative function selectors
    #[test]
    fn test_function_selectors_pinned() {
        use alloy_sol_types::SolCall;

        let stake = IStaking::stakeCall::SELECTOR;
        let topup = IStaking::topupCall::SELECTOR;
        let submit_double = ISlashIndicator::submitDoubleProposalEvidenceCall::SELECTOR;
        let stake_selector = selector("stake(address,uint256,bytes,bytes32,bytes,uint8,bytes)");
        let topup_selector = selector("topup(address,uint256)");

        assert_eq!(stake.len(), 4);
        assert_eq!(topup.len(), 4);
        assert_eq!(submit_double.len(), 4);
        assert_eq!(stake, stake_selector);
        assert_eq!(topup, topup_selector);
        assert_ne!(stake, selector("stake(address,uint256)"));
    }

    fn selector(signature: &str) -> [u8; 4] {
        use alloy_primitives::keccak256;

        let hash = keccak256(signature.as_bytes());
        let mut selector = [0_u8; 4];
        selector.copy_from_slice(&hash[..4]);
        selector
    }

    // Pin representative event topic hashes used by receipt/log tooling.
    #[test]
    fn test_event_topics_pinned() {
        use alloy_primitives::keccak256;
        use alloy_sol_types::SolEvent;

        assert_eq!(
            IValidatorSet::ValidatorRegistered::SIGNATURE_HASH,
            keccak256("ValidatorRegistered(address,uint64)")
        );
        assert_eq!(
            ISlashIndicator::ProposerFelony::SIGNATURE_HASH,
            keccak256("ProposerFelony(address,uint64,uint64)")
        );
        assert_eq!(
            INod::NodGroupCalled::SIGNATURE_HASH,
            keccak256("NodGroupCalled(bytes32,uint64,uint64)")
        );
        assert_eq!(
            ICycle::CycleTriggerExecuted::SIGNATURE_HASH,
            keccak256("CycleTriggerExecuted(uint32,uint64,uint64,uint64)")
        );
    }
}
