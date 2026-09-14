//! Standalone Rehearsal GEM wallet: no Node.js or external CLI process.
mod abi;
mod confidential;
mod paynote;
mod rpc;
mod store;
mod transfer;
mod wallet;

use abi::{IGem, IGemFactory, IPayNote, IPromis, IRudisFactory, IERC20};
use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use clap::{Args, Parser, Subcommand};
use confidential::Keys;
use eyre::{ensure, Result};
use outbe_tee::protocol::PromisOp;
use rpc::{call, Rpc};
use serde_json::json;
use std::path::{Path, PathBuf};
use store::Journal;
use wallet::Wallet;
use zeroize::Zeroizing;

const CHAIN: u64 = 70860602;
const WUSDC: Address = address!("dD9eD2f161c4F9A642471BCF49331A95F5B2B1d3");
const GEM: Address = address!("0000000000000000000000000000000000001013");
const FACTORY: Address = address!("0000000000000000000000000000000000002013");
const PROMIS: Address = address!("0000000000000000000000000000000000001337");
const PROMIS_FACTORY: Address = address!("0000000000000000000000000000000000002337");
const PAYNOTE: Address = address!("0000000000000000000000000000000000001019");
const NATIVE_PER_PROMIS_MINOR: u64 = 1_000_000_000_000;

#[derive(Parser)]
#[command(
    name = "rudis-gems",
    about = "GEM -> PayNote settlement -> PROMIS -> native RUDIS (COEN)"
)]
struct Options {
    #[arg(long, global = true, default_value = "https://125.253.92.5")]
    rpc_url: String,
    /// Signing key, with or without 0x. Never transmitted to RPC or saved.
    #[arg(long, global = true, conflicts_with = "address")]
    private_key: Option<String>,
    /// Read-only list without a signing key.
    #[arg(long, global = true)]
    address: Option<Address>,
    /// Recovery files. Default: .rudis-gems under the repository root or current directory.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// GEM IDs, PROMIS loads, WUSDC settlement prices and wallet balances.
    List,
    /// Native RUDIS balance of an address; no private key required.
    Balance { account: Address },
    /// Send native RUDIS to an address.
    Send(SendArgs),
    /// Settle one GEM, mine its PROMIS and convert that exact amount to RUDIS.
    Settle {
        #[arg(long, value_parser = parse_gem_id)]
        gem_id: U256,
        /// Maximum WUSDC settlement cost, e.g. 30 or 26.85907 (gas excluded).
        #[arg(long, value_parser = parse_wusdc)]
        max_settlement: Option<U256>,
        /// Read the plan without proving or submitting transactions.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
struct SendArgs {
    #[arg(value_parser = parse_recipient)]
    to_address: Address,
    /// RUDIS amount, with up to 18 decimal places (gas paid separately).
    #[arg(long, value_parser = parse_rudis)]
    rudis: U256,
    /// Estimate gas and check balance without sending a transaction.
    #[arg(long)]
    dry_run: bool,
}

fn parse_recipient(text: &str) -> Result<Address> {
    let address: Address = text.parse()?;
    ensure!(!address.is_zero(), "Recipient must not be the zero address");
    Ok(address)
}

fn parse_rudis(text: &str) -> Result<U256> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= 18
            && fraction.bytes().all(|b| b.is_ascii_digit()),
        "RUDIS must have at most 18 decimal places"
    );
    let amount = U256::from_str_radix(&format!("{whole}{fraction:0<18}"), 10)?;
    ensure!(!amount.is_zero(), "RUDIS amount must be positive");
    Ok(amount)
}

fn parse_gem_id(text: &str) -> Result<U256> {
    let value = match text.strip_prefix("0x") {
        Some(hex) => U256::from_str_radix(hex, 16),
        None => U256::from_str_radix(text, 10),
    }
    .map_err(|_| eyre::eyre!("GEM ID must be a uint256 in decimal or 0x hex"))?;
    ensure!(!value.is_zero(), "GEM ID must be positive");
    Ok(value)
}

fn parse_wusdc(text: &str) -> Result<U256> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= 6
            && fraction.bytes().all(|b| b.is_ascii_digit()),
        "WUSDC must have at most six decimal places"
    );
    Ok(U256::from_str_radix(&format!("{whole}{fraction:0<6}"), 10)?)
}

fn units(amount: U256, decimals: usize) -> String {
    let raw = amount.to_string();
    if decimals == 0 {
        return raw;
    }
    let raw = format!("{raw:0>width$}", width = decimals + 1);
    let (whole, fraction) = raw.split_at(raw.len() - decimals);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        whole.to_owned()
    } else {
        format!("{whole}.{fraction}")
    }
}

