use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryMutatorV1 {
    RegisterEnclave,
    RenewEnclave,
    TransitionEnclaveMeasurement,
    ReplaceEnclaveBinding,
    PrepareEnclaveUpgrade,
}

/// Canonical dimensions used to charge one block-1 `OST3` system call.
///
/// `full_calldata_len` includes the four-byte selector and one-byte version.
/// Collateral deduplication affects only that encoded length. The precharge counts
/// every entry in `logical_evidence_lengths` as a complete QVL verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeeBootstrapGasInputV1<'a> {
    pub full_calldata_len: usize,
    pub logical_evidence_lengths: &'a [usize],
    pub active_rule_count: usize,
    pub collateral_component_count: usize,
    pub committee_signature_count: usize,
}

/// Focused canonical system-gas schedule for the V1 `OST3` bootstrap path.
///
/// The fixed/count terms include the bounded state mutation work. Storage is
/// not charged a second time on top of this protocol precharge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemGasScheduleV1 {
    tee_bootstrap_fixed: u64,
    tee_bootstrap_input_byte: u64,
    tee_bootstrap_participant: u64,
    tee_bootstrap_collateral_component: u64,
    tee_bootstrap_committee_signature: u64,
}

impl SystemGasScheduleV1 {
    pub const fn normative() -> Self {
        Self {
            tee_bootstrap_fixed: 300_000,
            tee_bootstrap_input_byte: 1,
            tee_bootstrap_participant: 100_000,
            tee_bootstrap_collateral_component: 15_000,
            tee_bootstrap_committee_signature: 10_000,
        }
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(41);
        out.push(PROTOCOL_VERSION_V1);
        for value in self.values() {
            put_u64(&mut out, value);
        }
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        decoder.version("SystemGasScheduleV1")?;
        let schedule = Self {
            tee_bootstrap_fixed: decoder.u64()?,
            tee_bootstrap_input_byte: decoder.u64()?,
            tee_bootstrap_participant: decoder.u64()?,
            tee_bootstrap_collateral_component: decoder.u64()?,
            tee_bootstrap_committee_signature: decoder.u64()?,
        };
        decoder.finish()?;
        schedule.validate()?;
        Ok(schedule)
    }

    pub fn schedule_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            SYSTEM_GAS_SCHEDULE_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub fn tee_bootstrap_precharge(
        &self,
        tee_registry: &TeeRegistryGasScheduleV1,
        input: TeeBootstrapGasInputV1<'_>,
    ) -> Result<u64, CodecError> {
        enforce_limit(
            "TeeBootstrapV2 full calldata",
            MAX_TEE_BOOTSTRAP_BYTES,
            input.full_calldata_len,
        )?;
        let participant_count = checked_usize(input.logical_evidence_lengths.len())?;
        let qvl =
            input
                .logical_evidence_lengths
                .iter()
                .try_fold(0_u64, |total, evidence_len| {
                    total
                        .checked_add(tee_registry.qvl_dcap(*evidence_len, input.active_rule_count)?)
                        .ok_or(CodecError::ArithmeticOverflow)
                })?;
        checked_sum(&[
            self.tee_bootstrap_fixed,
            checked_mul(
                self.tee_bootstrap_input_byte,
                checked_usize(input.full_calldata_len)?,
            )?,
            checked_mul(self.tee_bootstrap_participant, participant_count)?,
            qvl,
            checked_mul(
                self.tee_bootstrap_collateral_component,
                checked_usize(input.collateral_component_count)?,
            )?,
            checked_mul(
                self.tee_bootstrap_committee_signature,
                checked_usize(input.committee_signature_count)?,
            )?,
        ])
    }

