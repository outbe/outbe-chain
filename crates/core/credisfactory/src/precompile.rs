//! ABI dispatch for the credisfactory precompile at `CREDIS_FACTORY_ADDRESS`.
//!
//! `requestCredis` consumes a pool commitment via the supplied ZK proof and
//! opens a credis position bound to `bundleAccount`. `anadosis` advances the
//! schedule and inserts the caller-supplied reclaim commitment for that
//! installment into the gratispool so the holder of the reclaim secret can
//! `unpledgeGratis` one installment's share immediately.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::runtime::RequestArgs;

sol!("../../../contracts/precompiles/src/ICredisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        ICredisFactory::ICredisFactoryCalls::abi_decode,
        |call| {
            use ICredisFactory::ICredisFactoryCalls::*;
            match call {
                requestCredis(c) => mutate(c, caller, |sender, c| {
                    let timestamp = read_timestamp(&storage)?;
                    let block_number = storage.block_number()?;
                    let args = RequestArgs {
                        merkle_root: c.args.merkleRoot,
                        nullifier_hash: c.args.nullifierHash,
                        denom_id: c.args.denomId,
                        receiver_binding: c.args.receiverBinding,
                        proof: c.args.proof.to_vec(),
                    };
                    let (position_id, amount_stables) = runtime::request_credis(
                        storage.clone(),
                        sender,
                        c.asset,
                        c.bundleAccount,
                        args,
                        timestamp,
                        block_number,
                    )?;
                    Ok(ICredisFactory::requestCredisReturn {
                        positionId: position_id,
                        amountStables: amount_stables,
                    })
                }),
                anadosis(c) => mutate_void(c, caller, |sender, c| {
                    let timestamp = read_timestamp(&storage)?;
                    let block_number = storage.block_number()?;
                    runtime::pay_anadosis(
                        storage.clone(),
                        sender,
                        c.positionId,
                        c.reclaimCommitment,
                        timestamp,
                        block_number,
                    )?;
                    Ok(())
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}

fn read_timestamp(storage: &StorageHandle<'_>) -> Result<u64> {
    Ok(storage.timestamp()?.to::<u64>())
}
