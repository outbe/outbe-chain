use alloy_primitives::{Address, Bytes, B256, U256};

use crate::{
    consensus::{DkgBoundaryArtifact, OUTBE_MAX_EXTRA_DATA_SIZE},
    error::{ReshareArtifactError, Result},
    validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS,
};

mod consensus;
mod fields;
mod late_finalize;
mod records;

#[cfg(test)]
mod publication_tests;
#[cfg(test)]
mod regression;

const MAGIC: &[u8; 4] = b"OART";
/// Initial OART v1 format. Header: magic (4 bytes), version, flags, record count.
/// Flag bit 0 enables repository publication. All other flag bits are invalid.
/// Each record has a tag and a u16 payload length.
const VERSION: u8 = 1;
const HEADER_LEN: usize = 7;
const PUBLICATION_FLAG: u8 = 1;
const RADICLE_PUBLICATION_TAG: u8 = 0x09;
pub const ENCLAVE_WITNESS_TAG: u8 = 0x0A;
/// Framed `extra_data` bytes (3-byte OART record header + compressed witness payload) reserved
/// for `ENCLAVE_WITNESS_TAG` before selecting optional Radicle publication attestations.
pub const ENCLAVE_WITNESS_RESERVED_BYTES: usize = 32_768;
const TEE_EXPIRED_TARGET_EXCLUSIONS_DOMAIN: &[u8] = b"outbe/tee-expired-target-exclusions/v1";
const EXECUTION_SUMMARY_TAG: u8 = 0x01;
const BOUNDARY_TAG: u8 = 0x02;
const DEALER_LOG_TAG: u8 = 0x03;
// Tag 0x04 is reserved. The codec rejects it.
const TIMESTAMP_MILLIS_PART_TAG: u8 = 0x05;
const LATE_FINALIZE_CREDITS_TAG: u8 = 0x06;
const COMMITTEE_PREANNOUNCE_TAG: u8 = 0x07;
// Tag 0x08 is retired. Entity records are committed by the EVM state root.
const EXECUTION_SUMMARY_LEN: usize = 32;
const TIMESTAMP_MILLIS_PART_LEN: usize = 8;
/// Raw BLS (MinPk) aggregate signature length carried per late-finalize credit.
const LATE_FINALIZE_SIG_LEN: usize = 96;
/// Max signer-bitmap bytes = `ceil(MAX_VALIDATORS / 8)` = `ceil(256 / 8)`.
///
/// Approved deviation: the codec enforces only this fixed
/// upper bound because it is committee-agnostic. It cannot know the committee
/// size of the block that it decodes. The committee-exact `ceil(committee/8)`
/// length check lives in `outbe_consensus::proof::late_finalize`
/// (`verify_late_finalize_proof`), where the epoch `CommitteeSnapshot` is
/// available. This split is intentional, not a missing check.
const LATE_FINALIZE_MAX_BITMAP_LEN: usize = 32;
/// Wire cap on per-block late-finalize credits in one block, pinned to the
/// inclusion window `K`. An honest proposer emits at most one
/// credit per in-window finalized block (`build_artifact` iterates
/// `[N-K, N-1]`), so `K` is the protocol maximum. Capping the wire to `K` (instead of an
/// arbitrary 256) stops an adversarial block inflating decode/snapshot/BLS-verify
/// work past the protocol bound. `K` is a small protocol constant (3) and always
/// fits `usize`.
const LATE_FINALIZE_MAX_BATCHES: usize = crate::consensus::LATE_FINALIZE_WINDOW_K as usize;
/// Fixed per-credit prefix before the variable-length bitmap and the fixed
/// signature: `fb_number(8) + fb_hash(32) + epoch(8) + view(8) + parent_view(8)
/// + committee_set_hash(32)`.
const PER_BLOCK_CREDIT_FIXED_LEN: usize = 8 + 32 + 8 + 8 + 8 + 32;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutbeBlockArtifacts {
    /// The publication flag must match the genesis configuration.
    pub radicle_publication_enabled: bool,
    pub radicle_publication: Option<crate::radicle_publication::PublicationBatch>,
    pub execution_summary: Option<ExecutionSummaryArtifact>,
    pub consensus_header_artifact: Option<ConsensusHeaderArtifact>,
    /// Sub-second part of the consensus block timestamp (0..1000).
    /// Carried inside `extra_data` under tag 0x05 so that the block hash
    /// is computed from a strictly Ethereum-spec-compliant header. That header
    /// has no extra top-level RLP fields. The integer-second part lives in
    /// `header.timestamp` as usual.
    pub timestamp_millis_part: u64,
    /// late-finalize credits (tag 0x06): a canonical batch of
    /// per-finalized-block late-finalize proofs the proposer gathered within the
    /// `K`-block inclusion window. `None`/empty when this block credits nothing.
    pub late_finalize_credits: Option<LateFinalizeCreditsArtifact>,
    /// Opaque compressed consensus enclave witness tape (tag 0x0A). `None` when the block made
    /// zero consensus enclave calls.
    pub enclave_witness: Option<Bytes>,
}

