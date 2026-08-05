use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1, profile::CapacityProfileV1, SchemaLimits,
};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::MetadosisContract;

use super::state::JobFsmLimits;

const REQUEST_PROFILE_MAGIC: [u8; 4] = *b"OMRP";
const REQUEST_PROFILE_VERSION: u16 = 1;
const REQUEST_PROFILE_FIXED_LEN: usize = 4 + 2 + 8 + 32 * 5 + 4;

/// Fork-installed authority needed to assemble `JobIntentV1`.
///
/// It deliberately carries an already-derived bundle hash rather than
/// pretending that the measurement-only OCM-04 shape manifest is armable.
/// OCM-26 supplies this value from the final network bundle.
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
    /// FSM bounds derived from the same immutable profile that owns the
    /// corresponding persisted capacity limit.
    pub(crate) const fn fsm_limits(&self) -> JobFsmLimits {
        JobFsmLimits {
            max_terminal_records: self.capacity_profile.max_terminal_job_records,
        }
    }

    /// Canonical fork-manifest representation of the request authority.
    pub fn encode_canonical(&self, limits: &SchemaLimits) -> Result<Vec<u8>> {
        encode_request_profile(self, limits)
    }

    /// Decodes and validates the canonical fork-manifest representation.
    pub fn decode_canonical(encoded: &[u8], limits: &SchemaLimits) -> Result<Self> {
        decode_request_profile(encoded, limits)
    }
}

/// Generated measurement ceilings used to bound canonical request objects.
///
/// These are compile ceilings only. They do not arm a network or provide a
/// `ProtocolBundleHash`; OCM-26 supplies the final network profile.
#[must_use]
pub fn poc_schema_limits() -> SchemaLimits {
    outbe_ocomp_protocol::profile::poc_schema_limits()
}

impl MetadosisContract<'_> {
    /// Installs the exact request profile once. Repeating the same profile is
    /// idempotent; replacing any identity or cap is rejected.
    pub fn initialize_ocomp_request_profile(
        &mut self,
        profile: &OcompRequestProfile,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let storage = self.storage.clone();
        (|| {
            validate_request_profile(profile)?;
            if storage.chain_id()? != profile.chain_id {
                return Err(fatal("OCOMP request profile chain id mismatch"));
            }
            match self.read_ocomp_request_profile(limits)? {
                Some(existing) if existing == *profile => Ok(()),
                Some(_) => Err(fatal("OCOMP request profile is immutable")),
                None => {
                    let encoded = encode_request_profile(profile, limits)?;
                    self.ocomp_request_profile.write(&encoded)?;
                    if self.read_ocomp_request_profile(limits)? != Some(profile.clone()) {
                        return Err(fatal("OCOMP request profile write/read mismatch"));
                    }
                    Ok(())
                }
            }
        })()
    }

    pub fn read_ocomp_request_profile(
        &self,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompRequestProfile>> {
        let len = self.ocomp_request_profile.len()?;
        if len == 0 {
            return Ok(None);
        }
        let max = REQUEST_PROFILE_FIXED_LEN
            .checked_add(limits.codec.max_body_bytes)
            .and_then(|value| value.checked_add(outbe_ocomp_protocol::OCB1_HEADER_LEN))
            .ok_or_else(|| fatal("OCOMP request profile byte cap overflow"))?;
        if len > max {
            return Err(fatal("OCOMP request profile exceeds byte cap"));
        }
        decode_request_profile(&self.ocomp_request_profile.read()?, limits).map(Some)
    }
}

