use alloy_primitives::Address;
use outbe_system_common::addresses::{
    DESIS_ADDRESS, HYPERLANE_CONTROLLER_ADDRESS, INTEX_FACTORY_ADDRESS, UPDATE_ADDRESS,
};

/// Precompiles that may make the owner call a contract through `callLocal` and
/// `callRemote`:
/// - HyperlaneController, for its ISM rotation, its bootstrap and its
///   Hyperlane chain votes;
/// - Update, for an approved `upgradeContracts` proposal;
/// - Desis and IntexFactory, for the Intex messages that their `flush()` sends.
pub const ALLOWED_CALLERS: &[Address] = &[
    HYPERLANE_CONTROLLER_ADDRESS,
    UPDATE_ADDRESS,
    DESIS_ADDRESS,
    INTEX_FACTORY_ADDRESS,
];

/// Destination gas that `callRemote` pays for. The IGP default of 50000 is
/// less than an Interchain Account call uses.
pub const DEFAULT_REMOTE_GAS_LIMIT: u64 = 200_000;
