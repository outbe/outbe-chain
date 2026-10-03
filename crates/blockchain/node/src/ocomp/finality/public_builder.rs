use super::*;

pub struct PublicFinalizedIntentProofBuilderV1<'a, S> {
    source: &'a S,
    limits: SchemaLimits,
}

impl<'a, S> PublicFinalizedIntentProofBuilderV1<'a, S> {
    #[must_use]
    pub const fn new(source: &'a S, limits: SchemaLimits) -> Self {
        Self { source, limits }
    }
}

impl<S> PublicFinalizedIntentProofBuilderV1<'_, S>
where
    S: PublicExactBlockProofSourceV1,
{
    pub fn build_and_verify(
        &self,
        request_height: u64,
        intent_id: B256,
        expected: ExpectedFinalizedIntentBindingV1,
    ) -> Result<
        (FinalizedIntentProofV1, VerifiedFinalizedIntentV1),
        PublicFinalizedIntentProofBuildError,
    > {
        let request = self.read_finalized_request(request_height)?;
        let committee = self.read_historical_committee(&request)?;
        let intent = self.read_pending_intent(&request, intent_id)?;
        let proof = self.assemble_proof(request, committee, intent)?;
        proof
            .encode_canonical(&self.limits)
            .map_err(|error| PublicFinalizedIntentProofBuildError::Intent(error.to_string()))?;
        let verified = proof.verify(
            expected,
            &FinalizedIntentVerifier::new(TrieHistoricalCommitteeAuthority),
            &self.limits,
        )?;
        if verified.intent_id != intent_id {
            return Err(PublicFinalizedIntentProofBuildError::Intent(
                "constructed proof opened a different IntentId".to_owned(),
            ));
        }
        Ok((proof, verified))
    }
}

struct ExactFinalizedRequest {
    public_bytes: PublicFinalizationBytesV1,
    finalized: outbe_consensus::follow::CertifiedFinalizedBlock,
    request_height: u64,
    block_hash: B256,
    request_state_root: B256,
    finalized_epoch: u64,
    committee_len: usize,
}
struct HistoricalCommittee {
    snapshot: CommitteeSnapshot,
    proof: PublicAccountProofV1,
    committee_set_hash: B256,
}
struct PendingIntent {
    record: OcompJobRecordV1,
    slots: Vec<(U256, U256)>,
    proof: PublicAccountProofV1,
}
struct ProofAssembly {
    committee_account_proof: AccountProof,
    committee_slots: Vec<(U256, U256)>,
    intent_account_proof: AccountProof,
    canonical_request_header_rlp: Vec<u8>,
    parent_accounting: CertifiedParentAccountingMetadataV2,
}

impl ExactFinalizedRequest {
    fn signer_bitmap(&self) -> Result<Vec<u8>, PublicFinalizedIntentProofBuildError> {
        let committee_len = self.committee_len;
        let finalization = &self.finalized.finalization;
        let mut signer_bitmap = vec![0_u8; committee_len];
        for signer in finalization.certificate.signers.iter() {
            let index = signer.get() as usize;
            let Some(entry) = signer_bitmap.get_mut(index) else {
                return Err(PublicFinalizedIntentProofBuildError::SignerIndex(index));
            };
            *entry = 1;
        }
        Ok(signer_bitmap)
    }
    fn parent_accounting(
        self,
        committee: &HistoricalCommittee,
        signer_bitmap: Vec<u8>,
        vrf_material_version: u16,
    ) -> CertifiedParentAccountingMetadataV2 {
        let Self {
            request_height,
            block_hash,
            finalized_epoch,
            public_bytes,
            finalized,
            ..
        } = self;
        let finalization = &finalized.finalization;
        let snapshot = &committee.snapshot;
        let committee_set_hash = committee.committee_set_hash;
        CertifiedParentAccountingMetadataV2 {
            finalized_block_number: request_height,
            finalized_block_hash: block_hash,
            finalized_epoch,
            finalized_view: finalization.proposal.round.view().get(),
            parent_view: finalization.proposal.parent.get(),
            ordered_committee: snapshot
                .committee
                .iter()
                .map(|entry| BoundedBytes(entry.address.as_slice().to_vec()))
                .collect(),
            signer_bitmap: BoundedBytes(signer_bitmap),
            canonical_commonware_finalization_proof: ProofBytes(public_bytes.finalization_bytes),
            committee_set_hash,
            vrf_material_version,
            vrf_group_public_key_hash: keccak256(&snapshot.vrf_group_public_key_bytes),
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        }
    }
}