/// A batch of late-finalize credits carried in `header.extra_data` (tag 0x06).
///
/// One block may credit several finalized blocks whose inclusion windows are
/// still open. Batches are in **canonical order** (strictly ascending
/// `(fb_number, fb_hash)`, one record per target). The codec rejects
/// out-of-order or duplicate targets so the bytes are deterministic across the
/// proposer and every validator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LateFinalizeCreditsArtifact {
    pub batches: Vec<PerBlockCredit>,
}

/// One finalized block's late-finalize proof: the BLS aggregate + signer bitmap
/// plus the full binding set needed to rebuild the signed `proposal.encode()`
/// and select the epoch committee. The signature and bitmap are
/// raw bytes here (primitives layer). The consensus verifier parses them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerBlockCredit {
    pub fb_number: u64,
    pub fb_hash: B256,
    pub epoch: u64,
    pub view: u64,
    pub parent_view: u64,
    pub committee_set_hash: B256,
    pub signer_bitmap: Vec<u8>,
    pub aggregate_signature: [u8; LATE_FINALIZE_SIG_LEN],
}

/// Finalized-parent consensus facts carried in Phase 1 system transaction input.
///
/// The V1 `finalize_votes` legacy field was removed. The certificate's own
/// signer bitmap drives V2 participation accounting entirely.
/// `missed_proposers: Vec<Address>` is retained for the V1 compatibility
/// adapter (`FinalizedParentAttestation` <-> `CertifiedParentAccountingMetadata`)
/// but is always empty under V2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinalizedParentAttestation {
    pub finalized_block_number: u64,
    pub finalized_block_hash: B256,
    pub finalized_epoch: u64,
    pub finalized_view: u64,
    pub parent_view: u64,
    pub ordered_committee: Vec<Address>,
    pub signer_bitmap: Vec<u8>,
    pub certificate: Bytes,
    pub missed_proposers: Vec<Address>,
}

/// Execution accounting data in the OART v1 envelope, under tag 0x01.
/// Consensus distributes this validator fee sum to voters and accumulates it in `daily_fee_sum_raw`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionSummaryArtifact {
    pub validator_fee_sum: U256,
}

// `BoundaryOutcome` is inherently larger than `DealerLog`. This is a consensus
// wire artifact. `extra_data` transports it, and the codec and consensus paths
// match and construct it. Boxing the large variant would change those sites
// for marginal stack savings on a low-frequency, deterministically-encoded type.
// The in-memory layout does not affect the encoded bytes. Keep it inline.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsensusHeaderArtifact {
    BoundaryOutcome(DkgBoundaryArtifact),
    DealerLog(Bytes),
    /// Path A committee-chaining pre-announce: the full DKG `outcome` (players +
    /// polynomial) for `epoch`. It is emitted in a block finalized by the OUTGOING
    /// (`epoch-1`) committee, before epoch `epoch`'s own boundary block
    /// `epoch*L+1`. Committee `epoch` itself finalizes that boundary block, so the
    /// boundary block cannot self-authenticate. A follower registers `epoch`'s
    /// committee from this via the existing `register_epoch_from_outcome`. The
    /// follower trusts it because the carrying block's cert verifies against the
    /// already-trusted `epoch-1` committee. Does NOT activate. Activation stays on
    /// the `BoundaryOutcome` at `epoch*L+1`.
    CommitteePreAnnounce {
        epoch: u64,
        outcome: Bytes,
    },
}

