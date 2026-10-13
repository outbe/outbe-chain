use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
#[cfg(feature = "e2e-test")]
use outbe_compressed_entities::ExecutionScope;
use outbe_compressed_entities::{ExecutionReaders, ParentBodySource};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::api;
use crate::errors::GemError;
use crate::schema::{GemContract, GemState, GemView};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// No global enumeration: gems are read by id and by owner only.
const SUPPORTED_INTERFACES: [[u8; 4]; 4] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
    ERC4906_INTERFACE_ID,
];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IGem.sol"
);

// A gem is stamped live, so only a throwaway build can move its issuance back
// behind the closed days a scenario seeds for the call sweep. The forfeit sweep
// opens a bucket only once its hour has closed. For that reason,
// `closeCallNoticeForTest` moves a deadline that already lapsed into one that the
// sweep will open on the next block.
#[cfg(feature = "e2e-test")]
sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IGemTestArming {
        function backdateGemForTest(uint256 gemId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 gemId, uint64 deadline) external;
    }
}

/// Re-issues the gem on `issued_at`: it leaves its bucket and joins the bucket of the
/// new day, which may open a new bucket body.
#[cfg(feature = "e2e-test")]
fn backdate_gem(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    gem_id: U256,
    issued_at: u64,
) -> Result<()> {
    let (item, bucket) =
        api::load_pair(storage, scope, parent, gem_id)?.ok_or(GemError::GemNotFound)?;
    if item.body().is_settled() {
        return Err(GemError::InvalidState.into());
    }
    let body = item.body().clone();
    let entry_price_minor = bucket.body().entry_price_minor;
    let mut gem = GemContract::new(storage.clone());
    storage.clone().with_checkpoint(|| {
        gem.record_gem_removed(scope, item, bucket)?;
        let mut reissued = body;
        reissued.issued_at = issued_at;
        reissued.bucket_key = crate::identity::bucket_key(
            reissued.worldwide_day(),
            entry_price_minor,
            reissued.reference_currency,
        );
        gem.record_gem_issued(scope, parent, &reissued, entry_price_minor)
    })
}

/// Move the queue entry of a called gem's bucket onto `deadline`.
#[cfg(feature = "e2e-test")]
fn requeue_called_gem(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    gem_id: U256,
    deadline: u64,
) -> Result<()> {
    let item = api::get_item(storage, scope, parent, gem_id)?.ok_or(GemError::GemNotFound)?;
    let mut gem = GemContract::new(storage.clone());
    let bucket = item.bucket_key;
    if gem.called_deadline.read(&bucket)? == 0 {
        return Err(GemError::InvalidState.into());
    }
    gem.remove_called(bucket)?;
    gem.push_called(bucket, deadline)
}

/// Dispatches Gem calls through the block-scoped compressed-body lifecycle.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    #[cfg(feature = "e2e-test")]
    if let Ok(call) =
        <IGemTestArming::backdateGemForTestCall as alloy_sol_types::SolCall>::abi_decode(data)
    {
        outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;
        backdate_gem(
            &storage,
            readers.scope,
            readers.parent,
            call.gemId,
            call.issuedAt,
        )?;
        return Ok(Bytes::new());
    }
    #[cfg(feature = "e2e-test")]
    if let Ok(call) =
        <IGemTestArming::closeCallNoticeForTestCall as alloy_sol_types::SolCall>::abi_decode(data)
    {
        outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;
        requeue_called_gem(
            &storage,
            readers.scope,
            readers.parent,
            call.gemId,
            call.deadline,
        )?;
        return Ok(Bytes::new());
    }
    dispatch_call(data, IGem::IGemCalls::abi_decode, |call| {
        use IGem::IGemCalls::*;
        match call {
            call @ (supportsInterface(_) | name(_) | symbol(_) | totalSupply(_)) => {
                dispatch_metadata(&storage, call)
            }
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(GemError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            call @ (balanceOf(_)
            | ownerOf(_)
            | tokenURI(_)
            | tokenOfOwnerByIndex(_)
            | getGemStatus(_)
            | isQualified(_)) => dispatch_entity(&storage, readers, call),
        }
    })
}

