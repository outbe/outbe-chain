//! Local bearer notes and proofs for the existing PayNote pool.

mod merge;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use alloy_primitives::{keccak256, Address, FixedBytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use clap::{Args, Subcommand};
use eyre::{ensure, Result, WrapErr};
use outbe_paynote::api::{settlement_context, SettlementDomain};
use outbe_paynote::Field;
use outbe_paynote::{
    client::{new_tree, witness},
    hash::{change_key, note_commitment, note_nullifier, note_sn},
    precompile::IPayNote,
    PayNoteTree,
};
use outbe_primitives::addresses::{
    GEM_FACTORY_ADDRESS, INTEX_FACTORY_ADDRESS, NOD_FACTORY_ADDRESS, PAYNOTE_ADDRESS,
};
use outbe_protocol::{
    codec::{field_to_b256, FieldElement},
    protocol::zk::{Circuit, CircuitId, ProofGenerator},
};
use outbe_zk_backend::barretenberg::{verify_circuit, Barretenberg};
use outbe_zk_canonical::noir::paynote::{
    alloy::{PublicInputs, Witness},
    Paynote,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::{parse_amount, require_signer};
use crate::{
    rpc::{wait_receipt, Rpc},
    tx::TxSigner,
};

sol!("../../contracts/tokens/src/interfaces/IERC20.sol");

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod settlement_abi {
    alloy_sol_types::sol!("../../contracts/precompiles/src/INodFactory.sol");
    alloy_sol_types::sol!("../../contracts/precompiles/src/IGemFactory.sol");
    alloy_sol_types::sol!("../../contracts/precompiles/src/IIntexFactory.sol");
}
use settlement_abi::{IGemFactory, IIntexFactory, INodFactory};

/// Deposit shielded paynotes and generate spend proofs.
///
/// Assets are ERC20 addresses. Amounts are positive integers in token base units.
/// Notes and proofs are saved under ./paynotes. Note files contain bearer secrets.
/// Deposits approve tokens when needed. Partial spends create a change note,
/// which becomes spendable after the proof is consumed on-chain.
/// Generating a proof does not submit a spend transaction.
///
/// Examples:
///   outbe-cli --rpc-url http://localhost:8545 --private-key "$PRIVATE_KEY" \
///     paynote deposit "$ASSET_ADDRESS" 1000000
///
///   outbe-cli --rpc-url http://localhost:8545 \
///     paynote spend-proof ./paynotes/0xCOMMITMENT.json 600000 --nod "$NOD_ID"
///
///   Bind an Intex holding. The series id is exactly 14 bytes, --owner owns the
///   units, and --units is the number of units being settled, which can differ
///   from the token amount:
///   outbe-cli --rpc-url http://localhost:8545 \
///     paynote spend-proof 0xCOMMITMENT 600000 --intex 20260212-TRY-U \
///     --owner "$OWNER_ADDRESS" --units 2
#[derive(Subcommand)]
#[command(verbatim_doc_comment)]
pub enum PaynoteCmd {
    /// Generate a private-value merge proof and durably retain its output note.
    MergeProof {
        #[arg(required = true, num_args = 2..=4)]
        paynotes: Vec<String>,
    },
    /// Merge selected notes, using successive batches for more than four inputs.
    Merge {
        #[arg(required_unless_present = "resume", conflicts_with = "resume", num_args = 2..)]
        paynotes: Vec<String>,
        /// Resume an immutable operation file after interruption or reorganization.
        #[arg(long)]
        resume: Option<PathBuf>,
    },
    /// Reconcile notes with a canonical snapshot. With no arguments, it checks ./paynotes.
    Status { paynotes: Vec<String> },
    /// Deposit ERC20 base units. Save the bearer secret in ./paynotes.
    Deposit {
        asset: Address,
        #[arg(value_parser = parse_amount)]
        amount: U256,
    },
    /// Generate a proof only. Change becomes spendable after on-chain consumption.
    SpendProof {
        /// Note JSON path or commitment ID from a previous command.
        paynote: String,
        #[arg(value_parser = parse_amount)]
        amount: U256,
        #[command(flatten)]
        target: SpendTarget,
        /// Units of `--intex` this proof settles.
        #[arg(long, conflicts_with_all = ["nod", "gem"], value_parser = parse_amount)]
        units: Option<U256>,
        /// Owner of the `--intex` units this proof settles.
        #[arg(long, conflicts_with_all = ["nod", "gem"])]
        owner: Option<Address>,
    },
}

/// Exactly one right the proof is allowed to settle.
#[derive(Args, Debug, Clone)]
#[group(required = true, multiple = false)]
pub(crate) struct SpendTarget {
    /// Nod id this proof may settle.
    #[arg(long)]
    nod: Option<U256>,
    /// Gem id this proof may settle.
    #[arg(long)]
    gem: Option<U256>,
    /// 14-byte Intex series id this proof may settle.
    #[arg(long, requires_all = ["units", "owner"])]
    intex: Option<String>,
}

impl PaynoteCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        let dir = Path::new("./paynotes");
        let output = match self {
            Self::MergeProof { paynotes } => merge::proof(client, dir, &paynotes).await?,
            Self::Merge { paynotes, resume } => {
                let signer = require_signer(private_key)?;
                merge::run(client, &signer, dir, &paynotes, resume.as_deref()).await?
            }
            Self::Status { paynotes } => merge::status(client, dir, &paynotes).await?,
            Self::Deposit { asset, amount } => {
                let signer = require_signer(private_key)?;
                let note = Note::random(client.eth_chain_id().await?, asset, amount)?;
                deposit(client, &signer, dir, &note).await?
            }
            Self::SpendProof {
                paynote,
                amount,
                target,
                units,
                owner,
            } => {
                let note = load_note(&resolve_note(dir, &paynote))?;
                let target = target.into_settlement(units, owner)?;
                spend_proof(client, dir, &note, amount, target).await?
            }
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        Ok(())
    }
}

/// The settlement a spend proof is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettlementTarget {
    Nod(U256),
    Gem(U256),
    Intex {
        series: [u8; 14],
        owner: Address,
        units: U256,
    },
}

