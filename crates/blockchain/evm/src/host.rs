//! Runtime host seam. Callers pass the host they were constructed with.

#[cfg(any(test, feature = "test-utils"))]
use std::sync::Arc;

use crate::errors::TeeBootstrapHostError;
use crate::precompiles::MetadosisMutationCall;
use crate::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use alloy_primitives::{Address, Bytes, B256, U256};
#[cfg(any(test, feature = "test-utils"))]
use outbe_primitives::error::PrecompileError;
#[cfg(any(test, feature = "test-utils"))]
use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    committee_snapshot::CommitteeSnapshot,
    consensus::{DkgBoundaryArtifact, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result,
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::{MetadosisMutationEntitlements, StorageHandle},
    system_tx_context::SystemTxExecutionContext,
    tee_attestation_v1::TeePolicyV1,
    tee_bootstrap_v2::TeeBootstrapV2,
    zero_fee_types::{
        BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView,
        SponsorshipAuthorization, ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeePolicyError,
        ZeroFeeTransaction,
    },
};

#[cfg(any(test, feature = "test-utils"))]
const HOST_MISSING: &str = "evm runtime host is not installed";

#[cfg(any(test, feature = "test-utils"))]
fn missing() -> PrecompileError {
    PrecompileError::Fatal(HOST_MISSING.into())
}

/// Fail-closed host for tests that do not execute precompiles.
#[cfg(any(test, feature = "test-utils"))]
pub fn stub_host() -> Arc<dyn EvmRuntimeHost> {
    Arc::new(FailClosedHost)
}

/// Classifies and authorizes user-transaction fee waivers and sponsorship.
pub trait UserTxFeePolicy: std::fmt::Debug + Send + Sync {
    /// `Ok(Some(_))` asks this hook for a waiver. `Ok(None)` uses normal fees.
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError>;

    /// `Some` means the envelope is the one-time bootstrap.
    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate>;

    /// Rechecks the pre-state account for a bootstrap candidate.
    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool;

    /// Reads the daily sponsorship quota. It does not write it.
    fn authorize_sponsorship(
        &self,
        storage: StorageHandle<'_>,
        signer: Address,
        block_timestamp_secs: u64,
    ) -> std::result::Result<SponsorshipAuthorization, ZeroFeePolicyError>;

    /// Consumes an authorization after the transaction is included.
    fn record_sponsorship_use(
        &self,
        storage: StorageHandle<'_>,
        authorization: SponsorshipAuthorization,
    ) -> std::result::Result<u32, ZeroFeePolicyError>;

    /// `true` means this transaction would exceed the block sponsorship budget.
    fn sponsored_gas_exceeds_block_budget(&self, used: u64, gas_limit: u64) -> bool;

    /// Stateful authorization for a candidate returned by [`Self::classify`].
    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError>;