fn dispatch_metadata(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    call: IGem::IGemCalls,
) -> Result<Bytes> {
    use IGem::IGemCalls::*;
    match call {
        supportsInterface(c) => view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0))),
        name(_) => metadata::<IGem::nameCall>(|| Ok(GemContract::name().to_string())),
        symbol(_) => metadata::<IGem::symbolCall>(|| Ok(GemContract::symbol().to_string())),
        totalSupply(_) => metadata::<IGem::totalSupplyCall>(|| {
            GemContract::new(storage.clone())
                .total_supply()
                .map(U256::from)
        }),
        _ => Err(outbe_primitives::error::PrecompileError::Fatal(
            "metadata route received an entity call".into(),
        )),
    }
}

fn dispatch_entity(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    call: IGem::IGemCalls,
) -> Result<Bytes> {
    let ExecutionReaders { scope, parent } = readers;
    use IGem::IGemCalls::*;
    match call {
        balanceOf(c) => view(c, |c| {
            let count = api::list_by_owner(storage, scope, parent, c.owner)?.len();
            Ok(U256::from(count))
        }),
        ownerOf(c) => view(c, |c| {
            Ok(api::get_item(storage, scope, parent, c.gemId)?
                .ok_or(GemError::GemNotFound)?
                .owner)
        }),
        tokenURI(c) => view(c, |c| {
            let gem =
                api::get_view(storage, scope, parent, c.gemId)?.ok_or(GemError::GemNotFound)?;
            let qualified = !gem.item.is_settled() && api::is_qualified(storage, &gem.item)?;
            let now = storage.timestamp()?.to::<u64>();
            Ok(crate::metadata::token_uri(&gem, qualified, now))
        }),
        tokenOfOwnerByIndex(c) => view(c, |c| {
            let idx = usize::try_from(c.index).map_err(|_| GemError::IndexOutOfBounds)?;
            api::list_by_owner(storage, scope, parent, c.owner)?
                .get(idx)
                .map(|item| item.gem_id.to_u256())
                .ok_or_else(|| GemError::IndexOutOfBounds.into())
        }),
        getGemStatus(c) => view(c, |c| {
            let gem =
                api::get_view(storage, scope, parent, c.gemId)?.ok_or(GemError::GemNotFound)?;
            let mut data = to_abi_data(&gem, storage.timestamp()?.to::<u64>());
            if data.state == GemState::Issued as u8 && api::is_qualified(storage, &gem.item)? {
                data.state = 1;
            }
            Ok(data)
        }),
        isQualified(c) => view(c, |c| {
            let item =
                api::get_item(storage, scope, parent, c.gemId)?.ok_or(GemError::GemNotFound)?;
            api::is_qualified(storage, &item)
        }),
        _ => Err(outbe_primitives::error::PrecompileError::Fatal(
            "entity route received a metadata call".into(),
        )),
    }
}

fn to_abi_data(gem: &GemView, now: u64) -> IGem::GemData {
    IGem::GemData {
        gemId: gem.item.gem_id.to_u256(),
        owner: gem.item.owner,
        gemType: gem.item.gem_type,
        state: gem.effective_state(now),
        promisLoadMinor: gem.item.promis_load_minor,
        entryPriceMinor: gem.bucket.entry_price_minor,
        floorPriceMinor: gem.item.floor_price_minor,
        issuanceCurrency: gem.item.issuance_currency,
        referenceCurrency: gem.item.reference_currency,
        issuedAt: gem.item.issued_at,
        callPriceMinor: gem.terms.call_price_minor,
        calledAt: gem.called_at(),
        callNoticePeriod: gem.terms.call_notice_period_seconds,
        callWindow: gem.terms.call_window_seconds,
        callThreshold: gem.terms.call_threshold_seconds,
        settlementDeadline: gem.settlement_deadline(),
    }
}
