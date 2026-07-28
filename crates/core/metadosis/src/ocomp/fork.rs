//! Immutable chain-manifest binding for the OCOMP PoC fork.
//!
//! The artifact is parsed once from `genesis.config` before node startup. It is
//! consensus configuration: local OCOMP process readiness and runtime file
//! reload never select these values.

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    committee::OcompCommitteeSnapshotV1, profile::ProtocolBundleV1, SchemaLimits,
};
use outbe_primitives::error::{PrecompileError, Result};

use super::{
    activation::{validate_activation_authority, OcompActivationAuthorityV1},
    schema::{validate_request_profile, OcompRequestProfile},
};
use crate::schema::MetadosisContract;

const FORK_INSTALL_MAGIC: [u8; 4] = *b"OFI1";
const FORK_INSTALL_VERSION: u16 = 1;
const FORK_INSTALL_FIXED_LEN: usize = 4 + 2 + 1 + 8 + 4 + 4 + 4;
const FORK_INSTALL_HASH_DOMAIN: &[u8] = b"OUTBE_OCOMP_FORK_INSTALL_V1\0";

/// Canonical fresh-devnet activation height for final PoC evidence.
pub const OCOMP_POC_FINAL_ACTIVATION_HEIGHT: u64 = 32;

/// Evidence eligibility of one chain-manifest binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OcompForkInstallClassification {
    Measurement = 1,
    Final = 2,
}

impl OcompForkInstallClassification {
    fn decode(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Measurement),
            2 => Ok(Self::Final),
            _ => Err(fatal("unknown OCOMP fork-install classification")),
        }
    }
}

/// Complete immutable authority installed by `OcompLifecycleBegin` at `H`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompForkInstallV1 {
    pub classification: OcompForkInstallClassification,
    pub activation_height: u64,
    pub request_profile: OcompRequestProfile,
    pub protocol_bundle: ProtocolBundleV1,
    pub result_committee: OcompCommitteeSnapshotV1,
}

impl OcompForkInstallV1 {
    /// Validates every chain and authority binding without touching state.
    pub fn validate_for_chain(
        &self,
        expected_chain_id: u64,
        expected_genesis_hash: B256,
        limits: &SchemaLimits,
    ) -> Result<()> {
        if self.activation_height == 0 {
            return Err(fatal("OCOMP fork activation height must be non-zero"));
        }
        if self.classification == OcompForkInstallClassification::Final
            && self.activation_height != OCOMP_POC_FINAL_ACTIVATION_HEIGHT
        {
            return Err(fatal("final OCOMP PoC fork must activate at height 32"));
        }

        validate_request_profile(&self.request_profile)?;
        if self.request_profile.chain_id != expected_chain_id {
            return Err(fatal("OCOMP fork-install chain id mismatch"));
        }
        if self.request_profile.genesis_hash != expected_genesis_hash {
            return Err(fatal("OCOMP fork-install genesis hash mismatch"));
        }
        if self.protocol_bundle.protocol_version != 1 {
            return Err(fatal("unsupported OCOMP protocol bundle version"));
        }
        validate_activation_authority(
            &self.request_profile,
            &self.protocol_bundle,
            &self.result_committee,
            limits,
        )?;
        if self.result_committee.ordered_members.iter().any(|member| {
            member.valid_from_height > self.activation_height
                || member.valid_until_height_exclusive <= self.activation_height
        }) {
            return Err(fatal(
                "OCOMP result committee is not valid at fork activation height",
            ));
        }
        Ok(())
    }

    /// Encodes the exact bounded startup artifact.
    pub fn encode_canonical(&self, limits: &SchemaLimits) -> Result<Vec<u8>> {
        self.validate_for_chain(
            self.request_profile.chain_id,
            self.request_profile.genesis_hash,
            limits,
        )?;
        let request_profile = self.request_profile.encode_canonical(limits)?;
        let protocol_bundle = self
            .protocol_bundle
            .encode_canonical(limits)
            .map_err(protocol_error)?;
        let result_committee = self
            .result_committee
            .encode_canonical(limits)
            .map_err(protocol_error)?;
        validate_nested_length("request profile", request_profile.len(), limits)?;
        validate_nested_length("protocol bundle", protocol_bundle.len(), limits)?;
        validate_nested_length("result committee", result_committee.len(), limits)?;

        let total = FORK_INSTALL_FIXED_LEN
            .checked_add(request_profile.len())
            .and_then(|value| value.checked_add(protocol_bundle.len()))
            .and_then(|value| value.checked_add(result_committee.len()))
            .ok_or_else(|| fatal("OCOMP fork-install length overflow"))?;
        validate_total_length(total, limits)?;
        let mut encoded = Vec::new();
        encoded
            .try_reserve_exact(total)
            .map_err(|_| fatal("allocate OCOMP fork-install encoding"))?;
        encoded.extend_from_slice(&FORK_INSTALL_MAGIC);
        encoded.extend_from_slice(&FORK_INSTALL_VERSION.to_be_bytes());
        encoded.push(self.classification as u8);
        encoded.extend_from_slice(&self.activation_height.to_be_bytes());
        append_bounded(&mut encoded, &request_profile)?;
        append_bounded(&mut encoded, &protocol_bundle)?;
        append_bounded(&mut encoded, &result_committee)?;
        if encoded.len() != total {
            return Err(fatal("OCOMP fork-install encoded length mismatch"));
        }
        Ok(encoded)
    }

