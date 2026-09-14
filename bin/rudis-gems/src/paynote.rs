//! Canonical PayNote hashes, Merkle witnesses and UltraHonk proofs in-process.
use crate::{
    abi::IPayNote,
    rpc::{self, Rpc},
    store, PAYNOTE,
};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use eyre::{ensure, Result};
use outbe_protocol::{
    codec::FieldElement,
    protocol::zk::{Circuit, ProofGenerator},
    Codec,
};
use outbe_zk_backend::barretenberg::{verify_circuit, Barretenberg};
use outbe_zk_canonical::{
    noir::paynote::{
        alloy::{PublicInputs, Witness},
        Paynote,
    },
    paynote::{
        hash::{change_key, empty_leaf, note_commitment, note_nullifier, note_sn, paynote_domain},
        Field, PayNoteSuite, Tree,
    },
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const DEPTH: usize = 32;

// Same bearer-note JSON as the existing canonical CLI, so earlier deposits can resume.
#[derive(Serialize, Deserialize)]
pub struct Note {
    version: u8,
    pub chain_id: u64,
    pool: Address,
    pub asset: Address,
    pub amount: String,
    spend_key: B256,
    pub commitment: B256,
}
impl Note {
    fn new(chain: u64, asset: Address, amount: U256, key: Field) -> Result<Self> {
        ensure!(
            !asset.is_zero() && !amount.is_zero() && key != Field::from(0),
            "Invalid note inputs"
        );
        let commitment =
            PayNoteSuite::field_to_b256(&note_commitment(chain, note_sn(key)?, asset, amount)?)?;
        ensure!(!commitment.is_zero(), "Zero note commitment");
        Ok(Self {
            version: 1,
            chain_id: chain,
            pool: PAYNOTE,
            asset,
            amount: amount.to_string(),
            spend_key: PayNoteSuite::field_to_b256(&key)?,
            commitment,
        })
    }
    pub fn random(chain: u64, asset: Address, amount: U256) -> Result<Self> {
        let mut bytes = Zeroizing::new([0; 32]);
        loop {
            SystemRandom::new()
                .fill(bytes.as_mut())
                .map_err(|_| eyre::eyre!("Note randomness failed"))?;
            if let Ok(key) = B256::from(*bytes).to_field() {
                if key != Field::from(0) {
                    return Self::new(chain, asset, amount, key);
                }
            }
        }
    }
    pub fn amount(&self) -> Result<U256> {
        Ok(U256::from_str_radix(&self.amount, 10)?)
    }
    pub fn serial(&self) -> Result<B256> {
        Ok(PayNoteSuite::field_to_b256(&note_sn(
            self.spend_key.to_field()?,
        )?)?)
    }
    fn nullifier(&self) -> Result<B256> {
        Ok(PayNoteSuite::field_to_b256(&note_nullifier(
            self.commitment.to_field()?,
            self.spend_key.to_field()?,
        )?)?)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.pool == PAYNOTE,
            "Unsupported PayNote format"
        );
        let expected = Self::new(
            self.chain_id,
            self.asset,
            self.amount()?,
            self.spend_key.to_field()?,
        )?;
        ensure!(
            expected.commitment == self.commitment,
            "Saved note commitment mismatch"
        );
        Ok(())
    }
    fn change(&self, amount: U256) -> Result<Option<Self>> {
        ensure!(!amount.is_zero(), "Cannot spend zero");
        let remaining = self
            .amount()?
            .checked_sub(amount)
            .ok_or_else(|| eyre::eyre!("Note does not cover settlement quote"))?;
        if remaining.is_zero() {
            return Ok(None);
        }
        Ok(Some(Self::new(
            self.chain_id,
            self.asset,
            remaining,
            change_key(self.spend_key.to_field()?, self.nullifier()?.to_field()?)?,
        )?))
    }
    pub fn save(&self, directory: &Path) -> Result<PathBuf> {
        self.validate()?;
        let path = directory.join(format!("{:#x}.json", self.commitment));
        store::save_json(&path, self, false)?;
        Ok(path)
    }
}
impl Drop for Note {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.spend_key.as_mut_slice().zeroize();
    }
}

pub fn new_tree(chain: u64) -> Result<Tree> {
    Ok(Tree::new(paynote_domain(), empty_leaf(chain)?, DEPTH)?)
}

pub fn decode_event<E: SolEvent>(receipt: &Value, emitter: Address) -> Result<E> {
    let logs = receipt["logs"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("Receipt has no logs"))?;
    let mut event = None;
    for log in logs {
        if log["address"]
            .as_str()
            .and_then(|s| s.parse::<Address>().ok())
            != Some(emitter)
        {
            continue;
        }
        let topics: Vec<B256> = serde_json::from_value(log["topics"].clone())?;
        if topics.first() != Some(&E::SIGNATURE_HASH) {
            continue;
        }
        ensure!(event.is_none(), "Duplicate expected event in receipt");
        let data: alloy_primitives::Bytes = serde_json::from_value(log["data"].clone())?;
        event = Some(E::decode_raw_log_validate(topics, &data)?);
    }
    event.ok_or_else(|| eyre::eyre!("Receipt is missing {}", E::SIGNATURE))
}

