use alloy_primitives::B256;
use outbe_ocomp_protocol::{profile::ProtocolBundleV1, SchemaLimits};
use outbe_primitives::error::Result;

use crate::{
    errors::corruption,
    profile::{validate_request_profile, OcompRequestProfile},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompProtocolAuthorityV1 {
    pub request_profile: OcompRequestProfile,
    pub protocol_bundle: ProtocolBundleV1,
}

/// One predecessor-bound OCOMP authority carried by an Update proposal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompSuccessorV1 {
    pub activation_height: u64,
    pub predecessor_protocol_bundle_hash: B256,
    pub authority: OcompProtocolAuthorityV1,
}

const AUTHORITY_MAGIC: [u8; 4] = *b"OCA1";
const SUCCESSOR_MAGIC: [u8; 4] = *b"OCS1";
const SUCCESSOR_VERSION: u16 = 1;

pub(super) struct CanonicalAuthorityParts {
    pub(super) request_profile: Vec<u8>,
    pub(super) protocol_bundle: Vec<u8>,
}

impl CanonicalAuthorityParts {
    pub(super) fn encode(
        authority: &OcompProtocolAuthorityV1,
        limits: &SchemaLimits,
    ) -> Result<Self> {
        let request_profile = authority.request_profile.encode_canonical(limits)?;
        let protocol_bundle = authority
            .protocol_bundle
            .encode_canonical(limits)
            .map_err(protocol_error)?;
        Ok(Self {
            request_profile,
            protocol_bundle,
        })
    }
}

