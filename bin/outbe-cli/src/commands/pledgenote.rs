//! Local owner-bound pledge notes. Secrets stay in owner-only files.
use super::{parse_amount, paynote::save_json};
use crate::rpc::Rpc;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use clap::Subcommand;
use eyre::{ensure, Result};
use outbe_gratis::{
    api::unpledge_context,
    client::{self, Note},
};
use outbe_gratisfactory::precompile::IGratisFactory;
use outbe_primitives::addresses::{GRATIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_protocol::codec;
use outbe_vaultrouter::api::IVaultRouter;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Subcommand)]
pub enum PledgeNoteCmd {
    /// Prepare a note before pledgeGratis; modify_key_file contains a 32-byte hex key.
    Prepare {
        owner: Address,
        #[arg(value_parser = parse_amount)]
        amount: U256,
        nonce: u64,
        modify_key_file: PathBuf,
    },
    /// Generate a proof bound to the exact stored reservation; retain its return key.
    IssueProof { note: PathBuf, reservation_id: U256 },
    /// Generate a withdrawal proof paying the note's original owner.
    UnpledgeProof {
        note: PathBuf,
        #[arg(value_parser = parse_amount)]
        amount: U256,
    },
    /// Recover a repayment note using the saved issue receipt and cumulative release.
    ReturnNote {
        issue_receipt: PathBuf,
        position_id: U256,
        #[arg(value_parser = parse_amount)]
        amount: U256,
        #[arg(value_parser = parse_amount)]
        released_total: U256,
    },
}
impl PledgeNoteCmd {
    pub async fn run(self, rpc: &(impl Rpc + Sync)) -> Result<()> {
        let dir = Path::new("./pledgenotes");
        let chain_id = rpc.eth_chain_id().await?;
        let output = match self {
            Self::Prepare {
                owner,
                amount,
                nonce,
                modify_key_file,
            } => {
                let text = Zeroizing::new(fs::read_to_string(modify_key_file)?);
                let bytes = Zeroizing::new(alloy_primitives::hex::decode(
                    text.trim().trim_start_matches("0x"),
                )?);
                let key: &[u8; 32] = bytes.as_slice().try_into()?;
                let note = Note::initial(chain_id, owner, key, amount, nonce)?;
                let path = save_note(dir, &note)?;
                let mut preimage = b"outbe/gratis/modify/v1".to_vec();
                preimage.extend_from_slice(owner.as_slice());
                preimage.push(outbe_tee::protocol::GratisOp::Pledge as u8);
                preimage.extend_from_slice(&amount.to_be_bytes::<32>());
                preimage.extend_from_slice(&nonce.to_be_bytes());
                preimage.extend_from_slice(&U256::from(chain_id).to_be_bytes::<32>());
                let mac = B256::from_slice(
                    ring::hmac::sign(
                        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key),
                        &preimage,
                    )
                    .as_ref(),
                );
                json!({"note": path, "commitment": note.commitment()?, "amount": amount, "auth": {"mac": mac, "opNonce": nonce}})
            }
            Self::IssueProof {
                note,
                reservation_id,
            } => {
                let note = load_note(&note, chain_id)?;
                let r = IVaultRouter::reservationOfCall::abi_decode_returns_validate(
                    &rpc.eth_call(
                        VAULT_ROUTER_ADDRESS,
                        &IVaultRouter::reservationOfCall { id: reservation_id }.abi_encode(),
                    )
                    .await?,
                )?;
                ensure!(!r.asset.is_zero(), "reservation not found");
                let context = outbe_credisfactory::runtime::reservation_context(
                    chain_id,
                    reservation_id,
                    &r,
                )?;
                let tree = read_tree(rpc, chain_id).await?;
                ensure_unspent(rpc, &note).await?;
                let proof = client::prove_issue(&note, &tree, r.gratisMinor, context)?;
                let change = note
                    .change(r.gratisMinor)?
                    .map(|n| save_note(dir, &n))
                    .transpose()?;
                // Persist the input and context before exposing a usable proof. They recover every return note.
                let path = save_json(
                    dir,
                    &format!(
                        "issue-{reservation_id}-{:#x}.json",
                        alloy_primitives::keccak256(&proof)
                    ),
                    &json!({"note": note, "context": context, "reservationId": reservation_id, "proof": Bytes::from(proof)}),
                )?;
                json!({"receipt": path, "change": change, "reservationId": reservation_id})
            }
            Self::UnpledgeProof { note, amount } => {
                let note = load_note(&note, chain_id)?;
                let context = unpledge_context(chain_id, note.owner, amount)?;
                let tree = read_tree(rpc, chain_id).await?;
                ensure_unspent(rpc, &note).await?;
                let proof = client::prove_unpledge(&note, &tree, amount, context)?;
                let change = note
                    .change(amount)?
                    .map(|n| save_note(dir, &n))
                    .transpose()?;
                let path = save_json(
                    dir,
                    &format!(
                        "unpledge-{:#x}-{amount}.json",
                        alloy_primitives::keccak256(&proof)
                    ),
                    &json!({"proof": Bytes::from(proof)}),
                )?;
                json!({"proofFile": path, "change": change, "destination": note.owner})
            }
            Self::ReturnNote {
                issue_receipt,
                position_id,
                amount,
                released_total,
            } => {
                let text = Zeroizing::new(fs::read(issue_receipt)?);
                let receipt: Value = serde_json::from_slice(&text)?;
                let note: Note = serde_json::from_value(receipt["note"].clone())?;
                ensure!(note.chain_id == chain_id, "note chain mismatch");
                let context: B256 = serde_json::from_value(receipt["context"].clone())?;
                let returned = note.returned(context, position_id, amount, released_total)?;
                let tree = read_tree(rpc, chain_id).await?;
                ensure!(
                    tree.leaves()
                        .contains(&codec::field_from_b256(&returned.commitment()?)?),
                    "return note not confirmed on chain"
                );
                json!({"note": save_note(dir, &returned)?, "commitment": returned.commitment()?})
            }
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        Ok(())
    }
}
fn save_note(dir: &Path, note: &Note) -> Result<PathBuf> {
    save_json(dir, &format!("{:#x}.json", note.commitment()?), note)
}
fn load_note(path: &Path, chain_id: u64) -> Result<Note> {
    let data = Zeroizing::new(fs::read(path)?);
    let note: Note = serde_json::from_slice(&data)?;
    ensure!(note.chain_id == chain_id, "note chain mismatch");
    note.commitment()?;
    Ok(note)
}
async fn ensure_unspent(rpc: &impl Rpc, note: &Note) -> Result<()> {
    let used = IGratisFactory::pledgeSpentCall::abi_decode_returns_validate(
        &rpc.eth_call(
            GRATIS_FACTORY_ADDRESS,
            &IGratisFactory::pledgeSpentCall {
                nullifier: note.nullifier()?,
            }
            .abi_encode(),
        )
        .await?,
    )?;
    ensure!(!used, "pledge note already spent");
    Ok(())
}
async fn read_tree(rpc: &impl Rpc, chain_id: u64) -> Result<outbe_zk_canonical::pledgenote::Tree> {
    let head = rpc.eth_block_number().await?;
    let tag = format!("0x{head:x}");
    let mut tree = client::new_tree(chain_id)?;
    let mut from = 0u64;
    // ponytail: scan all note events, cache the tree when pool history becomes costly.
    loop {
        let to = from.saturating_add(999u64).min(head);
        let logs = rpc
            .eth_get_logs(
                GRATIS_FACTORY_ADDRESS,
                &[Some(format!(
                    "{:#x}",
                    IGratisFactory::PledgeNote::SIGNATURE_HASH
                ))],
                &format!("0x{from:x}"),
                &format!("0x{to:x}"),
            )
            .await?;
        let mut events = Vec::new();
        for log in logs {
            ensure!(
                log.get("removed").and_then(Value::as_bool) != Some(true),
                "removed pledge log"
            );
            let address: Address = serde_json::from_value(log["address"].clone())?;
            ensure!(address == GRATIS_FACTORY_ADDRESS, "wrong pledge emitter");
            let topics: Vec<B256> = serde_json::from_value(log["topics"].clone())?;
            let data: Bytes = serde_json::from_value(log["data"].clone())?;
            events.push(IGratisFactory::PledgeNote::decode_raw_log_validate(
                topics, &data,
            )?);
        }
        events.sort_by_key(|e| e.leafIndex);
        for event in events {
            ensure!(
                usize::try_from(event.leafIndex)? == tree.leaves().len(),
                "missing or duplicate pledge event"
            );
            tree.append(codec::field_from_b256(&event.commitment)?)?;
            ensure!(
                codec::field_to_b256(&tree.root())? == event.rootAfter,
                "pledge event root mismatch"
            );
        }
        if to == head {
            break;
        }
        from = to + 1;
    }
    let root = IGratisFactory::pledgeRootCall::abi_decode_returns_validate(
        &rpc.eth_call_at(
            GRATIS_FACTORY_ADDRESS,
            &IGratisFactory::pledgeRootCall {}.abi_encode(),
            &tag,
        )
        .await?,
    )?;
    ensure!(
        !tree.leaves().is_empty() && root == codec::field_to_b256(&tree.root())?,
        "pledge history does not match chain"
    );
    Ok(tree)
}
