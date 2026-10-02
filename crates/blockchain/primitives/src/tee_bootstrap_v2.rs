//! Canonical block-1 TEE bootstrap payload.
//!
//! Production `DcapRequired` payloads deduplicate complete Intel collateral.
//! A separate `GramineDirectDev` chain uses the same OST3 envelope with direct
//! development evidence and an empty collateral pool.

mod assembly;
mod codec;
mod validation;

use std::{cmp::Ordering, collections::BTreeMap};

use alloy_primitives::{keccak256, Address, Bytes, B256};

use crate::system_tx::{
    BOOTSTRAP_BLOCK_GAS_LIMIT, SYSTEM_TX_NON_ZERO_BYTE_GAS, SYSTEM_TX_VISIBLE_GAS_FLOOR,
};
use crate::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, CodecError,
    DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1, GramineDirectEvidenceV1,
    RegistrationIntentV1, SystemGasScheduleV1, TeeBootstrapGasInputV1, TeePolicyV1,
    TeeRegistryGasScheduleV1, ValidatorNodeBindingV1, MAX_ATTESTATION_EVIDENCE_BYTES,
    MAX_COLLATERAL_COMPONENT_BYTES, MAX_EVIDENCE_CALL_FRAMING_BYTES, MAX_QUOTE_BYTES,
    MAX_TEE_BOOTSTRAP_BYTES,
};

