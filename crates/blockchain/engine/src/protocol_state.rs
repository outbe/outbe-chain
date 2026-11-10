//! Protocol facts the engine reads from chain state.
//!
//! Consensus startup asks this trait for validator sets, committee snapshots,
//! and TEE admission facts. Callers pass the installed state. A missing
//! installation fails closed, including the binary-compatibility check.

use alloy_primitives::{Address, B256};
use outbe_primitives::committee_snapshot::CommitteeSnapshot;
use outbe_primitives::error::{PrecompileError, Result as PrecompileResult};
use outbe_primitives::protocol_version::{format_protocol_version, ProtocolVersion};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1;
use outbe_primitives::validator_dto::ValidatorRecord;
use thiserror::Error;

/// ABI status byte for a jailed validator.
pub const VALIDATOR_STATUS_JAILED: u8 = 6;

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
#[derive(Debug)]
pub struct EngineValidatorRow {
    /// Validator record from the set query.
    pub record: ValidatorRecord,
    /// P2P address registry outcome for that validator.
    pub p2p: EngineP2pRead,
}

/// Raw P2P address registry outcome. Decoding stays in the engine.
#[derive(Debug)]
pub enum EngineP2pRead {
    /// Encoded address at `version`.
    Present { version: u8, encoded: Vec<u8> },
    /// The validator has no P2P address entry.
    Missing,
    /// The registry read failed.
    Failed { error: PrecompileError },
}

/// Startup compatibility failure reported by the installed protocol state.
#[derive(Debug, Error)]
pub enum EngineProtocolError {
    /// Storage or precompile read failed while checking compatibility.
    #[error("{0}")]
    State(#[from] PrecompileError),
    /// The running binary is older than the on-chain active protocol version.
    #[error(
        "binary protocol version {} is older than on-chain active protocol version {}; please upgrade the binary",
        format_protocol_version(*.binary),
        format_protocol_version(*.active)
    )]
    BinaryOlderThanActive {
        /// Running binary protocol version.
        binary: ProtocolVersion,
        /// Active on-chain protocol version.
        active: ProtocolVersion,
    },
    /// No engine protocol state is installed.
    #[error("engine protocol state is not installed")]
    NotInstalled,
}

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
