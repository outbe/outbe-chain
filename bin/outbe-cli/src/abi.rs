//! ABI definitions for outbe precompile contracts.
//!
//! Every interface is generated from its canonical Solidity source under
//! `contracts/precompiles/src/`.

use alloy_primitives::{address, Address};
use alloy_sol_types::sol;

pub use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1 as ITeeRegistry;

pub use outbe_system_common::addresses::VOTE_ADDRESS;

// Precompile contract addresses
pub const VALIDATOR_SET_ADDR: Address = address!("0x000000000000000000000000000000000000EE00");
pub const SLASH_INDICATOR_ADDR: Address = address!("0x000000000000000000000000000000000000EE01");
pub const STAKING_ADDR: Address = address!("0x000000000000000000000000000000000000EE02");
pub const TRIBUTE_ADDR: Address = address!("0x0000000000000000000000000000000000001101");
pub const TRIBUTE_FACTORY_ADDR: Address = address!("0x0000000000000000000000000000000000001100");
pub const TEE_REGISTRY_ADDR: Address = address!("0x000000000000000000000000000000000000EE0A");

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
