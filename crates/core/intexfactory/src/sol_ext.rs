//! External contract ABIs invoked via `storage.call` / `staticcall`.

use alloy_sol_types::sol;

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