fn state_name(state: u8) -> &'static str {
    match state {
        0 => "Issued",
        1 => "Qualified",
        2 => "Called",
        3 => "Settled",
        _ => "Unknown",
    }
}

fn settleable(item: &IGem::GemData, timestamp: u64) -> Result<()> {
    ensure!(
        item.state == 1 || item.state == 2,
        "GEM is {}; settlement requires Qualified or Called",
        state_name(item.state)
    );
    if item.state == 2 {
        let deadline = item
            .calledAt
            .checked_add(u64::from(item.callNoticePeriod))
            .ok_or_else(|| eyre::eyre!("GEM deadline overflow"))?;
        ensure!(timestamp <= deadline, "GEM settlement deadline expired");
    }
    Ok(())
}

fn state_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    for ancestor in cwd.ancestors() {
        if ancestor.join("bin/rudis-gems/Cargo.toml").is_file() {
            return Ok(ancestor.join(".rudis-gems"));
        }
    }
    Ok(cwd.join(".rudis-gems"))
}

async fn balances(rpc: &impl Rpc, owner: Address, block: &str) -> Result<()> {
    let stable = call(rpc, WUSDC, IERC20::balanceOfCall { account: owner }, block).await?;
    let native = rpc::quantity(&rpc.request("eth_getBalance", json!([owner, block])).await?)?;
    println!(
        "WUSDC: {}\nRUDIS (COEN): {}",
        units(stable, 6),
        units(native, 18)
    );
    Ok(())
}

async fn list(rpc: &impl Rpc, owner: Address) -> Result<()> {
    let block = rpc::latest(rpc).await?;
    let number = rpc::quantity(&block["number"])?;
    let timestamp: u64 = rpc::quantity(&block["timestamp"])?.try_into()?;
    let tag = format!("{number:#x}");
    println!("Wallet: {owner:#x}\nRudis chain: {CHAIN}, block {number}");
    balances(rpc, owner, &tag).await?;
    let count = call(rpc, GEM, IGem::balanceOfCall { owner }, &tag).await?;
    println!("GEMs: {count}\nGEM ID\tPROMIS\tSettlement WUSDC\tState");
    let mut index = U256::ZERO;
    while index < count {
        let id = call(
            rpc,
            GEM,
            IGem::tokenOfOwnerByIndexCall { owner, index },
            &tag,
        )
        .await?;
        let item = call(rpc, GEM, IGem::getGemStatusCall { gemId: id }, &tag).await?;
        let cost = if item.state == 3 {
            "0 (paid)".to_owned()
        } else {
            match call(
                rpc,
                FACTORY,
                IGemFactory::quoteSettlementCall {
                    gemId: id,
                    asset: WUSDC,
                },
                &tag,
            )
            .await
            {
                Ok(quote) => units(quote.payableUnits, 6),
                Err(error) => format!("unavailable ({error})"),
            }
        };
        let expired = item.state == 2 && settleable(&item, timestamp).is_err();
        println!(
            "0x{id:064x}\t{}\t{cost}\t{}",
            units(item.promisLoad, 6),
            if expired {
                "Called (expired)"
            } else {
                state_name(item.state)
            }
        );
        index += U256::from(1);
    }
    Ok(())
}

async fn quote(rpc: &impl Rpc, id: U256) -> Result<U256> {
    Ok(call(
        rpc,
        FACTORY,
        IGemFactory::quoteSettlementCall {
            gemId: id,
            asset: WUSDC,
        },
        "latest",
    )
    .await?
    .payableUnits)
}

async fn promis_balance(rpc: &impl Rpc, wallet: &Wallet, keys: &Keys) -> Result<U256> {
    let blob = call(
        rpc,
        PROMIS,
        IPromis::balanceOfCall {
            account: wallet.address,
        },
        "latest",
    )
    .await?;
    keys.balance(wallet.address, &blob)
}

fn pow_nonce(id: U256) -> Result<u64> {
    for nonce in 0..1_000_000u64 {
        if outbe_common::pow::validate_pow(id, nonce).is_ok() {
            return Ok(nonce);
        }
    }
    eyre::bail!("PoW search exhausted one million attempts")
}

