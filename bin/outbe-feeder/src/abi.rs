//! Precompile ABI surface used by the feeder.
//!
//! Generated from the canonical Solidity sources so vote calldata and read
//! decoding always match the selectors the node dispatches.

use alloy_sol_types::sol;

sol!("../../contracts/precompiles/src/IOracle.sol");

sol!("../../contracts/precompiles/src/IValidatorSet.sol");
