use super::*;
use alloy_sol_types::SolCall;
use outbe_compressed_entities::{
    begin_block, body_commitment, encode_nod_item_v2, AuthenticatedParentTree,
    AuthenticatedParentTreeFactory, Commitment, EntityRef, ExactParentIdentity, FinalLeafMutation,
    PartitionRef, ProvisionalTreeBatch, ACTIVE_COMMITMENT_SCHEME, NOD_BODY_SCHEMA_V2,
};
use outbe_nod::{precompile::INod, NodRepositoryWriter};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS},
    block::BlockContext,
    projection::{ExecutionReadBudget, ExecutionReadCancelled},
    storage::{direct::DirectStorageProvider, StorageHandle},
    time::WorldwideDay,
};
use revm::database::{CacheDB, EmptyDB};

#[derive(Debug, Clone)]
struct Parent {
    id: outbe_compressed_entities::WwdEntityId,
    commitment: Commitment,
    root: B256,
    block_hash: B256,
}

impl AuthenticatedParentTreeFactory for Parent {
    fn open_parent(
        &self,
        _: ExactParentIdentity,
    ) -> outbe_primitives::error::Result<Arc<dyn AuthenticatedParentTree>> {
        Ok(Arc::new(self.clone()))
    }
}

impl AuthenticatedParentTree for Parent {
    fn parent_block_hash(&self) -> B256 {
        self.block_hash
    }
    fn parent_root(&self) -> B256 {
        self.root
    }
    fn read_leaf_verified(
        &self,
        entity: EntityRef,
        root: B256,
    ) -> outbe_primitives::error::Result<Option<Commitment>> {
        let expected_root = self.parent_root();
        assert_eq!(root, expected_root);
        Ok((entity == EntityRef::NodItem(self.id)).then_some(self.commitment))
    }
    fn partition_present_verified(
        &self,
        _: PartitionRef,
        root: B256,
    ) -> outbe_primitives::error::Result<bool> {
        let expected_root = self.parent_root();
        assert_eq!(root, expected_root);
        Ok(false)
    }
    fn prepare_seal(
        &self,
        height: u64,
        _: &[FinalLeafMutation],
        _: &[PartitionRef],
    ) -> outbe_primitives::error::Result<ProvisionalTreeBatch> {
        ProvisionalTreeBatch::new_identity(height, self.parent_block_hash(), self.parent_root())
            .map_err(|e| outbe_primitives::error::PrecompileError::Fatal(e.to_string()))
    }
}

fn fixture() -> (
    OutbeEvm<CacheDB<EmptyDB>, NoOpInspector, PrecompilesMap>,
    Bytes,
    Address,
) {
    let (evm, calldata, owner, _) = supervised_fixture(false);
    (evm, calldata, owner)
}

type SupervisedFixture = (
    OutbeEvm<CacheDB<EmptyDB>, NoOpInspector, PrecompilesMap>,
    Bytes,
    Address,
    tokio::sync::watch::Receiver<Option<outbe_offchain_data::RuntimeBodyFailure>>,
);

fn supervised_fixture(mismatch: bool) -> SupervisedFixture {
    let owner = Address::repeat_byte(0x11);
    let day = WorldwideDay::new(20261007);
    let id = outbe_nod::NodContract::generate_nod_id(owner, day).unwrap();
    let item = outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id: id,
            owner,
            gratis_load_minor: alloy_primitives::U256::from(11),
            worldwide_day: day,
            league_id: 3,
            bucket_key: B256::repeat_byte(0x42),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_700_000_000,
        },
        alloy_primitives::U256::ONE,
    );
    let storage = Arc::new(MemoryStorage::new());
    NodRepositoryWriter::new(storage.clone(), storage.clone())
        .put_nod(&item)
        .unwrap();
    let payload = encode_nod_item_v2(&outbe_nod::canonical_item(&item)).unwrap();
    let parent = Parent {
        root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
        block_hash: B256::ZERO,
        id,
        commitment: body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            NOD_BODY_SCHEMA_V2,
            id,
            if mismatch { b"previous body" } else { &payload },
        )
        .unwrap(),
    };
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let factory = OutbeEvmFactory::with_runtime_body_readers(RuntimeBodyReaders::new_supervised(
        storage, failure_tx,
    ));
    let mut evm = factory.create_evm(CacheDB::new(EmptyDB::default()), super::tests::test_env());
    let scope = evm.execution_scope().clone();
    scope
        .configure_parent_tree_factory(Arc::new(parent), ACTIVE_COMMITMENT_SCHEME, 0, B256::ZERO)
        .unwrap();
    let block = BlockContext::new(1, 1, 1, owner, vec![owner]);
    let mut provider =
        DirectStorageProvider::new(&mut evm.inner.ctx.journaled_state.database, block);
    StorageHandle::enter(&mut provider, |storage| {
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                alloy_primitives::U256::ZERO,
                alloy_primitives::U256::from(4),
            )
            .unwrap();
        let root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                alloy_primitives::U256::from(1),
                alloy_primitives::U256::from_be_slice(root.as_slice()),
            )
            .unwrap();
        begin_block(storage, &scope).unwrap();
    });
    provider.flush().unwrap();
    drop(provider);
    (
        evm,
        INod::ownerOfCall {
            nodId: id.to_u256(),
        }
        .abi_encode()
        .into(),
        owner,
        failure_rx,
    )
}