impl SpendTarget {
    fn into_settlement(
        self,
        units: Option<U256>,
        owner: Option<Address>,
    ) -> Result<SettlementTarget> {
        if let Some(nod) = self.nod {
            return Ok(SettlementTarget::Nod(nod));
        }
        if let Some(gem) = self.gem {
            return Ok(SettlementTarget::Gem(gem));
        }
        let Some(series) = self.intex else {
            return Err(eyre::eyre!("a settlement target is required"));
        };
        let Some(units) = units else {
            return Err(eyre::eyre!("--intex requires --units"));
        };
        let Some(owner) = owner else {
            return Err(eyre::eyre!("--intex requires --owner"));
        };
        ensure!(!owner.is_zero(), "--owner must not be the zero address");
        let bytes = series.as_bytes();
        ensure!(
            bytes.len() == 14,
            "intex series id must be exactly 14 bytes"
        );
        let mut series_id = [0u8; 14];
        series_id.copy_from_slice(bytes);
        Ok(SettlementTarget::Intex {
            series: series_id,
            owner,
            units,
        })
    }
}

struct QuotedSettlement {
    context: B256,
    domain: &'static str,
    target: B256,
    series: Option<String>,
    owner: Option<Address>,
    units: U256,
    snapshot_id: U256,
}