    pub(super) fn values(&self) -> [u64; 5] {
        [
            self.tee_bootstrap_fixed,
            self.tee_bootstrap_input_byte,
            self.tee_bootstrap_participant,
            self.tee_bootstrap_collateral_component,
            self.tee_bootstrap_committee_signature,
        ]
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if *self != Self::normative() {
            return Err(CodecError::NonCanonical(
                "non-normative system gas schedule",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeeRegistryGasScheduleV1 {
    pub input_byte: u64,
    pub qvl_dcap_fixed: u64,
    pub qvl_dev_fixed: u64,
    pub qvl_component_byte: u64,
    pub qvl_certificate: u64,
    pub qvl_crl: u64,
    pub qvl_signed_json: u64,
    pub qvl_measurement_rule: u64,
    pub secp256k1_verify: u64,
    pub ed25519_verify: u64,
    pub register_fixed: u64,
    pub renew_fixed: u64,
    pub measurement_transition_fixed: u64,
    pub profile_replacement_fixed: u64,
    pub delivery_fixed: u64,
    pub view_fixed: u64,
    pub view_output_byte: u64,
}

impl TeeRegistryGasScheduleV1 {
    pub const fn normative() -> Self {
        Self {
            input_byte: 4,
            qvl_dcap_fixed: 1_500_000,
            qvl_dev_fixed: 200_000,
            qvl_component_byte: 6,
            qvl_certificate: 120_000,
            qvl_crl: 180_000,
            qvl_signed_json: 160_000,
            qvl_measurement_rule: 10_000,
            secp256k1_verify: 40_000,
            ed25519_verify: 25_000,
            register_fixed: 600_000,
            renew_fixed: 500_000,
            measurement_transition_fixed: 700_000,
            profile_replacement_fixed: 900_000,
            delivery_fixed: 400_000,
            view_fixed: 100_000,
            view_output_byte: 4,
        }
    }

    /// Hash-committed portion of `register_fixed` reserved for production
    /// warm-SLOAD and SSTORE-reset charges. No independent consensus constant
    /// exists. Changing the allowance requires changing the canonical schedule.
    pub const fn register_storage_gas_allowance(&self) -> u64 {
        self.register_fixed / 2
    }

    /// Hash-committed storage allowance for each evidence-bearing registry
    /// mutation. It remains part of the corresponding fixed charge.
    pub const fn mutator_storage_gas_allowance(&self, kind: RegistryMutatorV1) -> u64 {
        match kind {
            RegistryMutatorV1::RegisterEnclave => self.register_fixed / 2,
            RegistryMutatorV1::RenewEnclave => self.renew_fixed / 2,
            RegistryMutatorV1::TransitionEnclaveMeasurement
            | RegistryMutatorV1::PrepareEnclaveUpgrade => self.measurement_transition_fixed / 2,
            RegistryMutatorV1::ReplaceEnclaveBinding => self.profile_replacement_fixed / 2,
        }
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        if *self != Self::normative() {
            return Err(CodecError::NonCanonical(
                "non-normative TeeRegistry gas schedule",
            ));
        }
        let mut out = Vec::with_capacity(137);
        out.push(PROTOCOL_VERSION_V1);
        for value in self.values() {
            put_u64(&mut out, value);
        }
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        decoder.version("TeeRegistryGasScheduleV1")?;
        let schedule = Self {
            input_byte: decoder.u64()?,
            qvl_dcap_fixed: decoder.u64()?,
            qvl_dev_fixed: decoder.u64()?,
            qvl_component_byte: decoder.u64()?,
            qvl_certificate: decoder.u64()?,
            qvl_crl: decoder.u64()?,
            qvl_signed_json: decoder.u64()?,
            qvl_measurement_rule: decoder.u64()?,
            secp256k1_verify: decoder.u64()?,
            ed25519_verify: decoder.u64()?,
            register_fixed: decoder.u64()?,
            renew_fixed: decoder.u64()?,
            measurement_transition_fixed: decoder.u64()?,
            profile_replacement_fixed: decoder.u64()?,
            delivery_fixed: decoder.u64()?,
            view_fixed: decoder.u64()?,
            view_output_byte: decoder.u64()?,
        };
        decoder.finish()?;
        if schedule != Self::normative() {
            return Err(CodecError::NonCanonical(
                "non-normative TeeRegistry gas schedule",
            ));
        }
        Ok(schedule)
    }

    pub fn schedule_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            TEE_REGISTRY_GAS_SCHEDULE_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub fn qvl_dcap(
        &self,
        evidence_len: usize,
        active_rule_count: usize,
    ) -> Result<u64, CodecError> {
        validate_qvl_dimensions(evidence_len, active_rule_count)?;
        checked_sum(&[
            self.qvl_dcap_fixed,
            checked_mul(self.qvl_component_byte, checked_usize(evidence_len)?)?,
            checked_mul(self.qvl_certificate, 9)?,
            checked_mul(self.qvl_crl, 2)?,
            checked_mul(self.qvl_signed_json, 2)?,
            checked_mul(self.qvl_measurement_rule, checked_usize(active_rule_count)?)?,
        ])
    }

    pub fn qvl_dev(
        &self,
        evidence_len: usize,
        active_rule_count: usize,
    ) -> Result<u64, CodecError> {
        validate_qvl_dimensions(evidence_len, active_rule_count)?;
        checked_sum(&[
            self.qvl_dev_fixed,
            checked_mul(self.qvl_component_byte, checked_usize(evidence_len)?)?,
            self.ed25519_verify,
            checked_mul(self.qvl_measurement_rule, checked_usize(active_rule_count)?)?,
        ])
    }

    pub fn maximum_transaction_gas(
        &self,
        kind: RegistryMutatorV1,
        input_len: usize,
        evidence_len: usize,
        active_rule_count: usize,
        mode: AttestationMode,
    ) -> Result<u64, CodecError> {
        if input_len < evidence_len {
            return Err(CodecError::NonCanonical(
                "registry input shorter than evidence",
            ));
        }
        let framing_len = input_len - evidence_len;
        enforce_limit(
            "evidence call framing",
            MAX_EVIDENCE_CALL_FRAMING_BYTES,
            framing_len,
        )?;
        let qvl = match mode {
            AttestationMode::DcapRequired => self.qvl_dcap(evidence_len, active_rule_count)?,
            AttestationMode::GramineDirectDev => self.qvl_dev(evidence_len, active_rule_count)?,
        };
        let input_charge = checked_mul(self.input_byte, checked_usize(input_len)?)?;
        let protocol_precharge = match kind {
            RegistryMutatorV1::RegisterEnclave => checked_sum(&[
                self.register_fixed,
                input_charge,
                qvl,
                // Initial registration authenticates the NodeHost intent and
                // both sides of the address-to-NodeHost association.
                checked_mul(self.secp256k1_verify, 3)?,
                self.ed25519_verify,
            ])?,
            RegistryMutatorV1::RenewEnclave => checked_sum(&[
                self.renew_fixed,
                input_charge,
                qvl,
                self.secp256k1_verify,
                self.ed25519_verify,
            ])?,
            RegistryMutatorV1::TransitionEnclaveMeasurement
            | RegistryMutatorV1::PrepareEnclaveUpgrade => checked_sum(&[
                self.measurement_transition_fixed,
                input_charge,
                qvl,
                self.secp256k1_verify,
                checked_mul(self.ed25519_verify, 2)?,
            ])?,
            RegistryMutatorV1::ReplaceEnclaveBinding => checked_sum(&[
                self.profile_replacement_fixed,
                input_charge,
                qvl,
                checked_mul(self.secp256k1_verify, 2)?,
                checked_mul(self.ed25519_verify, 2)?,
            ])?,
        };
        self.maximum_calldata_intrinsic_gas(input_len)?
            .checked_add(protocol_precharge)
            .ok_or(CodecError::ArithmeticOverflow)
    }

    pub fn maximum_calldata_intrinsic_gas(&self, input_len: usize) -> Result<u64, CodecError> {
        checked_mul(16, checked_usize(input_len)?)?
            .checked_add(21_000)
            .ok_or(CodecError::ArithmeticOverflow)
    }

    pub(super) fn values(&self) -> [u64; 17] {
        [
            self.input_byte,
            self.qvl_dcap_fixed,
            self.qvl_dev_fixed,
            self.qvl_component_byte,
            self.qvl_certificate,
            self.qvl_crl,
            self.qvl_signed_json,
            self.qvl_measurement_rule,
            self.secp256k1_verify,
            self.ed25519_verify,
            self.register_fixed,
            self.renew_fixed,
            self.measurement_transition_fixed,
            self.profile_replacement_fixed,
            self.delivery_fixed,
            self.view_fixed,
            self.view_output_byte,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceScheduleV1 {
    pub system_gas_schedule_hash: B256,
    pub tee_registry_gas_schedule_hash: B256,
    pub bootstrap_block_gas_limit: u64,
    pub steady_block_gas_limit: u64,
}

impl ResourceScheduleV1 {
    pub fn normative() -> Result<Self, CodecError> {
        Ok(Self {
            system_gas_schedule_hash: SystemGasScheduleV1::normative().schedule_hash()?,
            tee_registry_gas_schedule_hash: TeeRegistryGasScheduleV1::normative()
                .schedule_hash()?,
            bootstrap_block_gas_limit: BOOTSTRAP_BLOCK_GAS_LIMIT,
            steady_block_gas_limit: STEADY_BLOCK_GAS_LIMIT,
        })
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(81);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(self.system_gas_schedule_hash.as_slice());
        out.extend_from_slice(self.tee_registry_gas_schedule_hash.as_slice());
        put_u64(&mut out, self.bootstrap_block_gas_limit);
        put_u64(&mut out, self.steady_block_gas_limit);
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        decoder.version("ResourceScheduleV1")?;
        let value = Self {
            system_gas_schedule_hash: B256::from(decoder.array::<32>()?),
            tee_registry_gas_schedule_hash: B256::from(decoder.array::<32>()?),
            bootstrap_block_gas_limit: decoder.u64()?,
            steady_block_gas_limit: decoder.u64()?,
        };
        decoder.finish()?;
        value.validate()?;
        Ok(value)
    }

    pub fn schedule_hash(&self) -> Result<B256, CodecError> {
        self.validate()?;
        Ok(domain_hash(
            RESOURCE_SCHEDULE_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        let normative = Self::normative()?;
        if self.system_gas_schedule_hash != normative.system_gas_schedule_hash
            || self.tee_registry_gas_schedule_hash != normative.tee_registry_gas_schedule_hash
        {
            return Err(CodecError::NonCanonical(
                "non-normative resource schedule hashes",
            ));
        }
        if self.bootstrap_block_gas_limit != BOOTSTRAP_BLOCK_GAS_LIMIT
            || self.steady_block_gas_limit != STEADY_BLOCK_GAS_LIMIT
        {
            return Err(CodecError::NonCanonical("non-normative block gas limits"));
        }
        Ok(())
    }
}
