//! Explicit opt-in hard retirement; zero anchors retain historical lease semantics.
use crate::{NodeEnclaveBindingV1, TeeRegistry};
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    tee_attestation_v1::TeePolicyV1,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnclaveUpgradeV1 {
    pub proposal_id: U256,
    pub activation_height: u64,
    pub mrenclave: B256,
    pub successor_policy_hash: B256,
    pub predecessor_policy_hash: B256,
}

impl TeeRegistry<'_> {
    #[cfg(feature = "tee-attestation-v1")]
    pub(crate) fn policy_for_evidence_v1(
        &self,
        evidence: &[u8],
        transition: bool,
    ) -> Result<TeePolicyV1> {
        use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
        // Preserve the legacy policy-selection and rejection order until explicit opt-in.
        if self.storage.enclave_upgrade_id()?.is_zero() {
            return if transition {
                self.staged_successor_policy_v1()?
                    .map(|(_, policy)| policy)
                    .ok_or_else(|| {
                        PrecompileError::Revert("no successor V1 policy is staged".into())
                    })
            } else {
                self.active_policy_v1()
            };
        }
        let decoded = AttestationEvidenceV1::decode_canonical(evidence).map_err(|e| {
            PrecompileError::Revert(format!("attestation evidence is not canonical: {e}"))
        })?;
        let hash = decoded.intent().policy_hash;
        if !self.policy_hash_admitted_v1(hash, transition)? {
            return Err(PrecompileError::Revert(
                "attestation targets a retired or unapproved TEE policy".into(),
            ));
        }
        if hash == self.active_v1_policy_hash.read()? {
            return self.active_policy_v1();
        }
        self.staged_successor_policy_v1()?
            .map(|(_, policy)| policy)
            .ok_or_else(|| PrecompileError::Revert("no successor V1 policy is staged".into()))
    }

    #[cfg(feature = "tee-attestation-v1")]
    pub(crate) fn policy_hash_admitted_v1(&self, hash: B256, transition: bool) -> Result<bool> {
        let upgrade = self.enclave_upgrade_v1()?;
        if upgrade.proposal_id.is_zero() {
            return Ok(hash
                == if transition {
                    self.staged_v1_policy_hash.read()?
                } else {
                    self.active_v1_policy_hash.read()?
                });
        }
        let height = self.storage.block_number()?;
        if hash == upgrade.successor_policy_hash {
            return Ok(hash == self.active_v1_policy_hash.read()?
                || (height < upgrade.activation_height
                    && hash == self.staged_v1_policy_hash.read()?));
        }
        Ok(!transition
            && height < upgrade.activation_height
            && hash == upgrade.predecessor_policy_hash
            && hash == self.active_v1_policy_hash.read()?)
    }

    #[cfg(feature = "tee-attestation-v1")]
    pub(crate) fn measurement_admission_height_v1(
        &self,
        policy: &TeePolicyV1,
        transition: bool,
    ) -> Result<u64> {
        let height = self.storage.block_number()?;
        if transition
            || (!self.storage.enclave_upgrade_id()?.is_zero() && height < policy.activation_height)
        {
            Ok(policy.activation_height)
        } else {
            Ok(height)
        }
    }
    pub fn enclave_upgrade_v1(&self) -> Result<EnclaveUpgradeV1> {
        if self.storage.enclave_upgrade_id()?.is_zero() {
            return Ok(EnclaveUpgradeV1::default());
        }
        Ok(EnclaveUpgradeV1 {
            proposal_id: self.storage.enclave_upgrade_id()?,
            activation_height: self.strict_upgrade_height.read()?,
            mrenclave: self.strict_upgrade_mrenclave.read()?,
            successor_policy_hash: self.strict_upgrade_successor.read()?,
            predecessor_policy_hash: self.strict_upgrade_predecessor.read()?,
        })
    }

    pub fn stage_measurement_upgrade_v1(
        &mut self,
        proposal: U256,
        mrenclave: B256,
        predecessor: B256,
        height: u64,
    ) -> Result<()> {
        let current = self.active_policy_v1()?;
        let current_hash = policy_hash(&current)?;
        if current_hash != predecessor
            || mrenclave.is_zero()
            || current.measurement_rules.len() != 1
        {
            return Err(PrecompileError::Revert(
                "enclave upgrade requires the exact single-measurement predecessor policy".into(),
            ));
        }
        if current.measurement_rules[0].mrenclave == mrenclave {
            return Err(PrecompileError::Revert(
                "enclave upgrade must change MRENCLAVE".into(),
            ));
        }
        let mut successor = current;
        successor.policy_version = successor
            .policy_version
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Revert("TEE policy version overflow".into()))?;
        successor.predecessor_policy_hash = predecessor;
        successor.activation_height = height;
        successor.measurement_rules[0].mrenclave = mrenclave;
        successor.measurement_rules[0].mrsigner = B256::ZERO;
        successor.measurement_rules[0].admit_from_height = height;
        successor.measurement_rules[0].admit_until_height_exclusive = u64::MAX;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.stage_successor_policy_v1(proposal, &successor)?;
            self.strict_upgrade_proposal.write(proposal)?;
            self.strict_upgrade_height.write(height)?;
            self.strict_upgrade_mrenclave.write(mrenclave)?;
            self.strict_upgrade_successor
                .write(policy_hash(&successor)?)?;
            self.strict_upgrade_predecessor.write(predecessor)
        })
    }

    /// Used by execution and finalized-state consumers; caller supplies the action height.
    pub fn binding_code_admitted_at_v1(
        &self,
        binding: &NodeEnclaveBindingV1,
        height: u64,
    ) -> Result<bool> {
        let upgrade = self.enclave_upgrade_v1()?;
        if upgrade.proposal_id.is_zero() {
            return Ok(true);
        }
        if binding.policy_hash == upgrade.successor_policy_hash {
            return Ok(binding.mrenclave == upgrade.mrenclave);
        }
        Ok(height < upgrade.activation_height
            && binding.policy_hash == upgrade.predecessor_policy_hash)
    }

    pub fn binding_code_admitted_v1(&self, binding: &NodeEnclaveBindingV1) -> Result<bool> {
        self.binding_code_admitted_at_v1(binding, self.storage.block_number()?)
    }

    pub fn strict_upgrade_pending_v1(&self) -> Result<bool> {
        let upgrade = self.enclave_upgrade_v1()?;
        Ok(!upgrade.proposal_id.is_zero()
            && self.staged_v1_policy_proposal_id.read()? == upgrade.proposal_id)
    }

    pub fn upgrade_penalty_applied_v1(&self, proposal: U256, validator: Address) -> Result<bool> {
        self.strict_upgrade_penalized
            .read(&penalty_key(proposal, validator))
    }
    pub fn mark_upgrade_penalty_v1(&mut self, proposal: U256, validator: Address) -> Result<()> {
        self.strict_upgrade_penalized
            .write(&penalty_key(proposal, validator), true)
    }
    pub fn upgrade_sweep_due_v1(&self) -> Result<Option<EnclaveUpgradeV1>> {
        let upgrade = self.enclave_upgrade_v1()?;
        Ok((!upgrade.proposal_id.is_zero()
            && self.storage.block_number()? >= upgrade.activation_height
            && self.active_v1_policy_proposal_id.read()? == upgrade.proposal_id
            && self.strict_upgrade_swept.read()? != upgrade.proposal_id)
            .then_some(upgrade))
    }
    pub fn mark_upgrade_swept_v1(&mut self, proposal: U256) -> Result<()> {
        self.strict_upgrade_swept.write(proposal)
    }

    /// Physical scalar slots authenticated by external finalized-state readers.
    pub fn enclave_upgrade_storage_slots_v1(&self) -> Vec<B256> {
        (45_u64..=49)
            .chain([52])
            .map(|slot| B256::from(U256::from(slot).to_be_bytes::<32>()))
            .collect()
    }
    pub fn last_enclave_retirement_height_v1(&self) -> Result<u64> {
        self.last_enclave_retirement_height.read()
    }
}

fn penalty_key(proposal: U256, validator: Address) -> B256 {
    let mut bytes = Vec::with_capacity(52);
    bytes.extend_from_slice(&proposal.to_be_bytes::<32>());
    bytes.extend_from_slice(validator.as_slice());
    keccak256(bytes)
}
fn policy_hash(policy: &TeePolicyV1) -> Result<B256> {
    policy
        .policy_hash()
        .map_err(|e| PrecompileError::Revert(format!("invalid TEE policy: {e}")))
}