fn prepare_assembly(
    request: ExactFinalizedRequest,
    committee: &HistoricalCommittee,
    intent: &PendingIntent,
) -> Result<ProofAssembly, PublicFinalizedIntentProofBuildError> {
    let snapshot = &committee.snapshot;
    let committee_set_hash = committee.committee_set_hash;
    let public_committee_proof = &committee.proof;
    let public_intent_proof = &intent.proof;
    let finalized_epoch = request.finalized_epoch;
    let header = request.finalized.block.header();
    let committee_account_proof = public_committee_proof.to_reth_proof();
    let committee_slots =
        historical_committee_storage_slots(finalized_epoch, committee_set_hash, snapshot);
    let intent_account_proof = public_intent_proof.to_reth_proof();
    let signer_bitmap = request.signer_bitmap()?;
    let vrf_material_version = u16::try_from(snapshot.vrf_material_version)
        .map_err(|_| PublicFinalizedIntentProofBuildError::VrfMaterialVersion)?;
    let mut canonical_request_header_rlp = Vec::new();
    header.encode(&mut canonical_request_header_rlp);
    let parent_accounting =
        request.parent_accounting(committee, signer_bitmap, vrf_material_version);
    Ok(ProofAssembly {
        committee_account_proof,
        committee_slots,
        intent_account_proof,
        canonical_request_header_rlp,
        parent_accounting,
    })
}