pub async fn read_tree(rpc: &impl Rpc, chain: u64) -> Result<Tree> {
    let block = rpc::latest(rpc).await?;
    let head: u64 = rpc::quantity(&block["number"])?.try_into()?;
    let mut tree = new_tree(chain)?;
    let mut from = 0u64;
    loop {
        let to = from.saturating_add(999).min(head);
        let logs = rpc.request("eth_getLogs", json!([{"address":PAYNOTE,
            "topics":[IPayNote::NewNote::SIGNATURE_HASH],"fromBlock":format!("{from:#x}"),"toBlock":format!("{to:#x}")}])).await?;
        let mut events = Vec::new();
        for log in logs
            .as_array()
            .ok_or_else(|| eyre::eyre!("Invalid logs result"))?
        {
            ensure!(
                log["removed"] != true,
                "Removed PayNote event; retry against stable history"
            );
            events.push(decode_event::<IPayNote::NewNote>(
                &json!({"logs":[log]}),
                PAYNOTE,
            )?);
        }
        events.sort_by_key(|event| event.leafIndex);
        for event in events {
            ensure!(
                usize::try_from(event.leafIndex)? == tree.leaves().len(),
                "Missing or duplicated PayNote leaves"
            );
            tree.append(event.commitment.to_field()?)?;
            ensure!(
                PayNoteSuite::field_to_b256(&tree.root())? == event.rootAfter,
                "PayNote event root mismatch"
            );
        }
        if to == head {
            break;
        }
        from = to + 1;
    }
    let tag = format!("{head:#x}");
    let count = rpc::call(rpc, PAYNOTE, IPayNote::leafCountCall {}, &tag).await?;
    let root = rpc::call(rpc, PAYNOTE, IPayNote::currentRootCall {}, &tag).await?;
    ensure!(
        count > 0
            && count == u64::try_from(tree.leaves().len())?
            && root == PayNoteSuite::field_to_b256(&tree.root())?,
        "PayNote history does not match RPC snapshot"
    );
    Ok(tree)
}

pub fn prove(
    note: &Note,
    amount: U256,
    owner: Address,
    tree: &Tree,
) -> Result<(Vec<u8>, Option<Note>, B256)> {
    note.validate()?;
    ensure!(!owner.is_zero(), "Proof owner must be nonzero");
    let change = note.change(amount)?;
    let leaf = note.commitment.to_field()?;
    let index = tree
        .leaves()
        .iter()
        .position(|value| *value == leaf)
        .ok_or_else(|| eyre::eyre!("Note deposit is not in the tree"))?;
    let leaf_index = u32::try_from(index)?;
    let path = tree.inclusion_path(u64::from(leaf_index))?;
    ensure!(
        path.root(leaf)? == tree.root() && path.domain == paynote_domain(),
        "Invalid Merkle witness"
    );
    let root = PayNoteSuite::field_to_b256(&tree.root())?;
    let public = PublicInputs {
        chain_id: note.chain_id,
        root,
        nullifier: note.nullifier()?,
        asset: note.asset,
        owner,
        spend_amount: amount,
        change_commitment: change.as_ref().map_or(B256::ZERO, |n| n.commitment),
    };
    let siblings: Vec<B256> = path
        .siblings
        .iter()
        .map(PayNoteSuite::field_to_b256)
        .collect::<std::result::Result<_, _>>()?;
    let auth_path: [B256; DEPTH] = siblings
        .try_into()
        .map_err(|_| eyre::eyre!("Wrong PayNote tree depth"))?;
    let witness = Witness {
        note_amount: note.amount()?,
        note_spend_key: note.spend_key,
        leaf_index,
        auth_path,
    };
    let public = public.try_into()?;
    let proof = ProofGenerator::<PayNoteSuite, Paynote>::generate(
        &Barretenberg::default(),
        &witness.try_into()?,
        &public,
    )
    .map_err(|_| eyre::eyre!("PayNote proving failed; check Barretenberg/SRS setup"))?;
    let fields = <Paynote as Circuit<PayNoteSuite>>::public_inputs(&public);
    let mut combined = u32::try_from(fields.len())?.to_be_bytes().to_vec();
    for field in fields {
        combined.extend_from_slice(PayNoteSuite::field_to_b256(&field)?.as_slice());
    }
    for word in proof.proof {
        combined.extend_from_slice(&word);
    }
    ensure!(
        verify_circuit::<Paynote>(&combined)?,
        "Generated PayNote proof failed verification"
    );
    Ok((combined, change, root))
}

pub async fn spend_proof(
    rpc: &impl Rpc,
    note: &Note,
    amount: U256,
    owner: Address,
    directory: &Path,
) -> Result<Vec<u8>> {
    let nullifier = note.nullifier()?;
    ensure!(
        !rpc::call(rpc, PAYNOTE, IPayNote::isSpentCall { nullifier }, "latest").await?,
        "PayNote already spent"
    );
    println!("Building PayNote membership witness and proof...");
    let tree = read_tree(rpc, note.chain_id).await?;
    let (proof, change, root) = prove(note, amount, owner, &tree)?;
    ensure!(
        rpc::call(rpc, PAYNOTE, IPayNote::isKnownRootCall { root }, "latest").await?,
        "Proof root expired; rerun to regenerate"
    );
    ensure!(
        !rpc::call(rpc, PAYNOTE, IPayNote::isSpentCall { nullifier }, "latest").await?,
        "PayNote was spent during proving"
    );
    if let Some(change) = change {
        change.save(directory)?;
    }
    Ok(proof)
}
