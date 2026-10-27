use super::{registered_status, status};
use crate::errors::ValidatorSetError;
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, P2pInfo, ValidatorLifecycle};
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::error::Result;

impl ValidatorSet<'_> {
    /// Registers a new validator in test fixtures directly into
    /// `WaitingForReadiness` (`PENDING`).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn register_validator(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
    ) -> Result<()> {
        let radicle_node_id = keccak256(validator_addr.as_slice());
        let bonded = self.read_authoritative_bonded(&validator_addr)?;
        self.register_validator_inner(&super::registration::RegistrationRequest {
            caller,
            validator: validator_addr,
            consensus_pubkey,
            radicle_node_id,
            bls_signature: None,
            p2p: P2pInfo::Unset,
            bonded,
            minimum: U256::ZERO,
            allow_bootstrap_without_pop: true,
        })
    }

    /// Test-only compatibility helper ensuring the validator is in
    /// `WaitingForReadiness` (`PENDING`).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn mark_pending(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        if matches!(before.lifecycle(), ValidatorLifecycle::Absent) {
            return Err(ValidatorSetError::ValidatorNotRegistered.into());
        }
        Ok(())
    }

    /// Test-only fixture activation through the canonical join transitions.
    /// Production activation is system-boundary-only.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn activate_validator(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        let active = match before.lifecycle().clone() {
            ValidatorLifecycle::Active(_) => return Ok(()),
            ValidatorLifecycle::Joining(joining) => state_machine::activate_at_boundary(joining),
            ValidatorLifecycle::WaitingForReadiness(waiting) => {
                state_machine::activate_at_boundary(state_machine::confirm_ready(waiting))
            }
            ValidatorLifecycle::Absent => {
                return Err(ValidatorSetError::ValidatorNotRegistered.into());
            }
            lifecycle => {
                return Err(ValidatorSetError::CannotActivateValidatorInTestFixture {
                    status: registered_status(&lifecycle)?,
                }
                .into())
            }
        };
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::Active(active))?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        self.pending_set_change.write(true)?;
        crate::metrics::record_validator_status(addr, status::ACTIVE);
        crate::metrics::record_pending_set_change(true);
        self.emit(IValidatorSet::ValidatorActivated { validator: addr })?;
        guard.commit();
        Ok(())
    }

    /// Compatibility wrapper retained while callers migrate to the named
    /// Staking-checked transition.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn unjail_to_pending(&mut self, addr: Address) -> Result<()> {
        self.unjail_after_stake_check(addr)
    }

    /// Test-only compatibility entrypoint. Production activation is reachable
    /// exclusively through the consensus boundary hook.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn activate_reshared_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
    ) -> Result<()> {
        self.activate_validated_boundary_set(new_active_set, active_set_hash, u64::MAX)
    }

    /// Applies inputs already validated against the locally expected consensus
    /// boundary artifact. Snapshot/hash validation remains in the EVM boundary
    /// orchestrator. This method owns only the ValidatorSet state transition.
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn activate_validated_boundary_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        freeze_height: u64,
    ) -> Result<()> {
        self.activate_validated_boundary_set_with_expiry_exclusions(
            new_active_set,
            active_set_hash,
            freeze_height,
            &[],
        )
    }
}
