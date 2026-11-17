use super::super::dispatch::{outbe_ctx_dispatch, OutbeDispatchRuntime, PANIC_WITH_NATIVE_FRAME};
use crate::precompiles::ExecutionAbortBridge;
use crate::storage::ReentrancyStack;
use crate::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use alloy_evm::{Evm, EvmEnv, EvmFactory};
use alloy_primitives::{Address, Bytes, LogData, TxKind, B256, U256};
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    committee_snapshot::CommitteeSnapshot,
    consensus::{DkgBoundaryArtifact, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::{PrecompileError, Result},
    projection::NodeLocalExecutionAbort,
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::{MutationEntitlements, StorageHandle},
    system_tx_context::SystemTxExecutionContext,
};
use revm::interpreter::{CallInput, CallInputs, CallScheme, CallValue, InstructionResult};
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    context_interface::{
        result::{ExecutionResult, HaltReason},
        JournalTr,
    },
    database::{CacheDB, EmptyDB},
    primitives::hardfork::SpecId,
};
use std::sync::Arc;

const OUTER: Address = Address::repeat_byte(0x81);
const INNER: Address = Address::repeat_byte(0x82);
const CALLER: Address = Address::repeat_byte(0x83);
const GAS_LIMIT: u64 = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanicAt {
    Handler,
    BaseGas,
    CycleProbe,
    Entitlements,
}

#[derive(Debug)]
struct PanicHost {
    stub: Arc<dyn crate::EvmRuntimeHost>,
    at: PanicAt,
}

impl PanicHost {
    fn new(at: PanicAt) -> Arc<Self> {
        Arc::new(Self {
            stub: crate::host::stub_host(),
            at,
        })
    }
    fn panic_at(&self, at: PanicAt) {
        assert_ne!(self.at, at, "secret panic text");
    }
}

impl crate::EvmRuntimeHost for PanicHost {
    fn block_order(&self) -> crate::system_tx::BlockOrder {
        self.stub.block_order()
    }

    fn marker_addresses(&self) -> &'static [Address] {
        self.stub.marker_addresses()
    }

    fn is_registered_precompile(&self, address: &Address) -> bool {
        matches!(*address, OUTER | INNER)
    }

    fn precompile_is_payable(&self, address: &Address) -> bool {
        let _ = address;
        false
    }

    fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
        let _ = (address, input);
        self.panic_at(PanicAt::BaseGas);
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    }

    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        let _ = (caller, value);
        match data {
            b"ok" => {
                assert_eq!(storage.sload(*address, U256::ZERO)?, U256::ZERO);
                assert_eq!(storage.tload(*address, U256::ZERO)?, U256::ZERO);
                assert_eq!(storage.sload(*address, U256::from(1))?, U256::ZERO);
                Ok(Bytes::from_static(b"ok"))
            }
            b"outer" => nested_call(storage, *address),
            b"abort" => {
                assert!(storage
                    .call_with_gas(INNER, U256::ZERO, Bytes::from_static(b"offline"), 50_000)
                    .is_err());
                panic!("secret panic text");
            }
            b"offline" => Err(PrecompileError::EnclaveUnavailable("offline".into())),
            _ => write_then_panic(storage, *address),
        }
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
        self.panic_at(PanicAt::Entitlements);
        MutationEntitlements::NONE
    }

    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
        let _ = (address, caller, data);
        self.panic_at(PanicAt::CycleProbe);
        false
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

fn write_then_panic(storage: StorageHandle<'_>, address: Address) -> Result<Bytes> {
    storage.sstore(address, U256::ZERO, U256::from(7))?;
    storage.tstore(address, U256::ZERO, U256::from(8))?;
    storage.emit_event(
        address,
        LogData::new_unchecked(vec![B256::ZERO], Bytes::from_static(b"before panic")),
    )?;
    storage.refund_gas(500)?;
    storage.with_checkpoint(|| {
        storage.sstore(address, U256::from(1), U256::from(9))?;
        panic!("secret panic text");
    })
}

fn nested_call(storage: StorageHandle<'_>, address: Address) -> Result<Bytes> {
    storage.sstore(address, U256::ZERO, U256::from(1))?;
    assert!(matches!(
        storage.call_with_gas(INNER, U256::ZERO, Bytes::new(), 50_000),
        Err(PrecompileError::Halt(_))
    ));
    assert_eq!(ReentrancyStack::depth(), 1);
    storage.sstore(address, U256::from(1), U256::from(2))?;
    Ok(Bytes::new())
}

type TestEvm =
    <crate::OutbeEvmFactory as EvmFactory>::Evm<CacheDB<EmptyDB>, revm::inspector::NoOpInspector>;

fn evm(at: PanicAt) -> TestEvm {
    crate::OutbeEvmFactory::new(PanicHost::new(at)).create_evm(
        CacheDB::default(),
        EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(1)
                .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
            block_env: BlockEnv {
                gas_limit: 30_000_000,
                ..Default::default()
            },
        },
    )
}

fn tx(data: &'static [u8]) -> TxEnv {
    TxEnv {
        caller: CALLER,
        kind: TxKind::Call(OUTER),
        data: Bytes::from_static(data),
        gas_limit: GAS_LIMIT,
        ..Default::default()
    }
}

fn assert_halt(result: &ExecutionResult<HaltReason>) {
    assert!(matches!(
        result,
        ExecutionResult::Halt {
            reason: HaltReason::PrecompileError,
            ..
        }
    ));
    assert_eq!(result.gas().tx_gas_used(), GAS_LIMIT);
    assert_eq!(result.gas().final_refunded(), 0);
    assert!(result.logs().is_empty());
    assert!(!format!("{result:?}").contains("secret panic text"));
    assert_eq!(ReentrancyStack::depth(), 0);
}

