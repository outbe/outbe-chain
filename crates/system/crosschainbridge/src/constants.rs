use alloy_primitives::Address;
use outbe_system_common::addresses::{HYPERLANE_CONTROLLER_ADDRESS, UPDATE_ADDRESS};

/// Precompiles that may make the owner call a contract through `callLocal` and
/// `callRemote`:
/// - HyperlaneController, for its ISM rotation, its bootstrap and its
///   Hyperlane chain votes;
/// - Update, for an approved `upgradeContracts` proposal.
pub const ALLOWED_CALLERS: &[Address] = &[HYPERLANE_CONTROLLER_ADDRESS, UPDATE_ADDRESS];

/// Gas limit of a `callRemote` message. The owner pays the IGP fee for this
/// gas. The gas limit does not limit the gas of the delivery.
pub const DEFAULT_REMOTE_GAS_LIMIT: u64 = 200_000;
