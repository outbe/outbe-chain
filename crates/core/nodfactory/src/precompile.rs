use alloy_primitives::{Address, Bytes, U256};
#[cfg(feature = "e2e-test")]
use alloy_sol_types::SolCall;
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::{PrecompileError, Result};

use crate::runtime;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/INodFactory.sol"
    );
}
pub use abi::INodFactory;

// Lysis issues a Nod over a certified generation. Only a throwaway build may
// skip that.
#[cfg(feature = "e2e-test")]
alloy_sol_types::sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface INodFactoryTestArming {
        function issueForTest(
            address owner,
            uint32 worldwideDay,
            uint256 gratisLoadMinor,
            uint256 entryPriceMinor,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint64 issuedAt
        ) external;
    }
}

/// Issue one Nod straight to its owner. A non-zero `issuedAt` restamps the bucket,
/// since calls and qualification count only days after it.
#[cfg(feature = "e2e-test")]
fn issue_for_test(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    call: INodFactoryTestArming::issueForTestCall,
) -> Result<()> {
    use outbe_nod::schema::{NodContract, NodIssueParams};

    let params = NodIssueParams {
        owner: call.owner,
        worldwide_day: outbe_primitives::time::WorldwideDay::new(call.worldwideDay),
        league_id: 1,
        gratis_load_minor: call.gratisLoadMinor,
        entry_price_minor: call.entryPriceMinor,
        issuance_currency: call.issuanceCurrency,
        reference_currency: call.referenceCurrency,
    };
    runtime::issue_nod(storage, scope, parent, &params)?;
    if call.issuedAt != 0 {
        let bucket_key = NodContract::bucket_key(
            params.worldwide_day,
            call.entryPriceMinor,
            params.reference_currency,
        );
        NodContract::new(storage.clone())
            .callable_bucket_issued_at
            .write(&bucket_key, call.issuedAt)?;
    }
    Ok(())
}

/// Dispatches NodFactory calls through the block-scoped compressed-body lifecycle.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    #[cfg(feature = "e2e-test")]
    if let Ok(call) = INodFactoryTestArming::issueForTestCall::abi_decode(data) {
        issue_for_test(&storage, scope, parent, call)?;
        return Ok(Bytes::new());
    }
    if data.get(..4)
        == Some(outbe_ocomp_protocol::abi::MATERIALIZE_CERTIFIED_NODS_SELECTOR.as_slice())
    {
        return dispatch_materialization(storage, scope, parent, data, caller);
    }
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            settleNod(c) => mutate(c, caller, |sender, c| {
                runtime::settle_nod(
                    &storage,
                    scope,
                    parent,
                    sender,
                    WwdEntityId::from(c.nodId),
                    c.asset,
                    c.snapshotId,
                )?;
                Ok(INodFactory::settleNodReturn {})
            }),
            quoteSettlement(c) => view(c, |c| {
                let (settlement_currency, amount, snapshot_id) = runtime::quote_settlement(
                    &storage,
                    scope,
                    parent,
                    WwdEntityId::from(c.nodId),
                    c.asset,
                )?;
                Ok(INodFactory::quoteSettlementReturn {
                    settlementCurrency: settlement_currency,
                    paymentMinor: amount,
                    snapshotId: snapshot_id,
                })
            }),
            mineGratis(c) => mutate(c, caller, |sender, c| {
                let auth = outbe_gratisfactory::api::ModifyAuth {
                    mac: c.mac.0,
                    op_nonce: c.opNonce,
                };
                runtime::mine_gratis(
                    &storage,
                    scope,
                    parent,
                    runtime::MineGratisRequest {
                        caller: sender,
                        nod_id: WwdEntityId::from(c.nodId),
                        nonce: c.nonce,
                        auth,
                    },
                )
            }),
            materializationHead(c) => view(c, |_| {
                let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
                let head =
                    outbe_nod::NodContract::new(storage.clone()).ocomp_materialization_head()?;
                let (exists, canonical_head) =
                    crate::materialization::encode_materialization_head(head, &limits)?;
                Ok(INodFactory::materializationHeadReturn {
                    exists,
                    canonicalHead: canonical_head,
                })
            }),
            materializeCertifiedNods(_) => Err(PrecompileError::Fatal(
                "Nod materialization bypassed its bounded dispatcher".into(),
            )),
        }
    })
}

fn dispatch_materialization(
    storage: outbe_primitives::storage::StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    data: &[u8],
    caller: Address,
) -> Result<Bytes> {
    crate::materialization::authorize_materializer(storage.clone(), caller)
        .map_err(crate::materialization::typed_materialization_error)?;
    let profile = outbe_chain_constants::NodMaterializationProfileV1 {
        batch_subtree_height: outbe_chain_constants::get_nod_materialization_batch_subtree_height(),
        retry_interval_blocks: outbe_chain_constants::get_nod_materialization_retry_interval_blocks(
        ),
        max_attempts_per_block:
            outbe_chain_constants::get_nod_materialization_max_attempts_per_block(),
    };
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    storage.clone().with_checkpoint(|| {
        crate::materialization::consume_materialization_attempt(&storage, profile)
            .map_err(crate::materialization::typed_materialization_error)?;
        let batch =
            outbe_ocomp_protocol::abi::decode_materialize_certified_nods_calldata(data, &limits)
                .map_err(|_| {
                    crate::materialization::typed_materialization_error(PrecompileError::from(
                        crate::errors::NodFactoryError::InvalidMaterializationBatchShape,
                    ))
                })?;
        crate::materialization::materialize_after_attempt(
            &storage, scope, parent, &batch, profile, &limits,
        )
        .map_err(crate::materialization::typed_materialization_error)?;
        Ok(Bytes::new())
    })
}
