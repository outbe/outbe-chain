//! Outbound sub-call ABI surfaces.
//!
//! Interfaces invoked by the gemfactory runtime via `StorageHandle::call`.
//! Not the precompile's own inbound ABI (which lives in
//! `precompile.rs::IGemFactory`).

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IERC20 {
        function transferFrom(address from, address to, uint256 amount)
            external returns (bool);
        function approve(address spender, uint256 amount)
            external returns (bool);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexNFT1155 {
        function parkIntex(address holder, uint32 seriesId, uint256 amount) external returns (uint256);
    }
}

// `IReferenceCurrency` is generated directly from the canonical Solidity
// interface (single source of truth) rather than hand-mirrored here.
sol!(
    #![sol(alloy_sol_types = alloy_sol_types)]
    "../../../contracts/tokens/src/interfaces/IReferenceCurrency.sol"
);
