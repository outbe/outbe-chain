use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_compressed_entities::{ExecutionReaders, ParentBodySource, WwdEntityId};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::api;
use crate::errors::NodError;
use crate::schema::{NodBucketState, NodCertifiedGenerationProjection, NodContract, NodItemState};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

const SUPPORTED_INTERFACES: [[u8; 4]; 5] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
    ERC721_ENUMERABLE_INTERFACE_ID,
    ERC4906_INTERFACE_ID,
];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INod.sol"
);

// The forfeit sweep opens a bucket only once its hour has closed, so a throwaway
// build moves a lapsed deadline into an hour the sweep opens on the next block.
#[cfg(feature = "e2e-test")]
sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface INodTestArming {
        function closeCallNoticeForTest(bytes32 bucketKey, uint64 deadline) external;
    }
}

#[cfg(feature = "e2e-test")]
fn requeue_called_bucket(
    storage: outbe_primitives::storage::StorageHandle,
    bucket_key: alloy_primitives::B256,
    deadline: u64,
) -> Result<()> {
    let mut nod = NodContract::new(storage);
    if nod.called_deadline.read(&bucket_key)? == 0 {
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "closeCallNoticeForTest: bucket not queued".into(),
        ));
    }
    nod.remove_called_bucket(bucket_key)?;
    nod.push_called_bucket(bucket_key, deadline)
}

/// Dispatches Nod calls through the block-scoped compressed-body lifecycle.
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
        <INodTestArming::closeCallNoticeForTestCall as alloy_sol_types::SolCall>::abi_decode(data)
    {
        requeue_called_bucket(storage.clone(), call.bucketKey, call.deadline)?;
        return Ok(Bytes::new());
    }
    dispatch_call(data, INod::INodCalls::abi_decode, |call| {
        let nod = NodContract::new(storage.clone());
        use INod::INodCalls::*;
        match call {
            call @ (supportsInterface(_) | name(_) | symbol(_) | totalSupply(_)) => {
                dispatch_metadata(&nod, call)
            }
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(NodError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            call @ (balanceOf(_)
            | ownerOf(_)
            | tokenURI(_)
            | tokenByIndex(_)
            | tokenOfOwnerByIndex(_)
            | nodData(_)
            | certifiedGeneration(_)) => dispatch_entity(storage, readers, &nod, call),
        }
    })
}

fn dispatch_metadata(nod: &NodContract<'_>, call: INod::INodCalls) -> Result<Bytes> {
    use INod::INodCalls::*;
    match call {
        supportsInterface(c) => view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0))),
        name(_) => metadata::<INod::nameCall>(|| Ok(NodContract::name().to_string())),
        symbol(_) => metadata::<INod::symbolCall>(|| Ok(NodContract::symbol().to_string())),
        totalSupply(_) => metadata::<INod::totalSupplyCall>(|| nod.total_supply().map(U256::from)),
        _ => unreachable!("metadata route requires a metadata call"),
    }
}

fn dispatch_entity(
    storage: outbe_primitives::storage::StorageHandle<'_>,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    nod: &NodContract<'_>,
    call: INod::INodCalls,
) -> Result<Bytes> {
    let ExecutionReaders { scope, parent } = readers;
    use INod::INodCalls::*;
    match call {
        balanceOf(c) => view(c, |c| {
            let count = api::list_by_owner(&storage, scope, parent, c.owner)?.len();
            Ok(U256::from(count))
        }),
        ownerOf(c) => view(c, |c| {
            let nod_id = WwdEntityId::from(c.nodId);
            Ok(api::get_item(&storage, scope, parent, nod_id)?
                .ok_or(NodError::NodNotFound)?
                .owner)
        }),
        tokenURI(c) => view(c, |c| {
            let nod_id = WwdEntityId::from(c.nodId);
            let item =
                api::get_item(&storage, scope, parent, nod_id)?.ok_or(NodError::NodNotFound)?;
            let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0);
            let bucket = api::get_bucket(&storage, scope, parent, bucket_id)?
                .ok_or(NodError::BucketNotFound)?;
            let qualified = api::is_qualified(&storage, &bucket)?;
            crate::metadata::token_uri(nod, &item, &bucket, qualified, storage.timestamp()?)
        }),
        tokenByIndex(c) => view(c, |c| {
            let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
            api::list_all(&storage, scope, parent)?
                .get(idx)
                .map(|item| item.nod_id.to_u256())
                .ok_or_else(|| NodError::IndexOutOfBounds.into())
        }),
        tokenOfOwnerByIndex(c) => view(c, |c| {
            let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
            api::list_by_owner(&storage, scope, parent, c.owner)?
                .get(idx)
                .map(|item| item.nod_id.to_u256())
                .ok_or_else(|| NodError::IndexOutOfBounds.into())
        }),
        nodData(c) => view(c, |c| {
            let nod_id = WwdEntityId::from(c.nodId);
            let item =
                api::get_item(&storage, scope, parent, nod_id)?.ok_or(NodError::NodNotFound)?;
            let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0);
            let bucket = api::get_bucket(&storage, scope, parent, bucket_id)?
                .ok_or(NodError::BucketNotFound)?;
            to_abi_data(&storage, &item, &bucket)
        }),
        certifiedGeneration(c) => view(c, |c| {
            let worldwide_day = WorldwideDay::new(c.worldwideDay);
            Ok(to_abi_certified_generation(
                worldwide_day,
                nod.ocomp_certified_generation(worldwide_day)?,
            ))
        }),
        _ => unreachable!("entity route requires an entity call"),
    }
}