fn load_source_note(directory: &Path, amount: U256) -> Result<paynote::Note> {
    let source = directory.join("source-note.json");
    if source.exists() {
        return store::read_json(&source);
    }
    let notes_dir = directory.join("paynotes");
    let legacy = directory.join("deposit-started.json");
    let mut candidate = None;
    if legacy.exists() {
        let marker: serde_json::Value = store::read_json(&legacy)?;
        if let Some(name) = marker["originalNote"].as_str() {
            ensure!(
                name.starts_with("0x")
                    && name.ends_with(".json")
                    && name.len() == 71
                    && name[2..66].bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid original PayNote filename"
            );
            candidate = Some(notes_dir.join(name));
        }
    }
    if candidate.is_none() && notes_dir.exists() {
        for entry in std::fs::read_dir(&notes_dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                ensure!(
                    candidate.is_none(),
                    "Multiple PayNotes without a source marker; inspect recovery files"
                );
                candidate = Some(path);
            }
        }
    }
    let note = if let Some(path) = candidate {
        // An old subprocess may have lost its deposit response. Preserve that uncertainty.
        store::save_json(&directory.join("legacy-deposit.json"), &true, false)?;
        store::read_json(&path)?
    } else {
        ensure!(
            !legacy.exists(),
            "Previous deposit was interrupted; inspect its saved transaction before retrying"
        );
        paynote::Note::random(CHAIN, WUSDC, amount)?
    };
    store::save_json(&source, &note, false)?;
    note.save(&notes_dir)?;
    Ok(note)
}

async fn payment(
    rpc: &impl Rpc,
    wallet: &Wallet,
    journal: &mut Journal,
    directory: &Path,
    cost: U256,
) -> Result<Vec<u8>> {
    ensure!(!cost.is_zero(), "Settlement quote is zero");
    let note = load_source_note(directory, cost)?;
    note.validate()?;
    ensure!(
        note.chain_id == CHAIN && note.asset == WUSDC && note.amount()? >= cost,
        "Saved PayNote does not cover this chain/asset/quote"
    );
    if let Some(receipt) = journal.resume(rpc, "deposit").await? {
        let event = paynote::decode_event::<IPayNote::NewNote>(&receipt, PAYNOTE)?;
        ensure!(
            event.commitment == note.commitment
                && event.asset == WUSDC
                && event.noteAmount == note.amount()?,
            "Deposit receipt does not match note"
        );
    }
    let present = call(
        rpc,
        PAYNOTE,
        IPayNote::hasCommitmentCall {
            commitment: note.commitment,
        },
        "latest",
    )
    .await?;
    if !present {
        ensure!(
            !directory.join("legacy-deposit.json").exists(),
            "Earlier PayNote deposit is unconfirmed; no duplicate deposit submitted"
        );
        ensure!(
            call(
                rpc,
                WUSDC,
                IERC20::balanceOfCall {
                    account: wallet.address
                },
                "latest"
            )
            .await?
                >= note.amount()?,
            "Insufficient WUSDC"
        );
        journal.resume(rpc, "approve-reset").await?;
        journal.resume(rpc, "approve").await?;
        let allowance = call(
            rpc,
            WUSDC,
            IERC20::allowanceCall {
                owner: wallet.address,
                spender: PAYNOTE,
            },
            "latest",
        )
        .await?;
        if allowance < note.amount()? {
            if !allowance.is_zero() {
                journal
                    .send(
                        rpc,
                        wallet,
                        "approve-reset",
                        WUSDC,
                        IERC20::approveCall {
                            spender: PAYNOTE,
                            amount: U256::ZERO,
                        }
                        .abi_encode(),
                    )
                    .await?;
                ensure!(
                    call(
                        rpc,
                        WUSDC,
                        IERC20::allowanceCall {
                            owner: wallet.address,
                            spender: PAYNOTE
                        },
                        "latest"
                    )
                    .await?
                    .is_zero(),
                    "Allowance reset failed"
                );
            }
            journal
                .send(
                    rpc,
                    wallet,
                    "approve",
                    WUSDC,
                    IERC20::approveCall {
                        spender: PAYNOTE,
                        amount: note.amount()?,
                    }
                    .abi_encode(),
                )
                .await?;
            ensure!(
                call(
                    rpc,
                    WUSDC,
                    IERC20::allowanceCall {
                        owner: wallet.address,
                        spender: PAYNOTE
                    },
                    "latest"
                )
                .await?
                    >= note.amount()?,
                "Approval failed"
            );
        }
        let receipt = journal
            .send(
                rpc,
                wallet,
                "deposit",
                PAYNOTE,
                IPayNote::depositCall {
                    asset: WUSDC,
                    amount: note.amount()?,
                    noteSn: note.serial()?,
                }
                .abi_encode(),
            )
            .await?;
        let event = paynote::decode_event::<IPayNote::NewNote>(&receipt, PAYNOTE)?;
        ensure!(
            event.commitment == note.commitment
                && event.asset == WUSDC
                && event.noteAmount == note.amount()?,
            "Deposit event mismatch"
        );
    }
    ensure!(
        call(
            rpc,
            PAYNOTE,
            IPayNote::hasCommitmentCall {
                commitment: note.commitment
            },
            "latest"
        )
        .await?,
        "PayNote deposit is not confirmed"
    );
    paynote::spend_proof(
        rpc,
        &note,
        cost,
        wallet.address,
        &directory.join("paynotes"),
    )
    .await
}