impl<S: PublicExactBlockProofSourceV1> PublicFinalizedIntentProofBuilderV1<'_, S> {
    fn read_finalized_request(
        &self,
        request_height: u64,
    ) -> Result<ExactFinalizedRequest, PublicFinalizedIntentProofBuildError> {
        let public_bytes = self
            .source
            .finalization(request_height)
            .map_err(public_source_error)?;
        let max_committee_len =
            usize::try_from(outbe_consensus::bls::MAX_VALIDATORS).map_err(|_| {
                PublicFinalizedIntentProofBuildError::Finalization(
                    "consensus validator bound exceeds usize".to_owned(),
                )
            })?;
        let finalized = decode_public_finalized_block(
            &public_bytes.finalization_bytes,
            &public_bytes.block_bytes,
            max_committee_len,
        )
        .map_err(|error| PublicFinalizedIntentProofBuildError::Finalization(error.to_string()))?;
        let block_hash = finalized.block.block_hash();
        let header = finalized.block.header();
        let block_view = self
            .source
            .block_by_hash(block_hash)
            .map_err(public_source_error)?;
        let finalized_identity_matches =
            finalized.block.number() == request_height && block_view.hash == block_hash;
        if !finalized_identity_matches
            || block_view.number != request_height
            || block_view.state_root != header.state_root()
        {
            return Err(PublicFinalizedIntentProofBuildError::HeaderMismatch);
        }

        let finalization = &finalized.finalization;
        if finalization.proposal.payload.0 != block_hash {
            return Err(PublicFinalizedIntentProofBuildError::Finalization(
                "finalization payload differs from finalized block hash".to_owned(),
            ));
        }
        let finalized_epoch = finalization.proposal.round.epoch().get();
        let committee_len = finalization.certificate.signers.len();
        validate_public_committee_len(committee_len, max_committee_len)?;
        let request_state_root = header.state_root();

        Ok(ExactFinalizedRequest {
            public_bytes,
            finalized,
            request_height,
            block_hash,
            request_state_root,
            finalized_epoch,
            committee_len,
        })
    }
    fn read_committee_snapshot_key(
        &self,
        request: &ExactFinalizedRequest,
    ) -> Result<B256, PublicFinalizedIntentProofBuildError> {
        let block_hash = request.block_hash;
        let request_state_root = request.request_state_root;
        let finalized_epoch = request.finalized_epoch;
        let ring_slot = historical_committee_ring_slot(finalized_epoch);
        let ring_proof = self
            .source
            .account_proof(VALIDATOR_SET_ADDRESS, &[ring_slot], block_hash)
            .map_err(public_source_error)?;
        verify_public_account_proof(
            &ring_proof,
            VALIDATOR_SET_ADDRESS,
            &[ring_slot],
            request_state_root,
        )?;
        let snapshot_key = B256::new(ring_proof.storage_value(ring_slot)?.to_be_bytes::<32>());
        if snapshot_key == B256::ZERO {
            return Err(PublicFinalizedIntentProofBuildError::MissingCommitteeSnapshot);
        }

        Ok(snapshot_key)
    }
    fn read_committee_vrf_length(
        &self,
        request: &ExactFinalizedRequest,
        snapshot_key: B256,
    ) -> Result<usize, PublicFinalizedIntentProofBuildError> {
        let block_hash = request.block_hash;
        let request_state_root = request.request_state_root;
        let committee_len = request.committee_len;
        let base_committee_slots = historical_committee_base_slot_keys(snapshot_key, committee_len);
        let base_committee_proof = self
            .source
            .account_proof(VALIDATOR_SET_ADDRESS, &base_committee_slots, block_hash)
            .map_err(public_source_error)?;
        verify_public_account_proof(
            &base_committee_proof,
            VALIDATOR_SET_ADDRESS,
            &base_committee_slots,
            request_state_root,
        )?;
        let vrf_length_slot = snapshot_key.mapping_slot(U256::from(38));
        let vrf_length = usize::try_from(
            base_committee_proof.storage_value(B256::new(vrf_length_slot.to_be_bytes::<32>()))?,
        )
        .map_err(|_| PublicFinalizedIntentProofBuildError::VrfKeyLength)?;
        if vrf_length == 0 || vrf_length > self.limits.max_proof_bytes {
            return Err(PublicFinalizedIntentProofBuildError::VrfKeyLength);
        }

        Ok(vrf_length)
    }
    fn read_historical_committee(
        &self,
        request: &ExactFinalizedRequest,
    ) -> Result<HistoricalCommittee, PublicFinalizedIntentProofBuildError> {
        let block_hash = request.block_hash;
        let request_state_root = request.request_state_root;
        let finalized_epoch = request.finalized_epoch;
        let committee_len = request.committee_len;
        let snapshot_key = self.read_committee_snapshot_key(request)?;
        let vrf_length = self.read_committee_vrf_length(request, snapshot_key)?;
        let full_committee_slots =
            historical_committee_full_slot_keys(snapshot_key, committee_len, vrf_length);
        let public_committee_proof = self
            .source
            .account_proof(VALIDATOR_SET_ADDRESS, &full_committee_slots, block_hash)
            .map_err(public_source_error)?;
        verify_public_account_proof(
            &public_committee_proof,
            VALIDATOR_SET_ADDRESS,
            &full_committee_slots,
            request_state_root,
        )?;
        let snapshot = reconstruct_public_committee_snapshot(
            &public_committee_proof,
            &CommitteeLocation {
                snapshot_key,
                committee_len,
                vrf_key_len: vrf_length,
                epoch: finalized_epoch,
            },
        )?;
        let committee_set_hash = snapshot.committee_set_hash_v2(finalized_epoch);
        if committee_snapshot_key(finalized_epoch, committee_set_hash) != snapshot_key {
            return Err(PublicFinalizedIntentProofBuildError::CommitteeSnapshotKey);
        }

        Ok(HistoricalCommittee {
            snapshot,
            proof: public_committee_proof,
            committee_set_hash,
        })
    }
    fn read_pending_intent(
        &self,
        request: &ExactFinalizedRequest,
        intent_id: B256,
    ) -> Result<PendingIntent, PublicFinalizedIntentProofBuildError> {
        let block_hash = request.block_hash;
        let request_state_root = request.request_state_root;
        let canonical_record = self
            .source
            .job_record(intent_id, block_hash)
            .map_err(public_source_error)?;
        if canonical_record.len() > self.limits.max_bounded_bytes {
            return Err(PublicFinalizedIntentProofBuildError::JobRecordTooLarge);
        }
        let record = OcompJobRecordV1::decode_canonical(&canonical_record, &self.limits)
            .map_err(|error| PublicFinalizedIntentProofBuildError::Intent(error.to_string()))?;
        if record.status != OcompJobStatus::AwaitingFinality
            || record
                .intent
                .intent_id(&self.limits)
                .map_err(|error| PublicFinalizedIntentProofBuildError::Intent(error.to_string()))?
                != intent_id
        {
            return Err(PublicFinalizedIntentProofBuildError::Intent(
                "public exact-block job record is not the requested pending intent".to_owned(),
            ));
        }
        let logical_key = outbe_ocomp_protocol::intent::intent_storage_key(intent_id)
            .map_err(|error| PublicFinalizedIntentProofBuildError::Intent(error.to_string()))?;
        let intent_slots = storage_bytes_slots(logical_key, &canonical_record);
        let intent_slot_keys = slot_keys(&intent_slots);
        let public_intent_proof = self
            .source
            .account_proof(METADOSIS_ADDRESS, &intent_slot_keys, block_hash)
            .map_err(public_source_error)?;
        verify_public_account_proof(
            &public_intent_proof,
            METADOSIS_ADDRESS,
            &intent_slot_keys,
            request_state_root,
        )?;

        Ok(PendingIntent {
            record,
            slots: intent_slots,
            proof: public_intent_proof,
        })
    }
    fn assemble_proof(
        &self,
        request: ExactFinalizedRequest,
        committee: HistoricalCommittee,
        intent: PendingIntent,
    ) -> Result<FinalizedIntentProofV1, PublicFinalizedIntentProofBuildError> {
        let ProofAssembly {
            committee_account_proof,
            committee_slots,
            intent_account_proof,
            canonical_request_header_rlp,
            parent_accounting,
        } = prepare_assembly(request, &committee, &intent)?;
        let snapshot = &committee.snapshot;
        let record = &intent.record;
        let intent_slots = &intent.slots;
        Ok(FinalizedIntentProofV1 {
            chain_id: record.intent.chain_id,
            genesis_hash: record.intent.genesis_hash,
            fork_id: record.intent.fork_id,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            canonical_request_header_rlp: ProofBytes(canonical_request_header_rlp),
            parent_accounting,
            historical_committee_membership_proof: ProofBytes(
                encode_historical_committee_witness(
                    snapshot,
                    &committee_account_proof,
                    &committee_slots,
                )
                .map_err(|error| {
                    PublicFinalizedIntentProofBuildError::Witness(error.to_string())
                })?,
            ),
            canonical_job_intent: BoundedBytes(
                record
                    .intent
                    .encode_canonical(&self.limits)
                    .map_err(|error| {
                        PublicFinalizedIntentProofBuildError::Intent(error.to_string())
                    })?,
            ),
            intent_account_proof: ProofBytes(
                encode_account_witness(&intent_account_proof, METADOSIS_ADDRESS).map_err(
                    |error| PublicFinalizedIntentProofBuildError::Witness(error.to_string()),
                )?,
            ),
            intent_storage_proof: ProofBytes(
                encode_storage_witness(&intent_account_proof, intent_slots).map_err(|error| {
                    PublicFinalizedIntentProofBuildError::Witness(error.to_string())
                })?,
            ),
        })
    }
}
fn reconstruct_public_committee_snapshot(
    proof: &PublicAccountProofV1,
    location: &CommitteeLocation,
) -> Result<CommitteeSnapshot, PublicFinalizedIntentProofBuildError> {
    let CommitteeLocation {
        snapshot_key,
        committee_len,
        vrf_key_len,
        epoch,
    } = *location;
    let storage = CommitteeStorage {
        proof,
        snapshot_key,
    };
    if storage.value(31)? != U256::from(1)
        || usize::try_from(storage.value(32)?)
            .ok()
            .filter(|length| *length == committee_len)
            .is_none()
    {
        return Err(PublicFinalizedIntentProofBuildError::CommitteeShape);
    }
    let mut committee = Vec::with_capacity(committee_len);
    for index in 0..committee_len {
        committee.push(storage.entry(index as u64)?);
    }
    let vrf_group_public_key_bytes = storage.vrf_key(vrf_key_len)?;
    if usize::try_from(storage.value(38)?).ok() != Some(vrf_key_len)
        || storage.value(37)? != U256::from_be_bytes(keccak256(&vrf_group_public_key_bytes).0)
    {
        return Err(PublicFinalizedIntentProofBuildError::VrfKey);
    }
    let snapshot = CommitteeSnapshot {
        committee,
        vrf_material_version: u64::try_from(storage.value(36)?)
            .map_err(|_| PublicFinalizedIntentProofBuildError::VrfMaterialVersion)?,
        vrf_group_public_key_bytes,
        vrf_public_polynomial_hash: B256::new(storage.value(47)?.to_be_bytes::<32>()),
    };
    let hash = snapshot.committee_set_hash_v2(epoch);
    if committee_snapshot_key(epoch, hash) != snapshot_key {
        return Err(PublicFinalizedIntentProofBuildError::CommitteeSnapshotKey);
    }
    Ok(snapshot)
}