pub(super) fn validate_request_profile(profile: &OcompRequestProfile) -> Result<()> {
    let capacity = &profile.capacity_profile;
    if profile.chain_id == 0
        || profile.genesis_hash.is_zero()
        || profile.fork_id.is_zero()
        || profile.protocol_bundle_hash.is_zero()
        || profile.correctness_profile_id.is_zero()
        || profile.source_availability_policy_id.is_zero()
        || capacity.profile_id.is_zero()
        || capacity.generated_limits_manifest_hash.is_zero()
    {
        return Err(fatal(
            "OCOMP request profile contains a reserved zero identity",
        ));
    }

    let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
    let max_tributes_per_work_shard = u32::try_from(candidate.max_tributes_per_work_shard)
        .map_err(|_| fatal("generated unit size exceeds u32"))?;
    let max_reference_currencies = u16::try_from(candidate.max_oracle_openings)
        .map_err(|_| fatal("generated reference-currency cap exceeds u16"))?;
    let max_oracle_entries = u32::try_from(candidate.max_oracle_wwd_pair_entries)
        .map_err(|_| fatal("generated Oracle entry cap exceeds u32"))?;
    let max_active_scurve_entries = u32::try_from(candidate.max_active_scurve_entries)
        .map_err(|_| fatal("generated S-curve entry cap exceeds u32"))?;

    if capacity.max_tributes_per_work_shard != max_tributes_per_work_shard
        || capacity.max_workers_per_domain != 4
        || capacity.max_pending_jobs != 2
        || capacity.max_intents_per_block != 1
        || capacity.max_activations_per_block != 1
        || capacity.max_ready_inspections_per_block != 1
        || capacity.max_expirations_per_block != 1
        || capacity.retry_backoff_blocks != 1
        || capacity.max_terminal_job_records != 365
        || capacity.max_reference_currencies == 0
        || capacity.max_reference_currencies > max_reference_currencies
        || capacity.max_oracle_wwd_pair_entries == 0
        || capacity.max_oracle_wwd_pair_entries > max_oracle_entries
        || capacity.max_active_scurve_entries == 0
        || capacity.max_active_scurve_entries > max_active_scurve_entries
        || capacity.result_deadline_blocks != 64
        || capacity.source_retention_after_terminal_blocks
            != candidate.source_retention_after_terminal_blocks
    {
        return Err(fatal("OCOMP request profile violates frozen PoC bounds"));
    }
    Ok(())
}

fn encode_request_profile(profile: &OcompRequestProfile, limits: &SchemaLimits) -> Result<Vec<u8>> {
    validate_request_profile(profile)?;
    let capacity = profile
        .capacity_profile
        .encode_canonical(limits)
        .map_err(|error| fatal(format!("encode OCOMP capacity profile: {error}")))?;
    let capacity_len =
        u32::try_from(capacity.len()).map_err(|_| fatal("capacity profile length exceeds u32"))?;
    let total = REQUEST_PROFILE_FIXED_LEN
        .checked_add(capacity.len())
        .ok_or_else(|| fatal("OCOMP request profile length overflow"))?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(total)
        .map_err(|_| fatal("allocate OCOMP request profile"))?;
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
        return Err(fatal("OCOMP request profile encoded length mismatch"));
    }
    Ok(encoded)
}

fn decode_request_profile(encoded: &[u8], limits: &SchemaLimits) -> Result<OcompRequestProfile> {
    if encoded.len() < REQUEST_PROFILE_FIXED_LEN {
        return Err(fatal("truncated OCOMP request profile"));
    }
    let mut reader = ProfileReader::new(encoded);
    if reader.take::<4>()? != REQUEST_PROFILE_MAGIC
        || u16::from_be_bytes(reader.take::<2>()?) != REQUEST_PROFILE_VERSION
    {
        return Err(fatal("OCOMP request profile magic/version mismatch"));
    }
    let chain_id = reader.u64()?;
    let genesis_hash = B256::from(reader.take::<32>()?);
    let fork_id = B256::from(reader.take::<32>()?);
    let protocol_bundle_hash = B256::from(reader.take::<32>()?);
    let correctness_profile_id = B256::from(reader.take::<32>()?);
    let source_availability_policy_id = B256::from(reader.take::<32>()?);
    let capacity_len = usize::try_from(reader.u32()?)
        .map_err(|_| fatal("OCOMP capacity profile length exceeds usize"))?;
    if capacity_len != reader.remaining() {
        return Err(fatal("OCOMP capacity profile length mismatch"));
    }
    let capacity_encoded = reader.take_dynamic(capacity_len)?;
    reader.finish()?;
    let capacity_profile = CapacityProfileV1::decode_canonical(capacity_encoded, limits)
        .map_err(|error| fatal(format!("decode OCOMP capacity profile: {error}")))?;
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
        return Err(fatal("non-canonical OCOMP request profile encoding"));
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
            .ok_or_else(|| fatal("OCOMP request profile offset overflow"))?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or_else(|| fatal("truncated OCOMP request profile"))?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| fatal("invalid OCOMP request profile field width"))
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
            .ok_or_else(|| fatal("OCOMP request profile offset overflow"))?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or_else(|| fatal("truncated OCOMP request profile field"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn finish(self) -> Result<()> {
        if self.offset == self.encoded.len() {
            Ok(())
        } else {
            Err(fatal("OCOMP request profile has trailing bytes"))
        }
    }
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
