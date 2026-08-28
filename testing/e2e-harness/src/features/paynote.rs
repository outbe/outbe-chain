//! Building and spending a Paynote against a live chain.
//!
//! A Nod's cost is discharged by burning a note rather than by a transfer, so
//! the settlement scenario has to do off-chain what a wallet would: pick a
//! spend key, deposit under its serial, rebuild the pool's Merkle tree from the
//! `NewNote` log, and prove membership.
//!
//! The tree is rebuilt from logs rather than read from storage because the pool
//! keeps only the frontier on chain — the auth path exists nowhere but in the
//! deposit history.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_paynote::hash::{
    address_field, field_from_be_bytes, field_to_be_bytes, note_commitment, note_nullifier,
    note_sn, Field,
};
use outbe_paynote::test_support::{combined_from, ReferenceTree};
use outbe_protocol::protocol::zk::ProofGenerator;
use outbe_protocol::OutbeV1;
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::noir::paynote::{Paynote, PublicInputs, Witness};

use crate::internal::{addresses, eth};
use crate::world::World;

/// A note this scenario owns: the spend key it was built around plus the
/// leaf the pool will derive from the deposit.
pub(crate) struct Note {
    chain_id: u64,
    asset: Address,
    amount: u128,
    spend_key: Field,
    serial: Field,
    commitment: Field,
}

impl Note {
    /// Derives a note for `amount` of `asset` under a fixed spend key. The key
    /// is a constant because the scenario is single-shot and deterministic
    /// evidence beats an unreproducible random draw.
    pub(crate) fn new(chain_id: u64, asset: Address, amount: u128) -> Self {
        let spend_key = Field::from(0x005e_771e_u64);
        let serial = note_sn(spend_key).expect("note serial");
        let commitment =
            note_commitment(chain_id, serial, asset.into(), amount).expect("note commitment");
        Self {
            chain_id,
            asset,
            amount,
            spend_key,
            serial,
            commitment,
        }
    }

    /// The `noteSn` argument `IPaynote.deposit` takes.
    pub(crate) fn serial_word(&self) -> B256 {
        B256::new(field_to_be_bytes(self.serial))
    }
}

/// Proves a full spend of `note` by `spender` against the pool's live tree.
///
/// Every leaf ever appended is read back from `NewNote`, so the proof is built
/// against the same root the chain will check it under — including any notes
/// other scenarios deposited.
pub(crate) fn prove_spend(world: &World, port: u16, note: &Note, spender: Address) -> Vec<u8> {
    let mut tree = ReferenceTree::new(note.chain_id);
    let mut leaf_index = None;
    for (index, commitment) in deposited_leaves(world, port) {
        let appended = tree.append(commitment);
        assert_eq!(
            appended, index,
            "NewNote leaf indexes must be dense and ordered"
        );
        if commitment == note.commitment {
            leaf_index = Some(appended);
        }
    }
    let leaf_index = leaf_index.expect("the scenario's own deposit must be in the pool");

    let public = PublicInputs {
        chain_id: note.chain_id,
        root: tree.root(),
        nullifier: note_nullifier(note.commitment, note.spend_key).expect("note nullifier"),
        asset: address_field(note.asset.into()),
        spender: address_field(spender.into()),
        spend_amount: note.amount,
        // A full spend leaves no change; the circuit requires the zero
        // sentinel rather than a note for nothing.
        change_commitment: Field::from(0_u64),
    };
    let witness = Witness {
        note_amount: note.amount,
        note_spend_key: note.spend_key,
        leaf_index,
        auth_path: tree.path_at(leaf_index),
    };
    let proof =
        ProofGenerator::<OutbeV1, Paynote>::generate(&Barretenberg::default(), &witness, &public)
            .expect("paynote spend proof");
    combined_from(&public, &proof.proof)
}

/// Every `(leafIndex, commitment)` the pool has logged, ordered by leaf index.
fn deposited_leaves(world: &World, port: u16) -> Vec<(u32, Field)> {
    let url = world.rpc.url(port);
    let head = eth::block_number(&url).expect("head block for paynote log scan");
    let topic0 = keccak256(b"NewNote(bytes32,uint32,bytes32,address,uint128)");
    let logs = eth::raw_json_with_params(
        &url,
        "eth_getLogs",
        serde_json::json!([{
            "address": format!("{:#x}", addresses::PAYNOTE_ADDR),
            "fromBlock": "0x0",
            "toBlock": format!("0x{head:x}"),
            "topics": [format!("{topic0:#x}")],
        }]),
    )
    .expect("paynote NewNote logs");

    let mut leaves: Vec<(u32, Field)> = logs
        .as_array()
        .expect("eth_getLogs returns an array")
        .iter()
        .map(|log| decode_new_note(log).expect("canonical NewNote log"))
        .collect();
    leaves.sort_by_key(|(index, _)| *index);
    leaves
}

/// `NewNote(bytes32 indexed commitment, uint32 leafIndex, bytes32 rootAfter,
/// address indexed asset, uint128 noteAmount)` — the commitment is topic 1 and
/// `leafIndex` is the first data word.
fn decode_new_note(log: &serde_json::Value) -> Option<(u32, Field)> {
    let topics = log.get("topics")?.as_array()?;
    if topics.len() != 3 {
        return None;
    }
    let commitment_bytes: [u8; 32] = hex::decode(topics.get(1)?.as_str()?.trim_start_matches("0x"))
        .ok()?
        .try_into()
        .ok()?;
    let commitment = field_from_be_bytes(&commitment_bytes)?;

    let data = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
    if data.len() != 3 * 32 {
        return None;
    }
    let leaf_index = u32::try_from(U256::from_be_slice(&data[..32])).ok()?;
    Some((leaf_index, commitment))
}
