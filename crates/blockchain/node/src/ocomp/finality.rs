//! Verification of the finalized request that becomes one signable OCOMP job.
//!
//! The proof object is intentionally self-contained, but none of its relayer
//! bytes are authority by themselves. This adapter authenticates the historical
//! committee, runs the production Commonware finalization verifier, opens the
//! canonical Ethereum header, and verifies the fixed Metadosis storage path.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::{Decodable as _, Encodable as _};
use outbe_consensus::{
    block::ConsensusBlock,
    finalization::parent_cert_store::{
        CertifiedParentProofRecord, FinalizedParentCertStore, ProofKind,
    },
    proof::{committee_snapshot_key, verify_v2_proof, CommitteeEntry, CommitteeSnapshot},
};
use outbe_metadosis::schema::OCOMP_JOB_RECORDS_BASE_SLOT;
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    intent::{
        CertifiedParentAccountingMetadataV2, ExpectedFinalizedIntentBindingV1,
        FinalizedIntentAuthorityError, FinalizedIntentProofAuthority, FinalizedIntentProofV1,
        FinalizedIntentVerificationError, FinalizedRequestBindingV1, JobIntentV1, ParentProofKind,
        VerifiedFinalizedIntentV1,
    },
    opening::{RawContractOpeningProofV1, RawStorageSlotV1},
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, VALIDATOR_SET_ADDRESS},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    header::OutbeHeader,
    storage::types::StorageKey as _,
};
use reth_primitives_traits::Account;
use reth_provider::StateProviderFactory;
use reth_storage_api::StateProvider;
use reth_trie::{AccountProof, StorageProof, TrieInput};

const ACCOUNT_PROOF_MAGIC: [u8; 4] = *b"OAPI";
const STORAGE_PROOF_MAGIC: [u8; 4] = *b"OSPI";
const HISTORICAL_COMMITTEE_PROOF_MAGIC: [u8; 4] = *b"OCHI";
const WITNESS_VERSION: u16 = 1;

/// Authenticated historical committee opening failure.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct HistoricalCommitteeError {
    message: String,
}

impl HistoricalCommitteeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Authority for opening the committee active at the request's finalized epoch.
///
/// Implementations must authenticate `membership_proof` against finalized
/// consensus history. Returning a snapshot copied from relayer metadata is not
/// a valid implementation.
pub trait HistoricalCommitteeAuthority {
    fn authenticate(
        &self,
        request_state_root: B256,
        epoch: u64,
        committee_set_hash: B256,
        membership_proof: &[u8],
        limits: &SchemaLimits,
    ) -> Result<CommitteeSnapshot, HistoricalCommitteeError>;
}

/// Stateless verifier for the ValidatorSet historical committee opening
/// carried by `FinalizedIntentProofV1`.
///
/// The witness includes the canonical snapshot values plus Ethereum
/// account/storage MPT paths. Values are accepted only after every derived
/// fixed ValidatorSet slot verifies against the finalized request state root.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrieHistoricalCommitteeAuthority;

impl HistoricalCommitteeAuthority for TrieHistoricalCommitteeAuthority {
    fn authenticate(
        &self,
        request_state_root: B256,
        epoch: u64,
        committee_set_hash: B256,
        membership_proof: &[u8],
        limits: &SchemaLimits,
    ) -> Result<CommitteeSnapshot, HistoricalCommitteeError> {
        if membership_proof.len() > limits.max_proof_bytes {
            return Err(HistoricalCommitteeError::new(
                "historical committee witness exceeds proof byte cap",
            ));
        }
        let witness = HistoricalCommitteeWitness::decode(membership_proof, limits)
            .map_err(|error| HistoricalCommitteeError::new(error.to_string()))?;
        if witness.snapshot.committee_set_hash_v2(epoch) != committee_set_hash {
            return Err(HistoricalCommitteeError::new(
                "historical committee snapshot hash mismatch",
            ));
        }

        let account_info = Account {
            nonce: witness.account.nonce,
            balance: witness.account.balance,
            bytecode_hash: Some(witness.account.code_hash),
        };
        AccountProof {
            address: VALIDATOR_SET_ADDRESS,
            info: Some(account_info),
            proof: witness.account.nodes,
            storage_root: witness.account.storage_root,
            storage_proofs: Vec::new(),
        }
        .verify(request_state_root)
        .map_err(|error| {
            HistoricalCommitteeError::new(format!(
                "historical committee account proof rejected: {error}"
            ))
        })?;

        let expected =
            historical_committee_storage_slots(epoch, committee_set_hash, &witness.snapshot);
        if witness.storage.proofs.len() != expected.len() {
            return Err(HistoricalCommitteeError::new(format!(
                "historical committee storage proof count mismatch: expected {}, got {}",
                expected.len(),
                witness.storage.proofs.len()
            )));
        }
        for (index, ((slot, value), nodes)) in
            expected.into_iter().zip(witness.storage.proofs).enumerate()
        {
            StorageProof {
                key: B256::new(slot.to_be_bytes::<32>()),
                value,
                ..StorageProof::new(B256::new(slot.to_be_bytes::<32>()))
            }
            .with_proof(nodes)
            .verify(witness.account.storage_root)
            .map_err(|error| {
                HistoricalCommitteeError::new(format!(
                    "historical committee storage proof {index} rejected: {error}"
                ))
            })?;
        }
        Ok(witness.snapshot)
    }
}

