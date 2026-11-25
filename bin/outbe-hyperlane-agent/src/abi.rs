//! Contract ABI surface used by the Hyperlane agent.
//!
//! Every interface comes from its Solidity source under `contracts/`.

use alloy_sol_types::sol;

sol!("../../contracts/precompiles/src/IHyperlaneController.sol");
sol!("../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../contracts/crosschain/src/interfaces/IHyperlane.sol");
