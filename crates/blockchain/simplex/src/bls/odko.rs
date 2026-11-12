//! Canonical public boundary outcome codec.
pub use outbe_primitives::proof::odko::*;

use alloy_primitives::{Bytes, B256};
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::Output,
    primitives::{sharing::Sharing, variant::MinSig},
};

pub fn encode_outcome(
    epoch: Epoch,
    output: &Output<MinSig, bls12381::PublicKey>,
    is_full_dkg: bool,
) -> Bytes {
    OdkoOutcome {
        epoch,
        is_full_dkg,
        output: output.clone(),
    }
    .encode()
}

pub fn public_polynomial_hash(polynomial: &Sharing<MinSig>) -> B256 {
    alloy_primitives::keccak256(commonware_codec::Encode::encode(polynomial))
}
