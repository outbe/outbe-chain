//! Outbound sub-call ABI surfaces.
//!
//! These are the external contract interfaces the vaultprovider runtime
//! invokes via `StorageHandle::call` / `StorageHandle::staticcall`. They are
//! NOT the precompile's own inbound ABI (which lives in
//! `precompile.rs::IVaultProvider`).
//!
//! The Solidity original used OpenZeppelin `SafeERC20` (`forceApprove`,
//! `safeTransferFrom`); here those become plain ERC-20 `approve` / `transferFrom`
//! calls whose boolean return is checked explicitly in the runtime.

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVaultV2 {
        function asset() external view returns (address);
        function deposit(uint256 assets, address onBehalf) external returns (uint256 shares);
        function previewWithdraw(uint256 assets) external view returns (uint256 shares);
        function withdraw(uint256 assets, address receiver, address onBehalf)
            external returns (uint256 shares);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface ITokenBundle {
        function topUp(address sender, address token, uint256 amount) external;
    }
}