fn to_abi_data(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    item: &NodItemState,
    bucket: &NodBucketState,
) -> Result<INod::NodData> {
    let nod = NodContract::new(storage.clone());
    let called_at = nod.bucket_called_at.read(&item.bucket_key)?;
    let terms = nod.read_call_terms(item.bucket_key)?;
    let deadline = if called_at == 0 {
        0
    } else {
        api::settlement_deadline_of(called_at, terms.call_notice_period_seconds)
    };
    let qualified = api::is_qualified(storage, bucket)?;
    let state = api::effective_state(item, qualified, called_at, deadline, storage.timestamp()?);
    Ok(INod::NodData {
        nodId: item.nod_id.to_u256(),
        owner: item.owner,
        worldwideDay: item.worldwide_day.into(),
        leagueId: item.league_id,
        floorPriceMinor: bucket.floor_price_minor()?,
        encryptedGratisAmount: item.encrypted.encrypted_gratis_amount.clone().into(),
        encryptedCreatorPublicKey: item.encrypted.encrypted_creator_public_key.clone().into(),
        encryptionBinding: item.encrypted.encryption_binding,
        chainId: item.encrypted.terms.chain_id,
        entryPriceMinor: bucket.entry_price_minor,
        settlementCostMinor: api::settlement_cost_minor(
            bucket.entry_price_minor,
            api::calculation_amount(item)?,
        )?,
        isQualified: qualified,
        issuanceCurrency: item.issuance_currency,
        referenceCurrency: item.reference_currency,
        issuedAt: item.issued_at,
        calledAt: called_at,
        isSettled: item.is_settled,
        effectiveState: state as u8,
        callPriceMinor: terms.call_price_minor,
        callRate: terms.call_rate,
        callWindow: terms.call_window_seconds,
        callThreshold: terms.call_threshold_seconds,
        callNoticePeriod: terms.call_notice_period_seconds,
        settlementDeadline: deadline,
    })
}

fn to_abi_certified_generation(
    worldwide_day: WorldwideDay,
    generation: Option<NodCertifiedGenerationProjection>,
) -> INod::CertifiedGenerationData {
    match generation {
        Some(generation) => INod::CertifiedGenerationData {
            exists: true,
            worldwideDay: generation.worldwide_day.into(),
            generation: generation.generation,
            nodRoot: generation.nod_root,
            bucketRoot: generation.bucket_root,
            outputManifestRoot: generation.output_manifest_root,
            tributeCount: generation.tribute_count,
            nodCount: generation.nod_count,
            bucketCount: generation.bucket_count,
            totalSettlementCostMinor: generation.nod_amount_total,
            lysisAllocationMinor: generation.lysis_allocation_minor,
            issuedAt: generation.issued_at,
        },
        None => INod::CertifiedGenerationData {
            exists: false,
            worldwideDay: worldwide_day.into(),
            generation: 0,
            nodRoot: Default::default(),
            bucketRoot: Default::default(),
            outputManifestRoot: Default::default(),
            tributeCount: 0,
            nodCount: 0,
            bucketCount: 0,
            totalSettlementCostMinor: U256::ZERO,
            lysisAllocationMinor: U256::ZERO,
            issuedAt: 0,
        },
    }
}
