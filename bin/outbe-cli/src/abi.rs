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
pub const TEE_REGISTRY_ADDR: Address = address!("0x000000000000000000000000000000000000EE0A");
pub const HYPERLANE_CONTROLLER_ADDR: Address =
    address!("0x000000000000000000000000000000000000EE14");

sol!("../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../contracts/precompiles/src/ISlashIndicator.sol");
sol!("../../contracts/precompiles/src/IStaking.sol");
sol!("../../contracts/precompiles/src/IHyperlaneController.sol");
sol!("../../contracts/precompiles/src/ICycle.sol");
sol!("../../contracts/precompiles/src/IUpdate.sol");
sol!("../../contracts/precompiles/src/IVote.sol");
sol!("../../contracts/precompiles/src/ICodeRegistry.sol");
