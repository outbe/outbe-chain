use alloy_primitives::{Address, Bytes, B256, U256};

use crate::{
    consensus::{DkgBoundaryArtifact, OUTBE_MAX_EXTRA_DATA_SIZE},
    error::{PrecompileError, Result},
    validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS,
};

mod consensus;
mod fields;
mod late_finalize;
mod records;

#[cfg(test)]
mod regression;

const MAGIC: &[u8; 4] = b"OART";
/// Version 0x0B adds the bounded ordered `tee_expired_target_exclusions` list
/// and its domain-separated commitment to the `DkgBoundaryArtifact` payload.
/// This pre-genesis hard fork makes the exact freeze-height expiry authority
/// committee-notarized and replayable by execution.
///
/// Version 0x06 extends the
/// `DkgBoundaryArtifact` boundary payload (tag 0x02) with
/// the V2 `committee_set_hash` (32 bytes) and the raw encoded VRF group
/// public key bytes (length-prefixed `u32`). Both fields are needed at
/// boundary activation so the executor can populate the V2
/// `CommitteeSnapshotStore` without rerunning the DKG.
///
/// Version 0x05 (Ethereum-header-hash compatibility, see
/// `feat/header-eth-compat-millis-in-extradata`): the sub-second
/// `timestamp_millis_part` was previously a top-level RLP field on
/// `OutbeHeader`. It now travels in `header.extra_data` under tag 0x05.
/// As a result, the block hash is `keccak256(rlp(standard_ethereum_header))`
/// without any Outbe-specific extra fields.
///
/// Version 0x04 dropped the
/// `total_emission_limit` field from
/// `ExecutionSummaryArtifact` because per-block emission no longer
/// exists. The Cycle handler computes the daily cap directly
/// from `outbe_emissionlimit::day_emission::day_emission_limit`.
///
/// Version 0x08 adds tag 0x06, which carries
/// `LateFinalizeCreditsArtifact`. This artifact is a canonical batch of
/// per-finalized-block late-finalize proofs
/// (aggregate signature + signer bitmap + binding fields)
/// gathered within the `K`-block inclusion window. Hard fork:
/// the new mandatory begin-zone phase and this version bump both change the
/// block hash.
///
/// Pre-genesis hard fork. Nodes built before this change will reject
/// blocks carrying earlier artifact versions.
const VERSION: u8 = 0x0B;
const TEE_EXPIRED_TARGET_EXCLUSIONS_DOMAIN: &[u8] = b"outbe/tee-expired-target-exclusions/v1";
const EXECUTION_SUMMARY_TAG: u8 = 0x01;
const BOUNDARY_TAG: u8 = 0x02;
const DEALER_LOG_TAG: u8 = 0x03;
// Tag 0x04 is permanently retired (legacy finalized-parent cert metadata). The
// active codec rejects it. Do not reuse it (see CLAUDE.md).
const TIMESTAMP_MILLIS_PART_TAG: u8 = 0x05;
const LATE_FINALIZE_CREDITS_TAG: u8 = 0x06;
const COMMITTEE_PREANNOUNCE_TAG: u8 = 0x07;
pub const COMPRESSED_ENTITIES_ROOT_TAG: u8 = 0x08;
const EXECUTION_SUMMARY_LEN: usize = 32;
const TIMESTAMP_MILLIS_PART_LEN: usize = 8;
pub const COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN: usize = 4 + 32;
pub const COMPRESSED_ENTITIES_ROOT_RECORD_LEN: usize = 1 + 2 + COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN;
pub const OUTBE_MAX_NON_ROOT_ARTIFACT_SIZE: usize =
    OUTBE_MAX_EXTRA_DATA_SIZE - COMPRESSED_ENTITIES_ROOT_RECORD_LEN;
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
    /// Execution-computed post-state compressed-entity root (tag 0x08).
    /// Structurally optional. Block execution, not this height-independent
    /// codec, enforces mandatory presence for block 1+.
    pub compressed_entities_root: Option<CompressedEntitiesRootArtifact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressedEntitiesRootArtifact {
    pub commitment_scheme_version: u32,
    pub r_sealed: B256,
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

/// Wire payload kept inside `header.extra_data` (tag 0x01) under the
/// `OART` v0x04 envelope. Holds the only piece of execution-side data
/// the consensus path needs: the validator fee sum that
/// `on_finalized_metadata` distributes to voters and accumulates into
/// `daily_fee_sum_raw`. The previous `total_emission_limit` field was
/// removed. The Cycle handler computes daily emission from the closed-form
/// formula. `extra_data` does not need to transport it.
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
    if let Some(root) = artifacts.compressed_entities_root {
        records.push((COMPRESSED_ENTITIES_ROOT_TAG, fields::encode_root(root)));
    }

    if records.is_empty() {
        return Ok(Bytes::new());
    }

    if records.len() > u8::MAX as usize {
        return Err(PrecompileError::Fatal(
            "too many block artifact records".into(),
        ));
    }

    let payload_len = records.iter().try_fold(0usize, |acc, (_, payload)| {
        ensure_payload_fits_u16("block artifact record", payload.len())?;
        acc.checked_add(1 + 2 + payload.len())
            .ok_or_else(|| PrecompileError::Fatal("block artifact length overflow".into()))
    })?;
    let total_len = 4 + 1 + 1 + payload_len;
    if total_len > OUTBE_MAX_EXTRA_DATA_SIZE {
        return Err(PrecompileError::Fatal(format!(
            "block artifacts exceed extra_data budget: {total_len} > {OUTBE_MAX_EXTRA_DATA_SIZE}"
        )));
    }

    let mut buf = Vec::with_capacity(total_len);
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
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
    let mut offset = 6usize;
    let mut artifacts = OutbeBlockArtifacts::default();

    for _ in 0..record_count {
        if offset + 3 > extra_data.len() {
            return Err(PrecompileError::Fatal(
                "truncated block artifact record header".into(),
            ));
        }
        let tag = extra_data[offset];
        offset += 1;
        let payload_len = u16::from_be_bytes([extra_data[offset], extra_data[offset + 1]]) as usize;
        offset += 2;
        let end = offset
            .checked_add(payload_len)
            .ok_or_else(|| PrecompileError::Fatal("block artifact record overflow".into()))?;
        let Some(payload) = extra_data.get(offset..end) else {
            return Err(PrecompileError::Fatal(
                "truncated block artifact record payload".into(),
            ));
        };
        offset = end;

        records::decode_into(&mut artifacts, tag, payload)?;
    }

    if offset != extra_data.len() {
        return Err(PrecompileError::Fatal(
            "trailing bytes in block artifacts".into(),
        ));
    }

    Ok(artifacts)
}

