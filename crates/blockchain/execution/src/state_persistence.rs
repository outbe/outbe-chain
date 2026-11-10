//! Startup invariants required by the native finalized execution.

use thiserror::Error;

/// Reth/Marshal settings that determine whether a finalized block can be
/// committed only after a real per-block execution persistence barrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatePersistenceRuntimeConfig {
    pub persistence_threshold: u64,
    pub memory_block_buffer_target: u64,
    pub max_pending_acks: usize,
    pub receipts_pruning_enabled: bool,
    pub account_history_pruning_enabled: bool,
    pub storage_history_pruning_enabled: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StatePersistenceConfigError {
    #[error("state persistence requires --engine.persistence-threshold=0, got {actual}")]
    PersistenceThreshold { actual: u64 },
    #[error("state persistence requires --engine.memory-block-buffer-target=0, got {actual}")]
    MemoryBlockBufferTarget { actual: u64 },
    #[error("state persistence requires consensus MAX_PENDING_ACKS=1, got {actual}")]
    PendingAcks { actual: usize },
    #[error("state persistence recovery requires receipt pruning to be disabled")]
    ReceiptsPruningEnabled,
    #[error("state persistence recovery requires account-history pruning to be disabled")]
    AccountHistoryPruningEnabled,
    #[error("state persistence recovery requires storage-history pruning to be disabled")]
    StorageHistoryPruningEnabled,
}

/// Rejects configurations that could acknowledge finalization before durable Reth state has reached the exact finalized block.
pub fn validate_state_persistence_runtime_config(
    config: StatePersistenceRuntimeConfig,
) -> Result<(), StatePersistenceConfigError> {
    validate_per_block_persistence(config)?;
    validate_recovery_pruning(config)
}

fn validate_per_block_persistence(
    config: StatePersistenceRuntimeConfig,
) -> Result<(), StatePersistenceConfigError> {
    if config.persistence_threshold != 0 {
        return Err(StatePersistenceConfigError::PersistenceThreshold {
            actual: config.persistence_threshold,
        });
    }
    if config.memory_block_buffer_target != 0 {
        return Err(StatePersistenceConfigError::MemoryBlockBufferTarget {
            actual: config.memory_block_buffer_target,
        });
    }
    if config.max_pending_acks != 1 {
        return Err(StatePersistenceConfigError::PendingAcks {
            actual: config.max_pending_acks,
        });
    }
    Ok(())
}

fn validate_recovery_pruning(
    config: StatePersistenceRuntimeConfig,
) -> Result<(), StatePersistenceConfigError> {
    if config.receipts_pruning_enabled {
        return Err(StatePersistenceConfigError::ReceiptsPruningEnabled);
    }
    if config.account_history_pruning_enabled {
        return Err(StatePersistenceConfigError::AccountHistoryPruningEnabled);
    }
    if config.storage_history_pruning_enabled {
        return Err(StatePersistenceConfigError::StorageHistoryPruningEnabled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> StatePersistenceRuntimeConfig {
        StatePersistenceRuntimeConfig {
            persistence_threshold: 0,
            memory_block_buffer_target: 0,
            max_pending_acks: 1,
            receipts_pruning_enabled: false,
            account_history_pruning_enabled: false,
            storage_history_pruning_enabled: false,
        }
    }

    #[test]
    fn exact_per_block_barrier_without_pruning_is_accepted() {
        validate_state_persistence_runtime_config(valid()).unwrap();
    }

    #[test]
    fn immediate_persistence_disables_reth_state_masking() {
        // Reth 2.6 enables partial persistence by default. The settings
        // accepted by our native persistence barrier must still persist the full state.
        let engine = reth_node_core::args::EngineArgs {
            persistence_threshold: 0,
            memory_block_buffer_target: Some(0),
            num_state_masking_blocks: 30,
            ..Default::default()
        };
        let mut config = valid();
        config.persistence_threshold = engine.persistence_threshold;
        config.memory_block_buffer_target = engine.memory_block_buffer_target();
        validate_state_persistence_runtime_config(config).unwrap();

        let tree = engine.tree_config();
        assert_eq!(tree.persistence_threshold(), 0);
        assert_eq!(tree.memory_block_buffer_target(), 0);
        assert_eq!(tree.num_state_masking_blocks(), 0);
    }

    #[test]
    fn every_incompatible_durability_setting_fails_startup() {
        let mut cases = Vec::new();

        let mut persistence = valid();
        persistence.persistence_threshold = 1;
        cases.push((
            persistence,
            StatePersistenceConfigError::PersistenceThreshold { actual: 1 },
        ));

        let mut memory = valid();
        memory.memory_block_buffer_target = 1;
        cases.push((
            memory,
            StatePersistenceConfigError::MemoryBlockBufferTarget { actual: 1 },
        ));

        let mut pending = valid();
        pending.max_pending_acks = 2;
        cases.push((
            pending,
            StatePersistenceConfigError::PendingAcks { actual: 2 },
        ));

        let mut receipts = valid();
        receipts.receipts_pruning_enabled = true;
        cases.push((
            receipts,
            StatePersistenceConfigError::ReceiptsPruningEnabled,
        ));

        let mut accounts = valid();
        accounts.account_history_pruning_enabled = true;
        cases.push((
            accounts,
            StatePersistenceConfigError::AccountHistoryPruningEnabled,
        ));

        let mut storage = valid();
        storage.storage_history_pruning_enabled = true;
        cases.push((
            storage,
            StatePersistenceConfigError::StorageHistoryPruningEnabled,
        ));

        for (config, expected) in cases {
            assert_eq!(
                validate_state_persistence_runtime_config(config),
                Err(expected)
            );
        }
    }
}
