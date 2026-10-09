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
use outbe_slashindicator::test_signing;

use crate::world::rpc::Rpc;
use crate::world::validators::Validator;

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
    let proposal1 = test_signing::proposal(epoch, view, parent, [0xA1; 32]);
    let proposal2 = test_signing::proposal(epoch, view, parent, [0xB2; 32]);
    let public = secret.sk_to_pk();
    Ok(ConflictingNotarizeEvidence {
        block1: test_signing::signed_evidence(
            &secret,
            &public,
            &namespace,
            &proposal1,
            test_signing::POP_DST,
        ),
        block2: test_signing::signed_evidence(
            &secret,
            &public,
            &namespace,
            &proposal2,
            test_signing::POP_DST,
        ),
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

#[cfg(test)]
mod tests {
    use outbe_slashindicator::test_signing;

    #[test]
    fn leb128_matches_consensus_proposal_shape() {
        // The varints of 127 then 128.
        let bytes = test_signing::nullify_payload(127, 128);
        assert_eq!(bytes, [0x7f, 0x80, 0x01]);

        let proposal = test_signing::proposal(1, 5, 4, [0xaa; 32]);
        assert_eq!(&proposal[..3], &[1, 5, 4]);
        assert_eq!(&proposal[3..], &[0xaa; 32]);
    }
}
