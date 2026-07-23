use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::dispatch::{dispatch_call, mutate};
use outbe_primitives::error::Result;

use crate::runtime::OfferTributeInput;
use crate::schema::TributeFactoryContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ITributeFactory.sol"
);

/// Dispatch for the tribute factory precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        ITributeFactory::ITributeFactoryCalls::abi_decode,
        |call| {
            use ITributeFactory::ITributeFactoryCalls::*;
            match call {
                offerTribute(c) => mutate(c, caller, |sender, c| {
                    let mut factory = TributeFactoryContract::new(storage);
                    factory
                        .offer_tribute(
                            scope,
                            parent,
                            OfferTributeInput {
                                caller: sender,
                                cipher_text: c.cipherText,
                                nonce: c.nonce,
                                ephemeral_pubkey: c.ephemeralPubkey,
                                reference_currency: c.referenceCurrency,
                                exclude_from_intex_issuance: c.excludeFromIntexIssuance,
                                zk_merkle_root: c.zkMerkleRoot,
                                signature: c.signature,
                            },
                        )
                        .map(|id| Bytes::copy_from_slice(id.as_bytes()))
                }),
            }
        },
    )
}
