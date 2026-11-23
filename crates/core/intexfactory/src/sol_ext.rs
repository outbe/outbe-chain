//! External contract ABIs invoked via `storage.call` / `staticcall`.

use alloy_sol_types::sol;

// `OriginRouter` sends are relay-float-funded: called with value 0, the router quotes and pays
// the bridge fee from its own native balance, so the precompile passes no fee/options/refund.
sol!("../../../contracts/intex/src/origin/interfaces/IOriginRouter.sol");

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");

sol!("../../../contracts/tokens/src/interfaces/IReferenceCurrency.sol");

#[allow(clippy::too_many_arguments)]
mod intex_nft1155 {
    alloy_sol_types::sol!("../../../contracts/intex/src/shared/interfaces/IIntexNFT1155.sol");
}
pub use intex_nft1155::IIntexNFT1155;

// `balanceOf` on an Intex series is inherited from ERC-1155, which the `sol!`
// path form cannot follow through `IIntexNFT1155.sol`'s `import`.
sol!("../../../contracts/tokens/src/interfaces/IERC1155.sol");