    /// Decodes an exact canonical artifact and rejects trailing or alternate
    /// representations.
    pub fn decode_canonical(encoded: &[u8], limits: &SchemaLimits) -> Result<Self> {
        validate_total_length(encoded.len(), limits)?;
        let mut reader = ForkReader::new(encoded);
        if reader.take::<4>()? != FORK_INSTALL_MAGIC
            || u16::from_be_bytes(reader.take::<2>()?) != FORK_INSTALL_VERSION
        {
            return Err(fatal("OCOMP fork-install magic/version mismatch"));
        }
        let classification = OcompForkInstallClassification::decode(reader.u8()?)?;
        let activation_height = reader.u64()?;
        let request_profile_bytes = reader.bounded(limits)?;
        let protocol_bundle_bytes = reader.bounded(limits)?;
        let result_committee_bytes = reader.bounded(limits)?;
        if reader.remaining() != 0 {
            return Err(fatal("trailing OCOMP fork-install bytes"));
        }
        let install = Self {
            classification,
            activation_height,
            request_profile: OcompRequestProfile::decode_canonical(request_profile_bytes, limits)?,
            protocol_bundle: ProtocolBundleV1::decode_canonical(protocol_bundle_bytes, limits)
                .map_err(protocol_error)?,
            result_committee: OcompCommitteeSnapshotV1::decode_canonical(
                result_committee_bytes,
                limits,
            )
            .map_err(protocol_error)?,
        };
        install.validate_for_chain(
            install.request_profile.chain_id,
            install.request_profile.genesis_hash,
            limits,
        )?;
        if install.encode_canonical(limits)? != encoded {
            return Err(fatal("non-canonical OCOMP fork-install encoding"));
        }
        Ok(install)
    }

    /// Hash recorded by the chain/evidence manifest.
    pub fn install_hash(&self, limits: &SchemaLimits) -> Result<B256> {
        let encoded = self.encode_canonical(limits)?;
        let total = FORK_INSTALL_HASH_DOMAIN
            .len()
            .checked_add(encoded.len())
            .ok_or_else(|| fatal("OCOMP fork-install hash preimage overflow"))?;
        let mut preimage = Vec::new();
        preimage
            .try_reserve_exact(total)
            .map_err(|_| fatal("allocate OCOMP fork-install hash preimage"))?;
        preimage.extend_from_slice(FORK_INSTALL_HASH_DOMAIN);
        preimage.extend_from_slice(&encoded);
        Ok(keccak256(preimage))
    }
}

impl MetadosisContract<'_> {
    /// Installs the complete fork authority once at the manifest activation
    /// height. Partial pre-existing state is never repaired implicitly.
    pub fn initialize_ocomp_fork_install(
        &mut self,
        install: &OcompForkInstallV1,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<()> {
        if current_height != install.activation_height {
            return Err(fatal(
                "OCOMP fork install attempted outside its activation height",
            ));
        }
        let chain_id = self.storage.chain_id()?;
        install.validate_for_chain(chain_id, install.request_profile.genesis_hash, limits)?;
        let expected_authority = OcompActivationAuthorityV1 {
            bundle: install.protocol_bundle.clone(),
            result_committee: install.result_committee.clone(),
        };
        match (
            self.read_ocomp_request_profile(limits)?,
            self.read_ocomp_activation_authority(limits)?,
        ) {
            (None, None) => {}
            (Some(profile), Some(authority))
                if profile == install.request_profile && authority == expected_authority =>
            {
                return Ok(());
            }
            (Some(_), Some(_)) => {
                return Err(fatal("OCOMP fork authority is immutable"));
            }
            _ => {
                return Err(fatal("partial OCOMP fork authority is fatal"));
            }
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.initialize_ocomp_request_profile(&install.request_profile, limits)?;
            self.initialize_ocomp_activation_authority(
                &install.protocol_bundle,
                &install.result_committee,
                limits,
            )
        })
    }
}

fn append_bounded(target: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let length =
        u32::try_from(value.len()).map_err(|_| fatal("OCOMP fork-install field exceeds u32"))?;
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(value);
    Ok(())
}

fn validate_nested_length(name: &'static str, length: usize, limits: &SchemaLimits) -> Result<()> {
    if length > limits.codec.max_allocation_bytes {
        return Err(fatal(format!("OCOMP fork-install {name} exceeds byte cap")));
    }
    Ok(())
}

fn validate_total_length(length: usize, limits: &SchemaLimits) -> Result<()> {
    let nested_cap = limits
        .codec
        .max_allocation_bytes
        .checked_mul(3)
        .and_then(|value| value.checked_add(FORK_INSTALL_FIXED_LEN))
        .ok_or_else(|| fatal("OCOMP fork-install byte cap overflow"))?;
    if length < FORK_INSTALL_FIXED_LEN || length > nested_cap {
        return Err(fatal("OCOMP fork-install exceeds byte cap"));
    }
    Ok(())
}

struct ForkReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ForkReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or_else(|| fatal("OCOMP fork-install offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| fatal("truncated OCOMP fork-install"))?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| fatal("OCOMP fork-install fixed-width decode"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take()?))
    }

    fn bounded(&mut self, limits: &SchemaLimits) -> Result<&'a [u8]> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| fatal("OCOMP fork-install field length exceeds usize"))?;
        validate_nested_length("field", length, limits)?;
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| fatal("OCOMP fork-install field offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| fatal("truncated OCOMP fork-install field"))?;
        self.offset = end;
        Ok(bytes)
    }
}

fn protocol_error(error: impl core::fmt::Display) -> PrecompileError {
    fatal(format!("invalid OCOMP fork-install object: {error}"))
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
