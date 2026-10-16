use alloy_primitives::B256;
use outbe_chain_constants::DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS;
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1, profile::CapacityProfileV1, SchemaLimits,
};
use outbe_primitives::error::Result;

use crate::errors::OcompRegistryError;

const REQUEST_PROFILE_MAGIC: [u8; 4] = *b"OMRP";
const REQUEST_PROFILE_VERSION: u16 = 1;
const REQUEST_PROFILE_FIXED_LEN: usize = 4 + 2 + 8 + 32 * 5 + 4;

/// Network policy used by OCOMP consumers when constructing a fresh job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompRequestProfile {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub fork_id: B256,
    pub protocol_bundle_hash: B256,
    pub correctness_profile_id: B256,
    pub capacity_profile: CapacityProfileV1,
    pub source_availability_policy_id: B256,
}

impl OcompRequestProfile {
    /// Validates the canonical request authority without encoding it.
    pub fn validate(&self) -> Result<()> {
        validate_request_profile(self)
    }

    pub fn encode_canonical(&self, limits: &SchemaLimits) -> Result<Vec<u8>> {
        encode_request_profile(self, limits)
    }

    pub fn decode_canonical(encoded: &[u8], limits: &SchemaLimits) -> Result<Self> {
        decode_request_profile(encoded, limits)
    }
}

#[must_use]
pub fn poc_schema_limits() -> SchemaLimits {
    outbe_ocomp_protocol::profile::poc_schema_limits()
}

pub(crate) fn validate_request_profile(profile: &OcompRequestProfile) -> Result<()> {
    let capacity = &profile.capacity_profile;
    if has_reserved_zero_identity(profile) {
        return Err(OcompRegistryError::RequestProfileReservedZeroIdentity.into());
    }

    let bounds = PocCapacityBounds::generated()?;
    if !matches_frozen_poc_policy(capacity, &bounds) {
        return Err(OcompRegistryError::RequestProfileViolatesFrozenPocBounds.into());
    }
    Ok(())
}

fn has_reserved_zero_identity(profile: &OcompRequestProfile) -> bool {
    profile.chain_id == 0
        || [
            &profile.genesis_hash,
            &profile.fork_id,
            &profile.protocol_bundle_hash,
            &profile.correctness_profile_id,
            &profile.source_availability_policy_id,
            &profile.capacity_profile.profile_id,
            &profile.capacity_profile.generated_limits_manifest_hash,
        ]
        .into_iter()
        .any(B256::is_zero)
}

struct PocCapacityBounds {
    max_tributes_per_work_shard: u32,
    max_reference_currencies: u16,
    max_oracle_entries: u32,
    max_active_scurve_entries: u32,
    source_retention_after_terminal_blocks: u64,
}

impl PocCapacityBounds {
    fn generated() -> Result<Self> {
        let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
        Ok(Self {
            max_tributes_per_work_shard: u32::try_from(candidate.max_tributes_per_work_shard)
                .map_err(|_| OcompRegistryError::GeneratedUnitSizeExceedsU32)?,
            max_reference_currencies: u16::try_from(candidate.max_oracle_openings)
                .map_err(|_| OcompRegistryError::GeneratedReferenceCurrencyCapExceedsU16)?,
            max_oracle_entries: u32::try_from(candidate.max_oracle_wwd_pair_entries)
                .map_err(|_| OcompRegistryError::GeneratedOracleEntryCapExceedsU32)?,
            max_active_scurve_entries: u32::try_from(candidate.max_active_scurve_entries)
                .map_err(|_| OcompRegistryError::GeneratedScurveEntryCapExceedsU32)?,
            source_retention_after_terminal_blocks: candidate
                .source_retention_after_terminal_blocks,
        })
    }
}

fn has_frozen_shard_policy(capacity: &CapacityProfileV1, bounds: &PocCapacityBounds) -> bool {
    capacity.max_tributes_per_work_shard == bounds.max_tributes_per_work_shard
        && capacity.max_workers_per_domain == 4
}

fn has_frozen_block_policy(capacity: &CapacityProfileV1) -> bool {
    [
        capacity.max_intents_per_block,
        capacity.max_activations_per_block,
        capacity.max_ready_inspections_per_block,
        capacity.max_expirations_per_block,
    ] == [1; 4]
}

fn has_bounded_opening_policy(capacity: &CapacityProfileV1, bounds: &PocCapacityBounds) -> bool {
    positive_at_most(
        capacity.max_reference_currencies,
        bounds.max_reference_currencies,
    ) && positive_at_most(
        capacity.max_oracle_wwd_pair_entries,
        bounds.max_oracle_entries,
    ) && positive_at_most(
        capacity.max_active_scurve_entries,
        bounds.max_active_scurve_entries,
    )
}

fn positive_at_most<T: Ord + From<u8>>(value: T, maximum: T) -> bool {
    value > T::from(0) && value <= maximum
}