#[test]
fn body_mismatch_reverts_real_evm_calls_without_stopping_the_node() {
    use alloy_sol_types::{Revert, SolError};
    for system in [false, true] {
        let (mut evm, calldata, owner, failures) = supervised_fixture(true);
        let result = if system {
            evm.transact_system_call(owner, NOD_ADDRESS, calldata)
        } else {
            evm.transact_raw(
                TxEnv::builder()
                    .caller(owner)
                    .kind(TxKind::Call(NOD_ADDRESS))
                    .data(calldata)
                    .gas_limit(1_000_000)
                    .build_fill(),
            )
        }
        .expect("body mismatch must return a transaction result, not abort execution");
        assert!(matches!(
            result.result,
            revm::context_interface::result::ExecutionResult::Revert { .. }
        ));
        let reason = Revert::abi_decode(result.result.output().unwrap()).unwrap();
        assert!(
            reason.reason.contains("commitment mismatch"),
            "{}",
            reason.reason
        );
        assert!(result.result.tx_gas_used() > 0);
        assert!(
            failures.borrow().is_none(),
            "request failure must not stop the node"
        );
        assert!(evm
            .transact_system_call(owner, Address::repeat_byte(0x77), Bytes::new())
            .unwrap()
            .result
            .is_success());
    }
}

#[test]
fn cancelled_read_preserves_its_type_through_real_evm_calls() {
    for system in [false, true] {
        let (mut evm, calldata, owner) = fixture();
        let budget = ExecutionReadBudget::new();
        let _guard = evm
            .runtime_body_readers()
            .unwrap()
            .enter_execution_budget(budget.clone());
        budget.cancel();
        let result = if system {
            evm.transact_system_call(owner, NOD_ADDRESS, calldata)
        } else {
            evm.transact_raw(
                TxEnv::builder()
                    .caller(owner)
                    .kind(TxKind::Call(NOD_ADDRESS))
                    .data(calldata)
                    .gas_limit(1_000_000)
                    .build_fill(),
            )
        };
        let error = result.unwrap_err();
        let cancelled = ExecutionReadCancelled::find(&error)
            .expect("body cancellation must retain its type across EVM");
        assert!(cancelled.budget.same_request(&budget));
    }
}

#[test]
fn fresh_execution_fork_reads_the_same_body_after_another_execution_cancels() {
    let (mut cancelled, calldata, owner) = fixture();
    let budget = ExecutionReadBudget::new();
    let _guard = cancelled
        .runtime_body_readers()
        .unwrap()
        .enter_execution_budget(budget.clone());
    budget.cancel();
    assert!(cancelled
        .transact_system_call(owner, NOD_ADDRESS, calldata)
        .is_err());
    let (mut fresh, calldata, owner) = fixture();
    let result = fresh
        .transact_system_call(owner, NOD_ADDRESS, calldata)
        .unwrap();
    assert!(result.result.is_success());
    assert_eq!(
        INod::ownerOfCall::abi_decode_returns(result.result.output().unwrap()).unwrap(),
        owner
    );
}

#[test]
fn factory_forks_keep_cancellation_local_over_one_shared_backend() {
    use outbe_compressed_entities::{ParentBodySource, WwdEntityId};
    let factory = OutbeEvmFactory::with_runtime_body_readers(RuntimeBodyReaders::new(Arc::new(
        MemoryStorage::new(),
    )));
    let first = factory.create_evm(EmptyDB::default(), super::tests::test_env());
    let second = factory.create_evm_with_inspector(
        EmptyDB::default(),
        super::tests::test_env(),
        NoOpInspector,
    );
    let first_budget = ExecutionReadBudget::new();
    let _first_guard = first
        .runtime_body_readers()
        .unwrap()
        .enter_execution_budget(first_budget.clone());
    let _second_guard = second
        .runtime_body_readers()
        .unwrap()
        .enter_execution_budget(ExecutionReadBudget::new());
    first_budget.cancel();
    let entity = EntityRef::Tribute(WwdEntityId::from_day_and_digest(
        WorldwideDay::new(20261007),
        [1; 32],
    ));
    assert!(ParentBodySource::get(first.runtime_body_readers().unwrap(), entity).is_err());
    assert!(
        ParentBodySource::get(second.runtime_body_readers().unwrap(), entity)
            .unwrap()
            .is_none()
    );
}

#[test]
fn nested_contract_call_keeps_the_exact_body_read_cancellation() {
    let (mut evm, calldata, owner) = fixture();
    let target = Address::repeat_byte(0x45);
    // Copy calldata, STATICCALL Nod, and return its 32-byte result.
    let mut code = vec![
        0x36, 0x60, 0, 0x60, 0, 0x37, 0x60, 0x20, 0x60, 0, 0x36, 0x60, 0, 0x73,
    ];
    code.extend_from_slice(NOD_ADDRESS.as_slice());
    code.extend_from_slice(&[0x5a, 0xfa, 0x50, 0x60, 0x20, 0x60, 0, 0xf3]);
    evm.inner.ctx.journaled_state.database.insert_account_info(
        target,
        revm::state::AccountInfo {
            code: Some(revm::state::Bytecode::new_raw(code.into())),
            ..Default::default()
        },
    );
    let budget = ExecutionReadBudget::new();
    let _guard = evm
        .runtime_body_readers()
        .unwrap()
        .enter_execution_budget(budget.clone());
    budget.cancel();
    let error = evm
        .transact_system_call(owner, target, calldata)
        .unwrap_err();
    assert!(ExecutionReadCancelled::find(&error)
        .unwrap()
        .budget
        .same_request(&budget));
}
