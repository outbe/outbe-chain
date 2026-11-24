use super::*;
use alloy_primitives::{Address, U256};
use evm2::{
    env::{BlockEnvExt, TxEnvExt},
    evm::{EmptyDB, SYSTEM_CALL_GAS_LIMIT},
    EvmFeatures,
};
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    committee_snapshot::CommitteeSnapshot,
    consensus::{DkgBoundaryArtifact, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result,
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::{MutationEntitlements, StorageHandle},
    system_tx_context::SystemTxExecutionContext,
};

struct NopModule;

impl outbe_primitives::block::ProtocolModule for NopModule {
    const NAME: &'static str = "nop";
}

#[derive(Debug)]
struct StepGasHost {
    stub: Arc<dyn crate::EvmRuntimeHost>,
}

impl crate::EvmRuntimeHost for StepGasHost {
    fn block_order(&self) -> crate::system_tx::BlockOrder {
        use crate::system_tx::{begin_step, consensus_step, end_step, BlockOrder};
        const ORDER: BlockOrder = BlockOrder {
            consensus: &[consensus_step::<NopModule>().with_gas_limit(123_456)],
            begin_block: &[begin_step::<NopModule>().with_gas_limit(234_567)],
            end_block: &[end_step::<NopModule>().with_gas_limit(345_678)],
        };
        ORDER
    }

    fn marker_addresses(&self) -> &'static [Address] {
        self.stub.marker_addresses()
    }

    fn is_registered_precompile(&self, address: &Address) -> bool {
        self.stub.is_registered_precompile(address)
    }

    fn precompile_is_payable(&self, address: &Address) -> bool {
        self.stub.precompile_is_payable(address)
    }

    fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
        self.stub.precompile_base_gas(address, input)
    }

    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        self.stub
            .dispatch_precompile(storage, address, data, caller, value)
    }

    fn dispatch_system_tx(
        &self,
        storage: StorageHandle<'_>,
        data: &[u8],
        caller: Address,
        value: U256,
        tee: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes> {
        self.stub
            .dispatch_system_tx(storage, data, caller, value, tee)
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
        _call: crate::precompiles::PrecompileMutationCall<'_>,
    ) -> MutationEntitlements {
        MutationEntitlements::NONE
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

    fn user_fee_policy(&self) -> &dyn crate::UserTxFeePolicy {
        self.stub.user_fee_policy()
    }
}

const USER_BLOCK_GAS_LIMIT: u64 = 30_000_000;

pub(super) fn test_env() -> EthEvmEnv<OutbeEvmTypes> {
    let block = BlockEnvExt {
        gas_limit: U256::from(USER_BLOCK_GAS_LIMIT),
        ..Default::default()
    };
    EthEvmEnv::new(SpecId::SHANGHAI, block, 1)
}

#[test]
fn outbe_system_call_resolves_step_gas_limits_and_fallbacks() {
    use crate::system_tx::SystemTx;

    let host = Arc::new(StepGasHost {
        stub: crate::host::stub_host(),
    });
    let factory = OutbeEvmFactory::new(host);
    let mut evm = factory.create_evm(EmptyDB::default(), test_env());
    let fallback = outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;
    for (data, expected) in [
        (
            SystemTx::Consensus {
                step_index: 0,
                payload: Bytes::new(),
            }
            .encode()
            .unwrap(),
            123_456,
        ),
        (
            SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
            234_567,
        ),
        (
            SystemTx::EndBlock { step_index: 0 }.encode().unwrap(),
            345_678,
        ),
        (
            SystemTx::BeginBlock { step_index: 99 }.encode().unwrap(),
            fallback,
        ),
        (Bytes::from_static(b"unknown"), fallback),
    ] {
        let _ = transact_outbe_system_call(
            &mut evm,
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            data,
        );
        assert_eq!(
            evm.ext().last_system_call.as_ref().unwrap().gas_limit,
            expected
        );
        assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
    }
    evm.ext_mut().runtime_host = None;
    let _ = transact_outbe_system_call(
        &mut evm,
        outbe_primitives::addresses::SYSTEM_ADDRESS,
        outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
        SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
    );
    assert_eq!(
        evm.ext().last_system_call.as_ref().unwrap().gas_limit,
        fallback
    );
    assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
}

#[test]
fn outbe_system_call_uses_artifact_gas_limit_without_changing_block_limit() {
    let factory = OutbeEvmFactory::new(crate::host::stub_host());
    let mut evm = factory.create_evm(EmptyDB::default(), test_env());

    let caller = outbe_primitives::addresses::SYSTEM_ADDRESS;
    let target = outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS;
    let data = Bytes::from_static(b"system call input");
    // A preceding user transaction must not leak fields into the system call.
    *evm.tx_env_mut() = TxEnvExt {
        origin: Address::repeat_byte(0x11),
        gas_price: U256::from(123),
        chain_id: U256::from(99),
        ..TxEnvExt::default()
    };
    let _ = transact_outbe_system_call(&mut evm, caller, target, data.clone());

    let last = evm.ext().last_system_call.as_ref().unwrap();
    assert_eq!(
        last.gas_limit,
        outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
    );
    assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
    assert_eq!(last.caller, caller);
    assert_eq!(last.system_contract_address, target);
    assert_eq!(last.data, data);
    assert_eq!(evm.tx_env().origin, caller);
    assert_eq!(evm.tx_env().gas_price, U256::ZERO);
    assert!(evm.feature(EvmFeatures::NONCE_CHECK));
}

#[test]
fn non_outbe_system_call_keeps_upstream_gas_limit() {
    let factory = OutbeEvmFactory::new(crate::host::stub_host());
    let mut evm = factory.create_evm(EmptyDB::default(), test_env());

    let _ = transact_outbe_system_call(
        &mut evm,
        outbe_primitives::addresses::SYSTEM_ADDRESS,
        Address::repeat_byte(0x42),
        Bytes::new(),
    );

    assert_eq!(
        evm.ext().last_system_call.as_ref().unwrap().gas_limit,
        SYSTEM_CALL_GAS_LIMIT
    );
    assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
}
