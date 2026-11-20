//! One-shot finality and Ethereum-state verification for onboarding.

use crate::{
    dcap_protocol::DcapOnboardingContextV1,
    finalized_admission::{
        onboarding_registry_slots_v1, CertifiedHeaderV1, FinalizedAdmissionWitnessV1,
        MptAccountProofV1, MptStorageProofV1,
    },
};
use alloy_consensus::{BlockHeader as _, Sealable as _};
use alloy_primitives::{B256, U256};
use alloy_rlp::Decodable as _;
use commonware_codec::ReadExt as _;
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381;
use commonware_utils::ordered::Set;
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact},
    tee_attestation_v1::TrustedNetworkDescriptorV1,
    OutbeHeader,
};
use outbe_simplex::bls::MAX_COMMITTEE_MEMBERS;
use outbe_simplex::committee_chain::{decode_public_finalization, CommitteeChain};
use reth_primitives_traits::Account;
use reth_trie::{AccountProof, StorageProof};

use super::{AdmissionError as FinalizedAdmissionError, Result as EnclaveResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Header identity and consensus time authenticated by BLS finality and Registry openings.
pub struct VerifiedAdmissionAnchorV1 {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
    pub consensus_timestamp: u64,
}

/// Streaming committee verifier rooted in the trusted genesis participant set.
pub struct FinalizedAdmissionVerifierV1 {
    chain: CommitteeChain,
    previous_height: u64,
    context: DcapOnboardingContextV1,
    upgrade: bool,
}

impl FinalizedAdmissionVerifierV1 {
    /// Bind the verifier to trusted genesis keys and its canonical epoch-zero outcome.
    pub fn new(
        descriptor: &TrustedNetworkDescriptorV1,
        context: &DcapOnboardingContextV1,
        anchor_outcome: &[u8],
    ) -> EnclaveResult<Self> {
        if context.chain_id != descriptor.network_binding.chain_id
            || context.genesis_hash != descriptor.network_binding.genesis_hash
        {
            return Err(FinalizedAdmissionError::NetworkIdentityNotReleaseMeasured.into());
        }

        let participants = descriptor
            .genesis_consensus_keys
            .iter()
            .map(|encoded| {
                let mut input = encoded.as_slice();
                let key = bls12381::PublicKey::read(&mut input)
                    .map_err(FinalizedAdmissionError::GenesisConsensusKey)?;
                if !input.is_empty() {
                    return Err(FinalizedAdmissionError::GenesisConsensusKeyTrailingBytes.into());
                }
                Ok(key)
            })
            .collect::<EnclaveResult<Vec<_>>>()?;
        let participants =
            Set::try_from(participants).map_err(|_| FinalizedAdmissionError::GenesisCommittee)?;
        let measured_chain_id =
            u64::try_from(U256::from_be_bytes(descriptor.network_binding.chain_id))
                .map_err(|_| FinalizedAdmissionError::ConsensusChainIdOverflow)?;
        outbe_simplex::config::init_consensus_chain_id(measured_chain_id)
            .map_err(FinalizedAdmissionError::ConsensusChainBinding)?;
        let mut chain = CommitteeChain::new(Epoch::new(0), participants);
        chain
            .register_epoch_from_outcome(Epoch::new(0), anchor_outcome)
            .map_err(|cause| FinalizedAdmissionError::Epoch0Anchor(cause.into()))?;
        chain.retain_only_highest();
        Ok(Self {
            chain,
            previous_height: 0,
            context: *context,
            upgrade: false,
        })
    }

    /// Require upgrade-authorization Registry slots instead of ordinary onboarding slots.
    pub fn for_upgrade(mut self) -> Self {
        self.upgrade = true;
        self
    }

    /// Authenticate one immediate successor committee before advancing the trusted chain.
    pub fn advance_committee(&mut self, encoded: &[u8]) -> EnclaveResult<()> {
        let transition = CertifiedHeaderV1::decode_canonical(encoded)
            .map_err(FinalizedAdmissionError::TransitionCodec)?;
        let (finalization, header, _) = decode_certified_header(&transition)?;
        if header.number() <= self.previous_height {
            return Err(FinalizedAdmissionError::TransitionEnvelopeOrOrder.into());
        }
        let epoch = finalization.proposal.round.epoch();
        let expected = self
            .chain
            .highest_registered()
            .ok_or(FinalizedAdmissionError::Epoch0NotRegistered)?;
        if epoch != expected {
            return Err(FinalizedAdmissionError::TransitionNotCurrentCommittee.into());
        }
        self.chain
            .verify_finalization(epoch, &finalization)
            .map_err(|cause| FinalizedAdmissionError::TransitionFinalization(cause.into()))?;
        let artifacts = decode_outbe_block_artifacts(header.extra_data().as_ref())
            .map_err(FinalizedAdmissionError::TransitionArtifacts)?;
        let Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: next,
            outcome,
        }) = artifacts.consensus_header_artifact
        else {
            return Err(FinalizedAdmissionError::TransitionLacksPreAnnounce.into());
        };
        if next != epoch.get().saturating_add(1) {
            return Err(FinalizedAdmissionError::TransitionNotImmediateSuccessor.into());
        }
        self.chain
            .register_epoch_from_outcome(Epoch::new(next), &outcome)
            .map_err(|cause| FinalizedAdmissionError::SuccessorCommittee(cause.into()))?;
        self.chain.retain_only_highest();
        self.previous_height = header
            .number()
            .checked_add(transition.descendants.len() as u64)
            .ok_or(FinalizedAdmissionError::CommitteeFinalityHeightOverflow)?;
        Ok(())
    }

    /// Verify the exact finalized header ancestry and required Registry MPT claims.
    pub fn verify_admission(&self, encoded: &[u8]) -> EnclaveResult<VerifiedAdmissionAnchorV1> {
        let proof = FinalizedAdmissionWitnessV1::decode_canonical(encoded)
            .map_err(FinalizedAdmissionError::AdmissionWitnessCodec)?;
        let (finalization, header, header_hash) = decode_certified_header(&proof.admission)?;
        if header.number() <= self.previous_height {
            return Err(FinalizedAdmissionError::AdmissionEnvelopeOrOrder.into());
        }
        let epoch = finalization.proposal.round.epoch();
        if self.chain.highest_registered() != Some(epoch) {
            return Err(FinalizedAdmissionError::AdmissionSkipsCommittee.into());
        }
        self.chain
            .verify_finalization(epoch, &finalization)
            .map_err(|cause| FinalizedAdmissionError::AdmissionFinalization(cause.into()))?;

        let state_root = header.state_root();
        verify_registry_account(state_root, &proof.registry_account)?;
        let expected = if self.upgrade {
            expected_upgrade_claims(&self.context, header.timestamp(), &proof.registry_storage)?
        } else {
            expected_registry_claims(&self.context, header.timestamp(), &proof.registry_storage)?
        };
        if proof.registry_storage.len() != expected.len() {
            return Err(FinalizedAdmissionError::RegistrySlotCount.into());
        }
        for (slot, value) in expected {
            let opening = proof
                .registry_storage
                .iter()
                .find(|opening| opening.key == slot)
                .ok_or(FinalizedAdmissionError::RegistryMissingSlot)?;
            if opening.value != value {
                return Err(FinalizedAdmissionError::RegistryValueMismatch.into());
            }
            verify_storage(proof.registry_account.storage_root, opening)?;
        }
        Ok(VerifiedAdmissionAnchorV1 {
            block_number: header.number(),
            block_hash: header_hash,
            state_root,
            consensus_timestamp: header.timestamp(),
        })
    }
}

