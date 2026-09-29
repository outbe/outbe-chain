//! Durable bounded merges. Immutable notes and operation files are the recovery
//! source; spendability is always recomputed from one canonical chain snapshot.
use super::*;
use outbe_paynote::client::merge_witness;
use outbe_zk_canonical::paynote_merge::{self, PaynoteMerge, MAX_MERGE_INPUTS};
use std::collections::{HashMap, HashSet};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    inputs: Vec<Note>,
    output: Note,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    version: u8,
    stages: Vec<Stage>,
}

fn sum_inputs(inputs: &[Note]) -> Result<U256> {
    let first = inputs
        .first()
        .ok_or_else(|| eyre::eyre!("merge requires at least two notes"))?;
    ensure!(inputs.len() >= 2, "merge requires at least two notes");
    let mut seen = HashSet::new();
    inputs.iter().try_fold(U256::ZERO, |sum, n| {
        n.validate()?;
        ensure!(
            n.chain_id == first.chain_id && n.pool == first.pool && n.asset == first.asset,
            "merge notes must have the same chain, pool and exact asset address"
        );
        ensure!(seen.insert(n.commitment), "duplicate merge note");
        sum.checked_add(n.amount)
            .ok_or_else(|| eyre::eyre!("merge amount overflow"))
    })
}

impl Operation {
    fn new(inputs: Vec<Note>) -> Result<Self> {
        sum_inputs(&inputs)?; // Check the entire selection before any batch starts.
        let mut remaining = inputs.into_iter();
        let mut batch = remaining
            .by_ref()
            .take(MAX_MERGE_INPUTS)
            .collect::<Vec<_>>();
        let mut stages = Vec::new();
        loop {
            let total = sum_inputs(&batch)?;
            let first = &batch[0];
            let mut output = Note::random(first.chain_id, first.asset, total)?;
            while batch.iter().any(|note| note.spend_key == output.spend_key) {
                output = Note::random(first.chain_id, first.asset, total)?;
            }
            stages.push(Stage {
                inputs: batch,
                output: output.clone(),
            });
            batch = vec![output];
            batch.extend(remaining.by_ref().take(MAX_MERGE_INPUTS - 1));
            if batch.len() == 1 {
                break;
            }
        }
        let operation = Self { version: 1, stages };
        operation.validate()?;
        Ok(operation)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && !self.stages.is_empty(),
            "unsupported merge operation"
        );
        let mut consumed = HashSet::new();
        let mut outputs = HashSet::new();
        let mut previous: Option<&Note> = None;
        for stage in &self.stages {
            ensure!(
                (2..=MAX_MERGE_INPUTS).contains(&stage.inputs.len()),
                "invalid merge stage size"
            );
            let sum = sum_inputs(&stage.inputs)?;
            stage.output.validate()?;
            let first = &stage.inputs[0];
            ensure!(
                stage.output.chain_id == first.chain_id
                    && stage.output.pool == first.pool
                    && stage.output.asset == first.asset
                    && stage.output.amount == sum,
                "merge output does not conserve its input asset and amount"
            );
            if let Some(prior) = previous {
                ensure!(
                    first == prior,
                    "merge stages are not linked by their saved output"
                );
            }
            for n in &stage.inputs {
                ensure!(
                    consumed.insert(n.commitment),
                    "merge operation consumes an input twice"
                );
                ensure!(
                    n.spend_key != stage.output.spend_key,
                    "merge output key is not fresh"
                );
            }
            ensure!(
                !consumed.contains(&stage.output.commitment)
                    && outputs.insert(stage.output.commitment),
                "duplicate or self-referential merge output"
            );
            previous = Some(&stage.output);
        }
        Ok(())
    }

    fn save(&self, dir: &Path) -> Result<PathBuf> {
        self.validate()?;
        for stage in &self.stages {
            for n in stage.inputs.iter().chain(std::iter::once(&stage.output)) {
                save_note(dir, n)?;
            }
        }
        // Nonempty is validated above. The final fresh commitment identifies the
        // operation without putting secrets in filenames or public proof files.
        let last = self
            .stages
            .last()
            .ok_or_else(|| eyre::eyre!("empty merge"))?;
        save_json(
            &dir.join("merges"),
            &format!("{:#x}.json", last.output.commitment),
            self,
        )
    }
}

