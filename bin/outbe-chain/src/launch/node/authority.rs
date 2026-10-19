use super::*;
use crate::errors::LaunchError;

pub(super) struct StartupChainAuthority {
    pub initial_tee_policy: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    pub dkg_prepare_window_blocks: u64,
    pub minimum_block_time_millis: u64,
}

pub(super) fn validate_startup_authority_and_pruning(
    config: &LaunchConfig,
) -> eyre::Result<StartupChainAuthority> {
    let tee_attestation_v1 =
        outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
            config.chain.as_ref(),
        );
    let tee_activation = tee_attestation_v1.activation().map_err(|error| {
        LaunchError::InvalidMandatoryTeeChainSpec {
            details: crate::errors::display_error(error),
        }
    })?;
    let initial_tee_policy = tee_activation
        .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)?;
    let extra = &config.chain.genesis.config.extra_fields;
    let dkg_prepare_window_blocks = extra
        .get_deserialized::<u64>("dkgPrepareWindowBlocks")
        .transpose()
        .map_err(|error| LaunchError::InvalidDkgPrepareWindowBlocks {
            details: crate::errors::display_error(error),
        })?
        .unwrap_or(outbe_consensus::config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS);
    let minimum_block_time_millis = extra
        .get_deserialized::<u64>("minBlockTimeMs")
        .transpose()
        .map_err(|error| LaunchError::InvalidMinBlockTimeMs {
            details: crate::errors::display_error(error),
        })?
        .unwrap_or(outbe_consensus::timing::DEFAULT_MIN_BLOCK_TIME_MS);
    info!(
        attestation_mode = ?initial_tee_policy.attestation_mode,
        activation_height = tee_activation.manifest.activation_height,
        policy_schedule_hash = %tee_activation.manifest.policy_schedule_hash,
        "validated mandatory TEE attestation ChainSpec authority"
    );
    let prune_config = config.pruning.prune_config(config.chain.as_ref());
    validate_state_persistence_runtime_config(StatePersistenceRuntimeConfig {
        persistence_threshold: config.engine.persistence_threshold,
        memory_block_buffer_target: config.engine.memory_block_buffer_target(),
        max_pending_acks: outbe_consensus::config::MAX_PENDING_ACKS,
        receipts_pruning_enabled: prune_config
            .as_ref()
            .is_some_and(|cfg| cfg.has_receipts_pruning()),
        account_history_pruning_enabled: prune_config
            .as_ref()
            .is_some_and(|cfg| cfg.segments.account_history.is_some()),
        storage_history_pruning_enabled: prune_config
            .as_ref()
            .is_some_and(|cfg| cfg.segments.storage_history.is_some()),
    })?;
    Ok(StartupChainAuthority {
        initial_tee_policy: initial_tee_policy.clone(),
        dkg_prepare_window_blocks,
        minimum_block_time_millis,
    })
}
