//! `OutbeRuntime` implements the infrastructure seam traits.
//!
//! Chain reads for RPC, the engine, and radicle live in [`super::wiring_state`].

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_evm::errors::TeeBootstrapHostError;
use outbe_evm::precompiles::MetadosisMutationCall;
use outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use outbe_evm::{EvmRuntimeHost, UserTxFeePolicy};
use outbe_node::tee_remote_session::TeeSessionStateReader;
use outbe_primitives::block::{BlockHeightRegime, BlockRuntimeContext};
use outbe_primitives::committee_snapshot::CommitteeSnapshot;
use outbe_primitives::consensus::{DkgBoundaryArtifact, GenesisValidators};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::enclave_upgrade::EnclaveUpgradeV1;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};
use outbe_primitives::storage::{MetadosisMutationEntitlements, StorageHandle};
use outbe_primitives::system_tx_context::SystemTxExecutionContext;
use outbe_primitives::tee_attestation_v1::{NodeIdV1, TeePolicyV1};
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;
use outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1;
use outbe_primitives::zero_fee_types::{
    BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView, SponsorshipAuthorization,
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeePolicyError, ZeroFeeTransaction,
};
use outbe_txpool::TxPoolZeroFeePolicy;

/// Composition root. One value implements every infrastructure seam.
#[derive(Debug, Default)]
pub struct OutbeRuntime;

impl OutbeRuntime {
    /// Returns the runtime host handle.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Host handle for the EVM factory and the block executor.
    #[must_use]
    pub fn host(self: &Arc<Self>) -> Arc<dyn EvmRuntimeHost> {
        Arc::clone(self) as Arc<dyn EvmRuntimeHost>
    }
}

impl UserTxFeePolicy for OutbeRuntime {
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        outbe_zerofee::registry().classify(tx)
    }

    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        outbe_zerofee::classify_bootstrap(tx)
    }

    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool {
        outbe_zerofee::authorize_bootstrap(candidate, account)
    }

    fn authorize_sponsorship(
        &self,
        storage: StorageHandle<'_>,
        signer: Address,
        block_timestamp_secs: u64,
    ) -> std::result::Result<SponsorshipAuthorization, ZeroFeePolicyError> {
        outbe_zerofee::authorize_sponsorship(storage, signer, block_timestamp_secs)
    }

    fn record_sponsorship_use(
        &self,
        storage: StorageHandle<'_>,
        authorization: SponsorshipAuthorization,
    ) -> std::result::Result<u32, ZeroFeePolicyError> {
        outbe_zerofee::record_sponsorship_use(storage, authorization)
    }

    fn sponsored_gas_exceeds_block_budget(&self, used: u64, gas_limit: u64) -> bool {
        outbe_zerofee::sponsored_gas_exceeds_block_budget(used, gas_limit)
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        outbe_zerofee::registry().authorize_fee_waiver(storage, candidate)
    }

    fn classify_sponsorship(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<(), ZeroFeePolicyError> {
        outbe_zerofee::classify_sponsorship(tx)
    }
}

impl TxPoolZeroFeePolicy for OutbeRuntime {
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        outbe_zerofee::registry().classify(tx)
    }

    fn authorize(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        outbe_zerofee::registry().authorize_fee_waiver(storage, candidate)
    }

    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        outbe_zerofee::classify_bootstrap(tx)
    }

    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool {
        outbe_zerofee::authorize_bootstrap(candidate, account)
    }

    fn classify_sponsorship(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<(), ZeroFeePolicyError> {
        outbe_zerofee::classify_sponsorship(tx)
    }

    fn precheck_sponsorship(&self, signer: Address) -> std::result::Result<(), ZeroFeePolicyError> {
        outbe_zerofee::precheck_sponsorship(signer)
    }
}