fn load_operation(path: &Path) -> Result<Operation> {
    let bytes = Zeroizing::new(fs::read(path).wrap_err("read merge operation")?);
    let operation: Operation =
        serde_json::from_slice(&bytes).map_err(|_| eyre::eyre!("invalid merge operation JSON"))?;
    operation.validate()?;
    Ok(operation)
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            ensure!(
                entry.file_type()?.is_file(),
                "note or operation file must not be a symlink"
            );
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn selected(dir: &Path, arguments: &[String]) -> Result<Vec<Note>> {
    arguments
        .iter()
        .map(|s| load_note(&resolve_note(dir, s)))
        .collect()
}

struct Snapshot {
    height: u64,
    hash: B256,
    chain_id: u64,
}

impl Snapshot {
    async fn read(client: &impl Rpc) -> Result<Self> {
        let chain_id = client.eth_chain_id().await?;
        let height = client.eth_block_number().await?;
        let hash = block_hash(&client.eth_get_block_by_number(height).await?)?;
        Ok(Self {
            height,
            hash,
            chain_id,
        })
    }

    async fn check(&self, client: &impl Rpc) -> Result<()> {
        ensure!(
            block_hash(&client.eth_get_block_by_number(self.height).await?)? == self.hash,
            "chain reorganized during note reconciliation; retry"
        );
        Ok(())
    }

    async fn state(&self, client: &impl Rpc, note: &Note) -> Result<NoteState> {
        ensure!(
            note.chain_id == self.chain_id,
            "note chain ID does not match RPC chain"
        );
        let tag = format!("0x{:x}", self.height);
        let present = IPayNote::hasCommitmentCall::abi_decode_returns_validate(
            &client
                .eth_call_at(
                    PAYNOTE_ADDRESS,
                    &IPayNote::hasCommitmentCall {
                        commitment: note.commitment,
                    }
                    .abi_encode(),
                    &tag,
                )
                .await?,
        )?;
        let spent = IPayNote::isSpentCall::abi_decode_returns_validate(
            &client
                .eth_call_at(
                    PAYNOTE_ADDRESS,
                    &IPayNote::isSpentCall {
                        nullifier: note.nullifier()?,
                    }
                    .abi_encode(),
                    &tag,
                )
                .await?,
        )?;
        ensure!(!spent || present, "inconsistent canonical note state");
        Ok(NoteState { present, spent })
    }
}

fn block_hash(block: &Value) -> Result<B256> {
    let hash: B256 = block
        .get("hash")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("block snapshot is missing its hash"))?
        .parse()?;
    ensure!(!hash.is_zero(), "block snapshot has a zero hash");
    Ok(hash)
}

#[derive(Clone, Copy)]
struct NoteState {
    present: bool,
    spent: bool,
}

// NB: scans saved operation history; add an index only when wallet size warrants it.
async fn reservations(client: &impl Rpc, dir: &Path, snapshot: &Snapshot) -> Result<HashSet<B256>> {
    let mut reserved = HashSet::new();
    let mut states = HashMap::new();
    for path in json_files(&dir.join("merges"))? {
        let operation = load_operation(&path)?;
        if operation.stages[0].output.chain_id != snapshot.chain_id {
            continue;
        }
        let mut pending = Vec::new();
        let mut conflict = false;
        for stage in operation.stages {
            for note in stage.inputs.iter().chain(std::iter::once(&stage.output)) {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    states.entry(note.commitment)
                {
                    entry.insert(snapshot.state(client, note).await?);
                }
            }
            if states[&stage.output.commitment].present {
                continue;
            }
            for note in stage.inputs {
                let state = states[&note.commitment];
                conflict |= state.spent;
                if state.present && !state.spent {
                    pending.push(note.commitment);
                }
            }
        }
        if !conflict {
            reserved.extend(pending);
        }
    }
    Ok(reserved)
}

async fn prepare(
    client: &impl Rpc,
    dir: &Path,
    arguments: &[String],
) -> Result<(Operation, PathBuf)> {
    let inputs = selected(dir, arguments)?;
    sum_inputs(&inputs)?;
    let snapshot = Snapshot::read(client).await?;
    let reserved = reservations(client, dir, &snapshot).await?;
    for note in &inputs {
        let state = snapshot.state(client, note).await?;
        ensure!(
            state.present && !state.spent,
            "selected note is absent or spent"
        );
        ensure!(
            !reserved.contains(&note.commitment),
            "selected note belongs to a pending merge; use --resume"
        );
    }
    snapshot.check(client).await?;
    let operation = Operation::new(inputs)?;
    let path = operation.save(dir)?;
    eprintln!("Saved merge recovery operation {}", path.display());
    Ok((operation, path))
}

