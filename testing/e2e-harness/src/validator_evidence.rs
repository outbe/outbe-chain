//! Canonical conflicting-notarize evidence constructed from live localnet keys.
//!
//! The helper deliberately uses the same committee-bound namespace and wire
//! format as SlashIndicator. This keeps the lifecycle scenarios black-box: the
//! only state-changing action is the public evidence transaction.

use std::fs;

use blst::min_pk::SecretKey;
use bytes::Bytes as CodecBytes;
use commonware_codec::{DecodeExt, Encode as _};
use commonware_cryptography::bls12381;
use commonware_utils::ordered::Set;
use eyre::{ensure, eyre, Result, WrapErr as _};

use crate::world::rpc::Rpc;
use crate::world::validators::Validator;

const INDIVIDUAL_SIGNATURE_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

#[derive(Clone, Debug)]
pub(crate) struct ConflictingNotarizeEvidence {
    pub block1: Vec<u8>,
    pub block2: Vec<u8>,
}

/// Build two valid signatures by `accused` over different proposals for one
/// `(epoch, view)`, binding them to the exact current on-chain committee.
pub(crate) fn conflicting_notarize_for_validator(
    rpc: &Rpc,
    port: u16,
    accused: &Validator,
    epoch: u64,
    view: u64,
) -> Result<ConflictingNotarizeEvidence> {
    let active = rpc
        .active_consensus_set(port)
        .ok_or_else(|| eyre!("read active consensus set"))?;
    ensure!(!active.is_empty(), "active consensus set is empty");

    let mut public_keys = Vec::with_capacity(active.len());
    for address in active {
        let record = rpc
            .validator_record(port, &format!("{address:#x}"))
            .ok_or_else(|| eyre!("read validator record for {address:#x}"))?;
        let key = <bls12381::PublicKey as DecodeExt<()>>::decode(CodecBytes::from(
            record.consensus_pubkey.to_vec(),
        ))
        .map_err(|error| eyre!("decode committee BLS key for {address:#x}: {error}"))?;
        public_keys.push(key);
    }
    let committee = Set::from_iter_dedup(public_keys);
    let chain_id = rpc
        .chain_id(port)
        .ok_or_else(|| eyre!("read evidence chain id"))?;
    let namespace = notarize_namespace(chain_id, &committee);

    let private_hex = fs::read_to_string(accused.signing_key_path())
        .wrap_err("read accused validator individual BLS key")?;
    let private_bytes = hex::decode(private_hex.trim().trim_start_matches("0x"))
        .wrap_err("decode accused validator individual BLS key")?;
    let private = <bls12381::PrivateKey as DecodeExt<()>>::decode(CodecBytes::from(private_bytes))
        .map_err(|error| eyre!("decode accused validator individual BLS key: {error}"))?;
    let secret = SecretKey::from_bytes(private.encode().as_ref())
        .map_err(|error| eyre!("convert accused BLS key for evidence: {error:?}"))?;

    let accused_address = rpc
        .address_of(&accused.evm_key()?)
        .ok_or_else(|| eyre!("derive accused EOA"))?;
    let accused_record = rpc
        .validator_record(port, &accused_address)
        .ok_or_else(|| eyre!("read accused validator record"))?;
    ensure!(
        secret.sk_to_pk().to_bytes().as_slice() == accused_record.consensus_pubkey.as_ref(),
        "individual BLS key does not match the accused on-chain identity"
    );

    let parent = view.saturating_sub(1);
    let proposal1 = proposal_bytes(epoch, view, parent, 0xA1);
    let proposal2 = proposal_bytes(epoch, view, parent, 0xB2);
    Ok(ConflictingNotarizeEvidence {
        block1: evidence_block(&secret, &namespace, &proposal1),
        block2: evidence_block(&secret, &namespace, &proposal2),
    })
}

fn notarize_namespace(chain_id: u64, committee: &Set<bls12381::PublicKey>) -> Vec<u8> {
    let mut namespace = b"outbe".to_vec();
    namespace.extend_from_slice(&chain_id.to_be_bytes());
    namespace.extend_from_slice(b"_NOTARIZE");
    namespace.extend_from_slice(&outbe_consensus::proof::participant_set_commitment(
        committee,
    ));
    namespace
}

fn evidence_block(secret: &SecretKey, namespace: &[u8], proposal: &[u8]) -> Vec<u8> {
    let mut signed = Vec::new();
    write_leb128(&mut signed, namespace.len() as u64);
    signed.extend_from_slice(namespace);
    signed.extend_from_slice(proposal);
    let signature = secret.sign(&signed, INDIVIDUAL_SIGNATURE_DST, &[]);

    let mut block = Vec::with_capacity(48 + 96 + proposal.len());
    block.extend_from_slice(&secret.sk_to_pk().to_bytes());
    block.extend_from_slice(&signature.to_bytes());
    block.extend_from_slice(proposal);
    block
}

fn proposal_bytes(epoch: u64, view: u64, parent: u64, digest: u8) -> Vec<u8> {
    let mut proposal = Vec::with_capacity(3 * 10 + 32);
    write_leb128(&mut proposal, epoch);
    write_leb128(&mut proposal, view);
    write_leb128(&mut proposal, parent);
    proposal.extend_from_slice(&[digest; 32]);
    proposal
}

fn write_leb128(output: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            output.push(byte);
            return;
        }
        output.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::{proposal_bytes, write_leb128};

    #[test]
    fn leb128_matches_consensus_proposal_shape() {
        let mut bytes = Vec::new();
        write_leb128(&mut bytes, 127);
        write_leb128(&mut bytes, 128);
        assert_eq!(bytes, [0x7f, 0x80, 0x01]);

        let proposal = proposal_bytes(1, 5, 4, 0xaa);
        assert_eq!(&proposal[..3], &[1, 5, 4]);
        assert_eq!(&proposal[3..], &[0xaa; 32]);
    }
}