async fn quote_settlement(
    client: &impl Rpc,
    note: &Note,
    amount: U256,
    target: SettlementTarget,
) -> Result<QuotedSettlement> {
    let (domain, target_word, owner, units, quote_units, snapshot_id, domain_tag) = match target {
        SettlementTarget::Nod(id) => {
            let quote = call(
                client,
                NOD_FACTORY_ADDRESS,
                INodFactory::quoteSettlementCall {
                    nodId: id,
                    asset: note.asset,
                },
            )
            .await?;
            (
                "nod",
                B256::from(id),
                None,
                U256::ONE,
                quote.paymentMinor,
                quote.snapshotId,
                SettlementDomain::Nod,
            )
        }
        SettlementTarget::Gem(id) => {
            let quote = call(
                client,
                GEM_FACTORY_ADDRESS,
                IGemFactory::quoteSettlementCall {
                    gemId: id,
                    asset: note.asset,
                },
            )
            .await?;
            (
                "gem",
                B256::from(id),
                None,
                U256::ONE,
                quote.paymentMinor,
                quote.snapshotId,
                SettlementDomain::Gem,
            )
        }
        SettlementTarget::Intex {
            series,
            owner,
            units,
        } => {
            let quote = call(
                client,
                INTEX_FACTORY_ADDRESS,
                IIntexFactory::quoteSettlementCall {
                    seriesId: FixedBytes::from(series),
                    asset: note.asset,
                    units,
                },
            )
            .await?;
            (
                "intex",
                outbe_paynote::api::intex_holding_target(&series, owner),
                Some(owner),
                units,
                quote.paymentMinor,
                quote.snapshotId,
                SettlementDomain::Intex,
            )
        }
    };
    ensure!(
        quote_units == amount,
        "quoted payable units {quote_units} do not match spend amount {amount}"
    );
    let context = settlement_context(domain_tag, target_word, units, snapshot_id)
        .map_err(|error| eyre::eyre!("{error}"))?;
    Ok(QuotedSettlement {
        context,
        domain,
        target: target_word,
        series: match target {
            SettlementTarget::Intex { series, .. } => {
                Some(String::from_utf8_lossy(&series).into_owned())
            }
            SettlementTarget::Nod(_) | SettlementTarget::Gem(_) => None,
        },
        owner,
        units,
        snapshot_id,
    })
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    version: u8,
    chain_id: u64,
    pool: Address,
    asset: Address,
    #[serde(with = "decimal_amount")]
    amount: U256,
    spend_key: B256,
    commitment: B256,
}

mod decimal_amount {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        amount: &U256,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&amount.to_string())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<U256, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_amount(&text).map_err(serde::de::Error::custom)
    }
}

impl Note {
    fn new(chain_id: u64, asset: Address, amount: U256, key: Field) -> Result<Self> {
        ensure!(!asset.is_zero(), "asset must be a non-zero ERC20 address");
        ensure!(!amount.is_zero(), "amount must be non-zero");
        ensure!(key != Field::from(0), "spend key must be non-zero");
        let serial = note_sn(key)?;
        ensure!(serial != Field::from(0), "note serial must be non-zero");
        let commitment = field_to_b256(&note_commitment(chain_id, serial, asset, amount)?)?;
        ensure!(commitment != B256::ZERO, "commitment must be non-zero");
        Ok(Self {
            version: 1,
            chain_id,
            pool: PAYNOTE_ADDRESS,
            asset,
            amount,
            spend_key: field_to_b256(&key)?,
            commitment,
        })
    }

    fn random(chain_id: u64, asset: Address, amount: U256) -> Result<Self> {
        let rng = SystemRandom::new();
        let mut bytes = Zeroizing::new([0u8; 32]);
        loop {
            rng.fill(bytes.as_mut())
                .map_err(|_| eyre::eyre!("spend-key randomness unavailable"))?;
            // Rejection sampling avoids reducing random words modulo the field.
            if let Ok(key) = B256::from(*bytes).to_field() {
                if key != Field::from(0) {
                    return Self::new(chain_id, asset, amount, key);
                }
            }
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.pool == PAYNOTE_ADDRESS,
            "unsupported note format or pool"
        );
        let derived = Self::new(self.chain_id, self.asset, self.amount, self.key()?)?;
        ensure!(
            derived.commitment == self.commitment,
            "note commitment does not match its contents"
        );
        Ok(())
    }

    fn key(&self) -> Result<Field> {
        Ok(self.spend_key.to_field()?)
    }

    fn nullifier(&self) -> Result<B256> {
        Ok(field_to_b256(&note_nullifier(
            self.commitment.to_field()?,
            self.key()?,
        )?)?)
    }

    fn change(&self, amount: U256) -> Result<Option<Self>> {
        ensure!(!amount.is_zero(), "spend amount must be non-zero");
        let remaining = self
            .amount
            .checked_sub(amount)
            .ok_or_else(|| eyre::eyre!("spend exceeds note amount"))?;
        if remaining.is_zero() {
            return Ok(None);
        }
        Ok(Some(Self::new(
            self.chain_id,
            self.asset,
            remaining,
            change_key(self.key()?, self.nullifier()?.to_field()?)?,
        )?))
    }
}

fn resolve_note(dir: &Path, argument: &str) -> PathBuf {
    match argument.parse::<B256>() {
        Ok(id) => dir.join(format!("{id:#x}.json")),
        Err(_) => PathBuf::from(argument),
    }
}

