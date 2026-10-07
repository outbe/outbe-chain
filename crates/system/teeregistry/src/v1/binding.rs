use super::*;

impl TeeRegistry<'_> {
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

    pub fn node_host_enclave_binding_v1(
        &self,
        reth_p2p_public: [u8; 33],
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        let node_id_hash = NodeIdV1 { reth_p2p_public }
            .node_id_hash()
            .map_err(|error| revert_codec("NodeHost P2P identity is invalid", error))?;
        self.node_enclave_binding_v1(node_id_hash)
    }

    /// Reads one V1 binding for `node_id.reth_p2p_public`.
    /// The hash compare uses the same `NodeIdV1` that the inner read rebuilds.
    /// Those two hashes match for every well-formed identity.
    /// This function does not read a profile field or an identity map.
    pub fn node_enclave_binding_for_identity_v1(
        &self,
        node_id: &NodeIdV1,
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        let expected_hash = node_id
            .node_id_hash()
            .map_err(|error| revert_codec("node identity is invalid", error))?;
        let binding = self.node_host_enclave_binding_v1(node_id.reth_p2p_public)?;
        let Some(binding) = binding else {
            return Ok(None);
        };
        if binding.node_id_hash != expected_hash {
            return Err(PrecompileError::Fatal(
                "stored V1 binding identity mismatch".into(),
            ));
        }
        Ok(Some(binding))
    }

    /// Returns the exact append-only storage slots read by
    /// [`Self::node_enclave_binding_for_identity_v1`]. External light clients
    /// use this canonical plan to request one bounded MPT proof. Keeping the
    /// plan beside the schema prevents a parallel hand-maintained layout.
    pub fn node_enclave_binding_storage_slots_v1(&self, node_id: &NodeIdV1) -> Result<Vec<B256>> {
        let node_hash = node_id
            .node_id_hash()
            .map_err(|error| revert_codec("node identity is invalid", error))?;
        let mut slots = Vec::with_capacity(22);
        for slot in [
            self.v1_node_enclave_id.slot(&node_hash).slot(),
            self.v1_node_binding_id.slot(&node_hash).slot(),
            self.v1_node_intent_hash.slot(&node_hash).slot(),
            self.v1_node_policy_hash.slot(&node_hash).slot(),
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
        slots.extend(self.enclave_upgrade_storage_slots_v1());
        slots.sort_unstable();
        slots.dedup();
        Ok(slots)
    }

    /// Deterministic attestation readiness only. Full nodes do not consult the
    /// validator set; the exact compressed Reth P2P key is their node identity.
    pub fn is_node_host_enclave_ready_v1(&self, reth_p2p_public: [u8; 33]) -> Result<bool> {
        let Some(binding) = self.node_host_enclave_binding_v1(reth_p2p_public)? else {
            return Ok(false);
        };
        Ok(!binding.binding_id.is_zero()
            && !binding.enclave_id.is_zero()
            && self.binding_code_admitted_v1(&binding)?
            && binding.valid_until > consensus_timestamp(&self.storage)?)
    }

    /// Deterministic attestation readiness only. Consensus membership/status is a
    /// separate consumer concern, but missing, expired or key-rotated bindings
    /// are never ready.
    pub fn is_validator_enclave_ready_v1(&self, validator: Address) -> Result<bool> {
        let Some(binding) = self.validator_enclave_binding_v1(validator)? else {
            return Ok(false);
        };
        if binding.binding_id.is_zero() || binding.enclave_id.is_zero() {
            return Ok(false);
        }
        let validators = ValidatorSet::new(self.storage.clone());
        if validators.get_validator(validator)?.is_none() {
            return Ok(false);
        }
        Ok(self.binding_code_admitted_v1(&binding)?
            && binding.valid_until > consensus_timestamp(&self.storage)?)
    }

    pub(super) fn node_enclave_binding_v1(
        &self,
        node_id_hash: B256,
    ) -> Result<Option<NodeEnclaveBindingV1>> {
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
