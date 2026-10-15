//! Genesis-selectable parameter profile of the rights: `PROD` (real terms) or `DEV`
//! (short terms). Genesis seeds the selector byte into each right's contract. An unset
//! byte resolves by network, so only mainnet runs PROD. The values stay with each right.

use crate::chain::is_mainnet;

/// Resolve by network: PROD on mainnet, DEV everywhere else.
pub const PROFILE_AUTO: u8 = 0;
pub const PROFILE_DEV: u8 = 1;
pub const PROFILE_PROD: u8 = 2;

/// Picks the right's `prod` or `dev` terms for a seeded selector. `None` for a selector
/// no profile answers to; each right reports it with its own error.
pub fn select<T>(selector: u8, chain_id: u64, prod: T, dev: T) -> Option<T> {
    match selector {
        PROFILE_AUTO if is_mainnet(chain_id) => Some(prod),
        PROFILE_AUTO | PROFILE_DEV => Some(dev),
        PROFILE_PROD => Some(prod),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID};

    #[test]
    fn auto_resolves_by_network_and_explicit_selectors_win() {
        assert_eq!(select(PROFILE_AUTO, MAINNET_CHAIN_ID, 1, 2).unwrap(), 1);
        assert_eq!(select(PROFILE_AUTO, DEVNET_CHAIN_ID, 1, 2).unwrap(), 2);
        assert_eq!(select(PROFILE_DEV, MAINNET_CHAIN_ID, 1, 2).unwrap(), 2);
        assert_eq!(select(PROFILE_PROD, DEVNET_CHAIN_ID, 1, 2).unwrap(), 1);
        assert!(select(3, DEVNET_CHAIN_ID, 1, 2).is_none());
    }
}
