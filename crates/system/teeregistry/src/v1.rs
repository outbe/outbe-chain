//! Inactive-until-I9 TeeRegistry V1 node-enclave registration state machine.
//!
//! The production entry point exists only with the tee-attestation-v1 feature;
//! default builds retain the legacy route until A0 activation. Accepted
//! hardware-free tests enter through the private typed post-verifier capability.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::sol;
use outbe_primitives::{
    error::{PrecompileError, Result},
    tee_attestation_v1::{EnclaveProfile, NodeIdV1, TeePolicyV1, MAX_TEE_POLICY_BYTES},
};
use outbe_validatorset::contract::ValidatorSet;

#[cfg(feature = "tee-attestation-v1")]
use crate::runtime::compute_keys_hash;
use crate::schema::TeeRegistry;
#[cfg(feature = "tee-attestation-v1")]
use alloy_primitives::keccak256;

#[cfg(feature = "tee-attestation-v1")]
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, PlatformTcbStatusSetV1,
    RegistrationIntentV1,
};
#[cfg(feature = "tee-attestation-v1")]
use outbe_tee::dcap_protocol::{
    dcap_evidence_hash_v1, DcapPlatformTcbStatusV1, DcapVerdictV1, DcapVerificationOutcomeV1,
};

sol! {
    /// Fixed-size consensus event. Evidence, collateral, advisories and signatures
    /// are deliberately excluded so a registration cannot create an unbounded log.
    #[derive(Debug)]
    event EnclaveRegisteredV1(
        bytes32 indexed nodeIdHash,
        bytes32 indexed enclaveId,
        bytes32 indexed bindingId,
        uint64 validUntil,
        uint64 bindingVersion
    );

    #[derive(Debug)]
    event EnclaveRenewedV1(
        bytes32 indexed nodeIdHash,
        bytes32 indexed enclaveId,
        bytes32 indexed bindingId,
        uint64 validUntil,
        uint64 registrationVersion,
        uint64 renewalNonce
    );

    #[derive(Debug)]
    event EnclaveBindingReplacedV1(
        bytes32 indexed nodeIdHash,
        bytes32 indexed enclaveId,
        bytes32 indexed bindingId,
        uint64 validUntil,
        uint64 bindingVersion
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V1RegistrationOutcome {
    Created,
    Idempotent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEnclaveBindingV1 {
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub binding_id: B256,
    pub intent_hash: B256,
    pub evidence_hash: B256,
    pub policy_hash: B256,
    pub binding_version: u64,
    pub registration_version: u64,
    pub renewal_nonce: u64,
    pub transition_nonce: u64,
    pub lease_started_at: u64,
    pub valid_until: u64,
    pub collateral_valid_until: u64,
    pub recipient_x25519: B256,
    pub attestation_ed25519: B256,
    pub noise_responder_x25519: B256,
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub platform_tcb_status: u8,
    pub verdict_hash: B256,
    pub node_host_authorization_hash: B256,
}

impl TeeRegistry<'_> {
    /// Installs the immutable first V1 policy. I7 adds the governance-controlled
    /// successor schedule; this I3 method intentionally cannot rotate a policy.
    pub fn install_initial_policy_v1(&mut self, policy: &TeePolicyV1) -> Result<()> {
        let canonical = policy
            .encode_canonical()
            .map_err(|error| revert_codec("invalid initial V1 policy", error))?;
        let expected_chain_id = chain_id_word(self.storage.chain_id()?);
        if policy.chain_id != expected_chain_id
            || policy.genesis_hash != self.storage.genesis_hash()?
        {
            return Err(PrecompileError::Revert(
                "initial V1 policy chain identity mismatch".into(),
            ));
        }
        if policy.policy_version != 1
            || policy.activation_height != 1
            || !policy.predecessor_policy_hash.is_zero()
        {
            return Err(PrecompileError::Revert(
                "initial V1 policy must be version one at block one".into(),
            ));
        }

        let policy_hash = policy
            .policy_hash()
            .map_err(|error| revert_codec("invalid initial V1 policy", error))?;
        let installed_len = self.active_v1_policy_len.read()?;
        if installed_len != 0 {
            let installed = self.read_policy_bytes_v1()?;
            if installed == canonical && self.active_v1_policy_hash.read()? == policy_hash {
                return Ok(());
            }
            return Err(PrecompileError::Revert(
                "initial V1 policy is already installed".into(),
            ));
        }
        let anchored_hash = self.active_v1_policy_hash.read()?;
        if !anchored_hash.is_zero() && anchored_hash != policy_hash {
            return Err(PrecompileError::Revert(
                "initial V1 policy hash conflicts with registry anchor".into(),
            ));
        }

        for (index, chunk) in canonical.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let index = u32::try_from(index)
                .map_err(|_| PrecompileError::Revert("V1 policy has too many chunks".into()))?;
            self.active_v1_policy_chunk
                .write(&index, B256::from(word))?;
        }
        let len = u32::try_from(canonical.len())
            .map_err(|_| PrecompileError::Revert("V1 policy is too large".into()))?;
        self.active_v1_policy_len.write(len)?;
        self.active_v1_policy_hash.write(policy_hash)?;
        Ok(())
    }

    /// Reads and authenticates current policy bytes from consensus storage.
    /// Calldata cannot supply or override policy authority.
    pub fn active_policy_v1(&self) -> Result<TeePolicyV1> {
        let canonical = self.read_policy_bytes_v1()?;
        let policy = TeePolicyV1::decode_canonical(&canonical).map_err(|error| {
            PrecompileError::Fatal(format!("stored V1 policy is non-canonical: {error}"))
        })?;
        if policy.chain_id != chain_id_word(self.storage.chain_id()?)
            || policy.genesis_hash != self.storage.genesis_hash()?
        {
            return Err(PrecompileError::Fatal(
                "stored V1 policy chain identity mismatch".into(),
            ));
        }
        let policy_hash = policy.policy_hash().map_err(|error| {
            PrecompileError::Fatal(format!("stored V1 policy cannot be hashed: {error}"))
        })?;
        if self.active_v1_policy_hash.read()? != policy_hash {
            return Err(PrecompileError::Fatal(
                "stored V1 policy hash does not match registry anchor".into(),
            ));
        }
        if policy.activation_height > self.storage.block_number()? {
            return Err(PrecompileError::Revert(
                "no V1 TEE policy is active at this height".into(),
            ));
        }
        Ok(policy)
    }

    fn read_policy_bytes_v1(&self) -> Result<Vec<u8>> {
        let len = usize::try_from(self.active_v1_policy_len.read()?)
            .map_err(|_| PrecompileError::Fatal("stored V1 policy length overflow".into()))?;
        if len == 0 {
            return Err(PrecompileError::Revert(
                "V1 TEE policy is not installed".into(),
            ));
        }
        if len > MAX_TEE_POLICY_BYTES {
            return Err(PrecompileError::Fatal(
                "stored V1 policy exceeds the protocol cap".into(),
            ));
        }
        let words = len.div_ceil(32);
        let mut canonical = Vec::with_capacity(words * 32);
        for index in 0..words {
            let index = u32::try_from(index)
                .map_err(|_| PrecompileError::Fatal("stored V1 policy index overflow".into()))?;
            canonical.extend_from_slice(self.active_v1_policy_chunk.read(&index)?.as_slice());
        }
        canonical.truncate(len);
        Ok(canonical)
    }

    pub fn validator_enclave_binding_v1(
        &self,
        validator: Address,
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        let node_id_hash = self.validator_v1_node_hash.read(&validator)?;
        if node_id_hash.is_zero() {
            return Ok(None);
        }
        self.node_enclave_binding_v1(node_id_hash)
    }

    pub fn full_node_enclave_binding_v1(
        &self,
        reth_p2p_public: [u8; 33],
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        let node_id_hash = NodeIdV1::FullNode { reth_p2p_public }
            .node_id_hash()
            .map_err(|error| revert_codec("full-node P2P identity is invalid", error))?;
        let binding = self.node_enclave_binding_v1(node_id_hash)?;
        if binding.is_some()
            && self.v1_node_profile.read(&node_id_hash)? != EnclaveProfile::FullNode as u64
        {
            return Err(PrecompileError::Fatal(
                "stored full-node binding has the wrong enclave profile".into(),
            ));
        }
        Ok(binding)
    }

    /// Reads one V1 binding by its complete canonical node identity and rejects
    /// a profile or identity-map mismatch. This is the shared read seam for
    /// finalized-state session admission; callers do not reconstruct Registry
    /// slots or trust an address-only validator lookup.
    pub fn node_enclave_binding_for_identity_v1(
        &self,
        node_id: &NodeIdV1,
        profile: EnclaveProfile,
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        let expected_hash = node_id
            .node_id_hash()
            .map_err(|error| revert_codec("node identity is invalid", error))?;
        let binding = match (profile, node_id) {
            (EnclaveProfile::Validator, NodeIdV1::Validator { address, .. }) => {
                self.validator_enclave_binding_v1(Address::from(*address))?
            }
            (EnclaveProfile::FullNode, NodeIdV1::FullNode { reth_p2p_public }) => {
                self.full_node_enclave_binding_v1(*reth_p2p_public)?
            }
            _ => {
                return Err(PrecompileError::Revert(
                    "node identity does not match the requested enclave profile".into(),
                ))
            }
        };
        let Some(binding) = binding else {
            return Ok(None);
        };
        if binding.node_id_hash != expected_hash
            || self.v1_node_profile.read(&expected_hash)? != profile as u64
        {
            return Err(PrecompileError::Fatal(
                "stored V1 binding identity/profile mismatch".into(),
            ));
        }
        Ok(Some(binding))
    }

    /// Returns the exact append-only storage slots read by
    /// [`Self::node_enclave_binding_for_identity_v1`]. External light clients
    /// use this canonical plan to request one bounded MPT proof; keeping the
    /// plan beside the schema prevents a parallel hand-maintained layout.
    pub fn node_enclave_binding_storage_slots_v1(&self, node_id: &NodeIdV1) -> Result<Vec<B256>> {
        let node_hash = node_id
            .node_id_hash()
            .map_err(|error| revert_codec("node identity is invalid", error))?;
        let mut slots = Vec::with_capacity(24);
        if let NodeIdV1::Validator { address, .. } = node_id {
            slots.push(B256::from(
                self.validator_v1_node_hash
                    .slot(&Address::from(*address))
                    .slot()
                    .to_be_bytes::<32>(),
            ));
        }
        for slot in [
            self.v1_node_enclave_id.slot(&node_hash).slot(),
            self.v1_node_binding_id.slot(&node_hash).slot(),
            self.v1_node_intent_hash.slot(&node_hash).slot(),
            self.v1_node_policy_hash.slot(&node_hash).slot(),
            self.v1_node_profile.slot(&node_hash).slot(),
            self.v1_node_binding_version.slot(&node_hash).slot(),
            self.v1_node_registration_version.slot(&node_hash).slot(),
            self.v1_node_renewal_nonce.slot(&node_hash).slot(),
            self.v1_node_transition_nonce.slot(&node_hash).slot(),
            self.v1_node_valid_until.slot(&node_hash).slot(),
            self.v1_node_collateral_valid_until.slot(&node_hash).slot(),
            self.v1_node_recipient_x25519.slot(&node_hash).slot(),
            self.v1_node_attestation_ed25519.slot(&node_hash).slot(),
            self.v1_node_noise_responder_x25519.slot(&node_hash).slot(),
            self.v1_node_mrenclave.slot(&node_hash).slot(),
            self.v1_node_mrsigner.slot(&node_hash).slot(),
            self.v1_node_isv_prod_id.slot(&node_hash).slot(),
            self.v1_node_isv_svn.slot(&node_hash).slot(),
            self.v1_node_platform_tcb_status.slot(&node_hash).slot(),
            self.v1_node_verdict_hash.slot(&node_hash).slot(),
            self.v1_node_evidence_hash.slot(&node_hash).slot(),
            self.v1_node_lease_started_at.slot(&node_hash).slot(),
            self.v1_node_host_authorization_hash.slot(&node_hash).slot(),
        ] {
            slots.push(B256::from(slot.to_be_bytes::<32>()));
        }
        slots.sort_unstable();
        slots.dedup();
        Ok(slots)
    }

    /// Deterministic attestation readiness only. Full nodes do not consult the
    /// validator set; the exact compressed Reth P2P key is their node identity.
    pub fn is_full_node_enclave_ready_v1(&self, reth_p2p_public: [u8; 33]) -> Result<bool> {
        let Some(binding) = self.full_node_enclave_binding_v1(reth_p2p_public)? else {
            return Ok(false);
        };
        Ok(!binding.binding_id.is_zero()
            && !binding.enclave_id.is_zero()
            && binding.valid_until > consensus_timestamp(&self.storage)?)
    }

    /// Deterministic attestation readiness only. Consensus membership/status is a
    /// separate consumer concern, but missing, expired or key-rotated bindings
    /// are never ready.
    pub fn is_validator_enclave_ready_v1(&self, validator: Address) -> Result<bool> {
        let Some(binding) = self.validator_enclave_binding_v1(validator)? else {
            return Ok(false);
        };
        if binding.binding_id.is_zero()
            || binding.enclave_id.is_zero()
            || self.v1_node_profile.read(&binding.node_id_hash)? != EnclaveProfile::Validator as u64
        {
            return Ok(false);
        }
        let validators = ValidatorSet::new(self.storage.clone());
        let Some(record) = validators.get_validator(validator)? else {
            return Ok(false);
        };
        let expected_node_hash = NodeIdV1::Validator {
            address: validator.into_array(),
            bls_minpk_public: record.consensus_pubkey,
        }
        .node_id_hash()
        .map_err(|error| revert_codec("validator node identity is invalid", error))?;
        if expected_node_hash != binding.node_id_hash {
            return Ok(false);
        }
        Ok(binding.valid_until > consensus_timestamp(&self.storage)?)
    }

    fn node_enclave_binding_v1(&self, node_id_hash: B256) -> Result<Option<NodeEnclaveBindingV1>> {
        if self.v1_node_intent_hash.read(&node_id_hash)?.is_zero() {
            return Ok(None);
        }
        let isv_prod_id = checked_u16(
            self.v1_node_isv_prod_id.read(&node_id_hash)?,
            "stored V1 ISV product id",
        )?;
        let isv_svn = checked_u16(
            self.v1_node_isv_svn.read(&node_id_hash)?,
            "stored V1 ISV SVN",
        )?;
        let platform_tcb_status = checked_u8(
            self.v1_node_platform_tcb_status.read(&node_id_hash)?,
            "stored V1 Platform TCB status",
        )?;
        Ok(Some(NodeEnclaveBindingV1 {
            node_id_hash,
            enclave_id: self.v1_node_enclave_id.read(&node_id_hash)?,
            binding_id: self.v1_node_binding_id.read(&node_id_hash)?,
            intent_hash: self.v1_node_intent_hash.read(&node_id_hash)?,
            evidence_hash: self.v1_node_evidence_hash.read(&node_id_hash)?,
            policy_hash: self.v1_node_policy_hash.read(&node_id_hash)?,
            binding_version: self.v1_node_binding_version.read(&node_id_hash)?,
            registration_version: self.v1_node_registration_version.read(&node_id_hash)?,
            renewal_nonce: self.v1_node_renewal_nonce.read(&node_id_hash)?,
            transition_nonce: self.v1_node_transition_nonce.read(&node_id_hash)?,
            lease_started_at: self.v1_node_lease_started_at.read(&node_id_hash)?,
            valid_until: self.v1_node_valid_until.read(&node_id_hash)?,
            collateral_valid_until: self.v1_node_collateral_valid_until.read(&node_id_hash)?,
            recipient_x25519: self.v1_node_recipient_x25519.read(&node_id_hash)?,
            attestation_ed25519: self.v1_node_attestation_ed25519.read(&node_id_hash)?,
            noise_responder_x25519: self.v1_node_noise_responder_x25519.read(&node_id_hash)?,
            mrenclave: self.v1_node_mrenclave.read(&node_id_hash)?,
            mrsigner: self.v1_node_mrsigner.read(&node_id_hash)?,
            isv_prod_id,
            isv_svn,
            platform_tcb_status,
            verdict_hash: self.v1_node_verdict_hash.read(&node_id_hash)?,
            node_host_authorization_hash: self
                .v1_node_host_authorization_hash
                .read(&node_id_hash)?,
        }))
    }
}

#[cfg(feature = "tee-attestation-v1")]
impl TeeRegistry<'_> {
    /// Production verifier boundary. The caller/relay is intentionally absent.
    pub fn register_enclave_v1(
        &mut self,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.register_enclave_with_active_policy_v1(
            evidence,
            node_signature,
            enclave_signature,
            &policy,
        )
    }

    pub fn renew_enclave_v1(
        &mut self,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.renew_enclave_with_active_policy_v1(
            evidence,
            node_signature,
            enclave_signature,
            &policy,
        )
    }

    pub fn replace_enclave_binding_v1(
        &mut self,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.replace_enclave_binding_with_active_policy_v1(
            evidence,
            node_signature,
            enclave_signature,
            &policy,
        )
    }

    pub(crate) fn register_enclave_with_active_policy_v1(
        &mut self,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::RegisterEnclave,
            evidence,
            node_signature,
            enclave_signature,
            policy,
        )
    }

    pub(crate) fn renew_enclave_with_active_policy_v1(
        &mut self,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::RenewEnclave,
            evidence,
            node_signature,
            enclave_signature,
            policy,
        )
    }

    pub(crate) fn replace_enclave_binding_with_active_policy_v1(
        &mut self,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            evidence,
            node_signature,
            enclave_signature,
            policy,
        )
    }

    fn apply_evidence_mutation_with_active_policy_v1(
        &mut self,
        expected_operation: AttestationOperationV1,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        let policy_bytes = policy.encode_canonical().map_err(|error| {
            PrecompileError::Fatal(format!("active V1 policy cannot be encoded: {error}"))
        })?;
        let outcome = outbe_tee::verify_dcap_evidence_v1(
            evidence,
            &policy_bytes,
            consensus_timestamp(&self.storage)?,
        )
        .map_err(|error| {
            PrecompileError::Fatal(format!(
                "enclave-resident DCAP verifier is unavailable or unauthenticated: {error}"
            ))
        })?;
        let verdict = match outcome {
            DcapVerificationOutcomeV1::Accepted(verdict) => verdict,
            DcapVerificationOutcomeV1::Rejected(code) => {
                return Err(PrecompileError::Revert(format!(
                    "DCAP evidence rejected with code {:#06x}",
                    code.code()
                )))
            }
        };
        let evidence_hash = dcap_evidence_hash_v1(evidence).map_err(|code| {
            PrecompileError::Fatal(format!(
                "accepted DCAP evidence cannot be hashed: {:#06x}",
                code.code()
            ))
        })?;
        let decoded = AttestationEvidenceV1::decode_canonical(evidence).map_err(|error| {
            PrecompileError::Fatal(format!(
                "enclave accepted non-canonical DCAP evidence: {error}"
            ))
        })?;
        let AttestationEvidenceV1::Dcap(evidence) = decoded else {
            return Err(PrecompileError::Fatal(
                "enclave accepted non-DCAP evidence for node registration".into(),
            ));
        };
        self.apply_verified_mutation_v1(
            expected_operation,
            &evidence.intent,
            node_signature,
            enclave_signature,
            policy,
            &verdict,
            evidence_hash,
        )
    }

    fn apply_verified_mutation_v1(
        &mut self,
        expected_operation: AttestationOperationV1,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
        verdict: &DcapVerdictV1,
        evidence_hash: B256,
    ) -> Result<V1RegistrationOutcome> {
        intent
            .encode_canonical()
            .map_err(|error| revert_codec("registration intent is not canonical", error))?;
        intent
            .validate_chain_identity(
                chain_id_word(self.storage.chain_id()?),
                self.storage.genesis_hash()?,
            )
            .map_err(|error| revert_codec("registration intent chain mismatch", error))?;
        if intent.operation != expected_operation
            || intent.attestation_mode != AttestationMode::DcapRequired
        {
            return Err(PrecompileError::Revert(
                "attestation intent operation does not match the DCAP registry mutator".into(),
            ));
        }
        let policy_hash = policy
            .policy_hash()
            .map_err(|error| revert_codec("active V1 policy is invalid", error))?;
        if intent.policy_hash != policy_hash || self.active_v1_policy_hash.read()? != policy_hash {
            return Err(PrecompileError::Revert(
                "registration intent does not bind the active V1 policy".into(),
            ));
        }
        if intent
            .derived_enclave_id()
            .map_err(|error| revert_codec("registration enclave identity is invalid", error))?
            != intent.enclave_id
        {
            return Err(PrecompileError::Revert(
                "registration enclave id is not derived from its persistent keys".into(),
            ));
        }
        if !intent.verify_node_signature(node_signature) {
            return Err(PrecompileError::Revert(
                "node proof of possession is invalid".into(),
            ));
        }
        if !intent.verify_enclave_signature(enclave_signature) {
            return Err(PrecompileError::Revert(
                "enclave proof of possession is invalid".into(),
            ));
        }

        let validator = match &intent.node_id {
            NodeIdV1::Validator {
                address,
                bls_minpk_public,
            } => {
                let validator = Address::from(*address);
                let validators = ValidatorSet::new(self.storage.clone());
                let Some(record) = validators.get_validator(validator)? else {
                    return Err(PrecompileError::Revert(
                        "validator identity is not registered".into(),
                    ));
                };
                if record.consensus_pubkey != *bls_minpk_public {
                    return Err(PrecompileError::Revert(
                        "validator consensus public key mismatch".into(),
                    ));
                }
                Some(validator)
            }
            NodeIdV1::FullNode { .. } => None,
        };

        let height = self.storage.block_number()?;
        let measurement_accepted = policy.measurement_rules.iter().any(|rule| {
            rule.enclave_profile == intent.enclave_profile
                && rule.mrenclave == verdict.mrenclave
                && rule.mrsigner == verdict.mrsigner
                && rule.isv_prod_id == verdict.isv_prod_id
                && verdict.isv_svn >= rule.minimum_isv_svn
                && height >= rule.admit_from_height
                && height < rule.admit_until_height_exclusive
        });
        if !measurement_accepted {
            return Err(PrecompileError::Revert(
                "QVL verdict does not match an active profile measurement rule".into(),
            ));
        }
        if verdict.platform_tcb_status == DcapPlatformTcbStatusV1::SWHardeningNeeded
            && policy.accepted_platform_tcb_statuses
                != PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded
        {
            return Err(PrecompileError::Revert(
                "QVL Platform TCB status is stricter than active policy allows".into(),
            ));
        }

        let now = consensus_timestamp(&self.storage)?;
        let node_id_hash = intent
            .node_id
            .node_id_hash()
            .map_err(|error| revert_codec("node identity is invalid", error))?;
        let intent_hash = intent
            .intent_hash()
            .map_err(|error| revert_codec("registration intent is invalid", error))?;
        let current_intent_hash = self.v1_node_intent_hash.read(&node_id_hash)?;
        if !current_intent_hash.is_zero() && current_intent_hash == intent_hash {
            if self.v1_node_evidence_hash.read(&node_id_hash)? != evidence_hash {
                return Err(PrecompileError::Revert(
                    "registry mutation is not an exact evidence replay".into(),
                ));
            }
            if self.v1_node_binding_id.read(&node_id_hash)? != intent.binding_id
                || self.v1_node_enclave_id.read(&node_id_hash)? != intent.enclave_id
            {
                return Err(PrecompileError::Fatal(
                    "stored V1 idempotency identity is inconsistent".into(),
                ));
            }
            return Ok(V1RegistrationOutcome::Idempotent);
        }

        let current = self.node_enclave_binding_v1(node_id_hash)?;
        match expected_operation {
            AttestationOperationV1::RegisterEnclave => {
                if intent.binding_version != 1
                    || intent.registration_version != 0
                    || intent.renewal_nonce != 0
                    || intent.transition_nonce != 0
                {
                    return Err(PrecompileError::Revert(
                        "initial registration versions and nonces are not canonical".into(),
                    ));
                }
            }
            AttestationOperationV1::RenewEnclave => {
                let current = current.as_ref().ok_or_else(|| {
                    PrecompileError::Revert("cannot renew a missing enclave binding".into())
                })?;
                if intent.enclave_profile as u64 != self.v1_node_profile.read(&node_id_hash)? {
                    return Err(PrecompileError::Revert(
                        "renewal changes the registered enclave profile".into(),
                    ));
                }
                ensure_continuous_binding(current, intent, verdict)?;
                if intent.binding_version != current.binding_version
                    || intent.registration_version
                        != next_counter(current.registration_version, "registration version")?
                    || intent.renewal_nonce != next_counter(current.renewal_nonce, "renewal nonce")?
                    || intent.transition_nonce != current.transition_nonce
                {
                    return Err(PrecompileError::Revert(
                        "renewal does not carry the exact next renewal version and nonce".into(),
                    ));
                }
                ensure_final_third_or_expired(current, now)?;
            }
            AttestationOperationV1::ReplaceEnclaveBinding => {
                let current = current.as_ref().ok_or_else(|| {
                    PrecompileError::Revert("cannot replace a missing enclave binding".into())
                })?;
                if intent.enclave_profile as u64 != self.v1_node_profile.read(&node_id_hash)?
                    || B256::from(intent.node_host_authorization_hash)
                        != current.node_host_authorization_hash
                {
                    return Err(PrecompileError::Revert(
                        "replacement changes the node profile or persistent NodeHost authorization"
                            .into(),
                    ));
                }
                if intent.enclave_id == current.enclave_id
                    || intent.binding_id == current.binding_id
                {
                    return Err(PrecompileError::Revert(
                        "replacement must use a fresh enclave and binding id".into(),
                    ));
                }
                if intent.binding_version
                    != next_counter(current.binding_version, "binding version")?
                    || intent.registration_version
                        != next_counter(current.registration_version, "registration version")?
                    || intent.renewal_nonce != current.renewal_nonce
                    || intent.transition_nonce != current.transition_nonce
                {
                    return Err(PrecompileError::Revert(
                        "replacement does not carry the exact next binding version".into(),
                    ));
                }
            }
            AttestationOperationV1::TransitionEnclaveMeasurement => {
                return Err(PrecompileError::Revert(
                    "measurement transition is owned by I7 and is not active".into(),
                ));
            }
        }

        let lease = intent
            .requested_valid_until
            .checked_sub(now)
            .ok_or_else(|| PrecompileError::Revert("requested lease is already expired".into()))?;
        if lease < policy.minimum_lease || lease > policy.maximum_lease {
            return Err(PrecompileError::Revert(
                "requested lease is outside active policy bounds".into(),
            ));
        }
        let collateral_limit = verdict
            .collateral_valid_until
            .checked_sub(policy.collateral_margin)
            .ok_or_else(|| {
                PrecompileError::Revert(
                    "verified collateral leaves no mandatory safety margin".into(),
                )
            })?;
        if intent.requested_valid_until > collateral_limit {
            return Err(PrecompileError::Revert(
                "requested lease exceeds verified collateral validity".into(),
            ));
        }
        if expected_operation == AttestationOperationV1::RegisterEnclave && current.is_some() {
            return Err(PrecompileError::Revert(
                "node already has a different enclave binding".into(),
            ));
        }

        if let Some(validator) = validator {
            let validator_node_hash = self.validator_v1_node_hash.read(&validator)?;
            if !validator_node_hash.is_zero() && validator_node_hash != node_id_hash {
                return Err(PrecompileError::Revert(
                    "validator address already has another node binding".into(),
                ));
            }
        }
        let enclave_owner = self.v1_enclave_node_hash.read(&intent.enclave_id)?;
        let binding_owner = self.v1_binding_node_hash.read(&intent.binding_id)?;
        match expected_operation {
            AttestationOperationV1::RegisterEnclave => {
                if !enclave_owner.is_zero() {
                    return Err(PrecompileError::Revert(
                        "enclave is already bound to another node".into(),
                    ));
                }
                if !binding_owner.is_zero() {
                    return Err(PrecompileError::Revert(
                        "binding id has already been used by another node".into(),
                    ));
                }
            }
            AttestationOperationV1::RenewEnclave => {
                if enclave_owner != node_id_hash || binding_owner != node_id_hash {
                    return Err(PrecompileError::Fatal(
                        "stored V1 renewal reverse ownership is inconsistent".into(),
                    ));
                }
            }
            AttestationOperationV1::ReplaceEnclaveBinding => {
                if !enclave_owner.is_zero() || !binding_owner.is_zero() {
                    return Err(PrecompileError::Revert(
                        "replacement enclave or binding id has already been used".into(),
                    ));
                }
            }
            AttestationOperationV1::TransitionEnclaveMeasurement => unreachable!(),
        }

        let verdict_bytes = verdict.encode_canonical().map_err(|code| {
            PrecompileError::Fatal(format!(
                "verified DCAP verdict cannot be encoded: {:#06x}",
                code.code()
            ))
        })?;
        let verdict_hash = keccak256(verdict_bytes);
        let recipient = B256::from(intent.recipient_x25519);
        let attestation = B256::from(intent.attestation_ed25519);
        let noise = B256::from(intent.noise_responder_x25519);

        if let Some(validator) = validator {
            self.validator_v1_node_hash
                .write(&validator, node_id_hash)?;
        }
        self.v1_node_enclave_id
            .write(&node_id_hash, intent.enclave_id)?;
        self.v1_enclave_node_hash
            .write(&intent.enclave_id, node_id_hash)?;
        self.v1_node_binding_id
            .write(&node_id_hash, intent.binding_id)?;
        self.v1_binding_node_hash
            .write(&intent.binding_id, node_id_hash)?;
        self.v1_node_intent_hash.write(&node_id_hash, intent_hash)?;
        self.v1_node_evidence_hash
            .write(&node_id_hash, evidence_hash)?;
        self.v1_node_policy_hash.write(&node_id_hash, policy_hash)?;
        self.v1_node_profile
            .write(&node_id_hash, intent.enclave_profile as u64)?;
        self.v1_node_binding_version
            .write(&node_id_hash, intent.binding_version)?;
        self.v1_node_registration_version
            .write(&node_id_hash, intent.registration_version)?;
        self.v1_node_renewal_nonce
            .write(&node_id_hash, intent.renewal_nonce)?;
        self.v1_node_transition_nonce
            .write(&node_id_hash, intent.transition_nonce)?;
        self.v1_node_lease_started_at.write(&node_id_hash, now)?;
        self.v1_node_valid_until
            .write(&node_id_hash, intent.requested_valid_until)?;
        self.v1_node_collateral_valid_until
            .write(&node_id_hash, verdict.collateral_valid_until)?;
        self.v1_node_recipient_x25519
            .write(&node_id_hash, recipient)?;
        self.v1_node_attestation_ed25519
            .write(&node_id_hash, attestation)?;
        self.v1_node_noise_responder_x25519
            .write(&node_id_hash, noise)?;
        self.v1_node_mrenclave
            .write(&node_id_hash, verdict.mrenclave)?;
        self.v1_node_mrsigner
            .write(&node_id_hash, verdict.mrsigner)?;
        self.v1_node_isv_prod_id
            .write(&node_id_hash, u64::from(verdict.isv_prod_id))?;
        self.v1_node_isv_svn
            .write(&node_id_hash, u64::from(verdict.isv_svn))?;
        self.v1_node_platform_tcb_status
            .write(&node_id_hash, verdict.platform_tcb_status as u64)?;
        self.v1_node_verdict_hash
            .write(&node_id_hash, verdict_hash)?;
        self.v1_node_host_authorization_hash.write(
            &node_id_hash,
            B256::from(intent.node_host_authorization_hash),
        )?;

        if let Some(validator) = validator {
            let first_registration = self.recipient_x25519.read(&validator)?.is_zero();
            self.recipient_x25519.write(&validator, recipient)?;
            self.attestation_pub.write(&validator, attestation)?;
            self.noise_static_pub.write(&validator, noise)?;
            self.mrenclave.write(&validator, verdict.mrenclave)?;
            self.mrsigner.write(&validator, verdict.mrsigner)?;
            self.isv_svn.write(&validator, u64::from(verdict.isv_svn))?;
            self.keys_hash.write(
                &validator,
                compute_keys_hash(
                    validator,
                    recipient,
                    attestation,
                    noise,
                    verdict.mrenclave,
                    verdict.mrsigner,
                    verdict.isv_svn,
                ),
            )?;
            if first_registration {
                let count = self
                    .registered_count
                    .read()?
                    .checked_add(1)
                    .ok_or_else(|| {
                        PrecompileError::Fatal("TEE registered-count overflow".into())
                    })?;
                self.registered_count.write(count)?;
            }
        }

        match expected_operation {
            AttestationOperationV1::RegisterEnclave => self.emit(EnclaveRegisteredV1 {
                nodeIdHash: node_id_hash,
                enclaveId: intent.enclave_id,
                bindingId: intent.binding_id,
                validUntil: intent.requested_valid_until,
                bindingVersion: intent.binding_version,
            })?,
            AttestationOperationV1::RenewEnclave => self.emit(EnclaveRenewedV1 {
                nodeIdHash: node_id_hash,
                enclaveId: intent.enclave_id,
                bindingId: intent.binding_id,
                validUntil: intent.requested_valid_until,
                registrationVersion: intent.registration_version,
                renewalNonce: intent.renewal_nonce,
            })?,
            AttestationOperationV1::ReplaceEnclaveBinding => {
                self.emit(EnclaveBindingReplacedV1 {
                    nodeIdHash: node_id_hash,
                    enclaveId: intent.enclave_id,
                    bindingId: intent.binding_id,
                    validUntil: intent.requested_valid_until,
                    bindingVersion: intent.binding_version,
                })?
            }
            AttestationOperationV1::TransitionEnclaveMeasurement => unreachable!(),
        }
        Ok(V1RegistrationOutcome::Created)
    }

    #[cfg(test)]
    pub(crate) fn register_enclave_after_verifier_for_test(
        &mut self,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        capability: PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.apply_verified_mutation_v1(
            AttestationOperationV1::RegisterEnclave,
            intent,
            node_signature,
            enclave_signature,
            &policy,
            &capability.verdict,
            capability.evidence_hash,
        )
    }

    #[cfg(test)]
    pub(crate) fn register_enclave_after_verifier_with_active_policy_for_test(
        &mut self,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
        capability: PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_verified_mutation_v1(
            AttestationOperationV1::RegisterEnclave,
            intent,
            node_signature,
            enclave_signature,
            policy,
            &capability.verdict,
            capability.evidence_hash,
        )
    }

    #[cfg(test)]
    pub(crate) fn renew_enclave_after_verifier_for_test(
        &mut self,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        capability: PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.apply_verified_mutation_v1(
            AttestationOperationV1::RenewEnclave,
            intent,
            node_signature,
            enclave_signature,
            &policy,
            &capability.verdict,
            capability.evidence_hash,
        )
    }

    #[cfg(test)]
    pub(crate) fn renew_enclave_after_verifier_with_active_policy_for_test(
        &mut self,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
        capability: PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_verified_mutation_v1(
            AttestationOperationV1::RenewEnclave,
            intent,
            node_signature,
            enclave_signature,
            policy,
            &capability.verdict,
            capability.evidence_hash,
        )
    }

    #[cfg(test)]
    pub(crate) fn replace_enclave_binding_after_verifier_for_test(
        &mut self,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        capability: PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.apply_verified_mutation_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            intent,
            node_signature,
            enclave_signature,
            &policy,
            &capability.verdict,
            capability.evidence_hash,
        )
    }

    #[cfg(test)]
    pub(crate) fn replace_enclave_binding_after_verifier_with_active_policy_for_test(
        &mut self,
        intent: &RegistrationIntentV1,
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
        policy: &TeePolicyV1,
        capability: PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_verified_mutation_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            intent,
            node_signature,
            enclave_signature,
            policy,
            &capability.verdict,
            capability.evidence_hash,
        )
    }
}

