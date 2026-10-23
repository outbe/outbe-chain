use super::*;

impl TeeRegistry<'_> {
    /// Installs the immutable first V1 policy. Only the existing protocol Update
    /// lifecycle stages and promotes successors. This bootstrap method
    /// intentionally cannot rotate a policy.
    pub fn install_initial_policy_v1(&mut self, policy: &TeePolicyV1) -> Result<()> {
        let canonical = policy
            .encode_canonical()
            .map_err(|error| revert_codec("invalid initial V1 policy", error))?;
        self.validate_initial_policy_v1(policy)?;
        let policy_hash = policy
            .policy_hash()
            .map_err(|error| revert_codec("invalid initial V1 policy", error))?;
        let installed_len = self.active_v1_policy_len.read()?;
        if installed_len != 0 {
            let installed = self.read_policy_bytes_v1()?;
            if installed == canonical && self.active_v1_policy_hash.read()? == policy_hash {
                return Ok(());
            }
            return Err(TeeRegistryError::InitialV1PolicyAlreadyInstalled.into());
        }
        let anchored_hash = self.active_v1_policy_hash.read()?;
        if !anchored_hash.is_zero() && anchored_hash != policy_hash {
            return Err(TeeRegistryError::InitialV1PolicyHashConflict.into());
        }

        for (index, chunk) in canonical.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let index =
                u32::try_from(index).map_err(|_| TeeRegistryError::V1PolicyTooManyChunks)?;
            self.active_v1_policy_chunk
                .write(&index, B256::from(word))?;
        }
        let len = u32::try_from(canonical.len()).map_err(|_| TeeRegistryError::V1PolicyTooLarge)?;
        self.active_v1_policy_len.write(len)?;
        self.active_v1_policy_hash.write(policy_hash)?;
        Ok(())
    }

    /// Stages the one exact successor authorized by an approved protocol
    /// update. The policy remains unavailable to ordinary admission until its
    /// activation height. I7 measurement transition is its only rollout path.
    pub fn stage_successor_policy_v1(
        &mut self,
        proposal_id: U256,
        policy: &TeePolicyV1,
    ) -> Result<()> {
        if proposal_id.is_zero() {
            return Err(TeeRegistryError::SuccessorPolicyProposalIdZero.into());
        }
        let canonical = policy
            .encode_canonical()
            .map_err(|error| revert_codec("invalid successor V1 policy", error))?;
        self.validate_successor_policy_v1(policy)?;
        let policy_hash = policy
            .policy_hash()
            .map_err(|error| revert_codec("invalid successor V1 policy", error))?;
        if self.staged_v1_policy_len.read()? != 0 {
            let staged = self.read_staged_policy_bytes_v1()?;
            if self.staged_v1_policy_proposal_id.read()? == proposal_id
                && self.staged_v1_policy_hash.read()? == policy_hash
                && staged == canonical
            {
                return Ok(());
            }
            return Err(TeeRegistryError::AnotherSuccessorV1PolicyStaged.into());
        }

        for (index, chunk) in canonical.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let index = u32::try_from(index)
                .map_err(|_| TeeRegistryError::SuccessorV1PolicyTooManyChunks)?;
            self.staged_v1_policy_chunk
                .write(&index, B256::from(word))?;
        }
        let len = u32::try_from(canonical.len())
            .map_err(|_| TeeRegistryError::SuccessorV1PolicyTooLarge)?;
        self.staged_v1_policy_len.write(len)?;
        self.staged_v1_policy_hash.write(policy_hash)?;
        self.staged_v1_policy_proposal_id.write(proposal_id)?;
        self.staged_v1_policy_activation_height
            .write(policy.activation_height)?;
        Ok(())
    }

    /// Returns the authenticated staged successor and its owning Update
    /// proposal, or `None` when no TEE policy update is pending.
    pub fn staged_successor_policy_v1(&self) -> Result<Option<(U256, TeePolicyV1)>> {
        let len = self.staged_v1_policy_len.read()?;
        if len == 0 {
            if !self.staged_v1_policy_hash.read()?.is_zero()
                || !self.staged_v1_policy_proposal_id.read()?.is_zero()
                || self.staged_v1_policy_activation_height.read()? != 0
            {
                return Err(TeeRegistryError::EmptyStagedV1PolicyNonEmptyAnchors.into());
            }
            return Ok(None);
        }
        let canonical = self.read_staged_policy_bytes_v1()?;
        let policy = TeePolicyV1::decode_canonical(&canonical)
            .map_err(TeeRegistryError::stored_staged_v1_policy_non_canonical)?;
        let policy_hash = policy
            .policy_hash()
            .map_err(TeeRegistryError::stored_staged_v1_policy_hashing_failed)?;
        if policy_hash != self.staged_v1_policy_hash.read()? {
            return Err(TeeRegistryError::StoredStagedV1PolicyHashMismatch.into());
        }
        if policy.activation_height != self.staged_v1_policy_activation_height.read()? {
            return Err(TeeRegistryError::StoredStagedV1PolicyActivationMismatch.into());
        }
        let proposal_id = self.staged_v1_policy_proposal_id.read()?;
        if proposal_id.is_zero() {
            return Err(TeeRegistryError::StoredStagedV1PolicyZeroProposalId.into());
        }
        Ok(Some((proposal_id, policy)))
    }

    /// Atomically promotes the successor owned by `proposal_id` once its
    /// software-update height is reached. Exact replay after promotion is a
    /// no-op. A different or absent proposal cannot rotate policy authority.
    pub fn promote_staged_successor_policy_v1(
        &mut self,
        proposal_id: U256,
        block_number: u64,
    ) -> Result<()> {
        let Some((staged_proposal_id, policy)) = self.staged_successor_policy_v1()? else {
            if self.active_v1_policy_proposal_id.read()? == proposal_id && !proposal_id.is_zero() {
                return Ok(());
            }
            return Err(TeeRegistryError::NoSuccessorV1PolicyStagedForUpdate.into());
        };
        self.validate_policy_promotion_v1(&policy, staged_proposal_id, proposal_id, block_number)?;
        let canonical = policy
            .encode_canonical()
            .map_err(TeeRegistryError::staged_successor_v1_policy_invalid)?;
        let policy_hash = policy
            .policy_hash()
            .map_err(TeeRegistryError::staged_successor_v1_policy_hashing_failed)?;
        for (index, chunk) in canonical.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let index = u32::try_from(index)
                .map_err(|_| TeeRegistryError::SuccessorV1PolicyChunkIndexOverflow)?;
            self.active_v1_policy_chunk
                .write(&index, B256::from(word))?;
        }
        let len = u32::try_from(canonical.len())
            .map_err(|_| TeeRegistryError::SuccessorV1PolicyLengthOverflow)?;
        self.active_v1_policy_len.write(len)?;
        self.active_v1_policy_hash.write(policy_hash)?;
        self.active_v1_policy_proposal_id.write(proposal_id)?;
        if self.storage.enclave_upgrade_id()? == proposal_id {
            self.last_enclave_retirement_height
                .write(policy.activation_height)?;
        }
        self.clear_staged_successor_policy_v1()?;
        self.emit(TeePolicyActivatedV1 {
            proposalId: proposal_id,
            policyHash: policy_hash,
            policyVersion: policy.policy_version,
            activationHeight: policy.activation_height,
        })?;
        Ok(())
    }

    /// Clears a staged successor only when a newer activated protocol version
    /// cancels its exact owning Update proposal.
    pub fn discard_staged_successor_policy_v1(&mut self, proposal_id: U256) -> Result<()> {
        let Some((staged_proposal_id, _)) = self.staged_successor_policy_v1()? else {
            return Ok(());
        };
        if staged_proposal_id != proposal_id {
            return Err(TeeRegistryError::CannotDiscardAnotherUpdateStagedPolicy.into());
        }
        self.clear_staged_successor_policy_v1()
    }

    fn clear_staged_successor_policy_v1(&mut self) -> Result<()> {
        self.staged_v1_policy_len.write(0)?;
        self.staged_v1_policy_hash.write(B256::ZERO)?;
        self.staged_v1_policy_proposal_id.write(U256::ZERO)?;
        self.staged_v1_policy_activation_height.write(0)
    }

    /// Reads and authenticates current policy bytes from consensus storage.
    /// Calldata cannot supply or override policy authority.
    pub fn active_policy_v1(&self) -> Result<TeePolicyV1> {
        let canonical = self.read_policy_bytes_v1()?;
        let policy = TeePolicyV1::decode_canonical(&canonical)
            .map_err(TeeRegistryError::stored_v1_policy_non_canonical)?;
        if policy.chain_id != chain_id_word(self.storage.chain_id()?)
            || policy.genesis_hash != self.storage.genesis_hash()?
        {
            return Err(TeeRegistryError::StoredV1PolicyChainIdentityMismatch.into());
        }
        let policy_hash = policy
            .policy_hash()
            .map_err(TeeRegistryError::stored_v1_policy_hashing_failed)?;
        if self.active_v1_policy_hash.read()? != policy_hash {
            return Err(TeeRegistryError::StoredV1PolicyHashMismatch.into());
        }
        if policy.activation_height > self.storage.block_number()? {
            return Err(TeeRegistryError::NoV1PolicyActiveAtHeight.into());
        }
        Ok(policy)
    }

    fn read_policy_bytes_v1(&self) -> Result<Vec<u8>> {
        read_policy_chunks_v1(
            self.active_v1_policy_len.read()?,
            |index| self.active_v1_policy_chunk.read(&index),
            [
                TeeRegistryError::StoredV1PolicyLengthOverflow,
                TeeRegistryError::V1PolicyNotInstalled,
                TeeRegistryError::StoredV1PolicyExceedsCap,
                TeeRegistryError::StoredV1PolicyIndexOverflow,
            ],
        )
    }

    fn read_staged_policy_bytes_v1(&self) -> Result<Vec<u8>> {
        read_policy_chunks_v1(
            self.staged_v1_policy_len.read()?,
            |index| self.staged_v1_policy_chunk.read(&index),
            [
                TeeRegistryError::StoredStagedV1PolicyLengthOverflow,
                TeeRegistryError::StagedV1PolicyEmptyRead,
                TeeRegistryError::StoredStagedV1PolicyExceedsCap,
                TeeRegistryError::StoredStagedV1PolicyIndexOverflow,
            ],
        )
    }
    fn validate_initial_policy_v1(&self, policy: &TeePolicyV1) -> Result<()> {
        let chain_id = self.storage.chain_id()?;
        let expected_chain_id = chain_id_word(chain_id);
        if policy.chain_id != expected_chain_id
            || policy.genesis_hash != self.storage.genesis_hash()?
        {
            return Err(TeeRegistryError::InitialV1PolicyChainIdentityMismatch.into());
        }
        if !is_attestation_mode_allowed_for_chain_id(chain_id, policy.attestation_mode) {
            return Err(TeeRegistryError::InitialV1PolicyAttestationModeDisallowed.into());
        }
        if policy.policy_version != 1
            || policy.activation_height != 1
            || !policy.predecessor_policy_hash.is_zero()
        {
            return Err(TeeRegistryError::InitialV1PolicyInvalidVersionOrHeight.into());
        }

        Ok(())
    }
    fn validate_successor_policy_v1(&self, policy: &TeePolicyV1) -> Result<()> {
        let current = self.active_policy_v1()?;
        let current_hash = current
            .policy_hash()
            .map_err(|error| revert_codec("invalid current V1 policy", error))?;
        let expected_version = current
            .policy_version
            .checked_add(1)
            .ok_or(TeeRegistryError::V1PolicyVersionOverflow)?;
        Self::validate_successor_identity_v1(policy, &current)?;
        Self::validate_successor_lineage_v1(policy, expected_version, current_hash)?;
        if policy.activation_height <= self.storage.block_number()? {
            return Err(TeeRegistryError::SuccessorV1PolicyActivationNotInFuture.into());
        }

        Ok(())
    }
    fn validate_policy_promotion_v1(
        &self,
        policy: &TeePolicyV1,
        staged_proposal_id: U256,
        proposal_id: U256,
        block_number: u64,
    ) -> Result<()> {
        if staged_proposal_id != proposal_id {
            return Err(TeeRegistryError::StagedSuccessorV1PolicyWrongProposal.into());
        }
        if block_number < policy.activation_height {
            return Err(TeeRegistryError::SuccessorV1PolicyActivationHeightNotReached.into());
        }
        let current = self.active_policy_v1()?;
        let current_hash = current
            .policy_hash()
            .map_err(|error| revert_codec("invalid current V1 policy", error))?;
        if policy.attestation_mode != current.attestation_mode {
            return Err(TeeRegistryError::StagedSuccessorV1PolicyChangesAttestationMode.into());
        }
        let same_chain =
            policy.chain_id == current.chain_id && policy.genesis_hash == current.genesis_hash;
        if !same_chain || !Self::policy_promotion_follows_v1(policy, &current, current_hash)? {
            return Err(TeeRegistryError::StagedSuccessorV1PolicyBrokenLineage.into());
        }
        Ok(())
    }
    fn policy_promotion_follows_v1(
        policy: &TeePolicyV1,
        current: &TeePolicyV1,
        current_hash: B256,
    ) -> Result<bool> {
        Ok(policy.predecessor_policy_hash == current_hash
            && policy.policy_version
                == current
                    .policy_version
                    .checked_add(1)
                    .ok_or(TeeRegistryError::FatalV1PolicyVersionOverflow)?)
    }

    fn validate_successor_identity_v1(policy: &TeePolicyV1, current: &TeePolicyV1) -> Result<()> {
        if policy.chain_id != current.chain_id || policy.genesis_hash != current.genesis_hash {
            return Err(TeeRegistryError::SuccessorV1PolicyChainIdentityMismatch.into());
        }
        if policy.attestation_mode != current.attestation_mode {
            return Err(TeeRegistryError::SuccessorV1PolicyCannotChangeAttestationMode.into());
        }
        Ok(())
    }
    fn validate_successor_lineage_v1(
        policy: &TeePolicyV1,
        expected_version: u64,
        current_hash: B256,
    ) -> Result<()> {
        if policy.policy_version != expected_version {
            return Err(TeeRegistryError::SuccessorV1PolicyInvalidVersion.into());
        }
        if policy.predecessor_policy_hash != current_hash {
            return Err(TeeRegistryError::SuccessorV1PolicyPredecessorHashMismatch.into());
        }
        Ok(())
    }
}

fn read_policy_chunks_v1(
    len: u32,
    read_chunk: impl Fn(u32) -> Result<B256>,
    errors: [TeeRegistryError; 4],
) -> Result<Vec<u8>> {
    let [length_error, empty_error, cap_error, index_error] = errors;
    let len = usize::try_from(len).map_err(|_| length_error)?;
    if len == 0 {
        return Err(empty_error.into());
    }
    if len > MAX_TEE_POLICY_BYTES {
        return Err(cap_error.into());
    }
    let words = len.div_ceil(32);
    let mut canonical = Vec::with_capacity(words * 32);
    for index in 0..words {
        let index = u32::try_from(index).map_err(|_| index_error.clone())?;
        canonical.extend_from_slice(read_chunk(index)?.as_slice());
    }
    canonical.truncate(len);
    Ok(canonical)
}
