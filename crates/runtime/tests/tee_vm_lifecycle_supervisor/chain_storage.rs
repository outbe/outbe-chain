//! Import a recovered block and its resulting state into a real provider database.
use alloy_primitives::{keccak256, B256};
use evm2::block::bundle_state::StateChangeset;
use outbe_primitives::{OutbeBlock, OutbeHeader, OutbePrimitives};
use reth_chainspec::ChainSpec;
use reth_primitives_traits::RecoveredBlock;
use reth_provider::{providers::ProviderNodeTypes, BlockWriter, ChainStateBlockWriter};
use reth_trie::{HashedPostState, HashedStorage};

pub(super) fn seed_recovered_anchor<N>(
    provider: &reth_provider::providers::BlockchainProvider<N>,
    blocks: Vec<OutbeBlock>,
    mut changes: StateChangeset,
) -> reth_provider::ProviderResult<()>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    use evm2::block::BundleState;
    use reth_primitives_traits::SealedHeader;
    use reth_provider::{CanonChainTracker, ExecutionOutcome};
    use reth_storage_api::{DBProvider, DatabaseProviderFactory, StateProviderFactory, TrieWriter};

    let first = blocks.first().unwrap().header.inner.number;
    let anchor = SealedHeader::seal_slow(blocks.last().unwrap().header.clone());
    let number = anchor.header().inner.number;
    for storage in &mut changes.storage {
        storage.storage.retain(|(_, value)| !value.is_zero());
    }
    changes
        .storage
        .retain(|storage| !storage.storage.is_empty());
    changes.accounts.retain(|(address, info)| {
        info.as_ref().is_some_and(|info| !info.is_empty())
            || changes
                .storage
                .iter()
                .any(|storage| storage.address == *address)
    });
    let hashed = hashed_changes(&changes);
    let (root, updates) = provider.latest()?.state_root_with_updates(hashed.clone())?;
    assert_eq!(root, anchor.header().inner.state_root);
    // The fixture contains one recovered post-state, not executions of the stub ancestors.
    let mut bundle = BundleState::builder(first..=number);
    for (address, info) in changes.accounts {
        bundle.set_state_present_account_info(address, info.unwrap());
    }
    for storage in changes.storage {
        bundle.set_state_storage(
            storage.address,
            storage
                .storage
                .into_iter()
                .map(|(slot, value)| (slot, (Default::default(), value)))
                .collect(),
        );
    }
    for (hash, bytecode) in changes.contracts {
        bundle.set_contract(hash, bytecode);
    }
    let receipts = vec![Vec::new(); blocks.len()];
    let recovered = blocks
        .into_iter()
        .map(|block| RecoveredBlock::new_unhashed(block, Vec::new()))
        .collect();
    let writer = provider.database_provider_rw()?;
    writer.append_blocks_with_state(
        recovered,
        &ExecutionOutcome::new(bundle.build(), receipts, first, Vec::new()),
        hashed.into_sorted(),
    )?;
    writer.write_trie_updates(updates)?;
    writer.save_finalized_block_number(number)?;
    writer.commit()?;
    provider.set_canonical_head(anchor.clone());
    provider.set_finalized(anchor);
    Ok(())
}

fn hashed_changes(changes: &StateChangeset) -> HashedPostState {
    HashedPostState::default()
        .with_accounts(
            changes
                .accounts
                .iter()
                .map(|(address, info)| (keccak256(address), info.clone().map(Into::into))),
        )
        .with_storages(changes.storage.iter().map(|storage| {
            let slots = storage
                .storage
                .iter()
                .map(|(slot, value)| (keccak256(B256::from(*slot)), *value));
            let hashed = HashedStorage::from_iter(slots);
            (keccak256(storage.address), hashed)
        }))
}

pub(super) type StorageSnapshot = std::collections::HashMap<
    (alloy_primitives::Address, alloy_primitives::U256),
    alloy_primitives::U256,