fn matches_frozen_poc_policy(capacity: &CapacityProfileV1, bounds: &PocCapacityBounds) -> bool {
    if !has_frozen_shard_policy(capacity, bounds)
        || !has_frozen_block_policy(capacity)
        || capacity.ready_backoff_blocks != 1
    {
        return false;
    }
    if !has_bounded_opening_policy(capacity, bounds)
        || !positive_at_most(
            capacity.result_deadline_blocks,
            DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        )
    {
        return false;
    }
    capacity.source_retention_after_terminal_blocks == bounds.source_retention_after_terminal_blocks
}

fn encode_request_profile(profile: &OcompRequestProfile, limits: &SchemaLimits) -> Result<Vec<u8>> {
    validate_request_profile(profile)?;
    let capacity = profile
        .capacity_profile
        .encode_canonical(limits)
        .map_err(OcompRegistryError::EncodeCapacityProfile)?;
    let capacity_len = u32::try_from(capacity.len())
        .map_err(|_| OcompRegistryError::CapacityProfileLengthExceedsU32)?;
    let total = REQUEST_PROFILE_FIXED_LEN
        .checked_add(capacity.len())
        .ok_or(OcompRegistryError::RequestProfileLengthOverflow)?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(total)
        .map_err(|_| OcompRegistryError::AllocateRequestProfile)?;
    encoded.extend_from_slice(&REQUEST_PROFILE_MAGIC);
    encoded.extend_from_slice(&REQUEST_PROFILE_VERSION.to_be_bytes());
    encoded.extend_from_slice(&profile.chain_id.to_be_bytes());
    encoded.extend_from_slice(profile.genesis_hash.as_slice());
    encoded.extend_from_slice(profile.fork_id.as_slice());
    encoded.extend_from_slice(profile.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(profile.correctness_profile_id.as_slice());
    encoded.extend_from_slice(profile.source_availability_policy_id.as_slice());
    encoded.extend_from_slice(&capacity_len.to_be_bytes());
    encoded.extend_from_slice(&capacity);
    if encoded.len() != total {
        return Err(OcompRegistryError::RequestProfileEncodedLengthMismatch.into());
    }
    Ok(encoded)
}

fn decode_request_profile(encoded: &[u8], limits: &SchemaLimits) -> Result<OcompRequestProfile> {
    if encoded.len() < REQUEST_PROFILE_FIXED_LEN {
        return Err(OcompRegistryError::TruncatedRequestProfile.into());
    }
    let mut reader = ProfileReader::new(encoded);
    if reader.take::<4>()? != REQUEST_PROFILE_MAGIC
        || u16::from_be_bytes(reader.take::<2>()?) != REQUEST_PROFILE_VERSION
    {
        return Err(OcompRegistryError::RequestProfileMagicOrVersionMismatch.into());
    }
    let chain_id = reader.u64()?;
    let genesis_hash = B256::from(reader.take::<32>()?);
    let fork_id = B256::from(reader.take::<32>()?);
    let protocol_bundle_hash = B256::from(reader.take::<32>()?);
    let correctness_profile_id = B256::from(reader.take::<32>()?);
    let source_availability_policy_id = B256::from(reader.take::<32>()?);
    let capacity_len = usize::try_from(reader.u32()?)
        .map_err(|_| OcompRegistryError::CapacityProfileLengthExceedsUsize)?;
    if capacity_len != reader.remaining() {
        return Err(OcompRegistryError::CapacityProfileLengthMismatch.into());
    }
    let capacity_encoded = reader.take_dynamic(capacity_len)?;
    reader.finish()?;
    let capacity_profile = CapacityProfileV1::decode_canonical(capacity_encoded, limits)
        .map_err(OcompRegistryError::DecodeCapacityProfile)?;
    let profile = OcompRequestProfile {
        chain_id,
        genesis_hash,
        fork_id,
        protocol_bundle_hash,
        correctness_profile_id,
        capacity_profile,
        source_availability_policy_id,
    };
    validate_request_profile(&profile)?;
    if encode_request_profile(&profile, limits)? != encoded {
        return Err(OcompRegistryError::NonCanonicalRequestProfileEncoding.into());
    }
    Ok(profile)
}

struct ProfileReader<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> ProfileReader<'a> {
    const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(OcompRegistryError::RequestProfileOffsetOverflow)?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or(OcompRegistryError::TruncatedRequestProfile)?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| OcompRegistryError::InvalidRequestProfileFieldWidth.into())
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take::<4>()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take::<8>()?))
    }

    fn remaining(&self) -> usize {
        self.encoded.len().saturating_sub(self.offset)
    }

    fn take_dynamic(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(OcompRegistryError::RequestProfileOffsetOverflow)?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or(OcompRegistryError::TruncatedRequestProfileField)?;
        self.offset = end;
        Ok(bytes)
    }

    fn finish(self) -> Result<()> {
        if self.offset == self.encoded.len() {
            Ok(())
        } else {
            Err(OcompRegistryError::RequestProfileTrailingBytes.into())
        }
    }
}
