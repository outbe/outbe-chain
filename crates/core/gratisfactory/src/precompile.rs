//! Gratisfactory precompile at `0x2003`. ABI dispatch only — the heavy
//! lifting (Gratis-balance movement + pool commitment / nullifier
//! bookkeeping) lives in [`crate::runtime`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_gratispool::SpendArgs;
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

sol!("../../../contracts/precompiles/src/IGratisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IGratisFactory::IGratisFactoryCalls::abi_decode,
        |call| {
            use IGratisFactory::IGratisFactoryCalls::*;
            match call {
                pledgeGratis(c) => mutate(c, caller, |sender, c| {
                    let (new_root, _leaf_index, _amount) =
                        runtime::pledge_gratis(storage.clone(), sender, c.denomId, c.commitment)?;
                    emit_pledged(&storage, sender, c.denomId, c.commitment)?;
                    Ok(new_root)
                }),
                unpledgeGratis(c) => mutate_void(c, caller, |sender, c| {
                    let args = SpendArgs {
                        merkle_root: c.args.merkleRoot,
                        nullifier_hash: c.args.nullifierHash,
                        denom_id: c.args.denomId,
                        receiver_binding: c.args.receiverBinding,
                        proof: c.args.proof.to_vec(),
                    };
                    let denom_id = args.denom_id;
                    let nullifier = args.nullifier_hash;
                    let amount = runtime::unpledge_gratis(storage.clone(), &args, sender)?;
                    emit_unpledged(&storage, sender, denom_id, amount)?;
                    outbe_gratispool::precompile::emit_nullifier_spent(
                        &storage,
                        nullifier,
                        outbe_gratispool::constants::ACTION_UNPLEDGE as u8,
                    )
                }),
                mineCoen(c) => mutate(c, caller, |sender, c| {
                    runtime::mine_coen(storage.clone(), sender, c.amount)
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}

fn emit_pledged(
    storage: &StorageHandle<'_>,
    account: Address,
    denom_id: u8,
    commitment: U256,
) -> Result<()> {
    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IGratisFactory::GratisPledged {
            account,
            denomId: denom_id,
            commitment,
        }),
    )
}

fn emit_unpledged(
    storage: &StorageHandle<'_>,
    account: Address,
    denom_id: u8,
    amount: U256,
) -> Result<()> {
    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IGratisFactory::GratisUnpledged {
            account,
            denomId: denom_id,
            amount,
        }),
    )
}