fn decode_certified_header(
    proof: &CertifiedHeaderV1,
) -> EnclaveResult<(
    outbe_simplex::marshal_types::Finalization,
    OutbeHeader,
    B256,
)> {
    let finalization = decode_public_finalization(&proof.finalization, MAX_COMMITTEE_MEMBERS)
        .map_err(FinalizedAdmissionError::PublicFinalization)?;
    let header = decode_header(&proof.header)?;
    let hash = header.hash_slow();
    let mut previous = header.clone();
    for encoded in &proof.descendants {
        let child = decode_header(encoded)?;
        if previous.number().checked_add(1) != Some(child.number())
            || child.parent_hash() != previous.hash_slow()
        {
            return Err(FinalizedAdmissionError::FinalityAncestryNotConsecutive.into());
        }
        previous = child;
    }
    if finalization.proposal.payload != outbe_simplex::digest::Digest(previous.hash_slow()) {
        return Err(FinalizedAdmissionError::FinalizationPayloadMismatch.into());
    }
    Ok((finalization, header, hash))
}

fn decode_header(encoded: &[u8]) -> EnclaveResult<OutbeHeader> {
    let mut bytes = encoded;
    let header =
        OutbeHeader::decode(&mut bytes).map_err(FinalizedAdmissionError::CanonicalHeaderRlp)?;
    if !bytes.is_empty() {
        return Err(FinalizedAdmissionError::HeaderTrailingBytes.into());
    }
    Ok(header)
}

