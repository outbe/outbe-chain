use super::super::dispatch::{outbe_ctx_dispatch, OutbeDispatchRuntime, PANIC_WITH_NATIVE_FRAME};
use crate::{
    factory::{OutbeEvm, OutbeEvmTypes},
    precompiles::ExecutionAbortBridge,
    storage::ReentrancyStack,
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
};
use alloy_consensus::TxLegacy;
use alloy_primitives::{Address, Bytes, LogData, TxKind, B256, U256};
use evm2::{
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{CacheDB, EmptyDB},
    interpreter::{GasTracker, InstrStop, Message, MessageExt, MessageKind},
    precompiles::{PrecompileError as EvmPrecompileError, PrecompileHalt, PrecompileResult},
    registry::HandlerError,
    SpecId, TxResult,
};
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
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;
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

fn evm(at: PanicAt) -> OutbeEvm<'static> {
    crate::OutbeEvmFactory::new(PanicHost::new(at)).create_evm(
        CacheDB::new(EmptyDB::default()),
        EthEvmEnv::new(
            SpecId::PRAGUE,
            BlockEnvExt {
                gas_limit: U256::from(30_000_000_u64),
                ..Default::default()
            },
            1,
        ),
    )
}

fn tx(data: &'static [u8]) -> Recovered<TxEnvelope> {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: GAS_LIMIT,
            to: TxKind::Call(OUTER),
            value: U256::ZERO,
            input: Bytes::from_static(data),
        }),
        CALLER,
    )
}

fn assert_halt(result: &TxResult<OutbeEvmTypes>) {
    assert!(!result.is_success());
    assert_eq!(result.stop, InstrStop::PrecompileError);
    assert_eq!(result.tx_gas_used(), GAS_LIMIT);
    assert_eq!(result.refunded, 0);
    assert!(result.logs.is_empty());
    assert!(!format!("{result:?}").contains("secret panic text"));
    assert_eq!(ReentrancyStack::depth(), 0);
}

#[test]
fn panic_in_host_prelude_before_provider_returns_payload_free_halt_and_spends_frame_gas() {
    for at in [PanicAt::BaseGas, PanicAt::CycleProbe, PanicAt::Entitlements] {
        let mut gas = GasTracker::new(GAS_LIMIT);
        let outcome = dispatch(&mut evm(at), at, &ExecutionAbortBridge::default(), &mut gas)
            .expect("route exists");
        assert!(matches!(
            outcome,
            Err(EvmPrecompileError::Halt(PrecompileHalt::Other(_)))
        ));
        assert_eq!(gas.spent(), GAS_LIMIT);
        assert_eq!(gas.refunded(), 0);
        assert_eq!(ReentrancyStack::depth(), 0);
        let mut e = evm(at);
        let executed = e.transact(&tx(b"panic")).expect("panic becomes a halt");
        let res = executed.detach();
        assert_halt(&res.result);
    }
}

#[test]
fn top_level_panic_after_writes_and_logs_rolls_back_frame_and_allows_next_call() {
    let mut evm = evm(PanicAt::Handler);
    let executed = evm.transact(&tx(b"panic")).expect("panic becomes a halt");
    let detached = executed.detach();
    assert_halt(&detached.result);
    if let Some(account) = detached.pending_state.account_changes_for(&OUTER) {
        assert!(account
            .storage
            .slots
            .values()
            .all(|slot| slot.value.current == U256::ZERO));
    }
    let executed = evm.transact(&tx(b"ok")).expect("next call succeeds");
    let res = executed.detach();
    assert!(res.result.is_success());
    assert_eq!(res.result.output, Bytes::from_static(b"ok"));
    assert_eq!(ReentrancyStack::depth(), 0);
}

#[test]
fn nested_outbe_precompile_panic_is_caught_at_innermost_dispatch_preserving_journal_depth() {
    let mut evm = evm(PanicAt::Handler);
    let executed = evm
        .transact(&tx(b"outer"))
        .expect("outer transaction completes");
    let detached = executed.detach();
    assert!(detached.result.is_success());
    assert!(detached.result.logs.is_empty());
    let account = detached
        .pending_state
        .account_changes_for(&OUTER)
        .expect("OUTER account changes present");
    assert_eq!(
        account.storage.slots[&U256::ZERO].value.current,
        U256::from(1)
    );
    assert_eq!(
        account.storage.slots[&U256::from(1)].value.current,
        U256::from(2)
    );
    if let Some(inner) = detached.pending_state.account_changes_for(&INNER) {
        assert!(inner
            .storage
            .slots
            .values()
            .all(|slot| slot.value.current == U256::ZERO));
    }
    assert_eq!(evm.ext().native_frame_depth, 0);
    assert_eq!(ReentrancyStack::depth(), 0);
}

#[test]
fn panic_after_latched_node_local_abort_preserves_node_local_abort() {
    let mut e = evm(PanicAt::Handler);
    let error = e
        .transact(&tx(b"abort"))
        .expect_err("node-local abort must survive");
    assert!(NodeLocalExecutionAbort::find(&error).is_some());
    assert_eq!(ReentrancyStack::depth(), 0);
}

fn message() -> Message<OutbeEvmTypes> {
    MessageExt {
        kind: MessageKind::Call,
        depth: 0,
        gas_limit: GAS_LIMIT,
        reservoir: 0,
        destination: OUTER,
        call_target: OUTER,
        caller: CALLER,
        input: Bytes::new().into(),
        value: U256::ZERO,
        code: Default::default(),
        code_address: OUTER,
        caller_is_static: false,
        salt: B256::ZERO,
        ext: (),
        _non_exhaustive: (),
    }
}

fn dispatch(
    evm: &mut OutbeEvm<'_>,
    at: PanicAt,
    bridge: &ExecutionAbortBridge,
    gas: &mut GasTracker,
) -> Option<PrecompileResult> {
    outbe_ctx_dispatch(
        evm,
        &message(),
        gas,
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
    let entry_depth = evm.ext().native_frame_depth;
    PANIC_WITH_NATIVE_FRAME.set(true);
    let mut gas = GasTracker::new(GAS_LIMIT);
    let error = dispatch(
        &mut evm,
        PanicAt::Handler,
        &ExecutionAbortBridge::default(),
        &mut gas,
    )
    .expect("route exists")
    .expect_err("dirty depth is fatal");
    assert!(matches!(error, EvmPrecompileError::Fatal(_)));
    assert!(error
        .to_string()
        .contains("precompile subcall unwound with dirty journal depth"));
    assert!(!error.to_string().contains("secret native frame panic"));
    assert_eq!(evm.ext().native_frame_depth, entry_depth + 1);

    // Even a dirty native frame must not erase the earlier node-local failure.
    let bridge = ExecutionAbortBridge::default();
    let call = bridge.begin_call();
    bridge.observe(&PrecompileError::EnclaveUnavailable("offline".into()));
    PANIC_WITH_NATIVE_FRAME.set(true);
    let mut gas = GasTracker::new(GAS_LIMIT);
    let error = dispatch(&mut evm, PanicAt::Handler, &bridge, &mut gas)
        .expect("route exists")
        .expect_err("abort wins over dirty depth");
    assert!(error
        .to_string()
        .contains("execution already stopped on a node-local failure"));
    let result: std::result::Result<(), HandlerError> =
        Err(HandlerError::Fatal(evm2::AnyError::new(error)));
    let error = call
        .finish(result)
        .expect_err("latched abort survives translation");
    assert!(NodeLocalExecutionAbort::find(&error).is_some());
}
