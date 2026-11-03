//! Outbound sub-call ABI surfaces.
//!
//! The Hyperlane contract interfaces that the controller reads through
//! `StorageHandle::staticcall`, and the owner precompile (`ICrosschainBridge`)
//! that runs its owner calls. This is NOT the precompile's own inbound ABI
//! (`precompile::IHyperlaneController`).

use alloy_sol_types::sol;

sol!("../../../contracts/crosschain/src/interfaces/IHyperlaneCore.sol");

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ICrosschainBridge.sol"
);

sol! {
    /// Remote-router table of Hyperlane's `Router`, which the InterchainAccountRouter extends.
    interface IHyperlaneRouter {
        function routers(uint32 _domain) external view returns (bytes32);
        function enrollRemoteRouter(uint32 _domain, bytes32 _router) external;
    }
}