#[derive(Clone, Copy)]
struct CommitteeLocation {
    snapshot_key: B256,
    committee_len: usize,
    vrf_key_len: usize,
    epoch: u64,
}
struct CommitteeStorage<'a> {
    proof: &'a PublicAccountProofV1,
    snapshot_key: B256,
}
impl CommitteeStorage<'_> {
    fn value(&self, base_slot: u64) -> Result<U256, PublicFinalizedIntentProofBuildError> {
        let slot = self.snapshot_key.mapping_slot(U256::from(base_slot));
        self.proof
            .storage_value(B256::new(slot.to_be_bytes::<32>()))
    }
    fn entry_value(
        &self,
        base_slot: u64,
        index: u64,
    ) -> Result<U256, PublicFinalizedIntentProofBuildError> {
        let slot = index.mapping_slot(self.snapshot_key.mapping_slot(U256::from(base_slot)));
        self.proof
            .storage_value(B256::new(slot.to_be_bytes::<32>()))
    }
    fn entry(&self, index: u64) -> Result<CommitteeEntry, PublicFinalizedIntentProofBuildError> {
        let address_word = self.entry_value(33, index)?.to_be_bytes::<32>();
        if address_word[..12].iter().any(|byte| *byte != 0) {
            return Err(PublicFinalizedIntentProofBuildError::CommitteeShape);
        }
        let low = self.entry_value(34, index)?.to_be_bytes::<32>();
        let high = self.entry_value(35, index)?.to_be_bytes::<32>();
        if high[16..].iter().any(|byte| *byte != 0) {
            return Err(PublicFinalizedIntentProofBuildError::CommitteeShape);
        }
        let mut consensus_pubkey = [0_u8; 48];
        consensus_pubkey[..32].copy_from_slice(&low);
        consensus_pubkey[32..].copy_from_slice(&high[..16]);
        Ok(CommitteeEntry {
            address: Address::from_slice(&address_word[12..]),
            consensus_pubkey,
        })
    }
    fn vrf_key(&self, vrf_key_len: usize) -> Result<Vec<u8>, PublicFinalizedIntentProofBuildError> {
        let mut vrf_group_public_key_bytes = Vec::with_capacity(vrf_key_len);
        for index in 0..vrf_key_len.div_ceil(32) {
            vrf_group_public_key_bytes
                .extend_from_slice(&self.entry_value(39, index as u64)?.to_be_bytes::<32>());
        }
        vrf_group_public_key_bytes.truncate(vrf_key_len);
        Ok(vrf_group_public_key_bytes)
    }
}