async fn settle(
    rpc: &impl Rpc,
    wallet: &Wallet,
    root: &Path,
    id: U256,
    cap: Option<U256>,
    dry_run: bool,
) -> Result<()> {
    let directory = root
        .join(CHAIN.to_string())
        .join(format!("{:#x}", wallet.address))
        .join(format!("0x{id:064x}"));
    // Acquire the OS lock before reading a journal. It releases automatically on process exit.
    let _lock = if dry_run {
        None
    } else {
        Some(store::lock(&directory)?)
    };
    let mut previous = Journal::load(&directory, wallet.address, id, CHAIN)?;
    let item = if previous.as_ref().is_some_and(|j| j.has("mine")) {
        None
    } else {
        Some(call(rpc, GEM, IGem::getGemStatusCall { gemId: id }, "latest").await?)
    };
    let amount = if let Some(item) = &item {
        ensure!(
            item.owner == wallet.address,
            "GEM belongs to another wallet"
        );
        item.promisLoad
    } else {
        U256::from_str_radix(
            &previous
                .as_ref()
                .ok_or_else(|| eyre::eyre!("Missing mint journal"))?
                .amount,
            10,
        )?
    };
    ensure!(!amount.is_zero(), "GEM has no PROMIS load");
    let cost = if let Some(item) = &item {
        if item.state == 3 {
            U256::ZERO
        } else {
            let block = rpc::latest(rpc).await?;
            settleable(item, rpc::quantity(&block["timestamp"])?.try_into()?)?;
            quote(rpc, id).await?
        }
    } else {
        U256::ZERO
    };
    ensure!(
        cap.is_none_or(|cap| cost <= cap),
        "Settlement cost exceeds --max-settlement"
    );
    println!(
        "Wallet: {:#x}\nGEM: 0x{id:064x}\nPROMIS -> RUDIS: {}\nSettlement WUSDC: {}",
        wallet.address,
        units(amount, 6),
        units(cost, 6)
    );
    if dry_run {
        return Ok(());
    }
    let mut journal = match previous.take() {
        Some(journal) => journal,
        None => Journal::new(&directory, wallet.address, id, amount, CHAIN)?,
    };
    ensure!(
        journal.amount == amount.to_string(),
        "GEM load differs from saved operation"
    );
    let keys = confidential::derive(rpc, wallet).await?;
    promis_balance(rpc, wallet, &keys).await?; // Fail an unavailable TEE before depositing WUSDC.
    let settlement = journal.resume(rpc, "settle").await?;
    let settlement = if settlement.is_none() && !cost.is_zero() {
        let proof = payment(rpc, wallet, &mut journal, &directory, cost).await?;
        let fresh = call(rpc, GEM, IGem::getGemStatusCall { gemId: id }, "latest").await?;
        let block = rpc::latest(rpc).await?;
        settleable(&fresh, rpc::quantity(&block["timestamp"])?.try_into()?)?;
        ensure!(
            quote(rpc, id).await? == cost,
            "Settlement quote changed; rerun to regenerate proof from the saved note"
        );
        Some(
            journal
                .send(
                    rpc,
                    wallet,
                    "settle",
                    FACTORY,
                    IGemFactory::settleGemCall {
                        gemId: id,
                        payNoteProof: proof.into(),
                    }
                    .abi_encode(),
                )
                .await?,
        )
    } else {
        settlement
    };
    if let Some(receipt) = settlement {
        let event = paynote::decode_event::<IGemFactory::GemSettled>(&receipt, FACTORY)?;
        ensure!(
            event.gemId == id && event.owner == wallet.address,
            "Settlement receipt mismatch"
        );
    }
    let mint = match journal.resume(rpc, "mine").await? {
        Some(receipt) => receipt,
        None => {
            let item = call(rpc, GEM, IGem::getGemStatusCall { gemId: id }, "latest").await?;
            ensure!(
                item.state == 3 && item.owner == wallet.address && item.promisLoad == amount,
                "GEM not settled with expected owner/load"
            );
            let op_nonce = call(
                rpc,
                PROMIS,
                IPromis::opNonceOfCall {
                    account: wallet.address,
                },
                "latest",
            )
            .await?;
            let nonce = pow_nonce(id)?;
            let mac = keys.mac(wallet.address, PromisOp::Mint, amount, op_nonce, CHAIN);
            println!("PoW nonce: {nonce}; mint MAC computed (opNonce {op_nonce})");
            journal
                .send(
                    rpc,
                    wallet,
                    "mine",
                    FACTORY,
                    IGemFactory::minePromisCall {
                        gemId: id,
                        nonce,
                        mac,
                        opNonce: op_nonce,
                    }
                    .abi_encode(),
                )
                .await?
        }
    };
    let event = paynote::decode_event::<IGemFactory::GemMined>(&mint, FACTORY)?;
    ensure!(
        event.gemId == id && event.owner == wallet.address && event.promisLoad == amount,
        "Mint receipt mismatch"
    );
    let conversion = match journal.resume(rpc, "convert").await? {
        Some(receipt) => receipt,
        None => {
            ensure!(
                promis_balance(rpc, wallet, &keys).await? >= amount,
                "Insufficient PROMIS to convert this GEM's amount"
            );
            let op_nonce = call(
                rpc,
                PROMIS,
                IPromis::opNonceOfCall {
                    account: wallet.address,
                },
                "latest",
            )
            .await?;
            let mac = keys.mac(wallet.address, PromisOp::Burn, amount, op_nonce, CHAIN);
            println!("Burn MAC computed with fresh opNonce {op_nonce}");
            journal
                .send(
                    rpc,
                    wallet,
                    "convert",
                    PROMIS_FACTORY,
                    IRudisFactory::mineRudisCall {
                        amount,
                        mac,
                        opNonce: op_nonce,
                    }
                    .abi_encode(),
                )
                .await?
        }
    };
    let event = paynote::decode_event::<IRudisFactory::RudisMined>(&conversion, PROMIS_FACTORY)?;
    let native = amount
        .checked_mul(U256::from(NATIVE_PER_PROMIS_MINOR))
        .ok_or_else(|| eyre::eyre!("Native amount overflow"))?;
    ensure!(
        event.sender == wallet.address && event.amount == native,
        "Conversion receipt mismatch"
    );
    println!(
        "Done: {} PROMIS converted to RUDIS (COEN)",
        units(amount, 6)
    );
    balances(rpc, wallet.address, "latest").await?;
    println!(
        "PROMIS: {}\nRecovery files: {}",
        units(promis_balance(rpc, wallet, &keys).await?, 6),
        directory.display()
    );
    Ok(())
}