pub fn encode_outbe_block_artifacts(artifacts: &OutbeBlockArtifacts) -> Result<Bytes> {
    let mut records = Vec::new();

    if let Some(summary) = artifacts.execution_summary {
        records.push((
            EXECUTION_SUMMARY_TAG,
            fields::encode_execution_summary(summary),
        ));
    }
    if let Some(artifact) = &artifacts.consensus_header_artifact {
        records.push(consensus::encode_record(artifact)?);
    }
    if let Some(credits) = &artifacts.late_finalize_credits {
        if !credits.batches.is_empty() {
            records.push((
                LATE_FINALIZE_CREDITS_TAG,
                late_finalize::encode_payload(credits)?,
            ));
        }
    }
    if artifacts.timestamp_millis_part != 0 {
        records.push((
            TIMESTAMP_MILLIS_PART_TAG,
            fields::encode_timestamp(artifacts.timestamp_millis_part),
        ));
    }
    if let Some(batch) = &artifacts.radicle_publication {
        if !artifacts.radicle_publication_enabled {
            return Err(ReshareArtifactError::PublicationFlagRequired.into());
        }
        records.push((RADICLE_PUBLICATION_TAG, batch.to_bytes()?));
    }
    if let Some(witness) = &artifacts.enclave_witness {
        if witness.is_empty() {
            return Err(ReshareArtifactError::EmptyEnclaveWitnessPayload.into());
        }
        records.push((ENCLAVE_WITNESS_TAG, witness.to_vec()));
    }

    if records.is_empty() && !artifacts.radicle_publication_enabled {
        return Ok(Bytes::new());
    }

    // Canonical order: strictly ascending tags. Each tag occurs at most once,
    // and the decoder rejects any other order.
    records.sort_unstable_by_key(|(tag, _)| *tag);

    if records.len() > u8::MAX as usize {
        return Err(ReshareArtifactError::TooManyRecords.into());
    }

    let payload_len = records
        .iter()
        .try_fold(0usize, |acc, (_, payload)| -> Result<usize> {
            ensure_payload_fits_u16("block artifact record", payload.len())?;
            acc.checked_add(1 + 2 + payload.len())
                .ok_or_else(|| ReshareArtifactError::LengthOverflow.into())
        })?;
    let total_len = HEADER_LEN + payload_len;
    if total_len > OUTBE_MAX_EXTRA_DATA_SIZE {
        return Err(ReshareArtifactError::ExceedsExtraDataBudget {
            total_len,
            max_len: OUTBE_MAX_EXTRA_DATA_SIZE,
        }
        .into());
    }

    let mut buf = Vec::with_capacity(total_len);
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    buf.push(u8::from(artifacts.radicle_publication_enabled));
    buf.push(records.len() as u8);
    for (tag, payload) in records {
        buf.push(tag);
        buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        buf.extend_from_slice(&payload);
    }

    Ok(Bytes::from(buf))
}

pub fn decode_outbe_block_artifacts(extra_data: &[u8]) -> Result<OutbeBlockArtifacts> {
    if extra_data.is_empty() {
        return Ok(OutbeBlockArtifacts::default());
    }

    let record_count = decode_block_artifact_record_count(extra_data)?;
    let mut offset = HEADER_LEN;
    let mut artifacts = OutbeBlockArtifacts {
        radicle_publication_enabled: extra_data[5] & PUBLICATION_FLAG != 0,
        ..Default::default()
    };
    // The encoder writes empty bytes for this case, never a 7-byte envelope.
    if record_count == 0 && !artifacts.radicle_publication_enabled {
        return Err(ReshareArtifactError::EmptyEnvelopeWithoutPublicationFlag.into());
    }

    let mut previous_tag: Option<u8> = None;
    for _ in 0..record_count {
        let (tag, payload) = read_record(extra_data, &mut offset, previous_tag)?;
        previous_tag = Some(tag);
        records::decode_into(&mut artifacts, tag, payload)?;
    }

    if offset != extra_data.len() {
        return Err(ReshareArtifactError::TrailingBytes.into());
    }

    Ok(artifacts)
}

