//! Import a recovered block and its resulting state into a real provider database.
use alloy_primitives::{keccak256, B256};
use outbe_primitives::{OutbeBlock, OutbeHeader, OutbePrimitives};
use reth_chainspec::ChainSpec;
use reth_primitives_traits::RecoveredBlock;
use reth_provider::{providers::ProviderNodeTypes, BlockWriter, ChainStateBlockWriter};
use reth_trie::{HashedPostState, HashedStorage};
use revm::database::states::StateChangeset;

pub(super) fn seed_recovered_anchor<N>(
    provider: &reth_provider::providers::BlockchainProvider<N>,
    blocks: Vec<OutbeBlock>,
    mut changes: StateChangeset,
) -> reth_provider::ProviderResult<()>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    use reth_primitives_traits::SealedHeader;
    use reth_provider::{CanonChainTracker, ExecutionOutcome};
    use reth_storage_api::{DBProvider, DatabaseProviderFactory, StateProviderFactory, TrieWriter};
    use revm::database::BundleState;

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