impl Options {
    async fn run(mut self) -> Result<()> {
        let private_key = self.private_key.take().map(Zeroizing::new);
        let wallet = private_key
            .as_ref()
            .map(|key| Wallet::new(key))
            .transpose()?;
        let owner = wallet.as_ref().map(|w| w.address).or(self.address);
        let rpc = rpc::Client::new(self.rpc_url)?;
        ensure!(
            rpc::chain_id(&rpc).await? == CHAIN,
            "Expected Rehearsal chain {CHAIN}"
        );
        let command = self.command;
        if matches!(command, Command::List | Command::Settle { .. }) {
            ensure!(
                call(&rpc, WUSDC, IERC20::decimalsCall {}, "latest").await? == 6
                    && call(&rpc, PROMIS, IPromis::decimalsCall {}, "latest").await? == 6,
                "Unsupported token decimals"
            );
        }
        match command {
            Command::List => {
                let owner =
                    owner.ok_or_else(|| eyre::eyre!("Specify --private-key or --address"))?;
                list(&rpc, owner).await
            }
            Command::Balance { account } => {
                let amount = rpc::quantity(
                    &rpc.request("eth_getBalance", json!([account, "latest"]))
                        .await?,
                )?;
                println!("Wallet: {account:#x}\nRUDIS: {}", units(amount, 18));
                Ok(())
            }
            Command::Send(args) => {
                let wallet = wallet.ok_or_else(|| eyre::eyre!("send requires --private-key"))?;
                let root = self.state_dir.map(Ok).unwrap_or_else(state_root)?;
                transfer::send(
                    &rpc,
                    &wallet,
                    &root,
                    args.to_address,
                    args.rudis,
                    args.dry_run,
                )
                .await
            }
            Command::Settle {
                gem_id,
                max_settlement,
                dry_run,
            } => {
                let wallet = wallet.ok_or_else(|| eyre::eyre!("settle requires --private-key"))?;
                let root = self.state_dir.map(Ok).unwrap_or_else(state_root)?;
                settle(&rpc, &wallet, &root, gem_id, max_settlement, dry_run).await
            }
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = Options::parse().run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