/// Detailed production verifier failures. Protocol callers receive only the
/// stable authority class; task-local tests use this type to pin exact behavior.
#[derive(Debug, thiserror::Error)]
pub enum FinalizedIntentVerifierError {
    #[error("canonical request header exceeds proof byte cap")]
    HeaderTooLarge,
    #[error("canonical request header RLP is invalid: {0}")]
    HeaderDecode(String),
    #[error("canonical request header RLP has trailing bytes")]
    HeaderTrailingBytes,
    #[error("canonical request header RLP is not byte-canonical")]
    HeaderNonCanonical,
    #[error("canonical request header hash does not match finalized metadata")]
    HeaderHashMismatch,
    #[error("canonical request header number does not match finalized metadata")]
    HeaderNumberMismatch,
    #[error("historical committee opening failed: {0}")]
    HistoricalCommittee(#[from] HistoricalCommitteeError),
    #[error("ordered committee entry {index} is not a 20-byte address")]
    CommitteeAddressLength { index: usize },
    #[error("Commonware finalization proof rejected: {0}")]
    ConsensusFinality(String),
    #[error("{kind} witness exceeds proof byte cap")]
    WitnessTooLarge { kind: &'static str },
    #[error("{kind} witness is malformed: {reason}")]
    WitnessMalformed {
        kind: &'static str,
        reason: &'static str,
    },
    #[error("intent account proof rejected: {0}")]
    AccountProof(String),
    #[error("intent storage proof count mismatch: expected {expected}, got {actual}")]
    StorageProofCount { expected: usize, actual: usize },
    #[error("intent storage proof {index} rejected: {reason}")]
    StorageProof { index: usize, reason: String },
    #[error("canonical OCOMP job record encoding failed: {0}")]
    JobRecordEncoding(String),
}

/// Production authority adapter for one finalized OCOMP request.
pub struct FinalizedIntentVerifier<A> {
    historical_committees: A,
}

impl<A> FinalizedIntentVerifier<A> {
    pub const fn new(historical_committees: A) -> Self {
        Self {
            historical_committees,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FinalizedIntentProofBuildError {
    #[error("exact finalized state is unavailable: {0}")]
    State(String),
    #[error("no persisted finalization proof exists for block {block_number} {block_hash}")]
    MissingFinalization { block_number: u64, block_hash: B256 },
    #[error(
        "multiple persisted finalization proofs exist for block {block_number} {block_hash}: {count}"
    )]
    AmbiguousFinalization {
        block_number: u64,
        block_hash: B256,
        count: usize,
    },
    #[error("persisted finalization proof is invalid: {0}")]
    Finalization(&'static str),
    #[error("finalized intent source is invalid: {0}")]
    Intent(String),
    #[error("Ethereum proof construction failed: {0}")]
    Trie(String),
    #[error("constructed finalized-intent proof failed verification: {0}")]
    Verification(#[from] FinalizedIntentVerificationError),
    #[error("constructed raw opening proof failed verification: {0}")]
    RawOpeningVerification(#[source] FinalizedIntentVerifierError),
}

/// Reth-backed proof builder bound to the consensus-owned finalization store.
///
/// It never accepts an event, caller-selected root, or live-tip lookup. The
/// exact finalized block opens both Metadosis intent storage and ValidatorSet
/// committee history, and the constructed proof is run through the production
/// verifier before it is returned.
#[derive(Clone)]
pub struct RethFinalizedIntentProofBuilder<P> {
    provider: P,
    parent_proofs: FinalizedParentCertStore,
    limits: SchemaLimits,
}

impl<P> RethFinalizedIntentProofBuilder<P> {
    pub fn new(provider: P, parent_proofs: FinalizedParentCertStore, limits: SchemaLimits) -> Self {
        Self {
            provider,
            parent_proofs,
            limits,
        }
    }
}

impl<P> RethFinalizedIntentProofBuilder<P>
where
    P: StateProviderFactory + Send + Sync,
{
    pub fn build_and_verify(
        &self,
        block: &ConsensusBlock,
        intent_id: B256,
    ) -> Result<(FinalizedIntentProofV1, VerifiedFinalizedIntentV1), FinalizedIntentProofBuildError>
    {
        self.build_and_verify_header(block.header(), block.block_hash(), intent_id)
    }

    /// Build authority from an exact sealed header recovered after restart.
    ///
    /// The body is not an authority input: the header identifies the finalized
    /// state root and the provider opens the fixed Metadosis/ValidatorSet paths
    /// at that exact block hash.
    pub fn build_and_verify_header(
        &self,
        header: &OutbeHeader,
        block_hash: B256,
        intent_id: B256,
    ) -> Result<(FinalizedIntentProofV1, VerifiedFinalizedIntentV1), FinalizedIntentProofBuildError>
    {
        let block_number = header.number();
        let mut canonical_request_header_rlp = Vec::new();
        header.encode(&mut canonical_request_header_rlp);
        if keccak256(&canonical_request_header_rlp) != block_hash {
            return Err(FinalizedIntentProofBuildError::Finalization(
                "recovered header hash does not match candidate",
            ));
        }
        let state = self
            .provider
            .state_by_block_hash(block_hash)
            .map_err(|error| {
                FinalizedIntentProofBuildError::State(format!(
                    "open block {} {}: {error}",
                    block_number, block_hash
                ))
            })?;
        let record = read_pending_job_record(state.as_ref(), intent_id, &self.limits)?;
        let finalization =
            exact_finalization_record(&self.parent_proofs, block_number, block_hash)?;
        let parent_accounting = protocol_parent_accounting(&finalization)?;
        let snapshot =
            read_historical_committee_snapshot(state.as_ref(), &finalization, &self.limits)?;

        let canonical_record = record
            .encode_canonical(&self.limits)
            .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?;
        let logical_key = outbe_ocomp_protocol::intent::intent_storage_key(intent_id)
            .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?;
        let intent_slots = storage_bytes_slots(logical_key, &canonical_record);
        let intent_proof = state
            .proof(
                TrieInput::default(),
                METADOSIS_ADDRESS,
                &slot_keys(&intent_slots),
            )
            .map_err(|error| {
                FinalizedIntentProofBuildError::Trie(format!(
                    "build Metadosis intent proof: {error}"
                ))
            })?;

        let committee_slots = historical_committee_storage_slots(
            finalization.finalized_epoch,
            finalization.committee_set_hash,
            &snapshot,
        );
        let committee_proof = state
            .proof(
                TrieInput::default(),
                VALIDATOR_SET_ADDRESS,
                &slot_keys(&committee_slots),
            )
            .map_err(|error| {
                FinalizedIntentProofBuildError::Trie(format!(
                    "build ValidatorSet committee proof: {error}"
                ))
            })?;

        let proof = FinalizedIntentProofV1 {
            chain_id: record.intent.chain_id,
            genesis_hash: record.intent.genesis_hash,
            fork_id: record.intent.fork_id,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            canonical_request_header_rlp: ProofBytes(canonical_request_header_rlp),
            parent_accounting,
            historical_committee_membership_proof: ProofBytes(encode_historical_committee_witness(
                &snapshot,
                &committee_proof,
                &committee_slots,
            )?),
            canonical_job_intent: BoundedBytes(
                record
                    .intent
                    .encode_canonical(&self.limits)
                    .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?,
            ),
            intent_account_proof: ProofBytes(encode_account_witness(
                &intent_proof,
                METADOSIS_ADDRESS,
            )?),
            intent_storage_proof: ProofBytes(encode_storage_witness(&intent_proof, &intent_slots)?),
        };
        proof
            .encode_canonical(&self.limits)
            .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?;
        let expected = ExpectedFinalizedIntentBindingV1 {
            chain_id: record.intent.chain_id,
            genesis_hash: record.intent.genesis_hash,
            fork_id: record.intent.fork_id,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
        };
        let verified = proof.verify(
            expected,
            &FinalizedIntentVerifier::new(TrieHistoricalCommitteeAuthority),
            &self.limits,
        )?;
        if verified.intent_id != intent_id {
            return Err(FinalizedIntentProofBuildError::Intent(
                "constructed proof opened a different IntentId".to_owned(),
            ));
        }
        Ok((proof, verified))
    }
}

impl<A: HistoricalCommitteeAuthority> FinalizedIntentVerifier<A> {
    pub fn verify_request(
        &self,
        proof: &FinalizedIntentProofV1,
        limits: &SchemaLimits,
    ) -> Result<FinalizedRequestBindingV1, FinalizedIntentVerifierError> {
        if proof.canonical_request_header_rlp.0.len() > limits.max_proof_bytes {
            return Err(FinalizedIntentVerifierError::HeaderTooLarge);
        }

        let encoded_header = proof.canonical_request_header_rlp.0.as_slice();
        let mut input = encoded_header;
        let header = OutbeHeader::decode(&mut input)
            .map_err(|error| FinalizedIntentVerifierError::HeaderDecode(error.to_string()))?;
        if !input.is_empty() {
            return Err(FinalizedIntentVerifierError::HeaderTrailingBytes);
        }
        let mut canonical = Vec::new();
        header.encode(&mut canonical);
        if canonical != encoded_header {
            return Err(FinalizedIntentVerifierError::HeaderNonCanonical);
        }

        let metadata = &proof.parent_accounting;
        let header_hash = keccak256(&canonical);
        if header_hash != metadata.finalized_block_hash {
            return Err(FinalizedIntentVerifierError::HeaderHashMismatch);
        }
        if header.inner.number != metadata.finalized_block_number {
            return Err(FinalizedIntentVerifierError::HeaderNumberMismatch);
        }

        let snapshot = self.historical_committees.authenticate(
            header.inner.state_root,
            metadata.finalized_epoch,
            metadata.committee_set_hash,
            &proof.historical_committee_membership_proof.0,
            limits,
        )?;
        let consensus_metadata = consensus_metadata(metadata)?;
        verify_v2_proof(
            &consensus_metadata,
            &snapshot,
            &metadata.canonical_commonware_finalization_proof.0,
            header_hash,
        )
        .map_err(|error| FinalizedIntentVerifierError::ConsensusFinality(error.to_string()))?;

        Ok(FinalizedRequestBindingV1 {
            block_number: header.inner.number,
            block_hash: header_hash,
            state_root: header.inner.state_root,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn verify_storage(
        &self,
        proof: &FinalizedIntentProofV1,
        intent: &JobIntentV1,
        intent_id: B256,
        intent_storage_key: B256,
        request_state_root: B256,
        limits: &SchemaLimits,
    ) -> Result<(), FinalizedIntentVerifierError> {
        ensure_witness_cap(
            "account",
            proof.intent_account_proof.0.len(),
            limits.max_proof_bytes,
        )?;
        ensure_witness_cap(
            "storage",
            proof.intent_storage_proof.0.len(),
            limits.max_proof_bytes,
        )?;

        let account = AccountWitness::decode(&proof.intent_account_proof.0, limits)?;
        let account_info = Account {
            nonce: account.nonce,
            balance: account.balance,
            bytecode_hash: Some(account.code_hash),
        };
        AccountProof {
            address: METADOSIS_ADDRESS,
            info: Some(account_info),
            proof: account.nodes,
            storage_root: account.storage_root,
            storage_proofs: Vec::new(),
        }
        .verify(request_state_root)
        .map_err(|error| FinalizedIntentVerifierError::AccountProof(error.to_string()))?;

        let record = OcompJobRecordV1 {
            intent: intent.clone(),
            status: OcompJobStatus::OffchainPending,
            terminal: None,
        };
        let encoded_record = record
            .encode_canonical(limits)
            .map_err(|error| FinalizedIntentVerifierError::JobRecordEncoding(error.to_string()))?;
        let expected_slots = storage_bytes_slots(intent_storage_key, &encoded_record);
        let storage =
            StorageWitness::decode(&proof.intent_storage_proof.0, limits, expected_slots.len())?;
        if storage.proofs.len() != expected_slots.len() {
            return Err(FinalizedIntentVerifierError::StorageProofCount {
                expected: expected_slots.len(),
                actual: storage.proofs.len(),
            });
        }

        // Recompute the binding instead of trusting the caller's already-derived
        // value at the fixed Metadosis path.
        let recomputed = outbe_ocomp_protocol::intent::intent_storage_key(intent_id)
            .map_err(|error| FinalizedIntentVerifierError::JobRecordEncoding(error.to_string()))?;
        if recomputed != intent_storage_key {
            return Err(FinalizedIntentVerifierError::WitnessMalformed {
                kind: "storage",
                reason: "intent storage key binding",
            });
        }

        for (index, ((slot, word), nodes)) in expected_slots
            .into_iter()
            .zip(storage.proofs.iter())
            .enumerate()
        {
            let storage_proof = StorageProof {
                key: B256::new(slot.to_be_bytes::<32>()),
                value: word,
                ..StorageProof::new(B256::new(slot.to_be_bytes::<32>()))
            }
            .with_proof(nodes.clone());
            storage_proof
                .verify(account.storage_root)
                .map_err(|error| FinalizedIntentVerifierError::StorageProof {
                    index,
                    reason: error.to_string(),
                })?;
        }
        Ok(())
    }
}

impl<A: HistoricalCommitteeAuthority> FinalizedIntentProofAuthority for FinalizedIntentVerifier<A> {
    fn verify_finalized_request(
        &self,
        proof: &FinalizedIntentProofV1,
        limits: &SchemaLimits,
    ) -> Result<FinalizedRequestBindingV1, FinalizedIntentAuthorityError> {
        self.verify_request(proof, limits)
            .map_err(classify_request_error)
    }

    fn verify_intent_inclusion(
        &self,
        proof: &FinalizedIntentProofV1,
        intent: &JobIntentV1,
        intent_id: B256,
        intent_storage_key: B256,
        request_state_root: B256,
        limits: &SchemaLimits,
    ) -> Result<(), FinalizedIntentAuthorityError> {
        self.verify_storage(
            proof,
            intent,
            intent_id,
            intent_storage_key,
            request_state_root,
            limits,
        )
        .map_err(classify_storage_error)
    }
}

fn classify_request_error(error: FinalizedIntentVerifierError) -> FinalizedIntentAuthorityError {
    match error {
        FinalizedIntentVerifierError::HeaderTooLarge
        | FinalizedIntentVerifierError::HeaderDecode(_)
        | FinalizedIntentVerifierError::HeaderTrailingBytes
        | FinalizedIntentVerifierError::HeaderNonCanonical
        | FinalizedIntentVerifierError::HeaderHashMismatch
        | FinalizedIntentVerifierError::HeaderNumberMismatch => {
            FinalizedIntentAuthorityError::CanonicalHeader
        }
        FinalizedIntentVerifierError::HistoricalCommittee(_)
        | FinalizedIntentVerifierError::CommitteeAddressLength { .. } => {
            FinalizedIntentAuthorityError::HistoricalCommittee
        }
        FinalizedIntentVerifierError::ConsensusFinality(_) => {
            FinalizedIntentAuthorityError::ConsensusFinality
        }
        _ => FinalizedIntentAuthorityError::ConsensusFinality,
    }
}

fn classify_storage_error(error: FinalizedIntentVerifierError) -> FinalizedIntentAuthorityError {
    match error {
        FinalizedIntentVerifierError::AccountProof(_)
        | FinalizedIntentVerifierError::WitnessTooLarge { kind: "account" }
        | FinalizedIntentVerifierError::WitnessMalformed {
            kind: "account", ..
        } => FinalizedIntentAuthorityError::IntentAccountProof,
        _ => FinalizedIntentAuthorityError::IntentStorageProof,
    }
}

fn consensus_metadata(
    metadata: &outbe_ocomp_protocol::intent::CertifiedParentAccountingMetadataV2,
) -> Result<CertifiedParentAccountingMetadata, FinalizedIntentVerifierError> {
    let mut ordered_committee = Vec::with_capacity(metadata.ordered_committee.len());
    for (index, encoded) in metadata.ordered_committee.iter().enumerate() {
        if encoded.0.len() != 20 {
            return Err(FinalizedIntentVerifierError::CommitteeAddressLength { index });
        }
        ordered_committee.push(Address::from_slice(&encoded.0));
    }
    let proof_kind = match metadata.proof_kind {
        ParentProofKind::Finalization => ParentParticipationProof::Finalization,
    };
    Ok(CertifiedParentAccountingMetadata {
        finalized_block_number: metadata.finalized_block_number,
        finalized_block_hash: metadata.finalized_block_hash,
        finalized_epoch: metadata.finalized_epoch,
        finalized_view: metadata.finalized_view,
        parent_view: metadata.parent_view,
        ordered_committee,
        signer_bitmap: metadata.signer_bitmap.0.clone(),
        proof: Bytes::copy_from_slice(&metadata.canonical_commonware_finalization_proof.0),
        committee_set_hash: metadata.committee_set_hash,
        vrf_material_version: u64::from(metadata.vrf_material_version),
        vrf_group_public_key_hash: metadata.vrf_group_public_key_hash,
        proof_kind,
        missed_proposers: Vec::new(),
    })
}

fn exact_finalization_record(
    store: &FinalizedParentCertStore,
    block_number: u64,
    block_hash: B256,
) -> Result<CertifiedParentProofRecord, FinalizedIntentProofBuildError> {
    let records = store.finalizations_for_block(block_number, block_hash);
    match records.len() {
        0 => Err(FinalizedIntentProofBuildError::MissingFinalization {
            block_number,
            block_hash,
        }),
        1 => Ok(records.into_iter().next().expect("length checked")),
        count => Err(FinalizedIntentProofBuildError::AmbiguousFinalization {
            block_number,
            block_hash,
            count,
        }),
    }
}

fn protocol_parent_accounting(
    record: &CertifiedParentProofRecord,
) -> Result<CertifiedParentAccountingMetadataV2, FinalizedIntentProofBuildError> {
    let finalized_block_number = match record.kind {
        ProofKind::Finalization {
            finalized_block_number,
        } => finalized_block_number,
        ProofKind::CertifiedNotarization => {
            return Err(FinalizedIntentProofBuildError::Finalization(
                "certified notarization cannot authorize an OCOMP JobId",
            ));
        }
    };
    let vrf_material_version = u16::try_from(record.vrf_material_version).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("VRF material version exceeds PoC u16 shape")
    })?;
    Ok(CertifiedParentAccountingMetadataV2 {
        finalized_block_number,
        finalized_block_hash: record.finalized_block_hash,
        finalized_epoch: record.finalized_epoch,
        finalized_view: record.finalized_view,
        parent_view: record.parent_view,
        ordered_committee: record
            .ordered_committee
            .iter()
            .map(|address| BoundedBytes(address.as_slice().to_vec()))
            .collect(),
        signer_bitmap: BoundedBytes(record.signer_bitmap.clone()),
        canonical_commonware_finalization_proof: ProofBytes(record.encoded_proof.to_vec()),
        committee_set_hash: record.committee_set_hash,
        vrf_material_version,
        vrf_group_public_key_hash: record.vrf_group_public_key_hash,
        proof_kind: ParentProofKind::Finalization,
        missed_proposers: Vec::new(),
    })
}

fn read_pending_job_record(
    state: &dyn StateProvider,
    intent_id: B256,
    limits: &SchemaLimits,
) -> Result<OcompJobRecordV1, FinalizedIntentProofBuildError> {
    let logical_key = outbe_ocomp_protocol::intent::intent_storage_key(intent_id)
        .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?;
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    let encoded = read_storage_bytes(state, METADOSIS_ADDRESS, base, limits.max_bounded_bytes)?;
    let record = OcompJobRecordV1::decode_canonical(&encoded, limits)
        .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?;
    let decoded_id = record
        .intent
        .intent_id(limits)
        .map_err(|error| FinalizedIntentProofBuildError::Intent(error.to_string()))?;
    if decoded_id != intent_id || record.status != OcompJobStatus::OffchainPending {
        return Err(FinalizedIntentProofBuildError::Intent(
            "exact state does not contain the requested pending intent".to_owned(),
        ));
    }
    Ok(record)
}

fn read_storage_bytes(
    state: &dyn StateProvider,
    address: Address,
    base: U256,
    max_len: usize,
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    let head = state
        .storage(address, B256::new(base.to_be_bytes::<32>()))
        .map_err(|error| FinalizedIntentProofBuildError::State(error.to_string()))?
        .unwrap_or_default();
    if !head.bit(0) {
        let encoded = head.to_be_bytes::<32>();
        let len = usize::from(encoded[31] / 2);
        if len > 31 || len > max_len {
            return Err(FinalizedIntentProofBuildError::Intent(
                "inline StorageBytes length exceeds bound".to_owned(),
            ));
        }
        return Ok(encoded[..len].to_vec());
    }

    let len_word = head.checked_sub(U256::from(1)).ok_or_else(|| {
        FinalizedIntentProofBuildError::Intent("invalid StorageBytes length word".to_owned())
    })? / U256::from(2);
    let len = usize::try_from(len_word).map_err(|_| {
        FinalizedIntentProofBuildError::Intent("StorageBytes length exceeds usize".to_owned())
    })?;
    if len <= 31 || len > max_len {
        return Err(FinalizedIntentProofBuildError::Intent(
            "external StorageBytes length is non-canonical or exceeds bound".to_owned(),
        ));
    }
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    let mut encoded = Vec::with_capacity(len);
    for offset in 0..len.div_ceil(32) {
        let word = state
            .storage(
                address,
                B256::new((data_base + U256::from(offset)).to_be_bytes::<32>()),
            )
            .map_err(|error| FinalizedIntentProofBuildError::State(error.to_string()))?
            .unwrap_or_default()
            .to_be_bytes::<32>();
        encoded.extend_from_slice(&word);
    }
    encoded.truncate(len);
    Ok(encoded)
}

fn read_historical_committee_snapshot(
    state: &dyn StateProvider,
    record: &CertifiedParentProofRecord,
    limits: &SchemaLimits,
) -> Result<CommitteeSnapshot, FinalizedIntentProofBuildError> {
    let key = committee_snapshot_key(record.finalized_epoch, record.committee_set_hash);
    let mapped = |base_slot: u64| key.mapping_slot(U256::from(base_slot));
    let nested = |base_slot: u64, index: u64| index.mapping_slot(mapped(base_slot));
    let word = |slot: U256| -> Result<U256, FinalizedIntentProofBuildError> {
        state
            .storage(VALIDATOR_SET_ADDRESS, B256::new(slot.to_be_bytes::<32>()))
            .map_err(|error| FinalizedIntentProofBuildError::State(error.to_string()))
            .map(Option::unwrap_or_default)
    };

    if word(mapped(31))? != U256::from(1) {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee snapshot is absent",
        ));
    }
    let committee_len = usize::try_from(word(mapped(32))?).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("historical committee length exceeds usize")
    })?;
    if committee_len == 0
        || committee_len > limits.max_collection_items
        || committee_len != record.ordered_committee.len()
    {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee length is outside the exact persisted shape",
        ));
    }
    let mut committee = Vec::with_capacity(committee_len);
    for index in 0..committee_len {
        let address_word = word(nested(33, index as u64))?.to_be_bytes::<32>();
        if address_word[..12].iter().any(|byte| *byte != 0) {
            return Err(FinalizedIntentProofBuildError::Finalization(
                "historical committee address has non-zero high bytes",
            ));
        }
        let low = word(nested(34, index as u64))?.to_be_bytes::<32>();
        let high = word(nested(35, index as u64))?.to_be_bytes::<32>();
        if high[16..].iter().any(|byte| *byte != 0) {
            return Err(FinalizedIntentProofBuildError::Finalization(
                "historical committee BLS suffix is not right padded",
            ));
        }
        let mut consensus_pubkey = [0_u8; 48];
        consensus_pubkey[..32].copy_from_slice(&low);
        consensus_pubkey[32..].copy_from_slice(&high[..16]);
        committee.push(CommitteeEntry {
            address: Address::from_slice(&address_word[12..]),
            consensus_pubkey,
        });
    }
    let vrf_material_version = u64::try_from(word(mapped(36))?).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("VRF material version exceeds u64")
    })?;
    let vrf_key_len = usize::try_from(word(mapped(38))?).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("VRF group key length exceeds usize")
    })?;
    if vrf_key_len == 0 || vrf_key_len > limits.max_proof_bytes {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "VRF group key length is outside proof bounds",
        ));
    }
    let mut vrf_group_public_key_bytes = Vec::with_capacity(vrf_key_len);
    for index in 0..vrf_key_len.div_ceil(32) {
        let chunk = word(nested(39, index as u64))?.to_be_bytes::<32>();
        vrf_group_public_key_bytes.extend_from_slice(&chunk);
    }
    vrf_group_public_key_bytes.truncate(vrf_key_len);
    if word(mapped(37))? != U256::from_be_bytes(keccak256(&vrf_group_public_key_bytes).0) {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "VRF group key hash does not match snapshot bytes",
        ));
    }
    let snapshot = CommitteeSnapshot {
        committee,
        vrf_material_version,
        vrf_group_public_key_bytes,
        vrf_public_polynomial_hash: B256::new(word(mapped(47))?.to_be_bytes::<32>()),
    };
    if snapshot.committee_set_hash_v2(record.finalized_epoch) != record.committee_set_hash
        || snapshot
            .committee
            .iter()
            .map(|entry| entry.address)
            .ne(record.ordered_committee.iter().copied())
        || snapshot.vrf_material_version != record.vrf_material_version
        || keccak256(&snapshot.vrf_group_public_key_bytes) != record.vrf_group_public_key_hash
    {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee snapshot does not match persisted finalization metadata",
        ));
    }
    Ok(snapshot)
}

