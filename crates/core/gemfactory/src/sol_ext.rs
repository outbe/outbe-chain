//! Outbound sub-call ABI surfaces.
//!
//! Interfaces invoked by the gemfactory runtime via `StorageHandle::call`.
//! Not the precompile's own inbound ABI (which lives in
//! `precompile.rs::IGemFactory`). `IVaultProvider` matches the canonical
//! interface in `outbe-vault/src/interfaces/IVaultProvider.sol` (same as
//! credisfactory).

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVaultProvider {
        function assetAt(uint256 index) external view returns (address asset);
        function depositLiquidity(
            address asset,
            uint256 assetsAmount
        ) external returns (uint256 sharesAmount);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IERC20 {
        function transferFrom(address from, address to, uint256 amount)
            external returns (bool);
        function approve(address spender, uint256 amount)
            external returns (bool);
    }
}
