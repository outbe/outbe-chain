use super::*;
use outbe_offchain_data::runtime_body_readers;

#[test]
fn executor_construction_scopes_read_budget_to_import_and_proposal() {
    use outbe_compressed_entities::WwdEntityId;
    use outbe_nod::NodRepositoryError;
    use outbe_offchain_storage::StorageError;
    use outbe_primitives::projection::ExecutionReadBudget;
    use reth_primitives_traits::SealedHeader;

    let missing_id = WwdEntityId::from_day_and_digest(
        outbe_primitives::time::WorldwideDay::new(20261004),
        B256::repeat_byte(0x37),
    );
    for proposal in [false, true] {
        for with_budget in [false, true] {
            let config = OutbeEvmConfig::new_with_runtime_body_readers(
                test_chain_spec(),
                runtime_body_readers(Arc::new(MemoryStorage::new())),
            );
            let mut state = State::builder()
                .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
                .with_bundle_update()
                .build();
            let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
            let readers = evm.runtime_scope().body_readers().unwrap().clone();
            let budget = ExecutionReadBudget::new();
            let mut ctx = execution_ctx(None, Bytes::new());
            ctx.execution_read_budget = with_budget.then(|| budget.clone());
            assert!(readers.nod().get(missing_id).unwrap().is_none());

            let check_cancelled_read = || {
                budget.cancel();
                let result = readers.nod().get(missing_id);
                if with_budget {
                    assert!(matches!(
                        result,
                        Err(NodRepositoryError::Storage(StorageError::RequestDeadline))
                    ));
                } else {
                    assert!(result.unwrap().is_none());
                }
            };
            if proposal {
                let parent = SealedHeader::seal_slow(outbe_primitives::OutbeHeader::default());
                let builder = config.create_block_builder(evm, &parent, ctx);
                check_cancelled_read();
                drop(builder);
            } else {
                let executor = config.create_executor(evm, ctx);
                check_cancelled_read();
                drop(executor);
            }
            assert!(readers.nod().get(missing_id).unwrap().is_none());
        }
    }
}