fn slot_keys(slots: &[(U256, U256)]) -> Vec<B256> {
    slots
        .iter()
        .map(|(slot, _)| B256::new(slot.to_be_bytes::<32>()))
        .collect()
}

pub(crate) fn build_verified_raw_contract_opening(
    state: &dyn StateProvider,
    state_root: B256,
    contract_address: Address,
    ordered_slots: &[B256],
    limits: &SchemaLimits,
) -> Result<RawContractOpeningProofV1, FinalizedIntentProofBuildError> {
    if ordered_slots.is_empty() || ordered_slots.len() > limits.max_collection_items {
        return Err(FinalizedIntentProofBuildError::Trie(
            "raw opening slot count is outside the bounded profile".to_owned(),
        ));
    }
    let expected_slots = ordered_slots
        .iter()
        .copied()
        .map(|slot| {
            state
                .storage(contract_address, slot)
                .map(|value| (U256::from_be_bytes(slot.0), value.unwrap_or_default()))
                .map_err(|error| {
                    FinalizedIntentProofBuildError::State(format!(
                        "read raw opening slot {slot}: {error}"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let proof = state
        .proof(TrieInput::default(), contract_address, ordered_slots)
        .map_err(|error| {
            FinalizedIntentProofBuildError::Trie(format!(
                "build raw opening proof for {contract_address}: {error}"
            ))
        })?;
    let opening = RawContractOpeningProofV1 {
        contract_address,
        state_root,
        ordered_slots: expected_slots
            .iter()
            .map(|(slot, value)| RawStorageSlotV1 {
                slot: B256::new(slot.to_be_bytes()),
                value: *value,
            })
            .collect(),
        account_proof: ProofBytes(encode_account_witness(&proof, contract_address)?),
        storage_proof: ProofBytes(encode_storage_witness(&proof, &expected_slots)?),
    };
    verify_raw_contract_opening(
        &opening,
        contract_address,
        state_root,
        ordered_slots,
        limits,
    )
    .map_err(FinalizedIntentProofBuildError::RawOpeningVerification)?;
    Ok(opening)
}

pub fn verify_raw_contract_opening(
    opening: &RawContractOpeningProofV1,
    expected_address: Address,
    expected_state_root: B256,
    expected_slots: &[B256],
    limits: &SchemaLimits,
) -> Result<(), FinalizedIntentVerifierError> {
    if opening.contract_address != expected_address
        || opening.state_root != expected_state_root
        || opening.ordered_slots.len() != expected_slots.len()
        || opening
            .ordered_slots
            .iter()
            .zip(expected_slots)
            .any(|(actual, expected)| actual.slot != *expected)
    {
        return Err(FinalizedIntentVerifierError::WitnessMalformed {
            kind: "raw opening",
            reason: "contract, root, or ordered slot binding",
        });
    }
    ensure_witness_cap(
        "account",
        opening.account_proof.0.len(),
        limits.max_proof_bytes,
    )?;
    ensure_witness_cap(
        "storage",
        opening.storage_proof.0.len(),
        limits.max_proof_bytes,
    )?;

    let account = AccountWitness::decode(&opening.account_proof.0, limits)?;
    let account_info = Account {
        nonce: account.nonce,
        balance: account.balance,
        bytecode_hash: Some(account.code_hash),
    };
    AccountProof {
        address: expected_address,
        info: Some(account_info),
        proof: account.nodes,
        storage_root: account.storage_root,
        storage_proofs: Vec::new(),
    }
    .verify(expected_state_root)
    .map_err(|error| FinalizedIntentVerifierError::AccountProof(error.to_string()))?;

    let storage = StorageWitness::decode(
        &opening.storage_proof.0,
        limits,
        opening.ordered_slots.len(),
    )?;
    if storage.proofs.len() != opening.ordered_slots.len() {
        return Err(FinalizedIntentVerifierError::StorageProofCount {
            expected: opening.ordered_slots.len(),
            actual: storage.proofs.len(),
        });
    }
    for (index, (raw, nodes)) in opening.ordered_slots.iter().zip(storage.proofs).enumerate() {
        StorageProof {
            key: raw.slot,
            value: raw.value,
            ..StorageProof::new(raw.slot)
        }
        .with_proof(nodes)
        .verify(account.storage_root)
        .map_err(|error| FinalizedIntentVerifierError::StorageProof {
            index,
            reason: error.to_string(),
        })?;
    }
    Ok(())
}

fn encode_account_witness(
    proof: &AccountProof,
    expected_address: Address,
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    if proof.address != expected_address {
        return Err(FinalizedIntentProofBuildError::Trie(
            "account proof address mismatch".to_owned(),
        ));
    }
    let account = proof.info.as_ref().ok_or_else(|| {
        FinalizedIntentProofBuildError::Trie("account proof opened no account".to_owned())
    })?;
    let code_hash = account.get_bytecode_hash();
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&ACCOUNT_PROOF_MAGIC);
    encoded.extend_from_slice(&WITNESS_VERSION.to_be_bytes());
    encoded.extend_from_slice(&account.nonce.to_be_bytes());
    encoded.extend_from_slice(&account.balance.to_be_bytes::<32>());
    encoded.extend_from_slice(proof.storage_root.as_slice());
    encoded.extend_from_slice(code_hash.as_slice());
    encode_nodes(&mut encoded, &proof.proof)?;
    Ok(encoded)
}

fn encode_storage_witness(
    proof: &AccountProof,
    expected_slots: &[(U256, U256)],
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&STORAGE_PROOF_MAGIC);
    encoded.extend_from_slice(&WITNESS_VERSION.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(expected_slots.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("storage proof count exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    for (slot, value) in expected_slots {
        let key = B256::new(slot.to_be_bytes::<32>());
        let matches = proof
            .storage_proofs
            .iter()
            .filter(|candidate| candidate.key == key)
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].value != *value {
            return Err(FinalizedIntentProofBuildError::Trie(
                "account proof does not contain one exact requested storage value".to_owned(),
            ));
        }
        encode_nodes(&mut encoded, &matches[0].proof)?;
    }
    Ok(encoded)
}

fn encode_nodes(
    encoded: &mut Vec<u8>,
    nodes: &[Bytes],
) -> Result<(), FinalizedIntentProofBuildError> {
    encoded.extend_from_slice(
        &u32::try_from(nodes.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("proof node count exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    for node in nodes {
        encoded.extend_from_slice(
            &u32::try_from(node.len())
                .map_err(|_| {
                    FinalizedIntentProofBuildError::Trie("proof node length exceeds u32".to_owned())
                })?
                .to_be_bytes(),
        );
        encoded.extend_from_slice(node);
    }
    Ok(())
}

fn encode_historical_committee_witness(
    snapshot: &CommitteeSnapshot,
    proof: &AccountProof,
    expected_slots: &[(U256, U256)],
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&HISTORICAL_COMMITTEE_PROOF_MAGIC);
    encoded.extend_from_slice(&WITNESS_VERSION.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(snapshot.committee.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("committee length exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    for entry in &snapshot.committee {
        encoded.extend_from_slice(entry.address.as_slice());
        encoded.extend_from_slice(&entry.consensus_pubkey);
    }
    encoded.extend_from_slice(&snapshot.vrf_material_version.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(snapshot.vrf_group_public_key_bytes.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("VRF group key length exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&snapshot.vrf_group_public_key_bytes);
    encoded.extend_from_slice(snapshot.vrf_public_polynomial_hash.as_slice());

    let account = encode_account_witness(proof, VALIDATOR_SET_ADDRESS)?;
    encoded.extend_from_slice(
        &u32::try_from(account.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie(
                    "committee account witness length exceeds u32".to_owned(),
                )
            })?
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&account);
    let storage = encode_storage_witness(proof, expected_slots)?;
    encoded.extend_from_slice(
        &u32::try_from(storage.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie(
                    "committee storage witness length exceeds u32".to_owned(),
                )
            })?
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&storage);
    Ok(encoded)
}

fn ensure_witness_cap(
    kind: &'static str,
    actual: usize,
    limit: usize,
) -> Result<(), FinalizedIntentVerifierError> {
    if actual > limit {
        return Err(FinalizedIntentVerifierError::WitnessTooLarge { kind });
    }
    Ok(())
}

struct AccountWitness {
    nonce: u64,
    balance: U256,
    storage_root: B256,
    code_hash: B256,
    nodes: Vec<Bytes>,
}

impl AccountWitness {
    fn decode(encoded: &[u8], limits: &SchemaLimits) -> Result<Self, FinalizedIntentVerifierError> {
        let mut input = WitnessReader::new(encoded, "account");
        input.expect_magic(ACCOUNT_PROOF_MAGIC)?;
        input.expect_version()?;
        let nonce = input.read_u64()?;
        let balance = U256::from_be_bytes(input.read_array::<32>()?);
        let storage_root = B256::new(input.read_array::<32>()?);
        let code_hash = B256::new(input.read_array::<32>()?);
        let nodes = input.read_nodes(limits)?;
        input.finish()?;
        Ok(Self {
            nonce,
            balance,
            storage_root,
            code_hash,
            nodes,
        })
    }
}

struct StorageWitness {
    proofs: Vec<Vec<Bytes>>,
}

impl StorageWitness {
    fn decode(
        encoded: &[u8],
        limits: &SchemaLimits,
        expected_proofs: usize,
    ) -> Result<Self, FinalizedIntentVerifierError> {
        let mut input = WitnessReader::new(encoded, "storage");
        input.expect_magic(STORAGE_PROOF_MAGIC)?;
        input.expect_version()?;
        let proof_count = input.read_count(limits.max_collection_items)?;
        if proof_count != expected_proofs {
            return Err(FinalizedIntentVerifierError::StorageProofCount {
                expected: expected_proofs,
                actual: proof_count,
            });
        }
        let mut proofs = Vec::with_capacity(proof_count);
        for _ in 0..proof_count {
            proofs.push(input.read_nodes(limits)?);
        }
        input.finish()?;
        Ok(Self { proofs })
    }
}

struct HistoricalCommitteeWitness {
    snapshot: CommitteeSnapshot,
    account: AccountWitness,
    storage: StorageWitness,
}

impl HistoricalCommitteeWitness {
    fn decode(encoded: &[u8], limits: &SchemaLimits) -> Result<Self, FinalizedIntentVerifierError> {
        let mut input = WitnessReader::new(encoded, "historical committee");
        input.expect_magic(HISTORICAL_COMMITTEE_PROOF_MAGIC)?;
        input.expect_version()?;
        let committee_len = input.read_count(limits.max_collection_items)?;
        let mut committee = Vec::with_capacity(committee_len);
        for _ in 0..committee_len {
            committee.push(CommitteeEntry {
                address: Address::from(input.read_array::<20>()?),
                consensus_pubkey: input.read_array::<48>()?,
            });
        }
        let vrf_material_version = input.read_u64()?;
        let vrf_key_len = input.read_count(limits.max_proof_bytes)?;
        let vrf_group_public_key_bytes = input.take(vrf_key_len)?.to_vec();
        let vrf_public_polynomial_hash = B256::new(input.read_array::<32>()?);

        let account_len = input.read_count(limits.max_proof_bytes)?;
        let account = AccountWitness::decode(input.take(account_len)?, limits)?;
        let snapshot = CommitteeSnapshot {
            committee,
            vrf_material_version,
            vrf_group_public_key_bytes,
            vrf_public_polynomial_hash,
        };
        let expected_storage = historical_committee_storage_slots(0, B256::ZERO, &snapshot).len();
        let storage_len = input.read_count(limits.max_proof_bytes)?;
        let storage = StorageWitness::decode(input.take(storage_len)?, limits, expected_storage)?;
        input.finish()?;
        Ok(Self {
            snapshot,
            account,
            storage,
        })
    }
}

struct WitnessReader<'a> {
    encoded: &'a [u8],
    offset: usize,
    kind: &'static str,
}

impl<'a> WitnessReader<'a> {
    const fn new(encoded: &'a [u8], kind: &'static str) -> Self {
        Self {
            encoded,
            offset: 0,
            kind,
        }
    }

    fn expect_magic(&mut self, expected: [u8; 4]) -> Result<(), FinalizedIntentVerifierError> {
        if self.read_array::<4>()? != expected {
            return self.malformed("magic");
        }
        Ok(())
    }

    fn expect_version(&mut self) -> Result<(), FinalizedIntentVerifierError> {
        if self.read_u16()? != WITNESS_VERSION {
            return self.malformed("version");
        }
        Ok(())
    }

    fn read_nodes(
        &mut self,
        limits: &SchemaLimits,
    ) -> Result<Vec<Bytes>, FinalizedIntentVerifierError> {
        let count = self.read_count(limits.max_collection_items)?;
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            let len = self.read_count(limits.max_proof_bytes)?;
            nodes.push(Bytes::copy_from_slice(self.take(len)?));
        }
        Ok(nodes)
    }

    fn read_count(&mut self, limit: usize) -> Result<usize, FinalizedIntentVerifierError> {
        let value =
            usize::try_from(self.read_u32()?).map_err(|_| self.error("count conversion"))?;
        if value > limit {
            return self.malformed("count cap");
        }
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16, FinalizedIntentVerifierError> {
        Ok(u16::from_be_bytes(self.read_array::<2>()?))
    }

    fn read_u32(&mut self) -> Result<u32, FinalizedIntentVerifierError> {
        Ok(u32::from_be_bytes(self.read_array::<4>()?))
    }

    fn read_u64(&mut self) -> Result<u64, FinalizedIntentVerifierError> {
        Ok(u64::from_be_bytes(self.read_array::<8>()?))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], FinalizedIntentVerifierError> {
        self.take(N)?
            .try_into()
            .map_err(|_| self.error("truncated fixed field"))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], FinalizedIntentVerifierError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| self.error("offset overflow"))?;
        if end > self.encoded.len() {
            return self.malformed("truncated field");
        }
        let bytes = &self.encoded[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn finish(&self) -> Result<(), FinalizedIntentVerifierError> {
        if self.offset != self.encoded.len() {
            return self.malformed("trailing bytes");
        }
        Ok(())
    }

    fn malformed<T>(&self, reason: &'static str) -> Result<T, FinalizedIntentVerifierError> {
        Err(self.error(reason))
    }

    const fn error(&self, reason: &'static str) -> FinalizedIntentVerifierError {
        FinalizedIntentVerifierError::WitnessMalformed {
            kind: self.kind,
            reason,
        }
    }
}

fn historical_committee_storage_slots(
    epoch: u64,
    committee_set_hash: B256,
    snapshot: &CommitteeSnapshot,
) -> Vec<(U256, U256)> {
    let snapshot_key = committee_snapshot_key(epoch, committee_set_hash);
    let mapped = |base_slot: u64| snapshot_key.mapping_slot(U256::from(base_slot));
    let nested = |base_slot: u64, index: u64| index.mapping_slot(mapped(base_slot));
    let mut slots = vec![
        (mapped(31), U256::from(1)),
        (mapped(32), U256::from(snapshot.committee.len())),
    ];
    for (index, entry) in snapshot.committee.iter().enumerate() {
        let index = index as u64;
        let mut high = [0_u8; 32];
        high[..16].copy_from_slice(&entry.consensus_pubkey[32..]);
        slots.extend([
            (
                nested(33, index),
                U256::from_be_slice(entry.address.as_slice()),
            ),
            (
                nested(34, index),
                U256::from_be_bytes::<32>(
                    entry.consensus_pubkey[..32]
                        .try_into()
                        .expect("fixed BLS public key prefix"),
                ),
            ),
            (nested(35, index), U256::from_be_bytes(high)),
        ]);
    }
    slots.extend([
        (mapped(36), U256::from(snapshot.vrf_material_version)),
        (
            mapped(37),
            U256::from_be_bytes(keccak256(&snapshot.vrf_group_public_key_bytes).0),
        ),
        (
            mapped(38),
            U256::from(snapshot.vrf_group_public_key_bytes.len()),
        ),
    ]);
    for (index, chunk) in snapshot.vrf_group_public_key_bytes.chunks(32).enumerate() {
        let mut word = [0_u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        slots.push((nested(39, index as u64), U256::from_be_bytes(word)));
    }
    slots.push((
        mapped(47),
        U256::from_be_bytes(snapshot.vrf_public_polynomial_hash.0),
    ));
    slots
}

fn storage_bytes_slots(logical_key: B256, encoded: &[u8]) -> Vec<(U256, U256)> {
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    if encoded.len() <= 31 {
        let mut inline = [0_u8; 32];
        inline[..encoded.len()].copy_from_slice(encoded);
        inline[31] = (encoded.len() * 2) as u8;
        return vec![(base, U256::from_be_bytes(inline))];
    }

    let mut slots = Vec::with_capacity(1 + encoded.len().div_ceil(32));
    slots.push((base, U256::from(encoded.len() * 2 + 1)));
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    for (index, chunk) in encoded.chunks(32).enumerate() {
        let mut word = [0_u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        slots.push((data_base + U256::from(index), U256::from_be_bytes(word)));
    }
    slots
}