/// Canonicalizes proposer-supplied artifact fragments before block execution.
///
/// This function never accepts execution-produced fields from payload attributes. The
/// caller must insert the local execution summary, timestamp remainder, and CE
/// root after compressed-entity sealing. The reduced size limit reserves the
/// mandatory tag `0x08` record inside the unchanged 64 KiB final envelope.
pub fn sanitize_prefinal_outbe_block_artifacts(extra_data: &[u8]) -> Result<Bytes> {
    let mut artifacts = decode_outbe_block_artifacts(extra_data)?;
    artifacts.execution_summary = None;
    artifacts.timestamp_millis_part = 0;
    artifacts.compressed_entities_root = None;
    let encoded = encode_outbe_block_artifacts(&artifacts)?;
    if encoded.len() > OUTBE_MAX_NON_ROOT_ARTIFACT_SIZE {
        return Err(PrecompileError::Fatal(format!(
            "pre-final block artifacts exceed reserved extra_data budget: {} > {}",
            encoded.len(),
            OUTBE_MAX_NON_ROOT_ARTIFACT_SIZE
        )));
    }
    Ok(encoded)
}

pub fn encode_consensus_header_artifact(artifact: &ConsensusHeaderArtifact) -> Result<Bytes> {
    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: Some(artifact.clone()),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
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
        return Err(PrecompileError::Fatal(format!(
            "TEE expiry exclusions exceed protocol cap: {} > {}",
            addresses.len(),
            MAX_TEE_EXPIRED_TARGET_EXCLUSIONS
        )));
    }
    let mut unique = std::collections::BTreeSet::new();
    for address in addresses {
        if address.is_zero() {
            return Err(PrecompileError::Fatal(
                "TEE expiry exclusions contain zero validator address".into(),
            ));
        }
        if !unique.insert(*address) {
            return Err(PrecompileError::Fatal(
                "TEE expiry exclusions contain duplicate validator address".into(),
            ));
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

fn ensure_count_fits_u16(name: &str, count: usize) -> Result<()> {
    if count > u16::MAX as usize {
        return Err(PrecompileError::Fatal(format!(
            "{name} list exceeds u16 count limit: {count}"
        )));
    }
    Ok(())
}

fn ensure_len_fits_u32(name: &str, len: usize) -> Result<()> {
    if len > u32::MAX as usize {
        return Err(PrecompileError::Fatal(format!(
            "{name} exceeds u32 length limit: {len}"
        )));
    }
    Ok(())
}

fn ensure_payload_fits_u16(name: &str, len: usize) -> Result<()> {
    if len > u16::MAX as usize {
        return Err(PrecompileError::Fatal(format!(
            "{name} payload exceeds u16 length limit: {len}"
        )));
    }
    Ok(())
}

fn decode_block_artifact_record_count(extra_data: &[u8]) -> Result<usize> {
    if extra_data.len() < 4 + 1 + 1 {
        return Err(PrecompileError::Fatal("block artifacts too short".into()));
    }

    if &extra_data[..4] != MAGIC {
        return Err(PrecompileError::Fatal(
            "unknown non-empty extra_data block artifact".into(),
        ));
    }

    if extra_data[4] != VERSION {
        return Err(PrecompileError::Fatal(format!(
            "unsupported block artifact version: {}",
            extra_data[4]
        )));
    }

    Ok(extra_data[5] as usize)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, Bytes, B256, U256};

    use super::{
        decode_boundary_artifact, decode_consensus_header_artifact, decode_outbe_block_artifacts,
        encode_boundary_artifact, encode_consensus_header_artifact, encode_outbe_block_artifacts,
        sanitize_prefinal_outbe_block_artifacts, tee_expired_target_exclusions_hash,
        CompressedEntitiesRootArtifact, ConsensusHeaderArtifact, ExecutionSummaryArtifact,
        OutbeBlockArtifacts, COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN,
        COMPRESSED_ENTITIES_ROOT_RECORD_LEN, OUTBE_MAX_NON_ROOT_ARTIFACT_SIZE,
    };
    use crate::consensus::{DkgBoundaryArtifact, ReshareResult};

    fn boundary_with_expiry_exclusions(exclusions: Vec<Address>) -> DkgBoundaryArtifact {
        let exclusions_hash = tee_expired_target_exclusions_hash(&exclusions).unwrap();
        DkgBoundaryArtifact {
            epoch: 1,
            dkg_cycle: 1,
            freeze_height: 100,
            planned_activation_height: 120,
            target_set_hash: B256::with_last_byte(0x11),
            vrf_material_version: 1,
            vrf_group_public_key: alloy_primitives::keccak256([]),
            vrf_group_public_key_bytes: Bytes::new(),
            committee_set_hash: B256::with_last_byte(0x22),
            is_validator_set_change: true,
            outcome: Bytes::new(),
            is_full_dkg: false,
            reshare: ReshareResult {
                new_active_set: Vec::new(),
                active_set_hash: B256::ZERO,
            },
            tee_recipient_pubkeys: Vec::new(),
            tee_expired_target_exclusions: exclusions,
            tee_expired_target_exclusions_hash: exclusions_hash,
        }
    }

    #[test]
    fn boundary_roundtrip_binds_ordered_tee_expiry_exclusions() {
        let exclusions = vec![
            address!("0x1111111111111111111111111111111111111111"),
            address!("0x2222222222222222222222222222222222222222"),
        ];
        let artifact = boundary_with_expiry_exclusions(exclusions.clone());
        let encoded = encode_boundary_artifact(&artifact).unwrap();
        let decoded = decode_boundary_artifact(&encoded).unwrap().unwrap();
        assert_eq!(decoded, artifact);
        assert_eq!(decoded.tee_expired_target_exclusions, exclusions);
    }

    #[test]
    fn boundary_wire_ends_after_tee_expiry_exclusions() {
        let artifact = boundary_with_expiry_exclusions(Vec::new());
        let encoded = encode_boundary_artifact(&artifact).unwrap();

        // OART envelope (6 bytes) + boundary record header (3 bytes) precede the
        // payload. With empty variable-length collections, the canonical boundary
        // payload ends at the expiry-exclusions count and is exactly 216 bytes.
        let payload_len = u16::from_be_bytes([encoded[7], encoded[8]]) as usize;
        assert_eq!(payload_len, 216);
        assert_eq!(encoded.len(), 6 + 3 + payload_len);
    }

    #[test]
    fn boundary_wire_rejects_data_after_tee_expiry_exclusions() {
        let artifact = boundary_with_expiry_exclusions(Vec::new());
        let mut encoded = encode_boundary_artifact(&artifact).unwrap().to_vec();
        let payload_len = u16::from_be_bytes([encoded[7], encoded[8]]);
        encoded[7..9].copy_from_slice(&(payload_len + 1).to_be_bytes());
        encoded.push(0);

        assert!(decode_boundary_artifact(&encoded).is_err());
    }

    #[test]
    fn boundary_rejects_duplicate_oversized_and_tampered_tee_expiry_exclusions() {
        let validator = address!("0x1111111111111111111111111111111111111111");
        assert!(tee_expired_target_exclusions_hash(&[validator, validator]).is_err());

        let oversized: Vec<_> = (0..=crate::validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS)
            .map(|index| {
                let word = U256::from(index + 1).to_be_bytes::<32>();
                Address::from_slice(&word[12..])
            })
            .collect();
        assert!(tee_expired_target_exclusions_hash(&oversized).is_err());

        let artifact = boundary_with_expiry_exclusions(vec![validator]);
        let mut encoded = encode_boundary_artifact(&artifact).unwrap().to_vec();
        // The sole exclusion address is the final value in the payload.
        let address_last_byte = encoded.len() - 1;
        encoded[address_last_byte] ^= 0x01;
        assert!(decode_boundary_artifact(&encoded).is_err());
    }

    fn roundtrip_boundary_fixture() -> DkgBoundaryArtifact {
        DkgBoundaryArtifact {
            epoch: 7,
            dkg_cycle: 1,
            freeze_height: 100,
            planned_activation_height: 200,
            target_set_hash: B256::with_last_byte(0x41),
            vrf_material_version: 1,
            vrf_group_public_key: B256::with_last_byte(0x42),
            vrf_group_public_key_bytes: Bytes::from_static(b"\x22\x22\x22"),
            committee_set_hash: B256::with_last_byte(0x4F),
            is_validator_set_change: true,
            outcome: Bytes::from_static(b"dkg-outcome"),
            is_full_dkg: true,
            tee_recipient_pubkeys: Vec::new(),
            tee_expired_target_exclusions: Vec::new(),
            tee_expired_target_exclusions_hash: B256::ZERO,
            reshare: ReshareResult {
                new_active_set: vec![
                    address!("0x1111111111111111111111111111111111111111"),
                    address!("0x2222222222222222222222222222222222222222"),
                ],
                active_set_hash: B256::with_last_byte(0x41),
            },
        }
    }

    #[test]
    fn roundtrip_block_artifacts_with_execution_summary_and_boundary() {
        let boundary = roundtrip_boundary_fixture();
        let summary = ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(3u64),
        };

        let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            execution_summary: Some(summary),
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                boundary.clone(),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            compressed_entities_root: None,
        })
        .unwrap();
        let decoded = decode_outbe_block_artifacts(&encoded).unwrap();

        assert_eq!(decoded.execution_summary, Some(summary));
        assert_eq!(
            decoded.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))
        );
    }

    #[test]
    fn roundtrip_boundary_header_artifact_wrapper() {
        let result = roundtrip_boundary_fixture();

        let encoded = encode_boundary_artifact(&result).unwrap();
        let decoded = decode_boundary_artifact(&encoded).unwrap();
        assert_eq!(decoded, Some(result));
    }

    #[test]
    fn roundtrip_dealer_log_header_artifact() {
        let encoded = encode_consensus_header_artifact(&ConsensusHeaderArtifact::DealerLog(
            Bytes::from_static(b"dealer-log"),
        ))
        .unwrap();
        let decoded = decode_consensus_header_artifact(&encoded).unwrap();
        assert_eq!(
            decoded,
            Some(ConsensusHeaderArtifact::DealerLog(Bytes::from_static(
                b"dealer-log"
            )))
        );
        assert_eq!(decode_boundary_artifact(&encoded).unwrap(), None);
    }

    #[test]
    fn roundtrip_committee_preannounce_header_artifact() {
        let artifact = ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: 7,
            outcome: Bytes::from_static(b"dkg-outcome-bytes"),
        };
        let encoded = encode_consensus_header_artifact(&artifact).unwrap();
        assert_eq!(
            decode_consensus_header_artifact(&encoded).unwrap(),
            Some(artifact)
        );
        // A pre-announce carries a committee for a follower. It is NOT an
        // activating boundary.
        assert_eq!(decode_boundary_artifact(&encoded).unwrap(), None);
    }

    #[test]
    fn committee_preannounce_preserves_epoch_with_empty_outcome() {
        let artifact = ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: 0xABCD,
            outcome: Bytes::new(),
        };
        let encoded = encode_consensus_header_artifact(&artifact).unwrap();
        match decode_consensus_header_artifact(&encoded).unwrap() {
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }) => {
                assert_eq!(epoch, 0xABCD);
                assert!(outcome.is_empty());
            }
            other => panic!("expected CommitteePreAnnounce, got {other:?}"),
        }
    }

    #[test]
    fn empty_extra_data_has_no_artifacts() {
        let decoded = decode_outbe_block_artifacts(&[]).unwrap();
        assert_eq!(decoded, OutbeBlockArtifacts::default());
        assert_eq!(decode_boundary_artifact(&[]).unwrap(), None);
    }

    #[test]
    fn unknown_non_empty_extra_data_is_rejected() {
        assert!(decode_boundary_artifact(b"NOPEnot-reshare").is_err());
    }

    #[test]
    fn legacy_finalized_parent_header_tag_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(super::MAGIC);
        buf.push(super::VERSION);
        buf.push(1u8);
        buf.push(0x04);
        buf.extend_from_slice(&0u16.to_be_bytes());
        let err = decode_outbe_block_artifacts(&buf).unwrap_err();
        assert!(format!("{err}").contains("unsupported block artifact tag: 4"));
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let mut encoded = encode_boundary_artifact(&DkgBoundaryArtifact {
            epoch: 1,
            dkg_cycle: 1,
            freeze_height: 10,
            planned_activation_height: 20,
            target_set_hash: B256::ZERO,
            vrf_material_version: 1,
            vrf_group_public_key: B256::ZERO,
            vrf_group_public_key_bytes: Bytes::new(),
            committee_set_hash: B256::ZERO,
            is_validator_set_change: false,
            outcome: Bytes::from_static(b"x"),
            is_full_dkg: false,
            tee_recipient_pubkeys: Vec::new(),
            tee_expired_target_exclusions: Vec::new(),
            tee_expired_target_exclusions_hash: B256::ZERO,
            reshare: ReshareResult {
                new_active_set: vec![],
                active_set_hash: B256::ZERO,
            },
        })
        .unwrap();
        let truncated_len = encoded.len() - 1;
        encoded.truncate(truncated_len);
        assert!(decode_outbe_block_artifacts(&encoded).is_err());
    }

    #[test]
    fn boundary_roundtrip_carries_tee_recipient_pubkeys() {
        let mut boundary = make_boundary(2, 8, 0, true, false);
        boundary.tee_recipient_pubkeys = vec![
            (
                address!("0x1111111111111111111111111111111111111111"),
                B256::repeat_byte(0xA1),
            ),
            (
                address!("0x2222222222222222222222222222222222222222"),
                B256::repeat_byte(0xA2),
            ),
        ];
        let encoded = encode_boundary_artifact(&boundary).expect("encodes");
        let decoded = decode_boundary_artifact(&encoded)
            .expect("decodes")
            .expect("boundary present");
        assert_eq!(
            decoded.tee_recipient_pubkeys,
            boundary.tee_recipient_pubkeys
        );
        assert_eq!(decoded, boundary);
    }

    // ---- TC-8: codec coverage (round-trip variety, size limit, tag rejection) ----

    use crate::consensus::OUTBE_MAX_EXTRA_DATA_SIZE;
    use proptest::prelude::*;

    /// Build a `DkgBoundaryArtifact` with caller-controlled variable-length
    /// fields so tests can sweep the field matrix without re-listing every
    /// fixed field at each call site.
    fn make_boundary(
        validator_count: usize,
        outcome_len: usize,
        vrf_bytes_len: usize,
        is_validator_set_change: bool,
        is_full_dkg: bool,
    ) -> DkgBoundaryArtifact {
        let new_active_set = (0..validator_count)
            .map(|i| {
                let mut raw = [0u8; 20];
                raw[19] = (i & 0xff) as u8;
                raw[18] = ((i >> 8) & 0xff) as u8;
                Address::from(raw)
            })
            .collect::<Vec<_>>();
        DkgBoundaryArtifact {
            epoch: 7,
            dkg_cycle: 1,
            freeze_height: 100,
            planned_activation_height: 200,
            target_set_hash: B256::with_last_byte(0x41),
            vrf_material_version: 3,
            vrf_group_public_key: B256::with_last_byte(0x42),
            vrf_group_public_key_bytes: Bytes::from(vec![0x22u8; vrf_bytes_len]),
            committee_set_hash: B256::with_last_byte(0x4F),
            is_validator_set_change,
            outcome: Bytes::from(vec![0xABu8; outcome_len]),
            is_full_dkg,
            tee_recipient_pubkeys: Vec::new(),
            tee_expired_target_exclusions: Vec::new(),
            tee_expired_target_exclusions_hash: B256::ZERO,
            reshare: ReshareResult {
                new_active_set,
                active_set_hash: B256::with_last_byte(0x41),
            },
        }
    }

    /// Assert byte-for-byte determinism: decode(encode(x)) == x AND
    /// encode(decode(encode(x))) == encode(x).
    fn assert_roundtrip(artifacts: &OutbeBlockArtifacts) {
        let encoded = encode_outbe_block_artifacts(artifacts).expect("encode");
        let decoded = decode_outbe_block_artifacts(&encoded).expect("decode");
        assert_eq!(&decoded, artifacts, "decoded value must equal original");
        let re_encoded = encode_outbe_block_artifacts(&decoded).expect("re-encode");
        assert_eq!(
            re_encoded, encoded,
            "encode(decode(encoded)) must be byte-identical"
        );
    }

    #[test]
    fn roundtrip_matrix_summary_x_consensus_header() {
        // {execution_summary present/absent} x {none / BoundaryOutcome / DealerLog},
        // crossed with boundary-field boundary values and the timestamp field.
        let summaries = [
            None,
            Some(ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            }),
            Some(ExecutionSummaryArtifact {
                validator_fee_sum: U256::from(1u64),
            }),
            Some(ExecutionSummaryArtifact {
                validator_fee_sum: U256::MAX,
            }),
        ];

        let consensus_headers: Vec<Option<ConsensusHeaderArtifact>> = vec![
            None,
            // empty variable fields
            Some(ConsensusHeaderArtifact::BoundaryOutcome(make_boundary(
                0, 0, 0, false, false,
            ))),
            // non-empty variable fields, flags set
            Some(ConsensusHeaderArtifact::BoundaryOutcome(make_boundary(
                3, 11, 5, true, true,
            ))),
            // DealerLog: empty and non-empty
            Some(ConsensusHeaderArtifact::DealerLog(Bytes::new())),
            Some(ConsensusHeaderArtifact::DealerLog(Bytes::from(vec![
                0x07u8;
                4096
            ]))),
        ];

        let timestamps = [0u64, 1, 999, u64::MAX];
        let roots = [
            None,
            Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: 1,
                r_sealed: B256::repeat_byte(0xA8),
            }),
        ];

        for summary in &summaries {
            for header in &consensus_headers {
                for &ts in &timestamps {
                    for root in roots {
                        assert_roundtrip(&OutbeBlockArtifacts {
                            execution_summary: *summary,
                            consensus_header_artifact: header.clone(),
                            timestamp_millis_part: ts,
                            late_finalize_credits: None,
                            compressed_entities_root: root,
                        });
                    }
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn proptest_roundtrip_block_artifacts(
            // U256 from 32 arbitrary bytes
            fee_bytes in proptest::collection::vec(any::<u8>(), 32..=32),
            has_summary in any::<bool>(),
            // 0 = none, 1 = BoundaryOutcome, 2 = DealerLog
            header_kind in 0u8..3,
            validator_count in 0usize..8,
            outcome in proptest::collection::vec(any::<u8>(), 0..64),
            vrf_bytes in proptest::collection::vec(any::<u8>(), 0..64),
            dealer_log in proptest::collection::vec(any::<u8>(), 0..256),
            is_vsc in any::<bool>(),
            is_full in any::<bool>(),
            timestamp_millis_part in any::<u64>(),
        ) {
            let execution_summary = has_summary.then(|| ExecutionSummaryArtifact {
                validator_fee_sum: U256::from_be_slice(&fee_bytes),
            });

            let consensus_header_artifact = match header_kind {
                1 => {
                    let mut b = make_boundary(validator_count, 0, 0, is_vsc, is_full);
                    b.outcome = Bytes::from(outcome.clone());
                    b.vrf_group_public_key_bytes = Bytes::from(vrf_bytes.clone());
                    Some(ConsensusHeaderArtifact::BoundaryOutcome(b))
                }
                2 => Some(ConsensusHeaderArtifact::DealerLog(Bytes::from(dealer_log.clone()))),
                _ => None,
            };

            let artifacts = OutbeBlockArtifacts {
                execution_summary,
                consensus_header_artifact,
                timestamp_millis_part,
                late_finalize_credits: None,
                compressed_entities_root: None,
            };

            let encoded = encode_outbe_block_artifacts(&artifacts).expect("encode");
            let decoded = decode_outbe_block_artifacts(&encoded).expect("decode");
            prop_assert_eq!(&decoded, &artifacts);
            let re_encoded = encode_outbe_block_artifacts(&decoded).expect("re-encode");
            prop_assert_eq!(re_encoded, encoded);
        }
    }

    #[test]
    fn encode_rejects_artifacts_over_extra_data_budget() {
        // A single boundary record whose payload sits just under the per-record
        // u16 cap (65535) but whose total framed length (6-byte envelope +
        // 3-byte record header + payload) exceeds OUTBE_MAX_EXTRA_DATA_SIZE.
        //
        // Fixed boundary payload prefix is 216 bytes. The rest is the outcome
        // blob. We size the outcome so the total framed length is just over the
        // 64 KiB budget while the record payload stays <= 65535.
        // (180 base fields + 2-byte tee_recipient_pubkeys count + 32-byte
        // expiry-exclusions commitment + 2-byte exclusions count.)
        const FIXED_PREFIX: usize =
            8 + 8 + 8 + 8 + 32 + 8 + 32 + 32 + 1 + 1 + 32 + 2 + 4 + 4 + 2 + 32 + 2;
        const ENVELOPE: usize = 4 + 1 + 1; // MAGIC + version + record count
        const RECORD_HEADER: usize = 1 + 2; // tag + u16 length

        // Oversize: record payload at the per-record u16 cap (65535). Framed
        // (envelope + record header + payload) this is 65544 > 64 KiB, so the
        // u16 cap passes but the total-length budget check rejects it.
        let oversize_payload_len = u16::MAX as usize;
        let oversize_total = ENVELOPE + RECORD_HEADER + oversize_payload_len;
        assert!(
            oversize_total > OUTBE_MAX_EXTRA_DATA_SIZE,
            "test premise: 65535-byte record must exceed 64 KiB budget once framed ({oversize_total})"
        );
        let oversize_outcome_len = oversize_payload_len - FIXED_PREFIX; // validator_count=0, vrf_bytes=0
        let oversize = OutbeBlockArtifacts {
            execution_summary: None,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                make_boundary(0, oversize_outcome_len, 0, false, false),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            compressed_entities_root: None,
        };
        let err = encode_outbe_block_artifacts(&oversize)
            .expect_err("encoding past the extra_data budget must be rejected");
        assert!(
            format!("{err}").contains("exceed extra_data budget"),
            "unexpected error: {err}"
        );

        // At the limit: size the payload so the framed total is exactly
        // OUTBE_MAX_EXTRA_DATA_SIZE (the budget check is strictly greater-than,
        // so equal must be accepted).
        let limit_payload_len = OUTBE_MAX_EXTRA_DATA_SIZE - ENVELOPE - RECORD_HEADER;
        let under_outcome_len = limit_payload_len - FIXED_PREFIX;
        let under_total = ENVELOPE + RECORD_HEADER + FIXED_PREFIX + under_outcome_len;
        assert_eq!(under_total, OUTBE_MAX_EXTRA_DATA_SIZE);
        let at_limit = OutbeBlockArtifacts {
            execution_summary: None,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                make_boundary(0, under_outcome_len, 0, false, false),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            compressed_entities_root: None,
        };
        let encoded =
            encode_outbe_block_artifacts(&at_limit).expect("at-limit artifact must encode");
        assert_eq!(encoded.len(), OUTBE_MAX_EXTRA_DATA_SIZE);
        // And it still round-trips.
        let decoded = decode_outbe_block_artifacts(&encoded).expect("decode at-limit");
        assert_eq!(decoded, at_limit);
    }

    #[test]
    fn decode_rejects_legacy_tag_0x04() {
        // Mirror the encoder's wire framing: MAGIC + VERSION + record_count
        // then one record of `tag (1) | len (u16 BE) | payload`. Tag 0x04 is the
        // rejected legacy finalized-parent metadata tag.
        let mut buf = Vec::new();
        buf.extend_from_slice(super::MAGIC);
        buf.push(super::VERSION);
        buf.push(1u8); // one record
        buf.push(0x04u8); // legacy tag
        let payload: &[u8] = b"legacy-finalized-parent-metadata";
        buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        buf.extend_from_slice(payload);

        let err = decode_outbe_block_artifacts(&buf)
            .expect_err("legacy tag 0x04 must be rejected by the active codec");
        assert!(
            format!("{err}").contains("unsupported block artifact tag: 4"),
            "unexpected error: {err}"
        );

        // Sanity positives: a valid 0x02 (BoundaryOutcome) and 0x03 (DealerLog)
        // record decode Ok, proving the rejection is specific to 0x04.
        let boundary_encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            execution_summary: None,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                make_boundary(2, 4, 4, true, false),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            compressed_entities_root: None,
        })
        .expect("boundary encode");
        // Byte 6 is the first record's tag (after MAGIC[0..4] + version + count).
        assert_eq!(boundary_encoded[6], super::BOUNDARY_TAG);
        assert!(decode_outbe_block_artifacts(&boundary_encoded).is_ok());

        let dealer_encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            execution_summary: None,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(
                Bytes::from_static(b"dealer-log"),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            compressed_entities_root: None,
        })
        .expect("dealer encode");
        assert_eq!(dealer_encoded[6], super::DEALER_LOG_TAG);
        assert!(decode_outbe_block_artifacts(&dealer_encoded).is_ok());
    }

    fn sample_credit(fb_number: u64, last: u8) -> super::PerBlockCredit {
        super::PerBlockCredit {
            fb_number,
            fb_hash: B256::with_last_byte(last),
            epoch: 3,
            view: fb_number + 10,
            parent_view: fb_number + 9,
            committee_set_hash: B256::with_last_byte(0xC0),
            signer_bitmap: vec![0b0000_0111],
            aggregate_signature: [last; super::LATE_FINALIZE_SIG_LEN],
        }
    }

    #[test]
    fn late_credits_codec_roundtrip() {
        let artifact = super::LateFinalizeCreditsArtifact {
            batches: vec![sample_credit(10, 0xAA), sample_credit(11, 0xBB)],
        };
        let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            late_finalize_credits: Some(artifact.clone()),
            ..Default::default()
        })
        .expect("encode late credits");
        assert_eq!(encoded[4], super::VERSION, "artifact version is 0x08");
        assert!(encoded.len() <= super::OUTBE_MAX_EXTRA_DATA_SIZE);
        let decoded = decode_outbe_block_artifacts(&encoded).expect("decode");
        assert_eq!(decoded.late_finalize_credits, Some(artifact));
    }

    #[test]
    fn late_credits_coexist_with_execution_summary_and_timestamp() {
        let original = OutbeBlockArtifacts {
            execution_summary: Some(ExecutionSummaryArtifact {
                validator_fee_sum: U256::from(99u64),
            }),
            consensus_header_artifact: None,
            timestamp_millis_part: 777,
            late_finalize_credits: Some(super::LateFinalizeCreditsArtifact {
                batches: vec![sample_credit(5, 0x55)],
            }),
            compressed_entities_root: None,
        };
        let encoded = encode_outbe_block_artifacts(&original).expect("encode");
        assert_eq!(
            decode_outbe_block_artifacts(&encoded).expect("decode"),
            original
        );
    }

    #[test]
    fn late_credits_reject_noncanonical_order_and_duplicates() {
        // Descending fb_number: not strictly ascending -> rejected.
        let descending = super::LateFinalizeCreditsArtifact {
            batches: vec![sample_credit(11, 0xBB), sample_credit(10, 0xAA)],
        };
        assert!(encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            late_finalize_credits: Some(descending),
            ..Default::default()
        })
        .is_err());
        // Duplicate target (same fb_number, fb_hash) -> rejected.
        let duplicate = super::LateFinalizeCreditsArtifact {
            batches: vec![sample_credit(10, 0xAA), sample_credit(10, 0xAA)],
        };
        assert!(encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            late_finalize_credits: Some(duplicate),
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn late_credits_decode_rejects_more_than_k_batches() {
        // The wire cap is the inclusion window `K`. The decoder rejects a count
        // header above `K` before it parses any per-credit body. This bounds
        // adversarial decode/snapshot/BLS-verify work to the protocol window.
        // An honest proposer never emits more than `K` batches.
        assert_eq!(
            super::LATE_FINALIZE_MAX_BATCHES as u64,
            crate::consensus::LATE_FINALIZE_WINDOW_K,
            "wire batch cap must equal K"
        );
        let over_k = (crate::consensus::LATE_FINALIZE_WINDOW_K + 1) as u16;
        let payload = over_k.to_be_bytes().to_vec();
        assert!(
            super::late_finalize::decode_payload(&payload).is_err(),
            "a batch count above K must be rejected at decode before body parsing"
        );
    }

    #[test]
    fn late_credits_full_batch_within_64kib() {
        let batches: Vec<_> = (0..super::LATE_FINALIZE_MAX_BATCHES as u64)
            .map(|i| sample_credit(i, (i % 256) as u8))
            .collect();
        let artifact = super::LateFinalizeCreditsArtifact { batches };
        let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            late_finalize_credits: Some(artifact.clone()),
            ..Default::default()
        })
        .expect("encode full batch");
        assert!(
            encoded.len() <= super::OUTBE_MAX_EXTRA_DATA_SIZE,
            "full {} -batch artifact must fit extra_data budget, got {}",
            super::LATE_FINALIZE_MAX_BATCHES,
            encoded.len()
        );
        assert_eq!(
            decode_outbe_block_artifacts(&encoded)
                .expect("decode")
                .late_finalize_credits,
            Some(artifact)
        );
    }

    #[test]
    fn late_credits_empty_batch_not_emitted() {
        // An empty batch must not produce a record (it is a no-op artifact).
        let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            late_finalize_credits: Some(super::LateFinalizeCreditsArtifact::default()),
            ..Default::default()
        })
        .expect("encode empty");
        assert!(
            encoded.is_empty(),
            "empty late-credits artifact emits no bytes"
        );
    }

    #[test]
    fn compressed_entities_root_has_pinned_tag_length_order_and_big_endian_bytes() {
        let artifact = CompressedEntitiesRootArtifact {
            commitment_scheme_version: 1,
            r_sealed: B256::repeat_byte(0xAB),
        };
        let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(artifact),
            ..Default::default()
        })
        .unwrap();

        let mut expected = b"OART\x0B\x01\x08\x00\x24\x00\x00\x00\x01".to_vec();
        expected.extend_from_slice(&[0xAB; 32]);
        assert_eq!(encoded.as_ref(), expected);
        assert_eq!(COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN, 36);
        assert_eq!(COMPRESSED_ENTITIES_ROOT_RECORD_LEN, 39);
        assert_eq!(OUTBE_MAX_NON_ROOT_ARTIFACT_SIZE, 65_536 - 39);
        assert_eq!(
            decode_outbe_block_artifacts(&encoded)
                .unwrap()
                .compressed_entities_root,
            Some(artifact)
        );
    }

    #[test]
    fn post_reset_block_one_extra_data_vector_is_pinned() {
        let root = B256::repeat_byte(0x11);
        let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            execution_summary: Some(ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            }),
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: 1,
                r_sealed: root,
            }),
            ..Default::default()
        })
        .unwrap();
        let mut expected = b"OART\x0B\x02\x01\x00\x20".to_vec();
        expected.extend_from_slice(&[0_u8; 32]);
        expected.extend_from_slice(b"\x08\x00\x24\x00\x00\x00\x01");
        expected.extend_from_slice(root.as_slice());
        assert_eq!(encoded.as_ref(), expected);
    }

    #[test]
    fn compressed_entities_root_decode_rejects_duplicate_wrong_lengths_and_truncation() {
        let valid = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: 0,
                r_sealed: B256::ZERO,
            }),
            ..Default::default()
        })
        .unwrap();
        assert!(
            decode_outbe_block_artifacts(&valid).is_ok(),
            "codec is structural only"
        );

        let record = &valid[6..];
        let mut duplicate = valid.to_vec();
        duplicate[5] = 2;
        duplicate.extend_from_slice(record);
        assert!(decode_outbe_block_artifacts(&duplicate).is_err());

        for length in [0_u16, 35, 37, u16::MAX] {
            let mut malformed = valid.to_vec();
            malformed[7..9].copy_from_slice(&length.to_be_bytes());
            assert!(decode_outbe_block_artifacts(&malformed).is_err());
        }
        assert_eq!(
            decode_outbe_block_artifacts(&[]).unwrap(),
            OutbeBlockArtifacts::default()
        );
        for end in 1..valid.len() {
            assert!(decode_outbe_block_artifacts(&valid[..end]).is_err());
        }
    }

    #[test]
    fn proposer_sanitization_strips_all_execution_produced_fields() {
        let consensus = ConsensusHeaderArtifact::DealerLog(Bytes::from_static(b"dealer"));
        let input = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            execution_summary: Some(ExecutionSummaryArtifact {
                validator_fee_sum: U256::from(7),
            }),
            consensus_header_artifact: Some(consensus.clone()),
            timestamp_millis_part: 321,
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: 99,
                r_sealed: B256::repeat_byte(0xEF),
            }),
            ..Default::default()
        })
        .unwrap();

        let sanitized = sanitize_prefinal_outbe_block_artifacts(&input).unwrap();
        let decoded = decode_outbe_block_artifacts(&sanitized).unwrap();
        assert_eq!(decoded.consensus_header_artifact, Some(consensus));
        assert_eq!(decoded.execution_summary, None);
        assert_eq!(decoded.timestamp_millis_part, 0);
        assert_eq!(decoded.compressed_entities_root, None);
    }

    #[test]
    fn root_reservation_accepts_exact_64k_final_and_rejects_one_more_non_root_byte() {
        let exact_dealer_len =
            OUTBE_MAX_EXTRA_DATA_SIZE - 6 - 3 - COMPRESSED_ENTITIES_ROOT_RECORD_LEN;
        let exact_non_root = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(Bytes::from(
                vec![0x5A; exact_dealer_len],
            ))),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(exact_non_root.len(), OUTBE_MAX_NON_ROOT_ARTIFACT_SIZE);
        assert_eq!(
            sanitize_prefinal_outbe_block_artifacts(&exact_non_root).unwrap(),
            exact_non_root
        );

        let mut final_artifacts = decode_outbe_block_artifacts(&exact_non_root).unwrap();
        final_artifacts.compressed_entities_root = Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: 1,
            r_sealed: B256::repeat_byte(0xA5),
        });
        assert_eq!(
            encode_outbe_block_artifacts(&final_artifacts)
                .unwrap()
                .len(),
            OUTBE_MAX_EXTRA_DATA_SIZE
        );

        let one_over = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(Bytes::from(
                vec![0x5A; exact_dealer_len + 1],
            ))),
            ..Default::default()
        })
        .unwrap();
        assert!(sanitize_prefinal_outbe_block_artifacts(&one_over).is_err());
    }
}
