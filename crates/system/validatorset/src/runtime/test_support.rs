use super::{registered_status, status};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorLifecycle};
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};

impl ValidatorSet<'_> {
    /// Registers a new validator.
    ///
    /// The caller must be either the config owner or the validator address itself.
    /// The address must not already be registered, and the count must be below max.
    /// Initial state is `WaitingForStake` (`REGISTERED`); reaching the minimum,
    /// confirming readiness, and boundary activation are separate transitions.
    ///
    /// `consensus_pubkey` is a 48-byte BLS12-381 MinPk public key.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn register_validator(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
    ) -> Result<()> {
        let radicle_node_id = keccak256(validator_addr.as_slice());
        self.register_validator_inner(
            caller,
            validator_addr,
            consensus_pubkey,
            radicle_node_id,
            None,
            true,
        )
    }

    /// Test-only compatibility helper for moving `WaitingForStake` to
    /// `WaitingForReadiness`. Production Staking uses [`Self::record_stake_increase`]
    /// with the authoritative minimum.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn mark_pending(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        let waiting = match before.lifecycle().clone() {
            ValidatorLifecycle::WaitingForStake(waiting) => waiting,
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            _ => return Ok(()),
        };
        let stake = *before.stake().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing stake projection".into())
        })?;
        let lifecycle = ValidatorLifecycle::WaitingForReadiness(state_machine::reach_minimum(
            waiting,
            stake,
            U256::ZERO,
        )?);
        let after = before.clone().with_lifecycle(lifecycle)?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        // Signal consensus to include this validator in the next reshare target.
        self.pending_set_change.write(true)?;
        guard.commit();

        crate::metrics::record_validator_status(addr, status::PENDING);
        crate::metrics::record_pending_set_change(true);

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
            ValidatorLifecycle::WaitingForStake(waiting) => {
                let stake = *before.stake().ok_or_else(|| {
                    PrecompileError::Fatal(
                        "registered validator is missing stake projection".into(),
                    )
                })?;
                let ready = state_machine::reach_minimum(waiting, stake, U256::ZERO)?;
                state_machine::activate_at_boundary(state_machine::confirm_ready(ready))
            }
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "cannot activate validator with status {}: only REGISTERED or PENDING allowed in test fixtures",
                    registered_status(&lifecycle)?
                )))
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
    /// orchestrator; this method owns only the ValidatorSet state transition.
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
