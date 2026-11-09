//! Protocol facts the engine reads from chain state.
//!
//! Consensus startup asks this trait for validator sets, committee snapshots,
//! and TEE admission facts. The crate that owns those contracts implements the
//! trait and installs it before launch. Until that install, reads fail closed,
//! including the binary-compatibility check.
//! The process-wide `OnceLock` is a temporary bridge until constructor injection in the runtime composition root.

use std::sync::{Arc, OnceLock};

use alloy_primitives::{Address, B256};
use outbe_primitives::committee_snapshot::CommitteeSnapshot;
use outbe_primitives::error::Result as PrecompileResult;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1;
use outbe_primitives::validator_dto::ValidatorRecord;
use thiserror::Error;

static INSTALLED: OnceLock<Arc<dyn EngineProtocolState>> = OnceLock::new();

/// ABI status byte for a jailed validator.
pub(crate) const VALIDATOR_STATUS_JAILED: u8 = 6;

/// Which validator set a caller wants from chain state.
#[derive(Clone, Copy, Debug)]
pub enum EngineValidatorQuery {
    /// `status == ACTIVE` only.
    ActiveValidators,
    /// ACTIVE, EXITING, and jail-retained validators that still have shares.
    ConsensusParticipants,
    /// ACTIVE plus readiness-confirmed joining validators.
    ReshareTarget,
    /// Pending joiners admitted as secondary peers.
    PendingValidators,
    /// Pending and jailed validators admitted as secondary peers.
    AdmittedNonConsensus,
}

/// One registry row plus the raw P2P address read.
#[derive(Clone, Debug)]
pub struct EngineValidatorRow {
    /// Validator record from the set query.
    pub record: ValidatorRecord,
    /// P2P address registry outcome for that validator.
    pub p2p: EngineP2pRead,
}

/// Raw P2P address registry outcome. Decoding stays in the engine.
#[derive(Clone, Debug)]
pub enum EngineP2pRead {
    /// Encoded address at `version`.
    Present { version: u8, encoded: Vec<u8> },
    /// The validator has no P2P address entry.
    Missing,
    /// The registry read failed. `error` is the source display text.
    Failed { error: String },
}

/// Startup compatibility failure reported by the installed protocol state.
#[derive(Debug, Error)]
pub enum EngineProtocolError {
    /// The installed state rejected the running binary.
    #[error("{0}")]
    Compatibility(String),
}

/// The production state was already installed.
#[derive(Debug, Error)]
#[error("engine protocol state is already installed")]
pub struct EngineProtocolStateAlreadyInstalled;

/// Object-safe chain reads used by the engine.
pub trait EngineProtocolState: Send + Sync {
    /// Validator rows for `query`, including each row's raw P2P address.
    fn validator_rows(
        &self,
        storage: StorageHandle<'_>,
        query: EngineValidatorQuery,
    ) -> PrecompileResult<Vec<EngineValidatorRow>>;

