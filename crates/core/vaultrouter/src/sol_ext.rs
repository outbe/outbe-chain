//! Outbound sub-call ABI surfaces.
//!
//! These are the external contract interfaces the vaultrouter runtime
//! invokes via `StorageHandle::call` / `StorageHandle::staticcall`. They are
//! NOT the precompile's own inbound ABI (which lives in
//! `api.rs::IVaultRouter`).
//!
//! The Solidity original used OpenZeppelin `SafeERC20` (`forceApprove`,
//! `safeTransferFrom`). Here those become plain ERC-20 `approve` / `transferFrom`
//! calls. The runtime checks their boolean return explicitly.

use alloy_sol_types::sol;

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");

sol!("../../../contracts/precompiles/src/IVaultV2.sol");

sol!("../../../contracts/tokens/src/interfaces/IReferenceCurrency.sol");
