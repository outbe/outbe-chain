//! Gratis mutations share the enclave-resident Gratis/Fidelity ledger.
use crate::precompile::IGratis;
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::{addresses::GRATIS_ADDRESS, error::Result, storage::StorageHandle};
use outbe_tee::{
    pledge_ledger,
    pledgenote::Command,
    protocol::{GratisOp, ModifyAuth},
};

fn apply(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
    op: GratisOp,
    fidelity: bool,
) -> Result<U256> {
    let outcome = pledge_ledger::execute(
        &storage,
        Command::Gratis {
            account,
            amount,
            op,
            auth,
            fidelity,
        },
    )?;
    if op == GratisOp::Mint {
        storage.emit_event(
            GRATIS_ADDRESS,
            IGratis::GratisMinted {
                account,
                amount,
                newTotalSupply: outcome.total_supply,
            }
            .encode_log_data(),
        )?;
    } else {
        storage.emit_event(
            GRATIS_ADDRESS,
            IGratis::GratisBurned {
                account,
                amount,
                remainingSupply: outcome.total_supply,
            }
            .encode_log_data(),
        )?;
    }
    Ok(outcome.total_supply)
}

pub(crate) fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    apply(storage, account, amount, auth, GratisOp::Mint, false).map(|_| ())
}
pub(crate) fn mint_with_fidelity(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    apply(storage, account, amount, auth, GratisOp::Mint, true)
}
pub(crate) fn burn(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    apply(storage, account, amount, auth, GratisOp::Burn, false)
}
pub(crate) fn burn_with_fidelity(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    apply(storage, account, amount, auth, GratisOp::Burn, true)
}