fn load_note(path: &Path) -> Result<Note> {
    let bytes =
        Zeroizing::new(fs::read(path).wrap_err_with(|| format!("read note {}", path.display()))?);
    // Do not include a deserializer error that might echo a bearer secret.
    let note: Note =
        serde_json::from_slice(&bytes).map_err(|_| eyre::eyre!("invalid note JSON"))?;
    note.validate()?;
    Ok(note)
}

fn private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => {
            // Persist the new directory entry as well as the file it will hold.
            #[cfg(unix)]
            fs::File::open(
                dir.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(dir)?.is_dir(),
                "{} must be a directory, not a symlink",
                dir.display()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    fs::metadata(dir)?.permissions().mode() & 0o022 == 0,
                    "{} must not be writable by other users",
                    dir.display()
                );
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Immutable, durable publication: interruption leaves either no file or all of it.
pub(super) fn save_json(dir: &Path, name: &str, value: &impl Serialize) -> Result<PathBuf> {
    private_dir(dir)?;
    let path = dir.join(name);
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(value)?);
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?; // owner-only on Unix
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file(),
                "refusing to overwrite {}",
                path.display()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    metadata.permissions().mode() & 0o077 == 0,
                    "{} must have owner-only permissions",
                    path.display()
                );
            }
            let existing = Zeroizing::new(fs::read(&path)?);
            ensure!(
                *existing == *bytes,
                "refusing to overwrite different contents in {}",
                path.display()
            );
        }
        Err(error) => return Err(error.error.into()),
    }
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    Ok(path)
}

fn save_note(dir: &Path, note: &Note) -> Result<PathBuf> {
    note.validate()?;
    save_json(dir, &format!("{:#x}.json", note.commitment), note)
}

async fn call<C: SolCall>(client: &impl Rpc, to: Address, request: C) -> Result<C::Return> {
    Ok(C::abi_decode_returns_validate(
        &client.eth_call(to, &request.abi_encode()).await?,
    )?)
}

async fn send(
    client: &(impl Rpc + Sync),
    signer: &TxSigner,
    to: Address,
    data: Vec<u8>,
) -> Result<Value> {
    let hash = signer.send_tx(client, to, data, U256::ZERO).await?;
    eprintln!("Submitted {hash}");
    wait_receipt(client, &hash, Duration::from_secs(120)).await
}

async fn allowance(client: &impl Rpc, asset: Address, owner: Address) -> Result<U256> {
    call(
        client,
        asset,
        IERC20::allowanceCall {
            owner,
            spender: PAYNOTE_ADDRESS,
        },
    )
    .await
}

async fn deposit(
    client: &(impl Rpc + Sync),
    signer: &TxSigner,
    dir: &Path,
    note: &Note,
) -> Result<Value> {
    let path = save_note(dir, note)?;
    eprintln!("Saved bearer note {}", path.display());
    let result: Result<Value> = async {
        let current = allowance(client, note.asset, signer.address()).await?;
        if current < note.amount {
            if !current.is_zero() {
                send(
                    client,
                    signer,
                    note.asset,
                    IERC20::approveCall {
                        spender: PAYNOTE_ADDRESS,
                        amount: U256::ZERO,
                    }
                    .abi_encode(),
                )
                .await?;
                ensure!(
                    allowance(client, note.asset, signer.address())
                        .await?
                        .is_zero(),
                    "allowance reset failed"
                );
            }
            send(
                client,
                signer,
                note.asset,
                IERC20::approveCall {
                    spender: PAYNOTE_ADDRESS,
                    amount: note.amount,
                }
                .abi_encode(),
            )
            .await?;
            ensure!(
                allowance(client, note.asset, signer.address()).await? >= note.amount,
                "approval did not establish sufficient allowance"
            );
        }
        let receipt = send(
            client,
            signer,
            PAYNOTE_ADDRESS,
            IPayNote::depositCall {
                asset: note.asset,
                amount: note.amount,
                noteSn: field_to_b256(&note_sn(note.key()?)?)?,
            }
            .abi_encode(),
        )
        .await?;
        let logs = receipt
            .get("logs")
            .and_then(Value::as_array)
            .ok_or_else(|| eyre::eyre!("deposit receipt has no logs"))?;
        let mut found = None;
        for log in logs {
            if log
                .get("address")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Address>().ok())
                != Some(PAYNOTE_ADDRESS)
                || log
                    .pointer("/topics/0")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<B256>().ok())
                    != Some(IPayNote::NewNote::SIGNATURE_HASH)
            {
                continue;
            }
            let event = decode_note(log)?;
            if event.commitment == note.commitment {
                ensure!(
                    found.is_none() && event.asset == note.asset && event.noteAmount == note.amount,
                    "deposit event does not match saved note"
                );
                found = Some(event);
            }
        }
        let event =
            found.ok_or_else(|| eyre::eyre!("deposit receipt is missing the saved commitment"))?;
        Ok(
            json!({ "note": path, "commitment": note.commitment, "asset": note.asset,
            "amount": note.amount.to_string(), "transaction_hash": receipt["transactionHash"],
            "leaf_index": event.leafIndex }),
        )
    }
    .await;
    result.wrap_err_with(|| format!("deposit note retained at {}", path.display()))
}