>;
pub(super) type HistoricalBlockInput = (u64, StorageSnapshot);

pub(super) fn seed_historical_storage_chain<N>(
    factory: reth_provider::ProviderFactory<N>,
    blocks: &[HistoricalBlockInput],
    finalized_height: u64,
) -> reth_provider::ProviderResult<(
    reth_provider::providers::BlockchainProvider<N>,
    Vec<reth_primitives_traits::SealedHeader<OutbeHeader>>,
)>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    use reth_provider::providers::BlockchainProvider;
    use std::collections::HashMap;

    assert!(
        !blocks.is_empty(),
        "historical storage chain requires at least genesis block 0"
    );
    let empty = HashMap::new();
    let (genesis_ts, genesis_storage) = &blocks[0];
    let genesis = append_historical_block_to_factory(
        &factory,
        0,
        *genesis_ts,
        B256::ZERO,
        &empty,
        genesis_storage,
    )?;
    let provider = BlockchainProvider::with_latest(factory, genesis.clone())?;
    let mut headers = vec![genesis];
    for (idx, (timestamp, storage)) in blocks.iter().enumerate().skip(1) {
        let prev_storage = &blocks[idx - 1].1;
        let parent_hash = headers[idx - 1].hash();
        let sealed = append_historical_block(
            &provider,
            idx as u64,
            *timestamp,
            parent_hash,
            prev_storage,
            storage,
        )?;
        headers.push(sealed);
    }
    let finalized = headers
        .get(finalized_height as usize)
        .expect("finalized_height must exist in seeded blocks")
        .clone();
    set_finalized_block(&provider, finalized)?;
    Ok((provider, headers))
}

pub(super) fn append_historical_block<N>(
    provider: &reth_provider::providers::BlockchainProvider<N>,
    number: u64,
    timestamp: u64,
    parent_hash: B256,
    prev_storage: &StorageSnapshot,
    next_storage: &StorageSnapshot,
) -> reth_provider::ProviderResult<reth_primitives_traits::SealedHeader<OutbeHeader>>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    use reth_provider::{CanonChainTracker, ExecutionOutcome};
    use reth_storage_api::{DBProvider, DatabaseProviderFactory, StateProviderFactory, TrieWriter};

    let (bundle, hashed) = build_historical_transition(number, prev_storage, next_storage);
    let (state_root, updates) = provider.latest()?.state_root_with_updates(hashed.clone())?;
    let header = OutbeHeader::new(alloy_consensus::Header {
        number,
        timestamp,
        parent_hash,
        state_root,
        ..Default::default()
    });
    let sealed = reth_primitives_traits::SealedHeader::seal_slow(header.clone());
    let block = OutbeBlock {
        header,
        ..Default::default()
    };
    let writer = provider.database_provider_rw()?;
    writer.append_blocks_with_state(
        vec![RecoveredBlock::new_unhashed(block, Vec::new())],
        &ExecutionOutcome::new(bundle, vec![Vec::new()], number, Vec::new()),
        hashed.into_sorted(),
    )?;
    writer.write_trie_updates(updates)?;
    writer.commit()?;
    provider.set_canonical_head(sealed.clone());
    Ok(sealed)
}

pub(super) fn set_finalized_block<N>(
    provider: &reth_provider::providers::BlockchainProvider<N>,
    sealed: reth_primitives_traits::SealedHeader<OutbeHeader>,
) -> reth_provider::ProviderResult<()>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    use reth_provider::CanonChainTracker;
    use reth_storage_api::{DBProvider, DatabaseProviderFactory};

    let writer = provider.database_provider_rw()?;
    writer.save_finalized_block_number(sealed.header().inner.number)?;
    writer.commit()?;
    provider.set_finalized(sealed);
    Ok(())
}

