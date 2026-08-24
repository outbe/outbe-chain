use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::{PrecompileError, Result};

use crate::runtime;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INodFactory.sol"
);

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
    if data.get(..4)
        == Some(outbe_ocomp_protocol::abi::MATERIALIZE_CERTIFIED_NODS_SELECTOR.as_slice())
    {
        return dispatch_materialization(storage, scope, parent, data, caller);
    }
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            settleNod(c) => mutate(c, caller, |sender, c| {
                runtime::settle_nod(&storage, scope, parent, sender, WwdEntityId::from(c.nodId))
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
                    sender,
                    WwdEntityId::from(c.nodId),
                    U256::from(c.nonce),
                    auth,
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