fn decode_note(log: &Value) -> Result<IPayNote::NewNote> {
    ensure!(
        log.get("removed").and_then(Value::as_bool) != Some(true),
        "removed NewNote log; retry against a stable chain"
    );
    let address: Address =
        serde_json::from_value(log.get("address").cloned().unwrap_or(Value::Null))?;
    ensure!(address == PAYNOTE_ADDRESS, "unexpected NewNote emitter");
    let topics: Vec<B256> =
        serde_json::from_value(log.get("topics").cloned().unwrap_or(Value::Null))?;
    let data: alloy_primitives::Bytes =
        serde_json::from_value(log.get("data").cloned().unwrap_or(Value::Null))?;
    ensure!(
        topics.len() == 3 && data.len() == 96,
        "malformed NewNote log"
    );
    let event = IPayNote::NewNote::decode_raw_log_validate(topics, &data)?;
    ensure!(
        event.commitment != B256::ZERO && !event.asset.is_zero(),
        "invalid NewNote commitment or asset"
    );
    let _: Field = event.commitment.to_field()?;
    let _: Field = event.rootAfter.to_field()?;
    Ok(event)
}

async fn read_tree(client: &impl Rpc, chain_id: u64) -> Result<PayNoteTree> {
    let head = client.eth_block_number().await?;
    let tag = format!("0x{head:x}");
    let mut tree = new_tree(chain_id)?;
    let mut from = 0u64;
    // NB: scan all history with O(leaves) memory. Cache the tree when pool size warrants it.
    loop {
        let to = from.saturating_add(999).min(head);
        let logs = client
            .eth_get_logs(
                PAYNOTE_ADDRESS,
                &[Some(format!("{:#x}", IPayNote::NewNote::SIGNATURE_HASH))],
                &format!("0x{from:x}"),
                &format!("0x{to:x}"),
            )
            .await?;
        let mut events = logs.iter().map(decode_note).collect::<Result<Vec<_>>>()?;
        events.sort_by_key(|event| event.leafIndex);
        for event in events {
            ensure!(
                usize::try_from(event.leafIndex)? == tree.leaves().len(),
                "NewNote history has duplicate or missing leaf indexes"
            );
            tree.append(event.commitment.to_field()?)?;
            ensure!(
                field_to_b256(&tree.root())? == event.rootAfter,
                "NewNote history root mismatch"
            );
        }
        if to == head {
            break;
        }
        from = to + 1; // to < head <= u64::MAX
    }
    let count = IPayNote::leafCountCall::abi_decode_returns_validate(
        &client
            .eth_call_at(
                PAYNOTE_ADDRESS,
                &IPayNote::leafCountCall {}.abi_encode(),
                &tag,
            )
            .await?,
    )?;
    ensure!(
        count != 0,
        "pool has no confirmed notes; deposit is not yet on-chain"
    );
    let root = IPayNote::currentRootCall::abi_decode_returns_validate(
        &client
            .eth_call_at(
                PAYNOTE_ADDRESS,
                &IPayNote::currentRootCall {}.abi_encode(),
                &tag,
            )
            .await?,
    )?;
    ensure!(
        count == u64::try_from(tree.leaves().len())? && root == field_to_b256(&tree.root())?,
        "NewNote history does not match chain snapshot"
    );
    Ok(tree)
}

