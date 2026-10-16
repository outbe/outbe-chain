//! Canonical fresh-genesis OCOMP authority manifest.

use std::collections::BTreeSet;

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    committee::OcompKeyRegistrationV1, profile::ProtocolBundleV1, SchemaLimits,
};
use outbe_primitives::error::Result;

use crate::{
    errors::OcompRegistryError,
    runtime::{validate_protocol_authority, OcompProtocolAuthorityV1},
    OcompRequestProfile,
};

const MAGIC: [u8; 4] = *b"OFI1";
const VERSION: u16 = 1;
const FIXED_LEN: usize = 4 + 2 + 1 + 8 + 4 + 4 + 4;
const HASH_DOMAIN: &[u8] = b"OUTBE_OCOMP_FORK_INSTALL_V1\0";

pub const OCOMP_POC_FINAL_ACTIVATION_HEIGHT: u64 = 32;

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
            _ => Err(OcompRegistryError::UnknownForkInstallClassification.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompForkInstallV1 {
    pub classification: OcompForkInstallClassification,
    pub activation_height: u64,
    pub request_profile: OcompRequestProfile,
    pub protocol_bundle: ProtocolBundleV1,
    pub founder_registrations: Vec<OcompKeyRegistrationV1>,
}

impl OcompForkInstallV1 {
    pub fn authority(&self) -> OcompProtocolAuthorityV1 {
        OcompProtocolAuthorityV1 {
            request_profile: self.request_profile.clone(),
            protocol_bundle: self.protocol_bundle.clone(),
        }
    }

    pub fn validate_for_chain(
        &self,
        expected_chain_id: u64,
        expected_genesis_hash: B256,
        limits: &SchemaLimits,
    ) -> Result<()> {
        if self.activation_height == 0 {
            return Err(OcompRegistryError::ForkActivationHeightZero.into());
        }
        if self.classification == OcompForkInstallClassification::Final
            && self.activation_height != OCOMP_POC_FINAL_ACTIVATION_HEIGHT
        {
            return Err(OcompRegistryError::FinalPocForkActivationHeightMismatch.into());
        }
        if self.request_profile.chain_id != expected_chain_id
            || self.request_profile.genesis_hash != expected_genesis_hash
        {
            return Err(OcompRegistryError::ForkInstallChainIdentityMismatch.into());
        }
        if self.protocol_bundle.protocol_version != 1 {
            return Err(OcompRegistryError::UnsupportedInitialProtocolBundleVersion.into());
        }
        validate_protocol_authority(&self.authority(), limits)?;
        self.validate_founders(limits)
    }

    fn validate_founders(&self, limits: &SchemaLimits) -> Result<()> {
        let max = usize::try_from(outbe_consensus::bls::MAX_VALIDATORS)
            .map_err(|_| OcompRegistryError::ConsensusValidatorBoundExceedsUsize)?;
        if self.founder_registrations.is_empty() || self.founder_registrations.len() > max {
            return Err(OcompRegistryError::FounderRegistrationCountOutOfBounds { max }.into());
        }
        let mut identities = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for registration in &self.founder_registrations {
            self.validate_founder_identity(registration)?;
            if !identities.insert(registration.core.validator_identity_hash)
                || !keys.insert(registration.core.ocomp_public_key_sec1)
            {
                return Err(OcompRegistryError::InvalidOrDuplicateFounderRegistration.into());
            }
            registration
                .validate_proof_of_possession(limits)
                .map_err(OcompRegistryError::from)?;
        }
        Ok(())
    }

    fn validate_founder_identity(&self, registration: &OcompKeyRegistrationV1) -> Result<()> {
        if registration.core.chain_id != self.request_profile.chain_id
            || registration.core.genesis_hash != self.request_profile.genesis_hash
            || registration.core.validator_identity_hash.is_zero()
        {
            return Err(OcompRegistryError::InvalidOrDuplicateFounderRegistration.into());
        }
        Ok(())
    }

    pub fn encode_canonical(&self, limits: &SchemaLimits) -> Result<Vec<u8>> {
        self.validate_for_chain(
            self.request_profile.chain_id,
            self.request_profile.genesis_hash,
            limits,
        )?;
        let profile = self.request_profile.encode_canonical(limits)?;
        let bundle = self
            .protocol_bundle
            .encode_canonical(limits)
            .map_err(OcompRegistryError::from)?;
        validate_nested(profile.len(), limits)?;
        validate_nested(bundle.len(), limits)?;
        let mut registrations = Vec::with_capacity(self.founder_registrations.len());
        for registration in &self.founder_registrations {
            let bytes = registration
                .encode_canonical(limits)
                .map_err(OcompRegistryError::from)?;
            validate_nested(bytes.len(), limits)?;
            registrations.push(bytes);
        }
        let total = registrations.iter().try_fold(
            FIXED_LEN
                .checked_add(profile.len())
                .and_then(|n| n.checked_add(bundle.len()))
                .ok_or(OcompRegistryError::ForkInstallLengthOverflow)?,
            |sum, item| -> Result<usize> {
                sum.checked_add(4)
                    .and_then(|n| n.checked_add(item.len()))
                    .ok_or_else(|| OcompRegistryError::ForkInstallLengthOverflow.into())
            },
        )?;
        validate_total(total, limits)?;
        let mut encoded = Vec::with_capacity(total);
        encoded.extend_from_slice(&MAGIC);
        encoded.extend_from_slice(&VERSION.to_be_bytes());
        encoded.push(self.classification as u8);
        encoded.extend_from_slice(&self.activation_height.to_be_bytes());
        append_bounded(&mut encoded, &profile)?;
        append_bounded(&mut encoded, &bundle)?;
        encoded.extend_from_slice(
            &u32::try_from(registrations.len())
                .map_err(|_| OcompRegistryError::FounderCountExceedsU32)?
                .to_be_bytes(),
        );
        for registration in registrations {
            append_bounded(&mut encoded, &registration)?;
        }
        Ok(encoded)
    }

    pub fn decode_canonical(encoded: &[u8], limits: &SchemaLimits) -> Result<Self> {
        validate_total(encoded.len(), limits)?;
        let mut reader = Reader::new(encoded);
        if reader.take::<4>()? != MAGIC || u16::from_be_bytes(reader.take::<2>()?) != VERSION {
            return Err(OcompRegistryError::ForkInstallMagicOrVersionMismatch.into());
        }
        let classification = OcompForkInstallClassification::decode(reader.take::<1>()?[0])?;
        let activation_height = u64::from_be_bytes(reader.take::<8>()?);
        let profile = OcompRequestProfile::decode_canonical(reader.bounded(limits)?, limits)?;
        let bundle = ProtocolBundleV1::decode_canonical(reader.bounded(limits)?, limits)
            .map_err(OcompRegistryError::from)?;
        let count = usize::try_from(u32::from_be_bytes(reader.take::<4>()?))
            .map_err(|_| OcompRegistryError::FounderCountExceedsUsize)?;
        let max = usize::try_from(outbe_consensus::bls::MAX_VALIDATORS)
            .map_err(|_| OcompRegistryError::ConsensusValidatorBoundExceedsUsize)?;
        if count == 0 || count > max {
            return Err(OcompRegistryError::InvalidFounderRegistrationCount.into());
        }
        let mut founder_registrations = Vec::with_capacity(count);
        for _ in 0..count {
            founder_registrations.push(
                OcompKeyRegistrationV1::decode_canonical(reader.bounded(limits)?, limits)
                    .map_err(OcompRegistryError::from)?,
            );
        }
        if reader.remaining() != 0 {
            return Err(OcompRegistryError::TrailingForkInstallBytes.into());
        }
        let install = Self {
            classification,
            activation_height,
            request_profile: profile,
            protocol_bundle: bundle,
            founder_registrations,
        };
        install.validate_for_chain(
            install.request_profile.chain_id,
            install.request_profile.genesis_hash,
            limits,
        )?;
        if install.encode_canonical(limits)? != encoded {
            return Err(OcompRegistryError::NonCanonicalForkInstallEncoding.into());
        }
        Ok(install)
    }

    pub fn install_hash(&self, limits: &SchemaLimits) -> Result<B256> {
        let encoded = self.encode_canonical(limits)?;
        let mut preimage = Vec::with_capacity(HASH_DOMAIN.len() + encoded.len());
        preimage.extend_from_slice(HASH_DOMAIN);
        preimage.extend_from_slice(&encoded);
        Ok(keccak256(preimage))
    }
}

fn append_bounded(target: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    target.extend_from_slice(
        &u32::try_from(bytes.len())
            .map_err(|_| OcompRegistryError::ForkInstallFieldExceedsU32)?
            .to_be_bytes(),
    );
    target.extend_from_slice(bytes);
    Ok(())
}

fn validate_nested(length: usize, limits: &SchemaLimits) -> Result<()> {
    if length > limits.codec.max_allocation_bytes {
        Err(OcompRegistryError::ForkInstallFieldExceedsByteCap.into())
    } else {
        Ok(())
    }
}

fn validate_total(length: usize, limits: &SchemaLimits) -> Result<()> {
    if length < FIXED_LEN || length > limits.codec.max_allocation_bytes {
        Err(OcompRegistryError::ForkInstallExceedsByteCap.into())
    } else {
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
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
            .ok_or(OcompRegistryError::ForkInstallOffsetOverflow)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(OcompRegistryError::TruncatedForkInstall)?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| OcompRegistryError::ForkInstallFixedWidthDecode.into())
    }
    fn bounded(&mut self, limits: &SchemaLimits) -> Result<&'a [u8]> {
        let length = usize::try_from(u32::from_be_bytes(self.take::<4>()?))
            .map_err(|_| OcompRegistryError::FieldLengthExceedsUsize)?;
        validate_nested(length, limits)?;
        let end = self
            .offset
            .checked_add(length)
            .ok_or(OcompRegistryError::FieldOffsetOverflow)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(OcompRegistryError::TruncatedForkInstallField)?;
        self.offset = end;
        Ok(bytes)
    }
}
