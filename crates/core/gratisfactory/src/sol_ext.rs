//! Outbound sub-call ABI surfaces.
//!
//! External contract interfaces the gratisfactory runtime invokes via
//! `StorageHandle::staticcall`. These are NOT the precompile's own inbound ABI. That ABI
//! lives in `precompile.rs::IGratisFactory`.

use alloy_sol_types::sol;

sol!("../../../contracts/tokens/src/interfaces/IReferenceCurrency.sol");

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");
sol!("../../../contracts/precompiles/src/IVaultRouter.sol");
