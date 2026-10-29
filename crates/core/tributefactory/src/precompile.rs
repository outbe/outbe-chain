use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;

use crate::runtime::OfferTributeInput;
use crate::schema::TributeFactoryContract;
use crate::worldwideday::queries::worldwide_day;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ITributeFactory.sol"
);

/// Every accepted `offerTribute` executes the pinned UltraHonk verifier, so
/// its base gas must cover verification before any admission checks run.
pub fn base_gas(input: &[u8]) -> u64 {
    let Some(selector) = input.get(..4) else {
        return PRECOMPILE_BASE_GAS;
    };
    if selector == ITributeFactory::offerTributeCall::SELECTOR {
        outbe_primitives::storage::gas::ZK_VERIFY_GAS
    } else {
        PRECOMPILE_BASE_GAS
    }
}

/// Dispatch for the tribute factory precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
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
                getWorldwideDay(c) => view(c, |c| {
                    let factory = TributeFactoryContract::new(storage.clone());
                    worldwide_day(&factory, c)
                }),
                getActiveWorldwideDays(c) => view(c, |_c| {
                    let factory = TributeFactoryContract::new(storage.clone());
                    Ok(factory
                        .active_wwd
                        .read_all()?
                        .into_iter()
                        .map(u32::from)
                        .collect::<Vec<_>>())
                }),
                offerTribute(c) => mutate(&storage, c, caller, |sender, c| {
                    let mut factory = TributeFactoryContract::new(storage.clone());
                    factory
                        .offer_tribute(OfferTributeInput {
                            caller: sender,
                            cipher_text: c.cipherText,
                            nonce: c.nonce,
                            ephemeral_pubkey: c.ephemeralPubkey,
                            worldwide_day: c.worldwideDay.into(),
                            tribute_currency: c.tributeCurrency,
                            reference_currency: c.referenceCurrency,
                            zk_proof: c.zkProof,
                            l2_chain_id: c.chainId,
                            circuit_version: c.version,
                            zk_merkle_root: c.zkMerkleRoot,
                            signature: c.signature,
                        })
                        .map(|id| id.to_u256())
                }),
            }
        },
    )
}

#[cfg(test)]
mod gas_tests {
    use super::*;

    #[test]
    fn offer_tribute_charges_zk_verification_base_gas() {
        assert_eq!(
            base_gas(&ITributeFactory::offerTributeCall::SELECTOR),
            outbe_primitives::storage::gas::ZK_VERIFY_GAS
        );
        assert_eq!(base_gas(&[]), PRECOMPILE_BASE_GAS);
        assert_eq!(base_gas(&[0xde, 0xad, 0xbe, 0xef]), PRECOMPILE_BASE_GAS);
    }
}
