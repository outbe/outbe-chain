use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum QvlTcbStatusV1 {
    UpToDate = 0x01,
}

impl QvlTcbStatusV1 {
    pub(super) fn decode(value: u8) -> Result<Self, CodecError> {
        match value {
            0x01 => Ok(Self::UpToDate),
            value => Err(CodecError::UnknownDiscriminant {
                field: "accepted QVL TCB status",
                value,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PlatformTcbStatusSetV1 {
    /// Admit only Intel `UpToDate`.
    UpToDateOnly = 0x01,
    /// Admit `UpToDate`, `SWHardeningNeeded`, or
    /// `ConfigurationAndSWHardeningNeeded` while preserving the exact verdict.
    UpToDateOrHardeningNeeded = 0x02,
}

impl PlatformTcbStatusSetV1 {
    pub(super) fn decode(value: u8) -> Result<Self, CodecError> {
        match value {
            0x01 => Ok(Self::UpToDateOnly),
            0x02 => Ok(Self::UpToDateOrHardeningNeeded),
            value => Err(CodecError::UnknownDiscriminant {
                field: "accepted Platform TCB status set",
                value,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeMeasurementRuleV1 {
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_prod_id: u16,
    pub minimum_isv_svn: u16,
    pub admit_from_height: u64,
    pub admit_until_height_exclusive: u64,
}

impl TeeMeasurementRuleV1 {
    pub(super) fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        self.validate()?;
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(self.mrenclave.as_slice());
        out.extend_from_slice(self.mrsigner.as_slice());
        put_u16(out, self.isv_prod_id);
        put_u16(out, self.minimum_isv_svn);
        put_u64(out, self.admit_from_height);
        put_u64(out, self.admit_until_height_exclusive);
        Ok(())
    }

    pub(super) fn decode_from(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        decoder.version("TeeMeasurementRuleV1")?;
        let value = Self {
            mrenclave: B256::from(decoder.array::<32>()?),
            mrsigner: B256::from(decoder.array::<32>()?),
            isv_prod_id: decoder.u16()?,
            minimum_isv_svn: decoder.u16()?,
            admit_from_height: decoder.u64()?,
            admit_until_height_exclusive: decoder.u64()?,
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn canonical_bytes(&self) -> Result<Vec<u8>, CodecError> {
        let mut out = Vec::with_capacity(85);
        self.encode_into(&mut out)?;
        Ok(out)
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if self.mrenclave.is_zero() || self.mrsigner.is_zero() {
            return Err(CodecError::NonCanonical(
                "measurement rule contains a zero measurement",
            ));
        }
        if self.admit_from_height >= self.admit_until_height_exclusive {
            return Err(CodecError::NonCanonical(
                "measurement admission interval is empty",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeePolicyV1 {
    pub policy_version: u64,
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub activation_height: u64,
    pub predecessor_policy_hash: B256,
    pub attestation_mode: AttestationMode,
    pub intel_root_der_hash: B256,
    pub quote_version: u16,
    pub tee_type: u32,
    pub attestation_key_type: u16,
    pub qe_vendor_id: [u8; 16],
    pub certification_data_type: u16,
    pub tcb_info_schema_version: u8,
    pub qe_identity_schema_version: u8,
    pub minimum_tcb_evaluation_data_number: u32,
    pub accepted_platform_tcb_statuses: PlatformTcbStatusSetV1,
    pub accepted_qe_tcb_status: QvlTcbStatusV1,
    pub minimum_lease: u64,
    pub maximum_lease: u64,
    pub collateral_margin: u64,
    pub resource_schedule_hash: B256,
    pub measurement_rules: Vec<TeeMeasurementRuleV1>,
}

impl TeePolicyV1 {
    pub const fn network_binding(&self) -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            attestation_mode: self.attestation_mode,
        }
    }

    /// Counts rules that admit one authenticated enclave measurement at a
    /// height. Consensus admission requires the result to equal exactly one;
    /// zero and overlapping matches are both fail-closed.
    pub fn measurement_rule_match_count(
        &self,
        mrenclave: B256,
        mrsigner: B256,
        isv_prod_id: u16,
        isv_svn: u16,
        height: u64,
    ) -> usize {
        self.measurement_rules
            .iter()
            .filter(|rule| {
                (rule.mrenclave, rule.mrsigner, rule.isv_prod_id)
                    == (mrenclave, mrsigner, isv_prod_id)
                    && isv_svn >= rule.minimum_isv_svn
                    && height >= rule.admit_from_height
                    && height < rule.admit_until_height_exclusive
            })
            .count()
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::new();
        out.push(PROTOCOL_VERSION_V1);
        put_u64(&mut out, self.policy_version);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        put_u64(&mut out, self.activation_height);
        out.extend_from_slice(self.predecessor_policy_hash.as_slice());
        out.push(self.attestation_mode as u8);
        out.extend_from_slice(self.intel_root_der_hash.as_slice());
        put_u16(&mut out, self.quote_version);
        put_u32(&mut out, self.tee_type);
        put_u16(&mut out, self.attestation_key_type);
        out.extend_from_slice(&self.qe_vendor_id);
        put_u16(&mut out, self.certification_data_type);
        out.push(self.tcb_info_schema_version);
        out.push(self.qe_identity_schema_version);
        put_u32(&mut out, self.minimum_tcb_evaluation_data_number);
        out.push(self.accepted_platform_tcb_statuses as u8);
        out.push(self.accepted_qe_tcb_status as u8);
        put_u64(&mut out, self.minimum_lease);
        put_u64(&mut out, self.maximum_lease);
        put_u64(&mut out, self.collateral_margin);
        out.extend_from_slice(self.resource_schedule_hash.as_slice());
        put_u16(
            &mut out,
            u16::try_from(self.measurement_rules.len())
                .map_err(|_| CodecError::ArithmeticOverflow)?,
        );
        for rule in &self.measurement_rules {
            rule.encode_into(&mut out)?;
        }
        enforce_limit("TEE policy", MAX_TEE_POLICY_BYTES, out.len())?;
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        enforce_limit("TEE policy", MAX_TEE_POLICY_BYTES, input.len())?;
        let mut decoder = Decoder::new(input);
        decoder.version("TeePolicyV1")?;
        let policy_version = decoder.u64()?;
        let chain_id = decoder.array()?;
        let genesis_hash = B256::from(decoder.array::<32>()?);
        let activation_height = decoder.u64()?;
        let predecessor_policy_hash = B256::from(decoder.array::<32>()?);
        let attestation_mode = AttestationMode::decode(decoder.u8()?)?;
        let intel_root_der_hash = B256::from(decoder.array::<32>()?);
        let quote_version = decoder.u16()?;
        let tee_type = decoder.u32()?;
        let attestation_key_type = decoder.u16()?;
        let qe_vendor_id = decoder.array()?;
        let certification_data_type = decoder.u16()?;
        let tcb_info_schema_version = decoder.u8()?;
        let qe_identity_schema_version = decoder.u8()?;
        let minimum_tcb_evaluation_data_number = decoder.u32()?;
        let accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::decode(decoder.u8()?)?;
        let accepted_qe_tcb_status = QvlTcbStatusV1::decode(decoder.u8()?)?;
        let minimum_lease = decoder.u64()?;
        let maximum_lease = decoder.u64()?;
        let collateral_margin = decoder.u64()?;
        let resource_schedule_hash = B256::from(decoder.array::<32>()?);
        let rule_count = usize::from(decoder.u16()?);
        enforce_limit(
            "active measurement rules",
            MAX_ACTIVE_MEASUREMENT_RULES,
            rule_count,
        )?;
        if rule_count == 0 {
            return Err(CodecError::NonCanonical(
                "TEE policy has no measurement rules",
            ));
        }
        let mut measurement_rules = Vec::with_capacity(rule_count);
        for _ in 0..rule_count {
            measurement_rules.push(TeeMeasurementRuleV1::decode_from(&mut decoder)?);
        }
        decoder.finish()?;
        let value = Self {
            policy_version,
            chain_id,
            genesis_hash,
            activation_height,
            predecessor_policy_hash,
            attestation_mode,
            intel_root_der_hash,
            quote_version,
            tee_type,
            attestation_key_type,
            qe_vendor_id,
            certification_data_type,
            tcb_info_schema_version,
            qe_identity_schema_version,
            minimum_tcb_evaluation_data_number,
            accepted_platform_tcb_statuses,
            accepted_qe_tcb_status,
            minimum_lease,
            maximum_lease,
            collateral_margin,
            resource_schedule_hash,
            measurement_rules,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn policy_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(POLICY_DOMAIN_V1, &self.encode_canonical()?))
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if self.policy_version == 0 || self.activation_height == 0 {
            return Err(CodecError::NonCanonical(
                "policy version and activation height must be nonzero",
            ));
        }
        if self.genesis_hash.is_zero()
            || self.intel_root_der_hash.is_zero()
            || self.resource_schedule_hash.is_zero()
        {
            return Err(CodecError::NonCanonical(
                "TEE policy contains a zero commitment",
            ));
        }
        self.validate_dcap_profile()?;
        if self.minimum_tcb_evaluation_data_number == 0 {
            return Err(CodecError::NonCanonical(
                "minimum TCB evaluation data number must be nonzero",
            ));
        }
        self.validate_lease_policy()?;
        self.validate_measurement_rules()
    }

    fn validate_dcap_profile(&self) -> Result<(), CodecError> {
        const INTEL_QE_VENDOR_ID: [u8; 16] = [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ];
        if (
            self.quote_version,
            self.tee_type,
            self.attestation_key_type,
            self.qe_vendor_id,
            self.certification_data_type,
        ) != (3, 0, 2, INTEL_QE_VENDOR_ID, 5)
            || self.tcb_info_schema_version != 3
            || self.qe_identity_schema_version != 2
        {
            return Err(CodecError::NonCanonical(
                "unsupported SGX DCAP policy profile",
            ));
        }
        Ok(())
    }

    fn validate_lease_policy(&self) -> Result<(), CodecError> {
        let supported_range = self.minimum_lease >= 3_600
            && self.maximum_lease <= 2_592_000
            && self.minimum_lease <= self.maximum_lease;
        if !supported_range
            || !self.maximum_lease.is_multiple_of(2)
            || self.collateral_margin != 3_600
        {
            return Err(CodecError::NonCanonical("invalid TEE lease policy"));
        }
        Ok(())
    }

    fn validate_measurement_rules(&self) -> Result<(), CodecError> {
        if self.measurement_rules.is_empty() {
            return Err(CodecError::NonCanonical(
                "TEE policy has no measurement rules",
            ));
        }
        enforce_limit(
            "active measurement rules",
            MAX_ACTIVE_MEASUREMENT_RULES,
            self.measurement_rules.len(),
        )?;
        let mut previous: Option<Vec<u8>> = None;
        for rule in &self.measurement_rules {
            let bytes = rule.canonical_bytes()?;
            if previous.as_ref().is_some_and(|value| value >= &bytes) {
                return Err(CodecError::NonCanonical(
                    "measurement rules must be strictly sorted and unique",
                ));
            }
            previous = Some(bytes);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeePolicyScheduleEntryV1 {
    pub activation_height: u64,
    pub policy: TeePolicyV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeePolicyScheduleV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub entries: Vec<TeePolicyScheduleEntryV1>,
}

impl TeePolicyScheduleV1 {
    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::new();
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        put_u16(
            &mut out,
            u16::try_from(self.entries.len()).map_err(|_| CodecError::ArithmeticOverflow)?,
        );
        for entry in &self.entries {
            let policy = entry.policy.encode_canonical()?;
            put_u64(&mut out, entry.activation_height);
            put_len_u32(&mut out, policy.len())?;
            out.extend_from_slice(&policy);
            out.extend_from_slice(entry.policy.policy_hash()?.as_slice());
        }
        enforce_limit(
            "TEE policy schedule",
            MAX_TEE_POLICY_SCHEDULE_BYTES,
            out.len(),
        )?;
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        enforce_limit(
            "TEE policy schedule",
            MAX_TEE_POLICY_SCHEDULE_BYTES,
            input.len(),
        )?;
        let mut decoder = Decoder::new(input);
        decoder.version("TeePolicyScheduleV1")?;
        let chain_id = decoder.array()?;
        let genesis_hash = B256::from(decoder.array::<32>()?);
        let entry_count = usize::from(decoder.u16()?);
        enforce_limit(
            "TEE policy schedule entries",
            MAX_TEE_POLICY_SCHEDULE_ENTRIES,
            entry_count,
        )?;
        if entry_count == 0 {
            return Err(CodecError::NonCanonical("TEE policy schedule is empty"));
        }
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let activation_height = decoder.u64()?;
            let policy_len = decoder.declared_len("TEE policy", MAX_TEE_POLICY_BYTES)?;
            let policy = TeePolicyV1::decode_canonical(decoder.take(policy_len)?)?;
            let cached_hash = B256::from(decoder.array::<32>()?);
            if cached_hash != policy.policy_hash()? {
                return Err(CodecError::NonCanonical("cached policy hash mismatch"));
            }
            entries.push(TeePolicyScheduleEntryV1 {
                activation_height,
                policy,
            });
        }
        decoder.finish()?;
        let value = Self {
            chain_id,
            genesis_hash,
            entries,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn schedule_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            POLICY_SCHEDULE_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub fn active_policy(&self, height: u64) -> Result<&TeePolicyV1, CodecError> {
        self.validate()?;
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.activation_height <= height)
            .map(|entry| &entry.policy)
            .ok_or(CodecError::NonCanonical(
                "no active TEE policy at requested height",
            ))
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if self.genesis_hash.is_zero() {
            return Err(CodecError::NonCanonical(
                "TEE policy schedule has zero genesis hash",
            ));
        }
        if self.entries.is_empty() {
            return Err(CodecError::NonCanonical("TEE policy schedule is empty"));
        }
        enforce_limit(
            "TEE policy schedule entries",
            MAX_TEE_POLICY_SCHEDULE_ENTRIES,
            self.entries.len(),
        )?;
        let first = &self.entries[0];
        if first.activation_height != 1
            || first.policy.activation_height != 1
            || first.policy.policy_version != 1
            || !first.policy.predecessor_policy_hash.is_zero()
        {
            return Err(CodecError::NonCanonical(
                "first TEE policy must be version one at block one",
            ));
        }
        let mut previous_hash = first.policy.policy_hash()?;
        let mut previous_version = first.policy.policy_version;
        let mut previous_height = first.activation_height;
        self.validate_entry_identity(first)?;
        for entry in self.entries.iter().skip(1) {
            self.validate_entry_identity(entry)?;
            Self::validate_successor(entry, previous_height, previous_version, previous_hash)?;
            previous_hash = entry.policy.policy_hash()?;
            previous_version = entry.policy.policy_version;
            previous_height = entry.activation_height;
        }
        Ok(())
    }

    fn validate_entry_identity(&self, entry: &TeePolicyScheduleEntryV1) -> Result<(), CodecError> {
        if entry.policy.chain_id != self.chain_id || entry.policy.genesis_hash != self.genesis_hash
        {
            return Err(CodecError::ChainIdentityMismatch);
        }
        Ok(())
    }

    fn validate_successor(
        entry: &TeePolicyScheduleEntryV1,
        previous_height: u64,
        previous_version: u64,
        previous_hash: B256,
    ) -> Result<(), CodecError> {
        if entry.activation_height <= previous_height
            || entry.policy.activation_height != entry.activation_height
        {
            return Err(CodecError::NonCanonical(
                "policy activations must be strictly increasing",
            ));
        }
        let expected_version = previous_version
            .checked_add(1)
            .ok_or(CodecError::ArithmeticOverflow)?;
        if entry.policy.policy_version != expected_version {
            return Err(CodecError::NonCanonical(
                "policy versions must increase by one",
            ));
        }
        if entry.policy.predecessor_policy_hash != previous_hash {
            return Err(CodecError::NonCanonical("policy predecessor hash mismatch"));
        }
        Ok(())
    }
}

pub const MAX_TEE_POLICY_SCHEDULE_BYTES: usize =
    1 + 32 + 32 + 2 + MAX_TEE_POLICY_SCHEDULE_ENTRIES * (8 + 4 + MAX_TEE_POLICY_BYTES + 32);
