//! Outbound sub-call ABI surfaces.
//!
//! The Hyperlane contract interfaces that the controller calls through
//! `StorageHandle::call` / `StorageHandle::staticcall`. This is NOT the
//! precompile's own inbound ABI (`precompile::ICrosschainGov`).

use alloy_sol_types::sol;

sol!("../../../contracts/crosschain/src/interfaces/IHyperlaneCore.sol");

sol! {
    /// Remote-router table of Hyperlane's `Router`, which the InterchainAccountRouter extends.
    interface IHyperlaneRouter {
        function routers(uint32 _domain) external view returns (bytes32);
        function enrollRemoteRouter(uint32 _domain, bytes32 _router) external;
    }
}
