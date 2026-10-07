//! Retained external ABI declarations from the former asset-valued pledge path.
//!
//! The current runtime does not use these interfaces.

use alloy_sol_types::sol;

sol!("../../../contracts/tokens/src/interfaces/IReferenceCurrency.sol");

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");
sol!("../../../contracts/precompiles/src/IVaultRouter.sol");