/// Reads one record header and payload at `offset` and advances `offset`.
///
/// Rejects a tag that is not strictly greater than `previous_tag`, so the
/// decoder accepts only the encoder's ascending tag order.
fn read_record<'a>(
    extra_data: &'a [u8],
    offset: &mut usize,
    previous_tag: Option<u8>,
) -> Result<(u8, &'a [u8])> {
    let header_end = offset
        .checked_add(3)
        .ok_or(ReshareArtifactError::RecordOverflow)?;
    let header = extra_data
        .get(*offset..header_end)
        .ok_or(ReshareArtifactError::TruncatedRecordHeader)?;
    let tag = header[0];
    if let Some(previous) = previous_tag.filter(|previous| tag <= *previous) {
        return Err(ReshareArtifactError::RecordTagsOutOfOrder { previous, tag }.into());
    }
    let payload_len = u16::from_be_bytes([header[1], header[2]]) as usize;
    let end = header_end
        .checked_add(payload_len)
        .ok_or(ReshareArtifactError::RecordOverflow)?;
    let payload = extra_data
        .get(header_end..end)
        .ok_or(ReshareArtifactError::TruncatedRecordPayload)?;
    *offset = end;
    Ok((tag, payload))
}

/// Canonicalizes proposer-supplied artifact fragments before block execution.
///
/// This function never accepts execution-produced fields from payload attributes. The
/// caller inserts the local execution summary and timestamp remainder after
/// execution. The encoder enforces the existing final envelope size limit.
pub fn sanitize_prefinal_outbe_block_artifacts(extra_data: &[u8]) -> Result<Bytes> {
    let mut artifacts = decode_outbe_block_artifacts(extra_data)?;
    artifacts.execution_summary = None;
    artifacts.timestamp_millis_part = 0;
    artifacts.enclave_witness = None;
    let encoded = encode_outbe_block_artifacts(&artifacts)?;
    Ok(encoded)
}

/// Adds only ready publication attestations after reserving every final execution field.
pub fn encode_proposal_block_artifacts(
    mut artifacts: OutbeBlockArtifacts,
    mailbox: &crate::radicle_publication::RepositoryPublicationMailbox,
) -> Result<Bytes> {
    if artifacts.radicle_publication_enabled {
        let mut reserved = artifacts.clone();
        reserved.radicle_publication = None;
        reserved.enclave_witness = None;
        reserved.execution_summary = Some(ExecutionSummaryArtifact {
            validator_fee_sum: alloy_primitives::U256::MAX,
        });
        reserved.timestamp_millis_part = 999;
        let reserved_length = encode_outbe_block_artifacts(&reserved)?
            .len()
            .saturating_add(ENCLAVE_WITNESS_RESERVED_BYTES);
        let payload_budget = OUTBE_MAX_EXTRA_DATA_SIZE
            .saturating_sub(reserved_length)
            .saturating_sub(3); // OART record tag and u16 length.
        artifacts.radicle_publication = mailbox.ready_prefix(payload_budget);
    }
    encode_outbe_block_artifacts(&artifacts)
}

pub fn encode_consensus_header_artifact(artifact: &ConsensusHeaderArtifact) -> Result<Bytes> {
    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(artifact.clone()),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })?;
    sanitize_prefinal_outbe_block_artifacts(&encoded)
}

pub fn decode_consensus_header_artifact(
    extra_data: &[u8],
) -> Result<Option<ConsensusHeaderArtifact>> {
    Ok(decode_outbe_block_artifacts(extra_data)?.consensus_header_artifact)
}

pub fn encode_boundary_artifact(result: &DkgBoundaryArtifact) -> Result<Bytes> {
    encode_consensus_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(result.clone()))
}

pub fn decode_boundary_artifact(extra_data: &[u8]) -> Result<Option<DkgBoundaryArtifact>> {
    match decode_consensus_header_artifact(extra_data)? {
        None => Ok(None),
        Some(ConsensusHeaderArtifact::BoundaryOutcome(result)) => Ok(Some(result)),
        Some(ConsensusHeaderArtifact::DealerLog(_)) => Ok(None),
        // A committee pre-announce carries an outcome for a follower to register a
        // future epoch's committee. It is not an activating boundary.
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce { .. }) => Ok(None),
    }
}