impl EvmRuntimeHost for OutbeRuntime {
    fn marker_addresses(&self) -> &'static [Address] {
        &outbe_evm::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES
    }

    fn precompile_is_payable(&self, address: &Address) -> bool {
        crate::precompiles::is_payable(address)
    }

    fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
        crate::precompiles::base_gas(address, input)
    }

    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        crate::precompiles::dispatch(storage, address, data, caller, value)
    }

    fn dispatch_system_tx(
        &self,
        storage: StorageHandle<'_>,
        data: &[u8],
        caller: Address,
        value: U256,
        tee: &TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes> {
        crate::system_tx::dispatch(storage, data, caller, value, tee)
    }

    fn validate_finalized_metadata(
        &self,
        storage: StorageHandle<'_>,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<()> {
        outbe_rewards::finalization::validate_finalized_metadata(storage, metadata)
    }

    fn run_finalization_and_slashing(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        metadata: &CertifiedParentAccountingMetadata,
        execution: &SystemTxExecutionContext,
    ) -> Result<()> {
        outbe_rewards::finalization::run_finalization_and_slashing(ctx, metadata, execution)
    }

    fn authenticate_late_credit(
        &self,
        storage: &StorageHandle<'_>,
        credit: &PerBlockCredit,
    ) -> Result<()> {
        outbe_rewards::finalization::authenticate_late_credit(storage, credit)
    }

    fn run_late_finalize_credits(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        artifact: &LateFinalizeCreditsArtifact,
        execution: Option<&SystemTxExecutionContext>,
    ) -> Result<()> {
        outbe_rewards::finalization::run_late_finalize_credits(ctx, artifact, execution)
    }

    fn apply_boundary_outcome(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        outbe_validatorset::boundary::apply_boundary_outcome(
            storage,
            boundary,
            block_number,
            timestamp,
        )
    }

    fn prepare_boundary(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
    ) -> Result<()> {
        outbe_slashindicator::prepare_boundary_epoch_counters(storage, boundary, block_number)
    }

    fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>> {
        let mut validators: Vec<Address> = outbe_validatorset::contract::ValidatorSet::new(storage)
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        validators.sort();
        Ok(validators)
    }

    fn is_consensus_participant(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<bool> {
        outbe_validatorset::is_consensus_participant(storage, proposer)
    }

    fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
        outbe_validatorset::is_validator(storage, proposer)
    }

    fn read_committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        key: B256,
    ) -> Result<Option<CommitteeSnapshot>> {
        outbe_validatorset::state::read_committee_snapshot(storage, key)
    }

    fn validate_genesis_state_for_regime(
        &self,
        regime: BlockHeightRegime,
        storage: StorageHandle<'_>,
        genesis_validators: Option<&GenesisValidators>,
    ) -> Result<()> {
        outbe_staking::genesis::validate_genesis_state_for_regime(
            regime,
            storage,
            genesis_validators,
        )
    }

    fn validate_genesis_state(
        &self,
        storage: StorageHandle<'_>,
        genesis: &GenesisValidators,
    ) -> Result<()> {
        outbe_staking::genesis::validate_genesis_state(storage, genesis)
    }

    fn proposer_attestation(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>> {
        Ok(outbe_teeregistry::TeeRegistry::new(storage)
            .validator_enclave_binding_v1(proposer)?
            .map(|binding| binding.attestation_ed25519.0))
    }

    fn validate_tee_bootstrap_for_build(
        &self,
        payload: &TeeBootstrapV2,
        expected_policy: &TeePolicyV1,
    ) -> std::result::Result<(), TeeBootstrapHostError> {
        match outbe_teeregistry::validate_tee_bootstrap_for_build(payload, expected_policy) {
            Ok(()) => Ok(()),
            Err(outbe_teeregistry::TeeBootstrapBuildError::InvalidBlock1Ost3BootstrapPayload(
                reason,
            )) => Err(TeeBootstrapHostError::InvalidPayload(reason)),
            Err(outbe_teeregistry::TeeBootstrapBuildError::Block1Ost3PolicyScheduleMismatch) => {
                Err(TeeBootstrapHostError::PolicyScheduleMismatch)
            }
        }
    }

    fn install_initial_tee_policy(
        &self,
        storage: StorageHandle<'_>,
        policy: &TeePolicyV1,
    ) -> Result<()> {
        outbe_teeregistry::TeeRegistry::new(storage).install_initial_policy_v1(policy)
    }

    fn run_tee_bootstrap(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        payload: &TeeBootstrapV2,
        expected_policy: &TeePolicyV1,
    ) -> Result<()> {
        outbe_teeregistry::run_tee_bootstrap_v1(ctx, payload, expected_policy)
    }

    fn mutation_entitlements(
        &self,
        call: MetadosisMutationCall<'_>,
    ) -> MetadosisMutationEntitlements {
        outbe_cycle::call_authority::metadosis_mutation_entitlements(
            outbe_cycle::call_authority::MetadosisMutationCall {
                address: call.address,
                data: call.data,
                caller: call.caller,
                is_static: call.is_static,
                value: call.value,
                chain_id: call.chain_id,
                block_number: call.block_number,
                timestamp: call.timestamp,
                cycle_active_utc_day: call.cycle_active_utc_day,
            },
        )
    }

    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
        outbe_cycle::call_authority::is_protocol_cycle_call(address, caller, data)
    }

    fn derive_active_utc_day(
        &self,
        storage: StorageHandle<'_>,
        genesis_timestamp: u64,
        block_timestamp: u64,
    ) -> Result<u32> {
        outbe_cycle::handler::derive_active_utc_day(storage, genesis_timestamp, block_timestamp)
    }

    fn settle_block_burned_fees(&self, mut storage: StorageHandle<'_>, amount: U256) -> Result<()> {
        outbe_promislimit::settle_block_burned_fees(&mut storage, amount)
    }

    fn sync_hyperlane_validators(&self, ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        outbe_hyperlanecontroller::lifecycle::sync_validators(ctx)
    }

    fn enforce_enclave_upgrade_deadline(&self, ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        outbe_slashindicator::enforce_enclave_upgrade_deadline(ctx)
    }

    fn enforce_tee_lease_deadlines(&self, ctx: &BlockRuntimeContext<'_>) -> Result<usize> {
        outbe_slashindicator::enforce_tee_lease_deadlines(ctx)
    }

    fn run_validator_liveness(&self, ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        outbe_slashindicator::run_validator_liveness(ctx)
    }

    fn user_fee_policy(&self) -> &dyn UserTxFeePolicy {
        self
    }
}

impl TeeSessionStateReader for OutbeRuntime {
    fn node_binding(
        &self,
        storage: StorageHandle<'_>,
        node_id: &NodeIdV1,
    ) -> std::result::Result<Option<NodeEnclaveBindingV1>, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).node_enclave_binding_for_identity_v1(node_id)
    }

    fn binding_admitted(
        &self,
        storage: StorageHandle<'_>,
        binding: &NodeEnclaveBindingV1,
        height: u64,
    ) -> std::result::Result<bool, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).binding_code_admitted_at_v1(binding, height)
    }

    fn binding_slots(
        &self,
        storage: StorageHandle<'_>,
        node_id: &NodeIdV1,
    ) -> std::result::Result<Vec<B256>, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).node_enclave_binding_storage_slots_v1(node_id)
    }

    fn retirement_height(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u64, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).last_enclave_retirement_height_v1()
    }

    fn successor(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<Option<(U256, TeePolicyV1)>, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).staged_successor_policy_v1()
    }

    fn enclave_upgrade(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<EnclaveUpgradeV1, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).enclave_upgrade_v1()
    }
}
