//! Signer-side fixtures for consensus vote evidence.
//!
//! These helpers use Commonware encoders for signer inputs. They do not call
//! the evidence verifier helpers. The tests can detect verifier framing errors
//! independently. Other crates use them through the `test-utils` feature.

use blst::min_pk::{PublicKey, SecretKey};
use commonware_codec::{varint::UInt, Write as _};
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::ordered::Set;

/// BLS signature domain separation tag of the Simplex signer.
pub const POP_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

/// The BLS MinPk keypair whose key material is 32 copies of `seed`.
pub fn keypair(seed: u8) -> Result<(SecretKey, PublicKey), blst::BLST_ERROR> {
    let sk = SecretKey::key_gen(&[seed; 32], &[])?;
    let pk = sk.sk_to_pk();
    Ok((sk, pk))
}

/// Proposal bytes: `varint(epoch) || varint(view) || varint(parent) || digest`.
pub fn proposal(epoch: u64, view: u64, parent: u64, digest: [u8; 32]) -> Vec<u8> {
    let mut buf = Vec::new();
    for value in [epoch, view, parent] {
        UInt(value).write(&mut buf);
    }
    buf.extend_from_slice(&digest);
    buf
}

/// Nullify payload bytes: `varint(epoch) || varint(view)`.
pub fn nullify_payload(epoch: u64, view: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    UInt(epoch).write(&mut buf);
    UInt(view).write(&mut buf);
    buf
}

/// Evidence block `pubkey || signature || payload`. The signature covers
/// `union_unique(namespace, payload)` under `dst`.
pub fn signed_evidence(
    sk: &SecretKey,
    pk: &PublicKey,
    namespace: &[u8],
    payload: &[u8],
    dst: &[u8],
) -> Vec<u8> {
    let signed = commonware_utils::union_unique(namespace, payload);
    let sig = sk.sign(&signed, dst, &[]);
    let mut data = Vec::new();
    data.extend_from_slice(&pk.to_bytes());
    data.extend_from_slice(&sig.to_bytes());
    data.extend_from_slice(payload);
    data
}

/// Consensus public keys of the fixed test committee, in seed order 1..=4.
pub fn committee_public_keys() -> Vec<bls12381::PublicKey> {
    (1u64..=4)
        .map(|seed| bls12381::PrivateKey::from_seed(seed).public_key())
        .collect()
}

/// The fixed test committee. The vote namespaces bind this set, so the
/// signing side and the evidence verifier must use the same committee.
pub fn committee() -> Set<bls12381::PublicKey> {
    Set::from_iter_dedup(committee_public_keys())
}
