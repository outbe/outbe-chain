use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::CROSSCHAIN_BRIDGE_ADDRESS;

/// `crosschainbridge` keeps no storage of its own: it is the account. The
/// struct gives the runtime its address, its storage handle and `emit`.
#[storage_schema]
#[contract(addr = CROSSCHAIN_BRIDGE_ADDRESS)]
pub struct CrosschainBridgeContract {}
