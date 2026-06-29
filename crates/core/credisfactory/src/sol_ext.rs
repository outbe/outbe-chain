//! Outbound sub-call ABI surfaces.
//!
//! These are the external contract interfaces the credisfactory runtime
//! invokes via `StorageHandle::call`. They are NOT the precompile's own
//! inbound ABI (which lives in `precompile.rs::ICredisFactory`).
//!
//! The vaultprovider is called in-process via `outbe_vaultprovider::api`, so it
//! needs no ABI interface here.

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IERC20 {
        function transferFrom(address from, address to, uint256 amount)
            external returns (bool);
        function approve(address spender, uint256 amount)
            external returns (bool);
    }
}
