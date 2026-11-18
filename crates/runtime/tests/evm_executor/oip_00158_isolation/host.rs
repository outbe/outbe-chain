//! Runtime delegation with one deliberately failing native route.
use super::*;
use outbe_evm::{system_tx::BlockOrder, EvmRuntimeHost};
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    committee_snapshot::CommitteeSnapshot,
    consensus::{DkgBoundaryArtifact, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::{PrecompileError, Result},
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::{MutationEntitlements, StorageHandle},
    system_tx_context::SystemTxExecutionContext,
};

#[derive(Debug, Clone, Copy)]
pub(super) enum Failure {
    None,
    HardBegin,
    SoftBegin,
    EndPanic,
    EndOog,
}

#[derive(Debug)]
pub(super) struct IsolationHost {
    stub: Arc<dyn EvmRuntimeHost>,
    order: BlockOrder,
    failure: Failure,
}
impl IsolationHost {
    pub(super) fn new(failure: Failure) -> Arc<Self> {
        let mut order = outbe_runtime::BLOCK_ORDER;
        if matches!(failure, Failure::SoftBegin) {
            let mut steps = order.begin_block.to_vec();
            steps[0] = steps[0].soft_fail();
            order.begin_block = Box::leak(steps.into_boxed_slice());
        }
        if matches!(failure, Failure::EndOog) {
            let mut steps = order.end_block.to_vec();
            steps[0] = steps[0].with_gas_limit(60_000);
            order.end_block = Box::leak(steps.into_boxed_slice());
        }
        Arc::new(Self {
            stub: Arc::new(outbe_runtime::OutbeRuntime::new()),
            order,
            failure,
        })
    }
}

fn dirty_panic(storage: StorageHandle<'_>, address: Address) -> Result<Bytes> {
    storage.sstore(address, U256::ZERO, U256::from(7))?;
    storage.tstore(address, U256::ZERO, U256::from(8))?;
    storage.emit_event(
        address,
        alloy_primitives::LogData::new_unchecked(
            vec![B256::ZERO],
            Bytes::from_static(b"dirty log"),
        ),
    )?;
    panic!("secret panic payload")
}

impl EvmRuntimeHost for IsolationHost {
    fn block_order(&self) -> BlockOrder {
        self.order
    }
    fn marker_addresses(&self) -> &'static [Address] {
        self.stub.marker_addresses()
    }
    fn is_registered_precompile(&self, address: &Address) -> bool {
        *address == PROBE || self.stub.is_registered_precompile(address)
    }
    fn precompile_is_payable(&self, address: &Address) -> bool {
        self.stub.precompile_is_payable(address)
    }
    fn precompile_base_gas(&self, address: &Address, data: &[u8]) -> u64 {
        if *address == PROBE {
            3_000
        } else {
            self.stub.precompile_base_gas(address, data)
        }
    }
    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        if *address != PROBE {
            return self
                .stub
                .dispatch_precompile(storage, address, data, caller, value);
        }
        match data {
            b"panic" => dirty_panic(storage, PROBE),
            b"transport" => {
                outbe_tee::witness::consensus_call(
                    "isolation transport",
                    || panic!("secret transport payload"),
                    |_, _| Ok(()),
                )
                .map_err(|error| PrecompileError::EnclaveUnavailable(error.to_string()))?;
                unreachable!("transport must panic")
            }
            _ => {
                assert_eq!(storage.sload(PROBE, U256::ZERO)?, U256::ZERO);
                assert_eq!(storage.tload(PROBE, U256::ZERO)?, U256::ZERO);
                storage.sstore(PROBE, U256::ONE, U256::ONE)?;
                Ok(Bytes::new())
            }
        }
    }
    fn dispatch_system_tx(
        &self,
        storage: StorageHandle<'_>,
        data: &[u8],
        caller: Address,
        value: U256,
        tee: &outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes> {
        let tx = SystemTx::decode(data).unwrap();
        match (self.failure, tx) {
            (Failure::HardBegin | Failure::SoftBegin, SystemTx::BeginBlock { step_index: 0 })
            | (Failure::EndPanic, SystemTx::EndBlock { step_index: 0 }) => {
                dirty_panic(storage, PROBE)
            }
            (Failure::EndOog, SystemTx::EndBlock { step_index: 0 }) => {
                for slot in 0..32 {
                    storage.sstore(PROBE, U256::from(slot), U256::ONE)?;
                }
                panic!("tight step budget must stop these writes")
            }
            _ => self
                .stub
                .dispatch_system_tx(storage, data, caller, value, tee),
        }
    }

    fn validate_finalized_metadata(
        &self,
        storage: StorageHandle<'_>,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<()> {
        self.stub.validate_finalized_metadata(storage, metadata)
    }

    fn run_finalization_and_slashing(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        metadata: &CertifiedParentAccountingMetadata,
        execution: &SystemTxExecutionContext,
    ) -> Result<()> {
        self.stub
            .run_finalization_and_slashing(ctx, metadata, execution)
    }

    fn authenticate_late_credit(
        &self,
        storage: &StorageHandle<'_>,
        credit: &PerBlockCredit,
    ) -> Result<()> {
        self.stub.authenticate_late_credit(storage, credit)
    }

    fn run_late_finalize_credits(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        artifact: &LateFinalizeCreditsArtifact,
        execution: Option<&SystemTxExecutionContext>,
    ) -> Result<()> {
        self.stub
            .run_late_finalize_credits(ctx, artifact, execution)
    }

    fn apply_boundary_outcome(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        self.stub
            .apply_boundary_outcome(storage, boundary, block_number, timestamp)
    }

    fn prepare_boundary(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
    ) -> Result<()> {
        self.stub.prepare_boundary(storage, boundary, block_number)
    }

    fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>> {
        self.stub.consensus_validators(storage)
    }

    fn is_consensus_participant(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<bool> {
        self.stub.is_consensus_participant(storage, proposer)
    }

    fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
        self.stub.is_validator(storage, proposer)
    }

    fn read_committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        key: B256,
    ) -> Result<Option<CommitteeSnapshot>> {
        self.stub.read_committee_snapshot(storage, key)
    }

    fn validate_genesis_state_for_regime(
        &self,
        regime: BlockHeightRegime,
        storage: StorageHandle<'_>,
        genesis_validators: Option<&GenesisValidators>,
    ) -> Result<()> {
        self.stub
            .validate_genesis_state_for_regime(regime, storage, genesis_validators)
    }

    fn validate_genesis_state(
        &self,
        storage: StorageHandle<'_>,
        genesis: &GenesisValidators,
    ) -> Result<()> {
        self.stub.validate_genesis_state(storage, genesis)
    }

    fn proposer_attestation(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>> {
        self.stub.proposer_attestation(storage, proposer)
    }

    fn mutation_entitlements(
        &self,
        call: outbe_evm::precompiles::PrecompileMutationCall<'_>,
    ) -> MutationEntitlements {
        self.stub.mutation_entitlements(call)
    }

    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
        self.stub.is_protocol_cycle_call(address, caller, data)
    }

    fn derive_active_utc_day(
        &self,
        storage: StorageHandle<'_>,
        genesis_timestamp: u64,
        block_timestamp: u64,
    ) -> Result<u32> {
        self.stub
            .derive_active_utc_day(storage, genesis_timestamp, block_timestamp)
    }

    fn settle_block_burned_fees(&self, storage: StorageHandle<'_>, amount: U256) -> Result<()> {
        self.stub.settle_block_burned_fees(storage, amount)
    }

    fn user_fee_policy(&self) -> &dyn outbe_evm::UserTxFeePolicy {
        self.stub.user_fee_policy()
    }
}
