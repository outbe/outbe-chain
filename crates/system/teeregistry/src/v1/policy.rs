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

    /// Stages the one exact successor authorized by an approved protocol
    /// update. The policy remains unavailable to ordinary admission until its
    /// activation height. I7 measurement transition is its only rollout path.
    pub fn stage_successor_policy_v1(
        &mut self,
        proposal_id: U256,
        policy: &TeePolicyV1,
    ) -> Result<()> {
        if proposal_id.is_zero() {
            return Err(PrecompileError::Revert(
                "successor policy proposal id must be nonzero".into(),
            ));
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
            return Err(PrecompileError::Revert(
                "another successor V1 policy is already staged".into(),
            ));
        }

        for (index, chunk) in canonical.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let index = u32::try_from(index).map_err(|_| {
                PrecompileError::Revert("successor V1 policy has too many chunks".into())
            })?;
            self.staged_v1_policy_chunk
                .write(&index, B256::from(word))?;
        }
        let len = u32::try_from(canonical.len())
            .map_err(|_| PrecompileError::Revert("successor V1 policy is too large".into()))?;
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
                return Err(PrecompileError::Fatal(
                    "empty staged V1 policy has non-empty anchors".into(),
                ));
            }
            return Ok(None);
        }
        let canonical = self.read_staged_policy_bytes_v1()?;
        let policy = TeePolicyV1::decode_canonical(&canonical).map_err(|error| {
            PrecompileError::Fatal(format!("stored staged V1 policy is non-canonical: {error}"))
        })?;
        let policy_hash = policy.policy_hash().map_err(|error| {
            PrecompileError::Fatal(format!("stored staged V1 policy cannot be hashed: {error}"))
        })?;
        if policy_hash != self.staged_v1_policy_hash.read()? {
            return Err(PrecompileError::Fatal(
                "stored staged V1 policy hash does not match its anchor".into(),
            ));
        }
        if policy.activation_height != self.staged_v1_policy_activation_height.read()? {
            return Err(PrecompileError::Fatal(
                "stored staged V1 policy activation does not match its anchor".into(),
            ));
        }
        let proposal_id = self.staged_v1_policy_proposal_id.read()?;
        if proposal_id.is_zero() {
            return Err(PrecompileError::Fatal(
                "stored staged V1 policy has zero proposal id".into(),
            ));
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
            return Err(PrecompileError::Revert(
                "no successor V1 policy is staged for this update".into(),
            ));
        };
        self.validate_policy_promotion_v1(&policy, staged_proposal_id, proposal_id, block_number)?;
        let canonical = policy.encode_canonical().map_err(|error| {
            PrecompileError::Fatal(format!("staged successor V1 policy is invalid: {error}"))
        })?;
        let policy_hash = policy.policy_hash().map_err(|error| {
            PrecompileError::Fatal(format!(
                "staged successor V1 policy cannot be hashed: {error}"
            ))
        })?;
        for (index, chunk) in canonical.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let index = u32::try_from(index).map_err(|_| {
                PrecompileError::Fatal("successor V1 policy chunk index overflow".into())
            })?;
            self.active_v1_policy_chunk
                .write(&index, B256::from(word))?;
        }
        let len = u32::try_from(canonical.len())
            .map_err(|_| PrecompileError::Fatal("successor V1 policy length overflow".into()))?;
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
            return Err(PrecompileError::Revert(
                "cannot discard another update's staged V1 policy".into(),
            ));
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

    fn read_staged_policy_bytes_v1(&self) -> Result<Vec<u8>> {
        let len = usize::try_from(self.staged_v1_policy_len.read()?).map_err(|_| {
            PrecompileError::Fatal("stored staged V1 policy length overflow".into())
        })?;
        if len == 0 {
            return Err(PrecompileError::Fatal(
                "staged V1 policy bytes requested while empty".into(),
            ));
        }
        if len > MAX_TEE_POLICY_BYTES {
            return Err(PrecompileError::Fatal(
                "stored staged V1 policy exceeds the protocol cap".into(),
            ));
        }
        let words = len.div_ceil(32);
        let mut canonical = Vec::with_capacity(words * 32);
        for index in 0..words {
            let index = u32::try_from(index).map_err(|_| {
                PrecompileError::Fatal("stored staged V1 policy index overflow".into())
            })?;
            canonical.extend_from_slice(self.staged_v1_policy_chunk.read(&index)?.as_slice());
        }
        canonical.truncate(len);
        Ok(canonical)
    }
    fn validate_initial_policy_v1(&self, policy: &TeePolicyV1) -> Result<()> {
        let chain_id = self.storage.chain_id()?;
        let expected_chain_id = chain_id_word(chain_id);
        if policy.chain_id != expected_chain_id
            || policy.genesis_hash != self.storage.genesis_hash()?
        {
            return Err(PrecompileError::Revert(
                "initial V1 policy chain identity mismatch".into(),
            ));
        }
        if !is_attestation_mode_allowed_for_chain_id(chain_id, policy.attestation_mode) {
            return Err(PrecompileError::Revert(
                "initial V1 policy attestation mode is not allowed for this chain".into(),
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
            .ok_or_else(|| PrecompileError::Revert("V1 policy version overflow".into()))?;
        Self::validate_successor_identity_v1(policy, &current)?;
        Self::validate_successor_lineage_v1(policy, expected_version, current_hash)?;
        if policy.activation_height <= self.storage.block_number()? {
            return Err(PrecompileError::Revert(
                "successor V1 policy activation must be in the future".into(),
            ));
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
            return Err(PrecompileError::Revert(
                "staged successor V1 policy belongs to another update".into(),
            ));
        }
        if block_number < policy.activation_height {
            return Err(PrecompileError::Revert(
                "successor V1 policy activation height has not been reached".into(),
            ));
        }
        let current = self.active_policy_v1()?;
        let current_hash = current
            .policy_hash()
            .map_err(|error| revert_codec("invalid current V1 policy", error))?;
        if policy.attestation_mode != current.attestation_mode {
            return Err(PrecompileError::Fatal(
                "staged successor V1 policy changes attestation mode".into(),
            ));
        }
        let same_chain =
            policy.chain_id == current.chain_id && policy.genesis_hash == current.genesis_hash;
        if !same_chain || !Self::policy_promotion_follows_v1(policy, &current, current_hash)? {
            return Err(PrecompileError::Fatal(
                "staged successor V1 policy no longer follows current policy".into(),
            ));
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
                    .ok_or_else(|| PrecompileError::Fatal("V1 policy version overflow".into()))?)
    }

    fn validate_successor_identity_v1(policy: &TeePolicyV1, current: &TeePolicyV1) -> Result<()> {
        if policy.chain_id != current.chain_id || policy.genesis_hash != current.genesis_hash {
            return Err(PrecompileError::Revert(
                "successor V1 policy chain identity mismatch".into(),
            ));
        }
        if policy.attestation_mode != current.attestation_mode {
            return Err(PrecompileError::Revert(
                "successor V1 policy cannot change attestation mode".into(),
            ));
        }
        Ok(())
    }
    fn validate_successor_lineage_v1(
        policy: &TeePolicyV1,
        expected_version: u64,
        current_hash: B256,
    ) -> Result<()> {
        if policy.policy_version != expected_version {
            return Err(PrecompileError::Revert(
                "successor V1 policy version is not current plus one".into(),
            ));
        }
        if policy.predecessor_policy_hash != current_hash {
            return Err(PrecompileError::Revert(
                "successor V1 policy predecessor hash mismatch".into(),
            ));
        }
        Ok(())
    }
}