async fn check_unspent(client: &impl Rpc, note: &Note) -> Result<()> {
    ensure!(
        !call(
            client,
            PAYNOTE_ADDRESS,
            IPayNote::isSpentCall {
                nullifier: note.nullifier()?
            }
        )
        .await?,
        "note is already spent"
    );
    Ok(())
}

fn prove(
    note: &Note,
    amount: U256,
    context: B256,
    tree: &PayNoteTree,
) -> Result<(Vec<u8>, Option<Note>, PublicInputs)> {
    note.validate()?;
    ensure!(context != B256::ZERO, "context must be non-zero");
    let change = note.change(amount)?;
    let (leaf_index, auth_path) = witness(tree, note.commitment.to_field()?)?;
    let public = PublicInputs {
        chain_id: note.chain_id,
        root: field_to_b256(&tree.root())?,
        nullifier: note.nullifier()?,
        asset: note.asset,
        context,
        spend_amount: amount,
        change_commitment: change.as_ref().map_or(B256::ZERO, |n| n.commitment),
    };
    let mut witness = Witness {
        note_amount: note.amount,
        note_spend_key: note.spend_key,
        leaf_index,
        auth_path: auth_path.map(|_| B256::ZERO),
    };
    for (word, field) in witness.auth_path.iter_mut().zip(auth_path) {
        *word = field_to_b256(&field)?;
    }
    let circuit_public = public.try_into()?;
    let proof = ProofGenerator::<Paynote>::generate(
        &Barretenberg::default(),
        &witness.try_into()?,
        &circuit_public,
    )
    .map_err(|_| eyre::eyre!("paynote proof generation failed; check Barretenberg/SRS setup"))?;
    let fields = <Paynote as Circuit>::public_inputs(&circuit_public);
    let mut combined = Vec::new();
    combined.extend_from_slice(&u32::try_from(fields.len())?.to_be_bytes());
    for value in fields {
        combined.extend_from_slice(field_to_b256(&value)?.as_slice());
    }
    for value in proof.proof {
        combined.extend_from_slice(&value);
    }
    ensure!(
        verify_circuit::<Paynote>(&combined)?,
        "generated paynote proof failed verification"
    );
    Ok((combined, change, public))
}

async fn spend_proof(
    client: &(impl Rpc + Sync),
    dir: &Path,
    note: &Note,
    amount: U256,
    target: SettlementTarget,
) -> Result<Value> {
    note.validate()?;
    ensure!(
        client.eth_chain_id().await? == note.chain_id,
        "note chain ID does not match RPC chain"
    );
    note.change(amount)?;
    check_unspent(client, note).await?;
    let quoted = quote_settlement(client, note, amount, target).await?;
    let tree = read_tree(client, note.chain_id).await?;
    let (combined, change, public) = prove(note, amount, quoted.context, &tree)?;
    ensure!(
        call(
            client,
            PAYNOTE_ADDRESS,
            IPayNote::isKnownRootCall { root: public.root }
        )
        .await?,
        "proof root expired during generation; rerun spend-proof"
    );
    check_unspent(client, note).await?;
    private_dir(dir)?;
    let change_path = change
        .as_ref()
        .map(|note| save_note(dir, note))
        .transpose()?;
    // This file alone can be sent to a relayer, so it never names the spent note.
    let mut output = spend_artifact(&combined, &public, &quoted);
    let proof_path = save_json(
        &dir.join("proofs"),
        &format!("{:#x}.json", keccak256(&combined)),
        &output,
    )?;
    output["source_commitment"] = json!(note.commitment);
    output["proof_file"] = json!(proof_path);
    output["change_note"] = json!(change_path);
    Ok(output)
}

fn spend_artifact(combined: &[u8], public: &PublicInputs, quoted: &QuotedSettlement) -> Value {
    json!({ "version": 1, "circuit": format!("{}@{}", Paynote::LABEL, Paynote::VERSION), "proof": format!("0x{}", hex::encode(combined)),
        "chain_id": public.chain_id, "pool": PAYNOTE_ADDRESS,
        "asset": public.asset, "context": public.context, "domain": quoted.domain,
        "target": quoted.target, "series": quoted.series, "owner": quoted.owner, "units": quoted.units.to_string(),
        "snapshotId": quoted.snapshot_id.to_string(), "spend_amount": public.spend_amount.to_string(),
        "root": public.root, "nullifier": public.nullifier, "change_commitment": public.change_commitment })
}

#[cfg(test)]
mod tests;
