use super::registered_status;
use crate::errors::ValidatorSetError;
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorLifecycle};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

impl ValidatorSet<'_> {
    /// Confirms that a pending validator can enter the next DKG target set.
    pub fn confirm_validator_ready(&mut self, caller: Address) -> Result<()> {
        let before = self.validator_state(caller)?;
        let minimum = self.read_authoritative_min_stake()?;
        let current = before.lifecycle().clone();
        let confirmed = matches!(current, ValidatorLifecycle::WaitingForReadiness(_));
        let lifecycle = ready_lifecycle(current, before.bonded_stake(), minimum)?;
        let guard = self.storage.checkpoint_guard()?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        self.persist_validator_state_delta(&before, &after)?;
        self.pending_set_change.write(true)?;
        if confirmed {
            self.emit(IValidatorSet::ValidatorReadyConfirmed { validator: caller })?;
        }
        guard.commit()?;
        crate::metrics::record_pending_set_change(true);
        Ok(())
    }
}

fn ready_lifecycle(
    lifecycle: ValidatorLifecycle,
    bonded: U256,
    minimum: U256,
) -> Result<ValidatorLifecycle> {
    match lifecycle {
        ValidatorLifecycle::WaitingForReadiness(waiting) => {
            if bonded < minimum {
                return Err(ValidatorSetError::BondedStakeBelowMinimum { bonded, minimum }.into());
            }
            Ok(ValidatorLifecycle::Joining(state_machine::confirm_ready(
                waiting,
            )))
        }
        ValidatorLifecycle::Joining(joining) => {
            if bonded < minimum {
                return Err(ValidatorSetError::BondedStakeBelowMinimum { bonded, minimum }.into());
            }
            Ok(ValidatorLifecycle::Joining(joining))
        }
        ValidatorLifecycle::Absent => Err(ValidatorSetError::ValidatorNotRegistered.into()),
        lifecycle => Err(ValidatorSetError::ConfirmValidatorReadyRequiresPending {
            status: registered_status(&lifecycle)?,
        }
        .into()),
    }
}