#[cfg(feature = "tee-attestation-v1")]
fn ensure_continuous_binding(
    current: &NodeEnclaveBindingV1,
    intent: &RegistrationIntentV1,
    verdict: &DcapVerdictV1,
) -> Result<()> {
    if intent.enclave_id != current.enclave_id
        || intent.binding_id != current.binding_id
        || B256::from(intent.recipient_x25519) != current.recipient_x25519
        || B256::from(intent.attestation_ed25519) != current.attestation_ed25519
        || B256::from(intent.noise_responder_x25519) != current.noise_responder_x25519
        || B256::from(intent.node_host_authorization_hash) != current.node_host_authorization_hash
    {
        return Err(PrecompileError::Revert(
            "renewal targets a superseded or different enclave identity".into(),
        ));
    }
    if verdict.mrenclave != current.mrenclave
        || verdict.mrsigner != current.mrsigner
        || verdict.isv_prod_id != current.isv_prod_id
        || verdict.isv_svn != current.isv_svn
    {
        return Err(PrecompileError::Revert(
            "renewal cannot replace the admitted enclave measurement".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "tee-attestation-v1")]
fn next_counter(current: u64, name: &'static str) -> Result<u64> {
    current
        .checked_add(1)
        .ok_or_else(|| PrecompileError::Revert(format!("{name} is exhausted")))
}

#[cfg(feature = "tee-attestation-v1")]
fn ensure_final_third_or_expired(current: &NodeEnclaveBindingV1, now: u64) -> Result<()> {
    if now >= current.valid_until {
        return Ok(());
    }
    if current.lease_started_at >= current.valid_until || now < current.lease_started_at {
        return Err(PrecompileError::Fatal(
            "stored V1 lease interval is inconsistent".into(),
        ));
    }
    let elapsed = u128::from(now - current.lease_started_at);
    let duration = u128::from(current.valid_until - current.lease_started_at);
    if elapsed * 3 < duration * 2 {
        return Err(PrecompileError::Revert(
            "renewal is not open before the final third of the current lease".into(),
        ));
    }
    Ok(())
}

/// Test-only typed capability that begins strictly after public QVL verification.
/// It is absent from every non-test artifact and cannot parse or bless evidence.
#[cfg(all(test, feature = "tee-attestation-v1"))]
pub(crate) struct PostVerifierDcapCapabilityV1 {
    verdict: DcapVerdictV1,
    evidence_hash: B256,
}

#[cfg(all(test, feature = "tee-attestation-v1"))]
impl PostVerifierDcapCapabilityV1 {
    pub(crate) fn new(verdict: DcapVerdictV1) -> Self {
        Self {
            verdict,
            evidence_hash: B256::repeat_byte(0xEC),
        }
    }

    pub(crate) fn with_evidence_hash(verdict: DcapVerdictV1, evidence_hash: B256) -> Self {
        Self {
            verdict,
            evidence_hash,
        }
    }
}

fn chain_id_word(chain_id: u64) -> [u8; 32] {
    U256::from(chain_id).to_be_bytes()
}

fn consensus_timestamp(storage: &outbe_primitives::storage::StorageHandle<'_>) -> Result<u64> {
    u64::try_from(storage.timestamp()?)
        .map_err(|_| PrecompileError::Revert("consensus timestamp exceeds u64".into()))
}

fn checked_u16(value: u64, field: &'static str) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| PrecompileError::Fatal(format!("{field} exceeds its canonical width")))
}

fn checked_u8(value: u64, field: &'static str) -> Result<u8> {
    u8::try_from(value)
        .map_err(|_| PrecompileError::Fatal(format!("{field} exceeds its canonical width")))
}

fn revert_codec(context: &'static str, error: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Revert(format!("{context}: {error}"))
}
