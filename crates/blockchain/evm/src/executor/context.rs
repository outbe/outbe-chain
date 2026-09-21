use super::*;

pub(in crate::executor) fn build_block_context<DB>(
    db: &mut DB,
    block_number: u64,
    timestamp: u64,
    chain_id: u64,
    genesis_hash: B256,
    proposer: Address,
) -> Result<BlockContext, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let mut provider = DirectStorageProvider::new(
        db,
        BlockContext::new_with_genesis_hash(
            block_number,
            timestamp,
            chain_id,
            genesis_hash,
            proposer,
            Vec::new(),
        ),
    );
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
        BlockExecutionError::Internal(InternalBlockExecutionError::Other(
            format!("block context: {e}").into(),
        ))
    })?;

    Ok(BlockContext::new_with_genesis_hash(
        block_number,
        timestamp,
        chain_id,
        genesis_hash,
        proposer,
        validators,
    ))
}

pub(in crate::executor) fn validate_genesis_state(
    storage: StorageHandle,
    genesis: &GenesisValidators,
) -> OutbeResult<()> {
    let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    if !vs.config_is_initialized.read()? {
        return Err(PrecompileError::Fatal(
            "ValidatorSet must be initialized in genesis; executor genesis backfill is disabled"
                .into(),
        ));
    }

    let epoch_length_blocks = vs.config_epoch_length_blocks.read()?;
    if epoch_length_blocks != genesis.epoch_length_blocks {
        return Err(PrecompileError::Fatal(format!(
            "genesis ValidatorSet epoch_length_blocks mismatch: state={epoch_length_blocks}, genesis={}",
            genesis.epoch_length_blocks
        )));
    }

    let active_consensus_count = vs.active_consensus_count()?;
    if active_consensus_count as usize != genesis.validators.len() {
        return Err(PrecompileError::Fatal(format!(
            "genesis active consensus set size mismatch: state={active_consensus_count}, genesis validators={}",
            genesis.validators.len()
        )));
    }

    let staking = outbe_staking::contract::Staking::new(storage.clone());
    let min_stake = staking.config_min_stake.read()?;
    if min_stake.is_zero() {
        return Err(PrecompileError::Fatal(
            "Staking min_stake must be initialized in genesis".into(),
        ));
    }

    let mut expected_total = U256::ZERO;
    for validator in &genesis.validators {
        let state = vs.validator_state(validator.address)?;
        if !state.is_registered() {
            return Err(PrecompileError::Fatal(format!(
                "genesis validator {} is missing from ValidatorSet",
                validator.address
            )));
        }

        if state.consensus_pubkey().copied() != Some(validator.consensus_pubkey) {
            return Err(PrecompileError::Fatal(format!(
                "genesis validator {} consensus pubkey mismatch",
                validator.address
            )));
        }
        if !matches!(state.lifecycle(), ValidatorLifecycle::Active(_)) {
            return Err(PrecompileError::Fatal(format!(
                "genesis validator {} must be active with a BLS share",
                validator.address
            )));
        }
        let bonded_stake = state.bonded_stake();
        if bonded_stake < min_stake {
            return Err(PrecompileError::Fatal(format!(
                "genesis validator {} stake below min_stake",
                validator.address
            )));
        }

        let staking_amount = staking.stake_amount.read(&validator.address)?;
        if staking_amount != bonded_stake {
            return Err(PrecompileError::Fatal(format!(
                "genesis validator {} stake mismatch between ValidatorSet and Staking",
                validator.address
            )));
        }
        expected_total = expected_total
            .checked_add(staking_amount)
            .ok_or_else(|| PrecompileError::Fatal("genesis total stake overflow".into()))?;
    }

    let total_staked = staking.total_staked.read()?;
    if total_staked != expected_total {
        return Err(PrecompileError::Fatal(format!(
            "genesis total_staked mismatch: state={total_staked}, expected={expected_total}"
        )));
    }

    Ok(())
}
