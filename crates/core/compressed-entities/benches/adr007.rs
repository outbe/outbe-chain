use alloy_primitives::{Address, U256};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use outbe_compressed_entities::bench_support::EmptyParentBodies;
use outbe_compressed_entities::{
    list, mint, BodyInput, CompressedEntitiesLifecycle, CompressedEntitiesLifecycleContext,
    ExecutionScope, IdPageRequest, QueryRef, TributeBodyV1, WwdEntityId,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::{BlockContext, BlockLifecycle, BlockRuntimeContext},
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const DAY: WorldwideDay = WorldwideDay::new(20_260_716);
const OWNER: Address = Address::repeat_byte(0x71);

fn body(index: u32) -> TributeBodyV1 {
    let mut digest = [0_u8; 32];
    digest[28..].copy_from_slice(&index.to_be_bytes());
    TributeBodyV1 {
        tribute_id: WwdEntityId::from_day_and_digest(DAY, digest),
        owner: OWNER,
        worldwide_day: DAY,
        issuance_amount_minor: U256::from(index + 1),
        issuance_currency: u16::MAX,
        nominal_amount_minor: U256::MAX,
        reference_currency: u16::MAX,
        tribute_price_minor: U256::MAX,
        exclude_from_intex_issuance: true,
    }
}

fn lifecycle<'a, 'storage>(
    storage: StorageHandle<'storage>,
    scope: &'a ExecutionScope,
) -> CompressedEntitiesLifecycleContext<'a, 'storage> {
    CompressedEntitiesLifecycleContext::new(
        BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 1, 1), storage),
        scope,
    )
}

fn bench_gas_saturated_touches(c: &mut Criterion) {
    c.bench_function("adr007_gas_saturated_touches_and_cleanup", |b| {
        b.iter(|| {
            let mut provider = HashMapStorageProvider::new(1);
            provider.set_gas_limit(BLOCK_GAS_LIMIT);
            let scope = ExecutionScope::new();
            StorageHandle::enter(&mut provider, |storage| {
                let lifecycle = lifecycle(storage.clone(), &scope);
                <CompressedEntitiesLifecycle as BlockLifecycle>::begin_block(&lifecycle).unwrap();
                let mut committed = 0_u32;
                loop {
                    match mint(
                        storage.clone(),
                        &scope,
                        BodyInput::Tribute(&body(committed)),
                    ) {
                        Ok(()) => committed += 1,
                        Err(PrecompileError::OutOfGas) => break,
                        Err(error) => panic!("unexpected saturated-touch failure: {error}"),
                    }
                }
                <CompressedEntitiesLifecycle as BlockLifecycle>::end_block(&lifecycle).unwrap();
                black_box(committed)
            })
        })
    });
}

fn bench_touched_list_merge(c: &mut Criterion) {
    c.bench_function("adr007_touched_owner_list_merge", |b| {
        b.iter(|| {
            let mut provider = HashMapStorageProvider::new(1);
            let scope = ExecutionScope::new();
            StorageHandle::enter(&mut provider, |storage| {
                let lifecycle = lifecycle(storage.clone(), &scope);
                <CompressedEntitiesLifecycle as BlockLifecycle>::begin_block(&lifecycle).unwrap();
                for index in 0..200 {
                    mint(storage.clone(), &scope, BodyInput::Tribute(&body(index))).unwrap();
                }
                let page = list(
                    storage.clone(),
                    &scope,
                    &EmptyParentBodies,
                    QueryRef::TributeByOwner(OWNER),
                    IdPageRequest {
                        after: None,
                        limit: 1_024,
                    },
                )
                .unwrap();
                let bodies = page.bodies().len();
                <CompressedEntitiesLifecycle as BlockLifecycle>::end_block(&lifecycle).unwrap();
                black_box(bodies)
            })
        })
    });
}

criterion_group!(
    adr007,
    bench_gas_saturated_touches,
    bench_touched_list_merge
);
criterion_main!(adr007);
