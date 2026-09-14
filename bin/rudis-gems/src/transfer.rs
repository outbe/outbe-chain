use crate::{
    rpc::Rpc,
    store::{self, SavedTx},
    units,
    wallet::Wallet,
    CHAIN,
};
use alloy_primitives::{keccak256, Address, U256};
use eyre::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transfer {
    chain_id: u64,
    owner: Address,
    recipient: Address,
    amount: U256,
    transaction: SavedTx,
}

pub async fn send(
    rpc: &impl Rpc,
    wallet: &Wallet,
    root: &Path,
    recipient: Address,
    amount: U256,
    dry_run: bool,
) -> Result<()> {
    ensure!(
        !recipient.is_zero(),
        "Recipient must not be the zero address"
    );
    ensure!(!amount.is_zero(), "RUDIS amount must be positive");
    println!(
        "From: {:#x}\nTo: {recipient:#x}\nRUDIS: {}",
        wallet.address,
        units(amount, 18)
    );
    if dry_run {
        wallet
            .prepare_value(rpc, recipient, Vec::new(), amount, CHAIN)
            .await?;
        println!("Gas estimated; balance covers amount plus gas. No transaction sent.");
        return Ok(());
    }
    let directory = root
        .join(CHAIN.to_string())
        .join(format!("{:#x}", wallet.address))
        .join("transfers");
    let _lock = store::lock(&directory)?;
    let pending = directory.join("pending.json");
    let transfer: Transfer =
        if pending.exists() {
            let saved: Transfer = store::read_json(&pending)?;
            ensure!(
            saved.chain_id == CHAIN && saved.owner == wallet.address
                && saved.recipient == recipient && saved.amount == amount,
            "Another transfer is pending; resume it with its original recipient and amount ({})",
            pending.display()
        );
            saved
        } else {
            let raw = wallet
                .prepare_value(rpc, recipient, Vec::new(), amount, CHAIN)
                .await?;
            let saved = Transfer {
                chain_id: CHAIN,
                owner: wallet.address,
                recipient,
                amount,
                transaction: SavedTx {
                    hash: keccak256(&raw),
                    raw: raw.into(),
                },
            };
            store::save_json(&pending, &saved, false)?;
            saved
        };
    let hash = transfer.transaction.hash;
    println!("send: {hash:#x}\nRecovery file: {}", pending.display());
    let receipt = transfer.transaction.receipt(rpc).await?;
    // Archive before clearing the pending operation, so a confirmed hash stays inspectable.
    store::save_json(&directory.join(format!("{hash:#x}.json")), &transfer, false)?;
    fs::remove_file(&pending)?;
    fs::File::open(&directory)?.sync_all()?;
    ensure!(receipt["status"] == "0x1", "Transfer reverted ({hash:#x})");
    println!(
        "Sent {} RUDIS to {recipient:#x}\nTransaction: {hash:#x}",
        units(amount, 18)
    );
    Ok(())
}