    /// Committee snapshot committed for `epoch`, if one exists.
    fn committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        epoch: u64,
    ) -> PrecompileResult<Option<CommitteeSnapshot>>;

    /// Validator-set pending-change flag.
    fn has_pending_set_change(&self, storage: StorageHandle<'_>) -> PrecompileResult<bool>;

    /// Tribute offer public key. Zero before block-1 bootstrap.
    fn tee_offer_public(&self, storage: StorageHandle<'_>) -> PrecompileResult<B256>;

    /// Tribute-offer epoch.
    fn tee_offer_epoch(&self, storage: StorageHandle<'_>) -> PrecompileResult<u64>;

    /// Recipient X25519 for `validator`. `Ok(None)` means no NodeHost binding.
    fn tee_recipient(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> PrecompileResult<Option<B256>>;

    /// NodeHost binding for the 33-byte Reth P2P public key.
    fn node_host_binding(
        &self,
        storage: StorageHandle<'_>,
        reth_p2p_public: [u8; 33],
    ) -> PrecompileResult<Option<NodeEnclaveBindingV1>>;

    /// Whether `binding` is still admitted at `block_number`.
    fn binding_code_admitted(
        &self,
        storage: StorageHandle<'_>,
        binding: &NodeEnclaveBindingV1,
        block_number: u64,
    ) -> PrecompileResult<bool>;

    /// Validator-to-enclave binding. `Ok(None)` means no binding.
    fn validator_enclave_binding(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> PrecompileResult<Option<NodeEnclaveBindingV1>>;

    /// One validator record. `Ok(None)` means the address is not registered.
    fn validator_record(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> PrecompileResult<Option<ValidatorRecord>>;

    /// Startup check against the active protocol version and waiting updates.
    ///
    /// Fails closed when no protocol state is installed.
    fn check_binary_compatibility(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<(), EngineProtocolError>;
}

/// Install `state` for this process. The engine copies it on each read.
pub fn install_engine_protocol_state(
    state: Arc<dyn EngineProtocolState>,
) -> std::result::Result<(), EngineProtocolStateAlreadyInstalled> {
    INSTALLED
        .set(state)
        .map_err(|_| EngineProtocolStateAlreadyInstalled)
}

pub(crate) fn active_engine_protocol_state() -> Arc<dyn EngineProtocolState> {
    if let Some(installed) = INSTALLED.get() {
        return Arc::clone(installed);
    }
    #[cfg(test)]
    {
        Arc::new(StorageEngineState)
    }
    #[cfg(not(test))]
    {
        Arc::new(UninstalledEngineState)
    }
}

/// Fail-closed stand-in used when no protocol state is installed.
/// `check_binary_compatibility` returns [`EngineProtocolError::Compatibility`].
struct UninstalledEngineState;

fn not_installed<T>() -> PrecompileResult<T> {
    Err(outbe_primitives::error::PrecompileError::storage(
        "engine protocol state is not installed",
    ))
}

impl EngineProtocolState for UninstalledEngineState {
    fn validator_rows(
        &self,
        _storage: StorageHandle<'_>,
        _query: EngineValidatorQuery,
    ) -> PrecompileResult<Vec<EngineValidatorRow>> {
        not_installed()
    }

    fn committee_snapshot(
        &self,
        _storage: StorageHandle<'_>,
        _epoch: u64,
    ) -> PrecompileResult<Option<CommitteeSnapshot>> {
        not_installed()
    }

    fn has_pending_set_change(&self, _storage: StorageHandle<'_>) -> PrecompileResult<bool> {
        not_installed()
    }

    fn tee_offer_public(&self, _storage: StorageHandle<'_>) -> PrecompileResult<B256> {
        not_installed()
    }

    fn tee_offer_epoch(&self, _storage: StorageHandle<'_>) -> PrecompileResult<u64> {
        not_installed()
    }

    fn tee_recipient(
        &self,
        _storage: StorageHandle<'_>,
        _validator: Address,
    ) -> PrecompileResult<Option<B256>> {
        not_installed()
    }

    fn node_host_binding(
        &self,
        _storage: StorageHandle<'_>,
        _reth_p2p_public: [u8; 33],
    ) -> PrecompileResult<Option<NodeEnclaveBindingV1>> {
        not_installed()
    }

    fn binding_code_admitted(
        &self,
        _storage: StorageHandle<'_>,
        _binding: &NodeEnclaveBindingV1,
        _block_number: u64,
    ) -> PrecompileResult<bool> {
        not_installed()
    }

    fn validator_enclave_binding(
        &self,
        _storage: StorageHandle<'_>,
        _validator: Address,
    ) -> PrecompileResult<Option<NodeEnclaveBindingV1>> {
        not_installed()
    }

    fn validator_record(
        &self,
        _storage: StorageHandle<'_>,
        _validator: Address,
    ) -> PrecompileResult<Option<ValidatorRecord>> {
        not_installed()
    }

    fn check_binary_compatibility(
        &self,
        _storage: StorageHandle<'_>,
    ) -> std::result::Result<(), EngineProtocolError> {
        Err(EngineProtocolError::Compatibility(
            "engine protocol state is not installed".to_string(),
        ))
    }
}

#[cfg(test)]
struct StorageEngineState;

#[cfg(test)]
fn rows_for(
    storage: StorageHandle<'_>,
    query: EngineValidatorQuery,
) -> PrecompileResult<Vec<EngineValidatorRow>> {
    let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
    let records = match query {
        EngineValidatorQuery::ActiveValidators => vs.get_active_validators()?,
        EngineValidatorQuery::ConsensusParticipants => vs.get_active_consensus_set()?,
        EngineValidatorQuery::ReshareTarget => vs.get_reshare_target_set()?,
        EngineValidatorQuery::PendingValidators => vs.get_pending_validators()?,
        EngineValidatorQuery::AdmittedNonConsensus => vs.get_admitted_non_consensus_validators()?,
    };
    let mut rows = Vec::with_capacity(records.len());
    for record in records {
        let p2p = match vs.get_p2p_address(record.validator_address) {
            Ok(Some((version, encoded))) => EngineP2pRead::Present { version, encoded },
            Ok(None) => EngineP2pRead::Missing,
            Err(error) => EngineP2pRead::Failed {
                error: error.to_string(),
            },
        };
        rows.push(EngineValidatorRow { record, p2p });
    }
    Ok(rows)
}

#[cfg(test)]
impl EngineProtocolState for StorageEngineState {
    fn validator_rows(
        &self,
        storage: StorageHandle<'_>,
        query: EngineValidatorQuery,
    ) -> PrecompileResult<Vec<EngineValidatorRow>> {
        rows_for(storage, query)
    }

    fn committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        epoch: u64,
    ) -> PrecompileResult<Option<CommitteeSnapshot>> {
        outbe_validatorset::state::read_committee_snapshot_for_epoch(storage, epoch)
    }

    fn has_pending_set_change(&self, storage: StorageHandle<'_>) -> PrecompileResult<bool> {
        outbe_validatorset::contract::ValidatorSet::new(storage).has_pending_set_change()
    }

    fn tee_offer_public(&self, storage: StorageHandle<'_>) -> PrecompileResult<B256> {
        outbe_teeregistry::TeeRegistry::new(storage).offer_public_key()
    }

    fn tee_offer_epoch(&self, storage: StorageHandle<'_>) -> PrecompileResult<u64> {
        outbe_teeregistry::TeeRegistry::new(storage).tribute_offer_epoch()
    }

    fn tee_recipient(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> PrecompileResult<Option<B256>> {
        Ok(outbe_teeregistry::TeeRegistry::new(storage)
            .validator_enclave_binding_v1(validator)?
            .map(|binding| binding.recipient_x25519))
    }

    fn node_host_binding(
        &self,
        storage: StorageHandle<'_>,
        reth_p2p_public: [u8; 33],
    ) -> PrecompileResult<Option<NodeEnclaveBindingV1>> {
        outbe_teeregistry::TeeRegistry::new(storage).node_host_enclave_binding_v1(reth_p2p_public)
    }

    fn binding_code_admitted(
        &self,
        storage: StorageHandle<'_>,
        binding: &NodeEnclaveBindingV1,
        block_number: u64,
    ) -> PrecompileResult<bool> {
        outbe_teeregistry::TeeRegistry::new(storage)
            .binding_code_admitted_at_v1(binding, block_number)
    }

    fn validator_enclave_binding(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> PrecompileResult<Option<NodeEnclaveBindingV1>> {
        outbe_teeregistry::TeeRegistry::new(storage).validator_enclave_binding_v1(validator)
    }

    fn validator_record(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> PrecompileResult<Option<ValidatorRecord>> {
        outbe_validatorset::contract::ValidatorSet::new(storage).get_validator(validator)
    }

    fn check_binary_compatibility(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<(), EngineProtocolError> {
        let update = outbe_update::schema::Update::new(storage);
        let active_version = update
            .get_active_version()
            .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?;
        outbe_update::startup::assert_binary_protocol_compatible(active_version)
            .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?;
        let mut waiting = Vec::new();
        for proposal_id in update
            .list_waiting_for_activation_proposal_ids()
            .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?
        {
            if let Some(record) = update
                .read_scheduled_update(proposal_id)
                .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?
            {
                waiting.push(record);
            }
        }
        outbe_update::startup::warn_missing_handlers_for_waiting_updates(
            &waiting,
            outbe_update::handlers::registry(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    use super::EngineProtocolState;

    #[test]
    fn jailed_status_byte_matches_validator_set() {
        assert_eq!(
            super::VALIDATOR_STATUS_JAILED,
            outbe_validatorset::runtime::status::JAILED
        );
    }

    #[test]
    fn uninstalled_binary_check_fails_closed() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let error = super::UninstalledEngineState
            .check_binary_compatibility(storage)
            .expect_err("uninstalled binary check must fail closed");
        match error {
            super::EngineProtocolError::Compatibility(message) => {
                assert_eq!(message, "engine protocol state is not installed");
            }
        }
    }
}