fn prove_stage(
    stage: &Stage,
    tree: &PayNoteTree,
) -> Result<(Vec<u8>, paynote_merge::alloy::PublicInputs)> {
    let inputs = stage
        .inputs
        .iter()
        .map(|n| (n.amount, n.spend_key))
        .collect::<Vec<_>>();
    let (public, witness) = merge_witness(
        tree,
        stage.output.chain_id,
        stage.output.asset,
        &inputs,
        stage.output.spend_key,
    )?;
    ensure!(
        public.output_commitment == stage.output.commitment,
        "saved merge output differs from proof"
    );
    let circuit_public = public.try_into()?;
    let proof = ProofGenerator::<PayNoteSuit, PaynoteMerge>::generate(
        &Barretenberg::default(),
        &witness.try_into()?,
        &circuit_public,
    )
    .map_err(|_| {
        eyre::eyre!("merge proof generation failed; check witnesses and Barretenberg/SRS setup")
    })?;
    let combined = paynote_merge::encode_combined_proof(circuit_public, proof.proof)?;
    ensure!(
        verify_circuit::<PaynoteMerge>(&combined)?,
        "generated merge proof failed verification"
    );
    Ok((combined, public))
}

async fn stage_proof(client: &impl Rpc, dir: &Path, stage: &Stage) -> Result<(Vec<u8>, Value)> {
    ensure!(
        client.eth_chain_id().await? == stage.output.chain_id,
        "merge chain differs from RPC"
    );
    let max = call(client, PAYNOTE_ADDRESS, IPayNote::maxMergeInputsCall {}).await?;
    ensure!(
        usize::try_from(max)? == MAX_MERGE_INPUTS,
        "node and wallet merge profiles disagree"
    );
    let snapshot = Snapshot::read(client).await?;
    for note in &stage.inputs {
        let state = snapshot.state(client, note).await?;
        ensure!(
            state.present && !state.spent,
            "merge input is absent or spent; reconcile the operation"
        );
    }
    ensure!(
        !snapshot.state(client, &stage.output).await?.present,
        "merge output already exists"
    );
    snapshot.check(client).await?;
    let head = client.eth_block_number().await?;
    let head_hash = block_hash(&client.eth_get_block_by_number(head).await?)?;
    let tree = read_tree(client, stage.output.chain_id).await?;
    let (combined, public) = prove_stage(stage, &tree)?;
    ensure!(
        block_hash(&client.eth_get_block_by_number(head).await?)? == head_hash,
        "chain reorganized during merge proving; resume with fresh witnesses"
    );
    ensure!(
        call(
            client,
            PAYNOTE_ADDRESS,
            IPayNote::isKnownRootCall { root: public.root }
        )
        .await?,
        "merge root expired during proving; resume with fresh witnesses"
    );
    for n in &stage.inputs {
        check_unspent(client, n).await?;
    }
    // This file alone can be sent to a relayer. Never add source commitments,
    // local paths, amounts or bearer keys to this public artifact.
    let public_artifact = proof_artifact(&combined, &public);
    let path = save_json(
        &dir.join("proofs"),
        &format!("{:#x}.json", keccak256(&combined)),
        &public_artifact,
    )?;
    Ok((
        combined,
        json!({"proof_file": path, "output_commitment": public.output_commitment,
        "output_note": dir.join(format!("{:#x}.json", public.output_commitment)),
        "amount": stage.output.amount.to_string(), "asset": stage.output.asset}),
    ))
}

fn proof_artifact(combined: &[u8], public: &paynote_merge::alloy::PublicInputs) -> Value {
    json!({
        "version": 1, "circuit": format!("{}@{}", PaynoteMerge::LABEL, PaynoteMerge::VERSION),
        "proof": format!("0x{}", hex::encode(combined)), "chain_id": public.chain_id,
        "pool": public.pool, "root": public.root, "input_count": public.input_count,
        "asset": public.asset, "nullifiers": public.nullifiers,
        "output_commitment": public.output_commitment,
    })
}

