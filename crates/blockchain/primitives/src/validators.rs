//! Validator set data types.
//!
//! These are pure data types shared between consensus internals (DKG manager,
//! application handler, tests) and the engine layer (`outbe-engine`), which
//! owns storage I/O. The engine module `outbe_engine::validators` reads
//! ValidatorSet from Reth state and constructs it for the consensus stack.

use alloy_primitives::Address;
use commonware_cryptography::bls12381;
use commonware_p2p::Address as CommonwareAddress;

/// Loaded validator set.
#[derive(Debug, Clone)]
pub struct ValidatorSet {
    /// Ordered list of BLS MinPk public keys (determines participant indices).
    pub public_keys: Vec<bls12381::PublicKey>,
    /// Corresponding Ethereum addresses (same order as public_keys).
    pub addresses: Vec<Address>,
    /// P2P addresses for each validator (same order).
    pub p2p_addresses: Vec<ValidatorP2pAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorP2pAddress {
    /// No registry address exists; static bootstrap may fill this gap.
    Missing,
    /// Registry contained an invalid address; exclude this peer and do not
    /// substitute static/bootstrap target addresses.
    Invalid,
    /// Valid decoded Commonware target address.
    Known(CommonwareAddress),
}
