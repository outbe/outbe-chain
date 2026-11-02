use alloy_primitives::Address;
use outbe_primitives::addresses::{CROSSCHAIN_GOV_ADDRESS, HYPERLANE_CONTROLLER_ADDRESS};

/// Precompiles that may make the owner call a contract through `callLocal` and
/// `callRemote`. The owner itself is in the list: its vote actions run
/// HyperlaneController methods, and they call back here from the owner frame.
pub const OWNER_CALLERS: &[Address] = &[HYPERLANE_CONTROLLER_ADDRESS, CROSSCHAIN_GOV_ADDRESS];
