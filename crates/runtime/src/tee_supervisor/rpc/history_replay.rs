use super::super::proof::invalid;
use super::super::*;
use alloy_consensus::{private::alloy_rlp, BlockHeader};
use alloy_primitives::{Bytes, B256};
use jsonrpsee::rpc_params;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_tee::finalized_admission::*;
use serde_json::{json, Value};

fn unauthenticated(failed: bool) -> Result<(), RpcLifecycleError> {
    if failed {
        Err(RpcLifecycleError::Unauthenticated)
    } else {
        Ok(())
    }
}

impl super::RpcLifecyclePort {
    pub(super) async fn certified(
        &self,
        height: u64,
        history: &mut super::AdmissionHistory,
    ) -> Result<(outbe_consensus::block::ConsensusBlock, CertifiedHeaderV1), RpcLifecycleError>
    {
        let public: Value = match self
            .client
            .request("outbe_getFinalityProof", rpc_params![height])
            .await
        {
            Err(jsonrpsee::core::client::Error::Call(e)) if e.code() == -32601 => {
                self.client
                    .request("outbe_getFinalization", rpc_params![height])
                    .await?
            }
            result => result?,
        };
        let finalization = bytes(&public, "finalizationHex")?;
        let encoded = bytes(&public, "blockHex")?;
        let certified = outbe_consensus::committee_chain::decode_public_finalized_block(
            &finalization,
            &encoded,
            outbe_consensus::bls::MAX_COMMITTEE_MEMBERS,
        )
        .map_err(|e| RpcLifecycleError::Finality(e.into()))?;
        if certified.finalization.proposal.payload != certified.block.digest() {
            return Err(invalid(ProofFailure::InvalidEnvelope));
        }
        let mut blocks = Vec::new();
        if let Some(ancestors) = public.get("ancestorBlocksHex") {
            let array = ancestors
                .as_array()
                .ok_or(RpcLifecycleError::Missing("ancestorBlocksHex array"))?;
            if array.len() > MAX_FINALITY_DESCENDANTS {
                return Err(invalid(ProofFailure::InvalidEnvelope));
            }
            for ancestor in array {
                let raw: Bytes = serde_json::from_value(ancestor.clone())?;
                blocks.push(
                    outbe_consensus::committee_chain::decode_public_block(&raw)
                        .map_err(|e| RpcLifecycleError::Finality(e.into()))?,
                );
            }
        }
        blocks.push(certified.block);
        if blocks[0].number() != height
            || blocks.windows(2).any(|p| {
                p[0].number().checked_add(1) != Some(p[1].number())
                    || p[1].parent_digest() != p[0].digest()
            })
        {
            return Err(invalid(ProofFailure::InvalidEnvelope));
        }
        let target = blocks[0].clone();
        let canonical = self.block(json!(format!("0x{height:x}"))).await?;
        if hash(&canonical, "hash")? != target.block_hash() {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let anchor_outcome =
            trusted_outcome(&target, history.anchor.as_deref().map(|bytes| &**bytes))
                .ok_or(RpcLifecycleError::Unauthenticated)?;
        let descriptor = self
            .network_descriptor()
            .filter(|descriptor| !descriptor.genesis_consensus_keys.is_empty())
            .ok_or(RpcLifecycleError::Unauthenticated)?;
        if let Err(error) = self.authenticate(
            history,
            &descriptor,
            &anchor_outcome,
            &certified.finalization,
        ) {
            self.clear_history();
            return Err(error);
        }
        let compact = CertifiedHeaderV1 {
            finalization,
            header: alloy_rlp::encode(target.header()),
            descendants: blocks
                .iter()
                .skip(1)
                .map(|b| alloy_rlp::encode(b.header()))
                .collect(),
        };
        Ok((target, compact))
    }

    fn authenticate(
        &self,
        history: &mut super::AdmissionHistory,
        descriptor: &outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1,
        anchor_outcome: &[u8],
        finalization: &outbe_consensus::marshal_types::Finalization,
    ) -> Result<(), RpcLifecycleError> {
        let epoch = finalization.proposal.round.epoch();
        let mut verifier = self.take_verifier();
        let fresh = verifier.is_none();
        if fresh {
            measured_anchor(descriptor, anchor_outcome)?;
            let chain =
                outbe_consensus::committee_chain::CommitteeChain::from_trusted_local_boundary(
                    Default::default(),
                    anchor_outcome,
                )
                .map_err(|_| RpcLifecycleError::Unauthenticated)?;
            verifier = Some(super::VerifierCheckpoint { chain, applied: 0 });
        }
        let checkpoint = verifier
            .as_mut()
            .ok_or(RpcLifecycleError::Unauthenticated)?;
        replay_new(checkpoint, &history.transitions)?;
        let active = checkpoint
            .chain
            .highest_registered()
            .ok_or(RpcLifecycleError::Unauthenticated)?;
        if history.boundary_transition.is_some() && epoch == active.next() {
            history.promote_boundary()?;
            replay_new(checkpoint, &history.transitions)?;
        }
        checkpoint
            .chain
            .verify_finalization(epoch, finalization)
            .map_err(|_| RpcLifecycleError::Unauthenticated)?;
        self.set_verifier(verifier);
        Ok(())
    }
}

fn measured_anchor(
    descriptor: &outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1,
    anchor_outcome: &[u8],
) -> Result<(), RpcLifecycleError> {
    let context = outbe_tee::dcap_protocol::DcapOnboardingContextV1 {
        chain_id: descriptor.network_binding.chain_id,
        genesis_hash: descriptor.network_binding.genesis_hash,
        intent_hash: B256::repeat_byte(1),
        node_id_hash: B256::repeat_byte(2),
        enclave_id: B256::repeat_byte(3),
        binding_id: B256::repeat_byte(4),
        policy_hash: B256::repeat_byte(5),
        recipient_x25519: [6; 32],
        network_encryption_public: [7; 32],
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    outbe_tee::in_process::admission::FinalizedAdmissionVerifierV1::new(
        descriptor,
        &context,
        anchor_outcome,
    )
    .map(|_| ())
    .map_err(|_| RpcLifecycleError::Unauthenticated)
}

fn replay_new(
    checkpoint: &mut super::VerifierCheckpoint,
    transitions: &[Bytes],
) -> Result<(), RpcLifecycleError> {
    while checkpoint.applied < transitions.len() {
        replay_transition(&mut checkpoint.chain, &transitions[checkpoint.applied])?;
        checkpoint.applied += 1;
    }
    Ok(())
}

fn replay_transition(
    chain: &mut outbe_consensus::committee_chain::CommitteeChain,
    encoded: &[u8],
) -> Result<(), RpcLifecycleError> {
    use alloy_consensus::Sealable as _;
    use alloy_rlp::Decodable as _;
    let transition = CertifiedHeaderV1::decode_canonical(encoded)
        .map_err(|_| RpcLifecycleError::Unauthenticated)?;
    let finalization = outbe_consensus::committee_chain::decode_public_finalization(
        &transition.finalization,
        outbe_consensus::bls::MAX_COMMITTEE_MEMBERS,
    )
    .map_err(|_| RpcLifecycleError::Unauthenticated)?;
    let mut bytes = transition.header.as_slice();
    let header = outbe_primitives::OutbeHeader::decode(&mut bytes)
        .map_err(|_| RpcLifecycleError::Unauthenticated)?;
    unauthenticated(!bytes.is_empty())?;
    let mut signed = header.hash_slow();
    let mut previous_number = header.number();
    let mut previous_hash = signed;
    for child in &transition.descendants {
        let mut child_bytes = child.as_slice();
        let decoded = outbe_primitives::OutbeHeader::decode(&mut child_bytes)
            .map_err(|_| RpcLifecycleError::Unauthenticated)?;
        unauthenticated(
            !child_bytes.is_empty()
                || previous_number.checked_add(1) != Some(decoded.number())
                || decoded.parent_hash() != previous_hash,
        )?;
        previous_number = decoded.number();
        previous_hash = decoded.hash_slow();
        signed = previous_hash;
    }
    unauthenticated(finalization.proposal.payload != outbe_consensus::digest::Digest(signed))?;
    let epoch = finalization.proposal.round.epoch();
    let expected = chain
        .highest_registered()
        .ok_or(RpcLifecycleError::Unauthenticated)?;
    unauthenticated(epoch != expected)?;
    chain
        .verify_finalization(epoch, &finalization)
        .map_err(|_| RpcLifecycleError::Unauthenticated)?;
    let artifacts = decode_outbe_block_artifacts(header.extra_data().as_ref())
        .map_err(|_| RpcLifecycleError::Unauthenticated)?;
    let (next, outcome) = match artifacts.consensus_header_artifact {
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: next,
            outcome,
        }) => (next, outcome),
        _ => return Err(RpcLifecycleError::Unauthenticated),
    };
    unauthenticated(epoch.get().checked_add(1) != Some(next))?;
    chain
        .register_epoch_from_outcome(epoch.next(), &outcome)
        .map_err(|_| RpcLifecycleError::Unauthenticated)?;
    chain.retain_only_highest();
    Ok(())
}

fn trusted_outcome(
    block: &outbe_consensus::block::ConsensusBlock,
    anchor: Option<&[u8]>,
) -> Option<Vec<u8>> {
    use alloy_consensus::BlockHeader as _;
    let artifacts = decode_outbe_block_artifacts(block.header().extra_data()).ok()?;
    if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
        artifacts.consensus_header_artifact
    {
        if boundary.epoch == 0 {
            return Some(boundary.outcome.to_vec());
        }
    }
    anchor.map(|bytes| bytes.to_vec())
}