pub(super) async fn proof(client: &impl Rpc, dir: &Path, arguments: &[String]) -> Result<Value> {
    ensure!(
        (2..=MAX_MERGE_INPUTS).contains(&arguments.len()),
        "merge-proof requires 2..4 inputs"
    );
    let (operation, path) = prepare(client, dir, arguments).await?;
    let result = stage_proof(client, dir, &operation.stages[0]).await;
    let (_, mut output) =
        result.wrap_err_with(|| format!("merge notes retained; resume {}", path.display()))?;
    output["operation_file"] = json!(path);
    Ok(output)
}

pub(super) async fn run(
    client: &(impl Rpc + Sync),
    signer: &TxSigner,
    dir: &Path,
    arguments: &[String],
    resume: Option<&Path>,
) -> Result<Value> {
    let (operation, path) = if let Some(path) = resume {
        let operation = load_operation(path)?;
        let saved = operation.save(dir)?;
        (operation, saved)
    } else {
        prepare(client, dir, arguments).await?
    };
    let result: Result<Value> = async {
        for stage in &operation.stages {
            let snapshot = Snapshot::read(client).await?;
            let output = snapshot.state(client, &stage.output).await?;
            let mut inputs_spent = true;
            for note in &stage.inputs { inputs_spent &= snapshot.state(client, note).await?.spent; }
            snapshot.check(client).await?;
            if output.present {
                ensure!(inputs_spent, "output exists without all merge inputs spent; refusing to skip stage");
                continue;
            }
            let (combined, _) = stage_proof(client, dir, stage).await?;
            let receipt = send(client, signer, PAYNOTE_ADDRESS, IPayNote::mergePayNotesCall { proof: combined.into() }.abi_encode()).await?;
            let logs = receipt.get("logs").and_then(Value::as_array).ok_or_else(|| eyre::eyre!("merge receipt has no logs"))?;
            let output_seen = logs.iter().filter(|log| log.get("address").and_then(Value::as_str)
                .and_then(|s| s.parse::<Address>().ok()) == Some(PAYNOTE_ADDRESS)
                && log.pointer("/topics/0").and_then(Value::as_str).and_then(|s| s.parse::<B256>().ok()) == Some(IPayNote::NewNote::SIGNATURE_HASH))
                .map(decode_note).collect::<Result<Vec<_>>>()?;
            ensure!(output_seen.iter().filter(|e| e.commitment == stage.output.commitment
                && e.asset == stage.output.asset && e.noteAmount.is_zero()).count() == 1,
                "merge receipt does not contain the saved output");
            let canonical = Snapshot::read(client).await?;
            ensure!(canonical.state(client, &stage.output).await?.present, "merge output was reorganized out; resume");
            for note in &stage.inputs { ensure!(canonical.state(client, note).await?.spent, "merge input remains unspent"); }
            canonical.check(client).await?;
        }
        let last = operation.stages.last().ok_or_else(|| eyre::eyre!("empty merge"))?;
        Ok(json!({"operation_file": path, "output_note": dir.join(format!("{:#x}.json", last.output.commitment)),
            "commitment": last.output.commitment, "asset": last.output.asset,
            "amount": last.output.amount.to_string(), "batches": operation.stages.len()}))
    }.await;
    result.wrap_err_with(|| {
        format!(
            "merge progress and all bearer notes retained; resume {}",
            path.display()
        )
    })
}

pub(super) async fn status(client: &impl Rpc, dir: &Path, arguments: &[String]) -> Result<Value> {
    let notes = if arguments.is_empty() {
        json_files(dir)?
            .iter()
            .map(|p| load_note(p))
            .collect::<Result<Vec<_>>>()?
    } else {
        selected(dir, arguments)?
    };
    let snapshot = Snapshot::read(client).await?;
    let reserved = reservations(client, dir, &snapshot).await?;
    let mut results = Vec::new();
    for note in notes {
        if note.chain_id != snapshot.chain_id {
            continue;
        }
        let state = snapshot.state(client, &note).await?;
        let label = if state.spent {
            "spent"
        } else if !state.present {
            "unconfirmed"
        } else if reserved.contains(&note.commitment) {
            "pending_merge"
        } else {
            "spendable"
        };
        results.push(json!({"commitment": note.commitment, "asset": note.asset,
            "amount": note.amount.to_string(), "state": label}));
    }
    snapshot.check(client).await?;
    Ok(
        json!({"chain_id": snapshot.chain_id, "block_number": snapshot.height, "block_hash": snapshot.hash, "notes": results}),
    )
}

#[cfg(test)]
mod tests;