fn append_historical_block_to_factory<N>(
    factory: &reth_provider::ProviderFactory<N>,
    number: u64,
    timestamp: u64,
    parent_hash: B256,
    prev_storage: &StorageSnapshot,
    next_storage: &StorageSnapshot,
) -> reth_provider::ProviderResult<reth_primitives_traits::SealedHeader<OutbeHeader>>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    use reth_provider::ExecutionOutcome;
    use reth_storage_api::TrieWriter;

    let (bundle, hashed) = build_historical_transition(number, prev_storage, next_storage);
    let (state_root, updates) = factory.latest()?.state_root_with_updates(hashed.clone())?;
    let header = OutbeHeader::new(alloy_consensus::Header {
        number,
        timestamp,
        parent_hash,
        state_root,
        ..Default::default()
    });
    let sealed = reth_primitives_traits::SealedHeader::seal_slow(header.clone());
    let block = OutbeBlock {
        header,
        ..Default::default()
    };
    let writer = factory.provider_rw()?;
    writer.append_blocks_with_state(
        vec![RecoveredBlock::new_unhashed(block, Vec::new())],
        &ExecutionOutcome::new(bundle, vec![Vec::new()], number, Vec::new()),
        hashed.into_sorted(),
    )?;
    writer.write_trie_updates(updates)?;
    writer.commit()?;
    Ok(sealed)
}

fn build_historical_transition(
    number: u64,
    prev_storage: &std::collections::HashMap<
        (alloy_primitives::Address, alloy_primitives::U256),
        alloy_primitives::U256,
    >,
    next_storage: &std::collections::HashMap<
        (alloy_primitives::Address, alloy_primitives::U256),
        alloy_primitives::U256,
    >,
) -> (evm2::block::BundleState, HashedPostState) {
    use alloy_primitives::{Address, U256};
    use evm2::block::BundleState;
    use std::collections::{BTreeMap, BTreeSet};

    let mut addresses: BTreeSet<Address> = BTreeSet::new();
    let mut slots_by_addr: BTreeMap<Address, BTreeSet<U256>> = BTreeMap::new();
    for (address, slot) in prev_storage.keys().chain(next_storage.keys()) {
        addresses.insert(*address);
        slots_by_addr.entry(*address).or_default().insert(*slot);
    }

    let mut bundle = BundleState::builder(number..=number);
    let mut hashed = HashedPostState::default();
    let account_info = evm2::evm::AccountInfo {
        nonce: 1,
        ..Default::default()
    };

    for address in addresses {
        let existed = prev_storage.keys().any(|(addr, _)| *addr == address);
        if existed {
            bundle.set_state_original_account_info(address, account_info.clone());
            bundle.set_state_present_account_info(address, account_info.clone());
            bundle.set_revert_account_info(number, address, None);
        } else {
            bundle.set_state_present_account_info(address, account_info.clone());
            bundle.set_revert_account_info(number, address, Some(None));
        }

        let mut changed_storage = Vec::new();
        let mut revert_storage = Vec::new();
        let mut hashed_slots = Vec::new();
        if let Some(slots) = slots_by_addr.get(&address) {
            for slot in slots {
                let prev = prev_storage
                    .get(&(address, *slot))
                    .copied()
                    .unwrap_or(U256::ZERO);
                let next = next_storage
                    .get(&(address, *slot))
                    .copied()
                    .unwrap_or(U256::ZERO);
                if prev != next {
                    changed_storage.push((*slot, (prev, next)));
                    revert_storage.push((*slot, prev));
                    hashed_slots.push((keccak256(B256::from(*slot)), next));
                }
            }
        }
        if !changed_storage.is_empty() {
            bundle.set_state_storage(address, changed_storage.into_iter().collect());
            bundle.set_revert_storage(number, address, revert_storage);
        }
        hashed = hashed
            .with_accounts([(keccak256(address), Some(account_info.clone().into()))])
            .with_storages([(keccak256(address), HashedStorage::from_iter(hashed_slots))]);
    }

    (bundle.build(), hashed)
}