/// Domain-separated commitment to the exact ordered expiry-exclusion list.
pub fn tee_expired_target_exclusions_hash(addresses: &[Address]) -> Result<B256> {
    validate_tee_expired_target_exclusions(addresses)?;
    if addresses.is_empty() {
        return Ok(B256::ZERO);
    }
    let mut bytes =
        Vec::with_capacity(TEE_EXPIRED_TARGET_EXCLUSIONS_DOMAIN.len() + 2 + addresses.len() * 20);
    bytes.extend_from_slice(TEE_EXPIRED_TARGET_EXCLUSIONS_DOMAIN);
    bytes.extend_from_slice(&(addresses.len() as u16).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    Ok(alloy_primitives::keccak256(bytes))
}

fn validate_tee_expired_target_exclusions(addresses: &[Address]) -> Result<()> {
    if addresses.len() > MAX_TEE_EXPIRED_TARGET_EXCLUSIONS {
        return Err(ReshareArtifactError::TeeExpiryExclusionsExceedCap {
            count: addresses.len(),
            max_count: MAX_TEE_EXPIRED_TARGET_EXCLUSIONS,
        }
        .into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for address in addresses {
        if address.is_zero() {
            return Err(ReshareArtifactError::TeeExpiryExclusionsZeroValidator.into());
        }
        if !unique.insert(*address) {
            return Err(ReshareArtifactError::TeeExpiryExclusionsDuplicateValidator.into());
        }
    }
    Ok(())
}

/// Encode a standalone `LateFinalizeCreditsArtifact` for the `LateFinalizeCredits`
/// system-transaction body (mirrors [`encode_boundary_artifact`]). An empty batch
/// encodes to empty bytes. The mandatory system tx then carries an empty body,
/// and its execution still closes the matured window as a side effect.
pub fn encode_late_finalize_credits_artifact(
    artifact: &LateFinalizeCreditsArtifact,
) -> Result<Bytes> {
    encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        late_finalize_credits: Some(artifact.clone()),
        ..Default::default()
    })
}

/// Decode a standalone `LateFinalizeCreditsArtifact` from a system-tx body. Empty
/// input decodes to `None`. Callers treat that as an empty (no-op) artifact.
pub fn decode_late_finalize_credits_artifact(
    extra_data: &[u8],
) -> Result<Option<LateFinalizeCreditsArtifact>> {
    Ok(decode_outbe_block_artifacts(extra_data)?.late_finalize_credits)
}

pub fn encode_radicle_publication_artifact(
    batch: &crate::radicle_publication::PublicationBatch,
) -> Result<Bytes> {
    encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        radicle_publication: Some(batch.clone()),
        ..Default::default()
    })
}

pub fn decode_radicle_publication_artifact(
    extra_data: &[u8],
) -> Result<Option<crate::radicle_publication::PublicationBatch>> {
    Ok(decode_outbe_block_artifacts(extra_data)?.radicle_publication)
}

fn ensure_count_fits_u16(name: &'static str, count: usize) -> Result<()> {
    if count > u16::MAX as usize {
        return Err(ReshareArtifactError::ListExceedsU16Limit { name, count }.into());
    }
    Ok(())
}

fn ensure_len_fits_u32(name: &'static str, len: usize) -> Result<()> {
    if len > u32::MAX as usize {
        return Err(ReshareArtifactError::ExceedsU32LengthLimit { name, len }.into());
    }
    Ok(())
}

fn ensure_payload_fits_u16(name: &'static str, len: usize) -> Result<()> {
    if len > u16::MAX as usize {
        return Err(ReshareArtifactError::PayloadExceedsU16LengthLimit { name, len }.into());
    }
    Ok(())
}

fn decode_block_artifact_record_count(extra_data: &[u8]) -> Result<usize> {
    if extra_data.len() < HEADER_LEN {
        return Err(ReshareArtifactError::TooShort.into());
    }

    if &extra_data[..4] != MAGIC {
        return Err(ReshareArtifactError::UnknownExtraDataArtifact.into());
    }

    if extra_data[4] != VERSION {
        return Err(ReshareArtifactError::UnsupportedVersion {
            version: extra_data[4],
        }
        .into());
    }

    let flags = extra_data[5];
    if flags & !PUBLICATION_FLAG != 0 {
        return Err(ReshareArtifactError::UnsupportedFlags { flags }.into());
    }
    Ok(extra_data[6] as usize)
}

#[cfg(test)]
mod tests;