    /// `Err` means the transaction is not a sponsorship request.
    fn classify_sponsorship(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<(), ZeroFeePolicyError>;
}

/// Domain operations the EVM calls without naming a domain crate.
pub trait EvmRuntimeHost: std::fmt::Debug + Send + Sync {
    /// EIP-161 marker addresses preserved at the start of every block.
    fn marker_addresses(&self) -> &'static [Address];
    /// Whether an exact route may be credited native value.
    fn precompile_is_payable(&self, address: &Address) -> bool;
    /// Base gas charged before the precompile runs.
    fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64;
    /// Dispatches one precompile call.
    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes>;
    /// Dispatches one system transaction.
    fn dispatch_system_tx(
        &self,
        storage: StorageHandle<'_>,
        data: &[u8],
        caller: Address,
        value: U256,
        tee: &TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes>;
    /// Checks committee shape, registration, missed proposers, and the signer bitmap.
    fn validate_finalized_metadata(
        &self,
        storage: StorageHandle<'_>,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<()>;
    /// Applies parent finalization, fees, and proposer slashing.
    fn run_finalization_and_slashing(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        metadata: &CertifiedParentAccountingMetadata,
        execution: &SystemTxExecutionContext,
    ) -> Result<()>;
    /// Checks a late credit against the escrow stored for its finalized block.
    fn authenticate_late_credit(
        &self,
        storage: &StorageHandle<'_>,
        credit: &PerBlockCredit,
    ) -> Result<()>;
    /// Records late voters and settles the matured reward window.
    fn run_late_finalize_credits(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        artifact: &LateFinalizeCreditsArtifact,
        execution: Option<&SystemTxExecutionContext>,
    ) -> Result<()>;
    /// Applies a certified boundary and writes the activated committee snapshot.
    fn apply_boundary_outcome(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()>;
    /// Resets outgoing-epoch counters when this block advances the epoch.
    fn prepare_boundary(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
    ) -> Result<()>;
    /// Active consensus validators, sorted by address.
    fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>>;
    /// Whether `proposer` is in the active consensus set.
    fn is_consensus_participant(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<bool>;
    /// Whether `proposer` is a registered validator.
    fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool>;
    /// Reads a committee snapshot, or `Ok(None)` when the key is absent.
    fn read_committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        key: B256,
    ) -> Result<Option<CommitteeSnapshot>>;
    /// Validates genesis staking state outside steady state.
    fn validate_genesis_state_for_regime(
        &self,
        regime: BlockHeightRegime,
        storage: StorageHandle<'_>,
        genesis_validators: Option<&GenesisValidators>,
    ) -> Result<()>;
    /// Verifies canonical genesis storage against `genesis`.
    fn validate_genesis_state(
        &self,
        storage: StorageHandle<'_>,
        genesis: &GenesisValidators,
    ) -> Result<()>;
    /// Ed25519 attestation key bound to `proposer`, when one is stored.
    fn proposer_attestation(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>>;
    /// Preflights a block-1 bootstrap payload against the resolved policy.
    fn validate_tee_bootstrap_for_build(
        &self,
        payload: &TeeBootstrapV2,
        expected_policy: &TeePolicyV1,
    ) -> std::result::Result<(), TeeBootstrapHostError>;
    /// Metadosis entitlements for one precompile call.
    fn mutation_entitlements(
        &self,
        call: MetadosisMutationCall<'_>,
    ) -> MetadosisMutationEntitlements;
    /// True when `data` is a `CycleTick` from the system caller.
    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool;
    /// Active UTC day derived from genesis and the block timestamp.
    fn derive_active_utc_day(
        &self,
        storage: StorageHandle<'_>,
        genesis_timestamp: u64,
        block_timestamp: u64,
    ) -> Result<u32>;
    /// Settles this block's burned fees.
    fn settle_block_burned_fees(&self, storage: StorageHandle<'_>, amount: U256) -> Result<()>;
    /// Fee policy stored by this host.
    fn user_fee_policy(&self) -> &dyn UserTxFeePolicy;
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
struct FailClosedFeePolicy;

#[cfg(any(test, feature = "test-utils"))]
impl UserTxFeePolicy for FailClosedFeePolicy {
    fn classify(
        &self,
        _tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        Ok(None)
    }

    fn classify_bootstrap(&self, _tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        None
    }

    fn authorize_bootstrap(
        &self,
        _candidate: BootstrapCandidate,
        _account: BootstrapAccountView,
    ) -> bool {
        false
    }

    fn authorize_sponsorship(
        &self,
        _storage: StorageHandle<'_>,
        _signer: Address,
        _block_timestamp_secs: u64,
    ) -> std::result::Result<SponsorshipAuthorization, ZeroFeePolicyError> {
        Err(missing().into())
    }

    fn record_sponsorship_use(
        &self,
        _storage: StorageHandle<'_>,
        _authorization: SponsorshipAuthorization,
    ) -> std::result::Result<u32, ZeroFeePolicyError> {
        Err(missing().into())
    }

    fn sponsored_gas_exceeds_block_budget(&self, _used: u64, _gas_limit: u64) -> bool {
        true
    }

    fn authorize_fee_waiver(
        &self,
        _storage: StorageHandle<'_>,
        _candidate: ZeroFeeCandidate,
    ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        Err(missing().into())
    }

    fn classify_sponsorship(
        &self,
        _tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<(), ZeroFeePolicyError> {
        Err(missing().into())
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
struct FailClosedHost;

#[cfg(any(test, feature = "test-utils"))]
impl EvmRuntimeHost for FailClosedHost {
    fn marker_addresses(&self) -> &'static [Address] {
        &crate::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES
    }

    fn precompile_is_payable(&self, address: &Address) -> bool {
        use outbe_primitives::addresses::{
            CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, CROSSCHAIN_BRIDGE_ADDRESS,
            INTEX_FACTORY_ADDRESS, STAKING_ADDRESS, VOTE_ADDRESS,
        };
        const PAYABLE: &[Address] = &[
            STAKING_ADDRESS,
            INTEX_FACTORY_ADDRESS,
            VOTE_ADDRESS,
            CREDIS_FACTORY_ADDRESS,
            CCA_REGISTRY_ADDRESS,
            CROSSCHAIN_BRIDGE_ADDRESS,
        ];
        PAYABLE.contains(address)
    }

    fn precompile_base_gas(&self, _address: &Address, _input: &[u8]) -> u64 {
        PRECOMPILE_BASE_GAS
    }

    fn dispatch_precompile(
        &self,
        _storage: StorageHandle<'_>,
        _address: &Address,
        _data: &[u8],
        _caller: Address,
        _value: U256,
    ) -> Result<Bytes> {
        Err(missing())
    }

    fn dispatch_system_tx(
        &self,
        _storage: StorageHandle<'_>,
        _data: &[u8],
        _caller: Address,
        _value: U256,
        _tee: &TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes> {
        Err(missing())
    }

    fn validate_finalized_metadata(
        &self,
        _storage: StorageHandle<'_>,
        _metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<()> {
        Err(missing())
    }

    fn run_finalization_and_slashing(
        &self,
        _ctx: &BlockRuntimeContext<'_>,
        _metadata: &CertifiedParentAccountingMetadata,
        _execution: &SystemTxExecutionContext,
    ) -> Result<()> {
        Err(missing())
    }

    fn authenticate_late_credit(
        &self,
        _storage: &StorageHandle<'_>,
        _credit: &PerBlockCredit,
    ) -> Result<()> {
        Err(missing())
    }

    fn run_late_finalize_credits(
        &self,
        _ctx: &BlockRuntimeContext<'_>,
        _artifact: &LateFinalizeCreditsArtifact,
        _execution: Option<&SystemTxExecutionContext>,
    ) -> Result<()> {
        Err(missing())
    }

    fn apply_boundary_outcome(
        &self,
        _storage: StorageHandle<'_>,
        _boundary: &DkgBoundaryArtifact,
        _block_number: u64,
        _timestamp: u64,
    ) -> Result<()> {
        Err(missing())
    }

    fn prepare_boundary(
        &self,
        _storage: StorageHandle<'_>,
        _boundary: &DkgBoundaryArtifact,
        _block_number: u64,
    ) -> Result<()> {
        Err(missing())
    }

    fn consensus_validators(&self, _storage: StorageHandle<'_>) -> Result<Vec<Address>> {
        Err(missing())
    }

    fn is_consensus_participant(
        &self,
        _storage: StorageHandle<'_>,
        _proposer: Address,
    ) -> Result<bool> {
        Err(missing())
    }

    fn is_validator(&self, _storage: StorageHandle<'_>, _proposer: Address) -> Result<bool> {
        Err(missing())
    }

    fn read_committee_snapshot(
        &self,
        _storage: StorageHandle<'_>,
        _key: B256,
    ) -> Result<Option<CommitteeSnapshot>> {
        Err(missing())
    }

    fn validate_genesis_state_for_regime(
        &self,
        _regime: BlockHeightRegime,
        _storage: StorageHandle<'_>,
        _genesis_validators: Option<&GenesisValidators>,
    ) -> Result<()> {
        Err(missing())
    }

    fn validate_genesis_state(
        &self,
        _storage: StorageHandle<'_>,
        _genesis: &GenesisValidators,
    ) -> Result<()> {
        Err(missing())
    }

    fn proposer_attestation(
        &self,
        _storage: StorageHandle<'_>,
        _proposer: Address,
    ) -> Result<Option<[u8; 32]>> {
        Err(missing())
    }

    fn validate_tee_bootstrap_for_build(
        &self,
        _payload: &TeeBootstrapV2,
        _expected_policy: &TeePolicyV1,
    ) -> std::result::Result<(), TeeBootstrapHostError> {
        Err(TeeBootstrapHostError::HostNotInstalled)
    }

    fn mutation_entitlements(
        &self,
        _call: MetadosisMutationCall<'_>,
    ) -> MetadosisMutationEntitlements {
        MetadosisMutationEntitlements::NONE
    }

    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
        address == outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS
            && caller == outbe_primitives::addresses::SYSTEM_ADDRESS
            && matches!(
                outbe_primitives::system_tx::SystemTx::decode(data),
                Ok(outbe_primitives::system_tx::SystemTx::CycleTick)
            )
    }

    fn derive_active_utc_day(
        &self,
        _storage: StorageHandle<'_>,
        _genesis_timestamp: u64,
        _block_timestamp: u64,
    ) -> Result<u32> {
        Err(missing())
    }

    fn settle_block_burned_fees(&self, _storage: StorageHandle<'_>, _amount: U256) -> Result<()> {
        Err(missing())
    }

    fn user_fee_policy(&self) -> &dyn UserTxFeePolicy {
        &FAIL_CLOSED_FEE_POLICY
    }
}

#[cfg(any(test, feature = "test-utils"))]
static FAIL_CLOSED_FEE_POLICY: FailClosedFeePolicy = FailClosedFeePolicy;