fn verify_registry_account(state_root: B256, witness: &MptAccountProofV1) -> EnclaveResult<()> {
    AccountProof {
        address: TEE_REGISTRY_ADDRESS,
        info: Some(Account {
            nonce: witness.nonce,
            balance: witness.balance,
            bytecode_hash: Some(witness.code_hash),
        }),
        proof: witness.nodes.iter().cloned().map(Into::into).collect(),
        storage_root: witness.storage_root,
        storage_proofs: Vec::new(),
    }
    .verify(state_root)
    .map_err(|cause| FinalizedAdmissionError::RegistryAccountProof(Box::new(cause)))?;
    Ok(())
}

fn verify_storage(root: B256, witness: &MptStorageProofV1) -> EnclaveResult<()> {
    StorageProof {
        key: witness.key,
        value: witness.value,
        ..StorageProof::new(witness.key)
    }
    .with_proof(witness.nodes.iter().cloned().map(Into::into).collect())
    .verify(root)
    .map_err(|cause| FinalizedAdmissionError::RegistryStorageProof(Box::new(cause)))?;
    Ok(())
}

fn expected_upgrade_claims(
    context: &DcapOnboardingContextV1,
    timestamp: u64,
    openings: &[MptStorageProofV1],
) -> EnclaveResult<Vec<(B256, U256)>> {
    let slots = crate::finalized_admission::upgrade_registry_slots_v1(context);
    let value = |i: usize| -> EnclaveResult<U256> {
        openings
            .iter()
            .find(|o| o.key == slots[i])
            .map(|o| o.value)
            .ok_or_else(|| FinalizedAdmissionError::MissingUpgradeSlot.into())
    };
    let expiry = value(4)?;
    let source = value(5)?;
    let nonce = value(8)?;
    if expiry <= U256::from(timestamp) || source.is_zero() || nonce.is_zero() {
        return Err(FinalizedAdmissionError::UpgradeAuthorizationAbsentOrExpired.into());
    }
    Ok(vec![
        (
            slots[0],
            U256::from_be_bytes(context.network_encryption_public),
        ),
        (slots[1], U256::from(context.key_epoch)),
        (slots[2], U256::from(context.network_encryption_epoch)),
        (slots[3], U256::from_be_bytes(context.context_hash().0)),
        (slots[4], expiry),
        (slots[5], source),
        (slots[6], source),
        (slots[7], U256::from_be_bytes(context.policy_hash.0)),
        (slots[8], nonce),
    ])
}

fn expected_registry_claims(
    context: &DcapOnboardingContextV1,
    block_timestamp: u64,
    openings: &[MptStorageProofV1],
) -> EnclaveResult<Vec<(B256, U256)>> {
    let slots = onboarding_registry_slots_v1(context);
    let valid_until_slot = slots[7];
    let valid_until = openings
        .iter()
        .find(|opening| opening.key == valid_until_slot)
        .map(|opening| opening.value)
        .ok_or(FinalizedAdmissionError::RegistryLacksLeaseExpiry)?;
    if valid_until <= U256::from(block_timestamp) {
        return Err(FinalizedAdmissionError::RegistryLeaseNotLive.into());
    }
    let values = [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.enclave_id.0),
        U256::from_be_bytes(context.binding_id.0),
        U256::from_be_bytes(context.intent_hash.0),
        U256::from_be_bytes(context.policy_hash.0),
        valid_until,
        U256::from_be_bytes(context.recipient_x25519),
    ];
    Ok(slots.into_iter().zip(values).collect())
}
