//! Validator set data types.
//!
//! These are pure data types shared between consensus internals (DKG manager,
//! application handler, tests) and the engine layer (`outbe-engine`), which
//! owns storage I/O. The engine module `outbe_engine::validators` reads
//! ValidatorSet from Reth state and constructs it for the consensus stack.

use alloy_primitives::Address;
use commonware_cryptography::bls12381;
use commonware_p2p::Address as CommonwareAddress;

/// BLS MinPk domain separation tag for ValidatorSet registration proofs.
pub const VALIDATOR_REGISTRATION_DST: &[u8] =
    b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_outbe_REGISTER";

/// Canonical message signed by a validator's individual BLS key when registering.
///
/// The fixed-width big-endian chain ID prevents a proof produced for one Outbe
/// chain from being replayed on another chain while preserving the existing
/// address binding and registration DST.
pub fn validator_registration_message(chain_id: u64, validator: Address) -> [u8; 28] {
    let mut message = [0_u8; 28];
    message[..8].copy_from_slice(&chain_id.to_be_bytes());
    message[8..].copy_from_slice(validator.as_slice());
    message
}

/// Consensus cap for canonical TEE-expiry exclusions carried by one DKG
/// boundary. This equals the protocol validator cap and is enforced before any
/// artifact is constructed or decoded.
pub const MAX_TEE_EXPIRED_TARGET_EXCLUSIONS: usize = 256;

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
