use alloy_primitives::{B256, U256};
use outbe_primitives::addresses::SLASH_INDICATOR_ADDRESS;
use outbe_slashindicator::constants::{
    DEFAULT_EVIDENCE_REWARD_PERCENT, DEFAULT_SLASH_AMOUNT_PERCENT, EVIDENCE_REWARD_PERCENT_SLOT,
    SLASH_AMOUNT_PERCENT_SLOT,
};
use outbe_slashindicator::runtime::admit_percent_word;

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
    // Validator and follower both enter this function before `launch()`.
    validate_slash_indicator_genesis(config)?;
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

fn validate_slash_indicator_genesis(config: &LaunchConfig) -> std::result::Result<(), LaunchError> {
    validate_slash_indicator_genesis_words(
        genesis_percent_word(config, SLASH_AMOUNT_PERCENT_SLOT),
        genesis_percent_word(config, EVIDENCE_REWARD_PERCENT_SLOT),
    )
}

fn validate_slash_indicator_genesis_words(
    slash_word: Option<B256>,
    reward_word: Option<B256>,
) -> std::result::Result<(), LaunchError> {
    admit_genesis_percent(
        slash_word,
        DEFAULT_SLASH_AMOUNT_PERCENT,
        "slash_amount_percent",
    )?;
    admit_genesis_percent(
        reward_word,
        DEFAULT_EVIDENCE_REWARD_PERCENT,
        "evidence_reward_percent",
    )?;
    Ok(())
}

fn admit_genesis_percent(
    word: Option<B256>,
    default_percent: u64,
    field: &'static str,
) -> std::result::Result<(), LaunchError> {
    let word = word
        .map(|bytes| U256::from_be_bytes(bytes.0))
        .unwrap_or(U256::ZERO);
    admit_percent_word(word, default_percent, field)
        .map(|_| ())
        .map_err(|cause| LaunchError::InvalidSlashIndicatorGenesisPercent { cause })
}

fn genesis_percent_word(config: &LaunchConfig, slot: u64) -> Option<B256> {
    let key = percent_slot_key(slot);
    config
        .chain
        .genesis
        .alloc
        .get(&SLASH_INDICATOR_ADDRESS)
        .and_then(|account| account.storage.as_ref())
        .and_then(|storage| storage.get(&key))
        .copied()
}

fn percent_slot_key(slot: u64) -> B256 {
    B256::from(U256::from(slot).to_be_bytes::<32>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_slashindicator::errors::PercentAdmissionError;

    fn word(value: u64) -> B256 {
        B256::from(U256::from(value).to_be_bytes::<32>())
    }

    fn wide_word() -> B256 {
        B256::from(U256::MAX.to_be_bytes::<32>())
    }

    fn assert_above(err: LaunchError, field: &str, value: u64) {
        match err {
            LaunchError::InvalidSlashIndicatorGenesisPercent {
                cause:
                    PercentAdmissionError::Above100 {
                        field: got,
                        value: got_value,
                    },
            } => {
                assert_eq!(got, field);
                assert_eq!(got_value, value);
            }
            other => panic!("expected percent above 100, got {other:?}"),
        }
    }

    fn assert_wide(err: LaunchError, field: &str) {
        match err {
            LaunchError::InvalidSlashIndicatorGenesisPercent {
                cause: PercentAdmissionError::WordExceedsU64 { field: got, word },
            } => {
                assert_eq!(got, field);
                assert_eq!(word, U256::MAX);
            }
            other => panic!("expected wide percent word, got {other:?}"),
        }
    }

    #[test]
    fn slashindicator_genesis_percents_follow_shared_admission() {
        validate_slash_indicator_genesis_words(None, None).unwrap();
        validate_slash_indicator_genesis_words(Some(word(0)), Some(word(0))).unwrap();
        validate_slash_indicator_genesis_words(Some(word(100)), Some(word(100))).unwrap();

        assert_above(
            validate_slash_indicator_genesis_words(Some(word(101)), Some(word(100))).unwrap_err(),
            "slash_amount_percent",
            101,
        );
        assert_above(
            validate_slash_indicator_genesis_words(Some(word(8)), Some(word(101))).unwrap_err(),
            "evidence_reward_percent",
            101,
        );
        assert_above(
            validate_slash_indicator_genesis_words(Some(word(u64::MAX)), None).unwrap_err(),
            "slash_amount_percent",
            u64::MAX,
        );
        assert_above(
            validate_slash_indicator_genesis_words(None, Some(word(u64::MAX))).unwrap_err(),
            "evidence_reward_percent",
            u64::MAX,
        );
        assert_wide(
            validate_slash_indicator_genesis_words(Some(wide_word()), None).unwrap_err(),
            "slash_amount_percent",
        );
        assert_wide(
            validate_slash_indicator_genesis_words(None, Some(wide_word())).unwrap_err(),
            "evidence_reward_percent",
        );
    }

    #[test]
    fn slashindicator_genesis_storage_key_is_big_endian_slot() {
        assert_eq!(
            percent_slot_key(SLASH_AMOUNT_PERCENT_SLOT).0,
            U256::from(SLASH_AMOUNT_PERCENT_SLOT).to_be_bytes::<32>()
        );
        assert_eq!(
            percent_slot_key(EVIDENCE_REWARD_PERCENT_SLOT).0,
            U256::from(EVIDENCE_REWARD_PERCENT_SLOT).to_be_bytes::<32>()
        );
        let mut storage = std::collections::BTreeMap::new();
        storage.insert(percent_slot_key(SLASH_AMOUNT_PERCENT_SLOT), word(100));
        assert_eq!(
            storage.get(&percent_slot_key(EVIDENCE_REWARD_PERCENT_SLOT)),
            None
        );
    }
}