#[test]
fn panic_in_host_prelude_before_provider_returns_payload_free_halt_and_spends_frame_gas() {
    for at in [PanicAt::BaseGas, PanicAt::CycleProbe, PanicAt::Entitlements] {
        let frame = dispatch(&mut evm(at), at, &ExecutionAbortBridge::default())
            .expect("dispatch does not unwind")
            .expect("route exists");
        assert_eq!(frame.result, InstructionResult::PrecompileError);
        assert_eq!(frame.gas.total_gas_spent(), GAS_LIMIT);
        assert_eq!(frame.gas.refunded(), 0);
        assert!(frame.output.is_empty());
        assert_eq!(ReentrancyStack::depth(), 0);
        let result = evm(at)
            .transact_raw(tx(b"panic"))
            .expect("panic becomes a halt");
        assert_halt(&result.result);
    }
}

#[test]
fn top_level_panic_after_writes_and_logs_rolls_back_frame_and_allows_next_call() {
    let mut evm = evm(PanicAt::Handler);
    let result = evm
        .transact_raw(tx(b"panic"))
        .expect("panic becomes a halt");
    assert_halt(&result.result);
    if let Some(account) = result.state.get(&OUTER) {
        assert!(account
            .storage
            .values()
            .all(|slot| slot.present_value == U256::ZERO));
    }
    let result = evm.transact_raw(tx(b"ok")).expect("next call succeeds");
    assert!(result.result.is_success());
    assert_eq!(result.result.output(), Some(&Bytes::from_static(b"ok")));
    assert_eq!(ReentrancyStack::depth(), 0);
}

#[test]
fn nested_outbe_precompile_panic_is_caught_at_innermost_dispatch_preserving_journal_depth() {
    let mut evm = evm(PanicAt::Handler);
    let result = evm
        .transact_raw(tx(b"outer"))
        .expect("outer transaction completes");
    assert!(result.result.is_success());
    assert!(result.result.logs().is_empty());
    let account = &result.state[&OUTER];
    assert_eq!(account.storage[&U256::ZERO].present_value, U256::from(1));
    assert_eq!(account.storage[&U256::from(1)].present_value, U256::from(2));
    if let Some(inner) = result.state.get(&INNER) {
        assert!(inner
            .storage
            .values()
            .all(|slot| slot.present_value == U256::ZERO));
    }
    assert_eq!(evm.ctx().journaled_state.depth(), 0);
    assert_eq!(ReentrancyStack::depth(), 0);
}

#[test]
fn panic_after_latched_node_local_abort_preserves_node_local_abort() {
    let error = evm(PanicAt::Handler)
        .transact_raw(tx(b"abort"))
        .expect_err("node-local abort must survive");
    assert!(NodeLocalExecutionAbort::find(&error).is_some());
    assert_eq!(ReentrancyStack::depth(), 0);
}

fn inputs() -> CallInputs {
    CallInputs {
        input: CallInput::Bytes(Bytes::new()),
        return_memory_offset: 0..0,
        gas_limit: GAS_LIMIT,
        reservoir: 0,
        bytecode_address: OUTER,
        known_bytecode: Default::default(),
        target_address: OUTER,
        caller: CALLER,
        value: CallValue::Transfer(U256::ZERO),
        scheme: CallScheme::Call,
        is_static: false,
        charged_new_account_state_gas: false,
    }
}

fn dispatch(
    evm: &mut TestEvm,
    at: PanicAt,
    bridge: &ExecutionAbortBridge,
) -> std::result::Result<
    Option<revm::interpreter::InterpreterResult>,
    crate::errors::OutbePrecompileFatalError,
> {
    outbe_ctx_dispatch(
        evm.ctx_mut(),
        &inputs(),
        OutbeDispatchRuntime {
            abort_bridge: bridge,
            spec: SpecId::PRAGUE,
            genesis_hash: B256::ZERO,
            genesis_timestamp: 0,
            tee_attestation_v1: &TeeAttestationChainSpecStateV1::Unbound,
            host: PanicHost::new(at),
        },
    )
}

#[test]
fn panic_with_leaked_native_journal_depth_escalates_to_fatal() {
    let mut evm = evm(PanicAt::Handler);
    let entry_depth = evm.ctx().journaled_state.depth();
    PANIC_WITH_NATIVE_FRAME.set(true);
    let error = dispatch(&mut evm, PanicAt::Handler, &ExecutionAbortBridge::default())
        .expect_err("dirty depth is fatal");
    assert!(error
        .to_string()
        .contains("precompile subcall unwound with dirty journal depth"));
    assert!(!error.to_string().contains("secret native frame panic"));
    assert_eq!(evm.ctx().journaled_state.depth(), entry_depth + 1);

    // Even a dirty native frame must not erase the earlier node-local failure.
    let bridge = ExecutionAbortBridge::default();
    let call = bridge.begin_call();
    bridge.observe(&PrecompileError::EnclaveUnavailable("offline".into()));
    PANIC_WITH_NATIVE_FRAME.set(true);
    let error =
        dispatch(&mut evm, PanicAt::Handler, &bridge).expect_err("abort wins over dirty depth");
    assert!(error
        .to_string()
        .contains("execution already stopped on a node-local failure"));
    let result: std::result::Result<
        (),
        revm::context_interface::result::EVMError<std::convert::Infallible>,
    > = Err(revm::context_interface::result::EVMError::Custom(
        error.into(),
    ));
    let error = call
        .finish(result)
        .expect_err("latched abort survives translation");
    assert!(NodeLocalExecutionAbort::find(&error).is_some());
}