const MAGIC: &[u8; 4] = b"TTB2";
const SIGNING_DOMAIN: &[u8] = b"outbe/tee/bootstrap/v2";
const SYSTEM_CALLDATA_FRAMING_BYTES: usize = 5;
const MAX_BOOTSTRAP_PARTICIPANTS: usize = 256;
const MAX_COLLATERAL_POOL_COMPONENTS: usize = MAX_BOOTSTRAP_PARTICIPANTS * 8;
const MAX_GROUP_PUBLIC_KEY_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeeBootstrapParticipantEvidenceV2 {
    Dcap {
        quote: Vec<u8>,
        collateral_component_indices: [u16; 8],
    },
    GramineDirectDev {
        dev_attestation_public: [u8; 32],
        dev_signature: [u8; 64],
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeBootstrapParticipantV2 {
    pub intent: RegistrationIntentV1,
    pub validator_binding: ValidatorNodeBindingV1,
    pub validator_signature: [u8; 65],
    pub node_binding_signature: [u8; 65],
    pub evidence: TeeBootstrapParticipantEvidenceV2,
    pub node_signature: [u8; 65],
    pub enclave_signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeBootstrapCommitteeSignatureV2 {
    pub validator: Address,
    pub signature: [u8; 65],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeBootstrapParticipantSubmissionV2 {
    pub evidence: AttestationEvidenceV1,
    pub validator_binding: ValidatorNodeBindingV1,
    pub validator_signature: [u8; 65],
    pub node_binding_signature: [u8; 65],
    pub node_signature: [u8; 65],
    pub enclave_signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeBootstrapAuthorityV2 {
    pub policy: TeePolicyV1,
    pub committee_snapshot_hash: B256,
    pub committee_snapshot_block: u64,
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
    pub dkg_transcript_hash: B256,
    pub tribute_offer_public_key: B256,
    pub tribute_offer_group_public_key: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeBootstrapV2 {
    pub policy: TeePolicyV1,
    pub committee_snapshot_hash: B256,
    pub committee_snapshot_block: u64,
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
    pub dkg_transcript_hash: B256,
    pub tribute_offer_public_key: B256,
    pub tribute_offer_group_public_key: Bytes,
    pub collateral_pool: Vec<DcapCollateralComponentV1>,
    pub participants: Vec<TeeBootstrapParticipantV2>,
    pub committee_signatures: Vec<TeeBootstrapCommitteeSignatureV2>,
}

impl TeeBootstrapV2 {
    /// Assemble the deterministic unsigned body from complete per-validator
    /// evidence and the existing DKG/offer-key result. Committee signature
    /// records are installed in canonical validator order with zeroed bytes so
    /// every node derives the same signing hash; coordination replaces only
    /// those excluded signature bytes.
    pub fn assemble_unsigned(
        authority: TeeBootstrapAuthorityV2,
        submissions: Vec<TeeBootstrapParticipantSubmissionV2>,
    ) -> Result<Self, CodecError> {
        let payload = assembly::assemble_unsigned(authority, submissions)?;
        payload.preflight()?;
        Ok(payload)
    }

    /// Reject an assembled payload before committee signing when its canonical
    /// bytes or worst-case visible gas cannot fit the bootstrap block.
    pub fn preflight(&self) -> Result<(), CodecError> {
        self.validate()?;
        let full_calldata_len = self
            .canonical_encoded_len()?
            .checked_add(SYSTEM_CALLDATA_FRAMING_BYTES)
            .ok_or(CodecError::ArithmeticOverflow)?;
        let worst_case_intrinsic = u64::try_from(full_calldata_len)
            .map_err(|_| CodecError::ArithmeticOverflow)?
            .checked_mul(SYSTEM_TX_NON_ZERO_BYTE_GAS)
            .and_then(|gas| gas.checked_add(SYSTEM_TX_VISIBLE_GAS_FLOOR))
            .ok_or(CodecError::ArithmeticOverflow)?;
        let protocol_precharge = self.protocol_precharge(
            &SystemGasScheduleV1::normative(),
            &TeeRegistryGasScheduleV1::normative(),
        )?;
        let required_gas = worst_case_intrinsic
            .checked_add(protocol_precharge)
            .ok_or(CodecError::ArithmeticOverflow)?;
        if required_gas > BOOTSTRAP_BLOCK_GAS_LIMIT {
            return Err(CodecError::LimitExceeded {
                field: "TeeBootstrapV2 worst-case visible gas",
                limit: usize::try_from(BOOTSTRAP_BLOCK_GAS_LIMIT)
                    .map_err(|_| CodecError::ArithmeticOverflow)?,
                actual: usize::try_from(required_gas)
                    .map_err(|_| CodecError::ArithmeticOverflow)?,
            });
        }
        Ok(())
    }

    pub fn encode_canonical(&self) -> Result<Bytes, CodecError> {
        self.validate()?;
        let mut out = self.encode_body()?;
        put_len_u16(&mut out, self.committee_signatures.len())?;
        for signature in &self.committee_signatures {
            out.extend_from_slice(signature.validator.as_slice());
            out.extend_from_slice(&signature.signature);
        }
        enforce_full_calldata_cap(out.len())?;
        Ok(Bytes::from(out))
    }

    pub fn signing_hash(&self) -> Result<B256, CodecError> {
        self.validate()?;
        let body = self.encode_body()?;
        let capacity = SIGNING_DOMAIN
            .len()
            .checked_add(body.len())
            .ok_or(CodecError::ArithmeticOverflow)?;
        let mut preimage = Vec::with_capacity(capacity);
        preimage.extend_from_slice(SIGNING_DOMAIN);
        preimage.extend_from_slice(&body);
        Ok(keccak256(preimage))
    }

    pub fn protocol_precharge(
        &self,
        system_schedule: &SystemGasScheduleV1,
        tee_registry_schedule: &TeeRegistryGasScheduleV1,
    ) -> Result<u64, CodecError> {
        self.validate()?;
        let full_calldata_len = self
            .canonical_encoded_len()?
            .checked_add(SYSTEM_CALLDATA_FRAMING_BYTES)
            .ok_or(CodecError::ArithmeticOverflow)?;
        let mut logical_evidence_lengths = Vec::with_capacity(self.participants.len());
        for participant_index in 0..self.participants.len() {
            logical_evidence_lengths.push(
                self.logical_evidence(participant_index)?
                    .encode_canonical()?
                    .len(),
            );
        }
        let collateral_component_count = match self.policy.attestation_mode {
            AttestationMode::DcapRequired => self
                .participants
                .len()
                .checked_mul(8)
                .ok_or(CodecError::ArithmeticOverflow)?,
            AttestationMode::GramineDirectDev => 0,
        };
        system_schedule.tee_bootstrap_precharge(
            tee_registry_schedule,
            TeeBootstrapGasInputV1 {
                full_calldata_len,
                logical_evidence_lengths: &logical_evidence_lengths,
                active_rule_count: self.policy.measurement_rules.len(),
                collateral_component_count,
                committee_signature_count: self.committee_signatures.len(),
            },
        )
    }

    fn encode_body(&self) -> Result<Vec<u8>, CodecError> {
        codec::encode_body(self)
    }

    fn encoded_body_len(&self) -> Result<usize, CodecError> {
        let policy_len = self.policy.encode_canonical()?.len();
        let mut len = 4usize;
        checked_add_len(&mut len, 4)?;
        checked_add_len(&mut len, policy_len)?;
        // committee hash + three epochs/heights + transcript hash + offer key
        checked_add_len(&mut len, 32 + 8 * 3 + 32 + 32)?;
        checked_add_len(&mut len, 4)?;
        checked_add_len(&mut len, self.tribute_offer_group_public_key.len())?;
        checked_add_len(&mut len, 2)?;
        for component in &self.collateral_pool {
            checked_add_len(&mut len, 1 + 4)?;
            checked_add_len(&mut len, component.bytes.len())?;
        }
        checked_add_len(&mut len, 2)?;
        for participant in &self.participants {
            let intent_len = participant.intent.encode_canonical()?.len();
            checked_add_len(&mut len, 4)?;
            checked_add_len(&mut len, intent_len)?;
            checked_add_len(&mut len, ValidatorNodeBindingV1::CANONICAL_LEN + 65 + 65)?;
            checked_add_len(&mut len, 1)?;
            match &participant.evidence {
                TeeBootstrapParticipantEvidenceV2::Dcap { quote, .. } => {
                    checked_add_len(&mut len, 4)?;
                    checked_add_len(&mut len, quote.len())?;
                    checked_add_len(&mut len, 8 * 2)?;
                }
                TeeBootstrapParticipantEvidenceV2::GramineDirectDev { .. } => {
                    checked_add_len(&mut len, 32 + 64)?;
                }
            }
            checked_add_len(&mut len, 65 + 64)?;
        }
        Ok(len)
    }

    fn canonical_encoded_len(&self) -> Result<usize, CodecError> {
        let signature_bytes = self
            .committee_signatures
            .len()
            .checked_mul(20 + 65)
            .ok_or(CodecError::ArithmeticOverflow)?;
        self.encoded_body_len()?
            .checked_add(2)
            .and_then(|len| len.checked_add(signature_bytes))
            .ok_or(CodecError::ArithmeticOverflow)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let payload = codec::decode_canonical(input)?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn logical_evidence(
        &self,
        participant_index: usize,
    ) -> Result<AttestationEvidenceV1, CodecError> {
        codec::logical_evidence(self, participant_index)
    }

    fn validate(&self) -> Result<(), CodecError> {
        validation::validate(self)
    }
}

fn checked_add_len(total: &mut usize, value: usize) -> Result<(), CodecError> {
    *total = total
        .checked_add(value)
        .ok_or(CodecError::ArithmeticOverflow)?;
    Ok(())
}

fn compare_components(
    left: &DcapCollateralComponentV1,
    right: &DcapCollateralComponentV1,
) -> Ordering {
    (left.kind as u8)
        .cmp(&(right.kind as u8))
        .then_with(|| left.bytes.cmp(&right.bytes))
}

fn enforce_full_calldata_cap(body_len: usize) -> Result<(), CodecError> {
    let full_len = body_len
        .checked_add(SYSTEM_CALLDATA_FRAMING_BYTES)
        .ok_or(CodecError::ArithmeticOverflow)?;
    enforce_limit(
        "TeeBootstrapV2 full calldata",
        MAX_TEE_BOOTSTRAP_BYTES,
        full_len,
    )
}

fn enforce_limit(field: &'static str, limit: usize, actual: usize) -> Result<(), CodecError> {
    if actual > limit {
        return Err(CodecError::LimitExceeded {
            field,
            limit,
            actual,
        });
    }
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_len_u16(out: &mut Vec<u8>, value: usize) -> Result<(), CodecError> {
    put_u16(
        out,
        u16::try_from(value).map_err(|_| CodecError::ArithmeticOverflow)?,
    );
    Ok(())
}

fn put_bytes_u32(out: &mut Vec<u8>, value: &[u8]) -> Result<(), CodecError> {
    out.extend_from_slice(
        &u32::try_from(value.len())
            .map_err(|_| CodecError::ArithmeticOverflow)?
            .to_be_bytes(),
    );
    out.extend_from_slice(value);
    Ok(())
}

struct Decoder<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or(CodecError::ArithmeticOverflow)?;
        let value = self
            .input
            .get(self.cursor..end)
            .ok_or(CodecError::UnexpectedEof)?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CodecError::UnexpectedEof)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn bounded_count_u16(
        &mut self,
        field: &'static str,
        limit: usize,
    ) -> Result<usize, CodecError> {
        let actual = usize::from(self.u16()?);
        enforce_limit(field, limit, actual)?;
        Ok(actual)
    }

    fn bounded_bytes(&mut self, field: &'static str, limit: usize) -> Result<&'a [u8], CodecError> {
        let actual = usize::try_from(self.u32()?).map_err(|_| CodecError::ArithmeticOverflow)?;
        enforce_limit(field, limit, actual)?;
        self.take(actual)
    }

    fn finish(self) -> Result<(), CodecError> {
        if self.cursor != self.input.len() {
            return Err(CodecError::TrailingBytes(self.input.len() - self.cursor));
        }
        Ok(())
    }
}