impl OcompSuccessorV1 {
    pub fn validate_against(
        &self,
        predecessor: &OcompProtocolAuthorityV1,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<()> {
        validate_successor(predecessor, self, current_height, limits)
    }

    fn validate_predecessor_binding(
        &self,
        active: &OcompProtocolAuthorityV1,
        current_height: u64,
    ) -> Result<()> {
        if self.activation_height <= current_height
            || self.predecessor_protocol_bundle_hash != active.request_profile.protocol_bundle_hash
        {
            return Err(successor_invariant_error());
        }
        if self.authority.request_profile.chain_id != active.request_profile.chain_id
            || self.authority.request_profile.genesis_hash != active.request_profile.genesis_hash
        {
            return Err(successor_invariant_error());
        }
        Ok(())
    }

    fn validate_immutable_policy(&self, active: &OcompProtocolAuthorityV1) -> Result<()> {
        if self.authority.request_profile.capacity_profile
            != active.request_profile.capacity_profile
            || self.authority.request_profile.source_availability_policy_id
                != active.request_profile.source_availability_policy_id
        {
            return Err(successor_invariant_error());
        }
        Ok(())
    }

    fn validate_protocol_transition(&self, active: &OcompProtocolAuthorityV1) -> Result<()> {
        if self.authority.protocol_bundle.protocol_version
            != active
                .protocol_bundle
                .protocol_version
                .checked_add(1)
                .ok_or_else(|| corruption("OCOMP protocol version overflow"))?
            || self
                .authority
                .protocol_bundle
                .consensus_state_schema_version
                != active.protocol_bundle.consensus_state_schema_version
        {
            return Err(successor_invariant_error());
        }
        Ok(())
    }

    pub fn encode_canonical(&self, limits: &SchemaLimits) -> Result<Vec<u8>> {
        let authority = encode_authority(&self.authority, limits)?;
        let authority_len = u32::try_from(authority.len())
            .map_err(|_| corruption("OCOMP successor authority exceeds u32"))?;
        let mut bytes = Vec::with_capacity(4 + 2 + 8 + 32 + 4 + authority.len());
        bytes.extend_from_slice(&SUCCESSOR_MAGIC);
        bytes.extend_from_slice(&SUCCESSOR_VERSION.to_be_bytes());
        bytes.extend_from_slice(&self.activation_height.to_be_bytes());
        bytes.extend_from_slice(self.predecessor_protocol_bundle_hash.as_slice());
        bytes.extend_from_slice(&authority_len.to_be_bytes());
        bytes.extend_from_slice(&authority);
        validate_encoded_cap(bytes.len(), limits)?;
        Ok(bytes)
    }

    pub fn decode_canonical(bytes: &[u8], limits: &SchemaLimits) -> Result<Self> {
        validate_encoded_cap(bytes.len(), limits)?;
        if bytes.len() < 50
            || bytes[..4] != SUCCESSOR_MAGIC
            || u16::from_be_bytes(
                bytes[4..6]
                    .try_into()
                    .map_err(|_| corruption("truncated OCOMP successor"))?,
            ) != SUCCESSOR_VERSION
        {
            return Err(corruption("OCOMP successor magic/version mismatch"));
        }
        let activation_height = u64::from_be_bytes(
            bytes[6..14]
                .try_into()
                .map_err(|_| corruption("truncated OCOMP successor height"))?,
        );
        let predecessor_protocol_bundle_hash = B256::from_slice(&bytes[14..46]);
        let authority_len =
            usize::try_from(u32::from_be_bytes(bytes[46..50].try_into().map_err(
                |_| corruption("truncated OCOMP successor authority length"),
            )?))
            .map_err(|_| corruption("OCOMP successor authority length exceeds usize"))?;
        if bytes.len().checked_sub(50) != Some(authority_len) {
            return Err(corruption("OCOMP successor authority length mismatch"));
        }
        let decoded = Self {
            activation_height,
            predecessor_protocol_bundle_hash,
            authority: decode_authority(&bytes[50..], limits)?,
        };
        if decoded.encode_canonical(limits)? != bytes {
            return Err(corruption("non-canonical OCOMP successor encoding"));
        }
        Ok(decoded)
    }
}

pub(crate) fn validate_protocol_authority(
    authority: &OcompProtocolAuthorityV1,
    limits: &SchemaLimits,
) -> Result<()> {
    validate_request_profile(&authority.request_profile)?;
    let bundle = &authority.protocol_bundle;
    let bundle_hash = bundle
        .protocol_bundle_hash(limits)
        .map_err(protocol_error)?;
    if bundle_hash != authority.request_profile.protocol_bundle_hash
        || bundle.fork_id != authority.request_profile.fork_id
    {
        return Err(corruption(
            "OCOMP protocol bundle differs from the request profile",
        ));
    }
    if bundle.correctness_profile_id != authority.request_profile.correctness_profile_id
        || bundle.capacity_profile_id != authority.request_profile.capacity_profile.profile_id
    {
        return Err(corruption(
            "OCOMP protocol bundle differs from the request profile",
        ));
    }
    bundle
        .validate_lysis_v1_input_codecs()
        .map_err(protocol_error)
}

pub(super) fn protocol_error(
    error: impl core::fmt::Display,
) -> outbe_primitives::error::PrecompileError {
    corruption(format!("invalid OCOMP protocol authority: {error}"))
}

pub(super) fn validate_successor(
    active: &OcompProtocolAuthorityV1,
    successor: &OcompSuccessorV1,
    current_height: u64,
    limits: &SchemaLimits,
) -> Result<()> {
    validate_protocol_authority(&successor.authority, limits)?;
    successor.validate_predecessor_binding(active, current_height)?;
    successor.validate_immutable_policy(active)?;
    successor.validate_protocol_transition(active)
}

fn successor_invariant_error() -> outbe_primitives::error::PrecompileError {
    corruption("OCOMP successor violates predecessor or immutable-policy invariants")
}

pub(super) fn encode_authority(
    authority: &OcompProtocolAuthorityV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>> {
    validate_protocol_authority(authority, limits)?;
    let profile = authority.request_profile.encode_canonical(limits)?;
    let bundle = authority
        .protocol_bundle
        .encode_canonical(limits)
        .map_err(protocol_error)?;
    let profile_len = u32::try_from(profile.len())
        .map_err(|_| corruption("OCOMP authority profile exceeds u32"))?;
    let bundle_len = u32::try_from(bundle.len())
        .map_err(|_| corruption("OCOMP authority bundle exceeds u32"))?;
    let mut bytes = Vec::with_capacity(12 + profile.len() + bundle.len());
    bytes.extend_from_slice(&AUTHORITY_MAGIC);
    bytes.extend_from_slice(&profile_len.to_be_bytes());
    bytes.extend_from_slice(&profile);
    bytes.extend_from_slice(&bundle_len.to_be_bytes());
    bytes.extend_from_slice(&bundle);
    validate_encoded_cap(bytes.len(), limits)?;
    Ok(bytes)
}

pub(super) fn decode_authority(
    bytes: &[u8],
    limits: &SchemaLimits,
) -> Result<OcompProtocolAuthorityV1> {
    validate_encoded_cap(bytes.len(), limits)?;
    if bytes.len() < 12 || bytes[..4] != AUTHORITY_MAGIC {
        return Err(corruption("OCOMP authority magic mismatch"));
    }
    let profile_len = usize::try_from(u32::from_be_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| corruption("truncated OCOMP authority profile length"))?,
    ))
    .map_err(|_| corruption("OCOMP authority profile length exceeds usize"))?;
    let profile_end = 8usize
        .checked_add(profile_len)
        .ok_or_else(|| corruption("OCOMP authority profile length overflow"))?;
    let bundle_len_end = profile_end
        .checked_add(4)
        .ok_or_else(|| corruption("OCOMP authority bundle offset overflow"))?;
    if bundle_len_end > bytes.len() {
        return Err(corruption("truncated OCOMP authority profile"));
    }
    let bundle_len = usize::try_from(u32::from_be_bytes(
        bytes[profile_end..bundle_len_end]
            .try_into()
            .map_err(|_| corruption("truncated OCOMP authority bundle length"))?,
    ))
    .map_err(|_| corruption("OCOMP authority bundle length exceeds usize"))?;
    let bundle_end = bundle_len_end
        .checked_add(bundle_len)
        .ok_or_else(|| corruption("OCOMP authority bundle length overflow"))?;
    if bundle_end != bytes.len() {
        return Err(corruption("OCOMP authority bundle length mismatch"));
    }
    let authority = OcompProtocolAuthorityV1 {
        request_profile: OcompRequestProfile::decode_canonical(&bytes[8..profile_end], limits)?,
        protocol_bundle: ProtocolBundleV1::decode_canonical(
            &bytes[bundle_len_end..bundle_end],
            limits,
        )
        .map_err(protocol_error)?,
    };
    validate_protocol_authority(&authority, limits)?;
    if encode_authority(&authority, limits)? != bytes {
        return Err(corruption("non-canonical OCOMP authority encoding"));
    }
    Ok(authority)
}

fn validate_encoded_cap(length: usize, limits: &SchemaLimits) -> Result<()> {
    if length == 0 || length > limits.codec.max_allocation_bytes {
        return Err(corruption(
            "OCOMP Registry canonical object exceeds byte cap",
        ));
    }
    Ok(())
}
