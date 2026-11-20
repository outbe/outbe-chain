//! Genesis-selectable parameter profile of the rights: `MAINNET` (real terms), `TESTNET`
//! (short terms) or `E2E` (the shortest waits, for the end-to-end tests). Genesis seeds the
//! selector byte into each right's contract. An unset byte resolves by network, so only
//! mainnet runs MAINNET. The values stay with each right.

use outbe_primitives::chain::is_mainnet;

/// Resolve by network: MAINNET on mainnet, TESTNET everywhere else.
pub const PROFILE_AUTO: u8 = 0;
pub const PROFILE_MAINNET: u8 = 1;
pub const PROFILE_TESTNET: u8 = 2;
pub const PROFILE_E2E: u8 = 3;

/// Picks the right's terms for a seeded selector. `None` for a selector no profile
/// answers to; each right reports it with its own error.
pub fn select<T>(selector: u8, chain_id: u64, mainnet: T, testnet: T, e2e: T) -> Option<T> {
    match selector {
        PROFILE_AUTO if is_mainnet(chain_id) => Some(mainnet),
        PROFILE_AUTO | PROFILE_TESTNET => Some(testnet),
        PROFILE_MAINNET => Some(mainnet),
        PROFILE_E2E => Some(e2e),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID};

    #[test]
    fn auto_resolves_by_network_and_explicit_selectors_win() {
        assert_eq!(select(PROFILE_AUTO, MAINNET_CHAIN_ID, 1, 2, 3).unwrap(), 1);
        assert_eq!(select(PROFILE_AUTO, DEVNET_CHAIN_ID, 1, 2, 3).unwrap(), 2);
        assert_eq!(
            select(PROFILE_TESTNET, MAINNET_CHAIN_ID, 1, 2, 3).unwrap(),
            2
        );
        assert_eq!(
            select(PROFILE_MAINNET, DEVNET_CHAIN_ID, 1, 2, 3).unwrap(),
            1
        );
        assert_eq!(select(PROFILE_E2E, DEVNET_CHAIN_ID, 1, 2, 3).unwrap(), 3);
        assert!(select(4, DEVNET_CHAIN_ID, 1, 2, 3).is_none());
    }
}
