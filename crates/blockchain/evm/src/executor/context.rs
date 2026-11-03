use super::*;

impl<E> OutbeBlockExecutor<'_, E> {
    pub(in crate::executor) fn block_context(
        &self,
        block_number: u64,
        timestamp: u64,
        proposer: Address,
    ) -> BlockContext {
        BlockContext {
            block_number,
            timestamp,
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            genesis_timestamp: self.genesis_timestamp,
            proposer,
            validators: Vec::new(),
        }
    }
}

pub(in crate::executor) fn read_committee_snapshot<DB>(
    db: &mut DB,
    context: BlockContext,
    snapshot_key: B256,
    validation_error: impl FnOnce(PrecompileError) -> crate::errors::ExecutorValidationError,
) -> Result<Option<outbe_validatorset::state::CommitteeSnapshot>, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let mut provider = DirectStorageProvider::new(db, context);
    let storage = StorageHandle::new(&mut provider);
    outbe_validatorset::state::read_committee_snapshot(storage, snapshot_key).map_err(|error| {
        if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
            BlockExecutionError::other(error)
        } else {
            validation_error(error).into()
        }
    })
}

pub(in crate::executor) fn build_block_context<DB>(
    db: &mut DB,
    mut context: BlockContext,
) -> Result<BlockContext, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let mut provider = DirectStorageProvider::new(db, context.clone());
    let storage = StorageHandle::new(&mut provider);
    let validators = (|| -> outbe_primitives::error::Result<Vec<Address>> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let mut validators: Vec<Address> = vs
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        validators.sort();
        Ok(validators)
    })()
    .map_err(|e| {
        if e.is_node_local() || matches!(e, PrecompileError::Fatal(_)) {
            BlockExecutionError::other(e)
        } else {
            crate::errors::ExecutorValidationError::block_context(e).into()
        }
    })?;

    context.validators = validators;
    Ok(context)
}

pub(in crate::executor) fn validate_genesis_state(
    storage: StorageHandle,
    genesis: &GenesisValidators,
) -> OutbeResult<()> {
    use crate::errors::ExecutorHookError;

    let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    validate_genesis_configuration(&vs, genesis)?;

    let staking = outbe_staking::contract::Staking::new(storage.clone());
    let min_stake = staking.min_stake()?;

    let mut expected_total = U256::ZERO;
    for validator in &genesis.validators {
        let state = vs.validator_state(validator.address)?;
        validate_genesis_member(&state, validator, min_stake)?;

        let staking_amount = staking.stake_amount.read(&validator.address)?;
        expected_total = expected_total
            .checked_add(staking_amount)
            .ok_or(ExecutorHookError::GenesisTotalStakeOverflow)?;
    }

    let total_staked = staking.total_staked.read()?;
    if total_staked != expected_total {
        return Err(ExecutorHookError::GenesisTotalStakedMismatch {
            state: total_staked,
            expected: expected_total,
        }
        .into());
    }

    Ok(())
}

fn validate_genesis_configuration(
    vs: &outbe_validatorset::contract::ValidatorSet<'_>,
    genesis: &GenesisValidators,
) -> OutbeResult<()> {
    use crate::errors::ExecutorHookError;

    if !vs.config_is_initialized.read()? {
        return Err(ExecutorHookError::ValidatorSetUninitializedInGenesis.into());
    }

    let epoch_length_blocks = vs.config_epoch_length_blocks.read()?;
    if epoch_length_blocks != genesis.epoch_length_blocks {
        return Err(ExecutorHookError::GenesisEpochLengthMismatch {
            state: epoch_length_blocks,
            genesis: genesis.epoch_length_blocks,
        }
        .into());
    }

    let active_consensus_count = vs.active_consensus_count()?;
    if active_consensus_count as usize != genesis.validators.len() {
        return Err(ExecutorHookError::GenesisActiveConsensusCountMismatch {
            state: active_consensus_count,
            genesis: genesis.validators.len(),
        }
        .into());
    }

    Ok(())
}

fn validate_genesis_member(
    state: &outbe_validatorset::ValidatorState,
    validator: &outbe_primitives::consensus::GenesisValidator,
    min_stake: U256,
) -> OutbeResult<U256> {
    use crate::errors::ExecutorHookError;

    if !state.is_registered() {
        return Err(ExecutorHookError::MissingGenesisValidator {
            address: validator.address,
        }
        .into());
    }

    if state.consensus_pubkey().copied() != Some(validator.consensus_pubkey) {
        return Err(ExecutorHookError::GenesisConsensusPubkeyMismatch {
            address: validator.address,
        }
        .into());
    }
    if !matches!(state.lifecycle(), ValidatorLifecycle::Active(_)) {
        return Err(ExecutorHookError::GenesisValidatorNotActive {
            address: validator.address,
        }
        .into());
    }
    let bonded_stake = state.bonded_stake();
    if bonded_stake < min_stake {
        return Err(ExecutorHookError::GenesisValidatorStakeBelowMinimum {
            address: validator.address,
        }
        .into());
    }

    Ok(bonded_stake)
}
