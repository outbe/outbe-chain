//! Building and spending a PayNote against a live chain.
//!
//! A Nod's cost is discharged by burning a note rather than by a transfer, so
//! the settlement scenario has to do off-chain what a wallet would: pick a
//! spend key, deposit under its serial, rebuild the pool's Merkle tree from the
//! `NewNote` log, and prove membership.
//!
//! The tree is rebuilt from logs rather than read from storage because the pool
//! keeps only the frontier on chain — the auth path exists nowhere but in the
//! deposit history.

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_paynote::client::{new_tree, witness};
use outbe_paynote::hash::{note_commitment, note_nullifier, note_sn, Field};
use outbe_paynote::test_support::combined_from;
use outbe_protocol::codec::{field_from_be_bytes, field_from_be_bytes_canonical};
use outbe_protocol::protocol::zk::ProofGenerator;
use outbe_protocol::Codec as _;
use outbe_protocol::OutbeV1;
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::noir::paynote::{Paynote as PayNote, PublicInputs, Witness};
use outbe_zk_canonical::u256;

use crate::internal::{addresses, eth};
use crate::world::World;

/// A note this scenario owns: the spend key it was built around plus the
/// leaf the pool will derive from the deposit.
pub(crate) struct Note {
    chain_id: u64,
    asset: Address,
    amount: U256,
    spend_key: Field,
    serial: Field,
    commitment: Field,
}

impl Note {
    /// Derives a note for `amount` of `asset` under its own spend key, counted
    /// up from a fixed base: the commitment follows from the serial, so one
    /// shared key would make two notes of equal value identical and the pool
    /// refuses the second. Counting keeps the evidence reproducible where a
    /// random draw would not.
    pub(crate) fn new(chain_id: u64, asset: Address, amount: U256) -> Self {
        static NEXT_SPEND_KEY: AtomicU64 = AtomicU64::new(0x005e_771e);
        let spend_key = Field::from(NEXT_SPEND_KEY.fetch_add(1, Ordering::Relaxed));
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

    /// The `noteSn` argument `IPayNote.deposit` takes.
    pub(crate) fn serial_word(&self) -> B256 {
        B256::from_slice(&OutbeV1::field_to_be_bytes(&self.serial))
    }
}

/// Deposits a note covering `cost_minor` of `asset` for `payer`, then proves a
/// spend of it — the proof `mineGratis` takes.
///
/// Every Nod is paid for by burning a note, so a Nod that costs nothing still
/// has to present one. The pool refuses a zero deposit, so a free Nod is paid
/// with the smallest note that can exist.
pub(crate) fn deposit_and_prove(
    world: &World,
    port: u16,
    payer_key: &str,
    payer: Address,
    asset: Address,
    cost_minor: U256,
) -> Vec<u8> {
    let amount = if cost_minor.is_zero() {
        U256::ONE
    } else {
        cost_minor
    };
    crate::features::settlement::fund_and_approve(
        world,
        asset,
        payer_key,
        payer,
        addresses::PAYNOTE_ADDR,
        amount,
    );
    let chain_id = world
        .rpc
        .chain_id(port)
        .expect("read chain ID for the note commitment");
    let note = Note::new(chain_id, asset, amount);
    let deposit = eth::send_call_outcome(
        &world.rpc.url(port),
        addresses::PAYNOTE_ADDR,
        payer_key,
        &eth::IPayNote::depositCall {
            asset,
            amount,
            noteSn: note.serial_word(),
        },
        None,
    )
    .expect("deposit the note that pays for the Nod");
    crate::features::settlement::assert_mined_success(
        &deposit,
        "deposit the note that pays for the Nod",
    );
    prove_spend(world, port, &note, payer)
}

/// Proves a full spend of `note` by `owner` against the pool's live tree.
///
/// Every leaf ever appended is read back from `NewNote`, so the proof is built
/// against the same root the chain will check it under — including any notes
/// other scenarios deposited.
pub(crate) fn prove_spend(world: &World, port: u16, note: &Note, owner: Address) -> Vec<u8> {
    let mut tree = new_tree(note.chain_id).expect("paynote tree");
    for (index, commitment) in deposited_leaves(world, port) {
        assert_eq!(
            tree.leaves().len(),
            usize::try_from(index).expect("leaf index fits usize"),
            "NewNote leaf indexes must be dense and ordered"
        );
        tree.append(commitment).expect("append NewNote commitment");
    }
    let (leaf_index, auth_path) =
        witness(&tree, note.commitment).expect("the scenario's own deposit must be in the pool");

    let public = PublicInputs {
        chain_id: note.chain_id,
        root: tree.root(),
        nullifier: note_nullifier(note.commitment, note.spend_key).expect("note nullifier"),
        asset: field_from_be_bytes::<Field>(note.asset.as_slice()),
        owner: field_from_be_bytes::<Field>(owner.as_slice()),
        spend_amount: u256::to_limbs(note.amount),
        // A full spend leaves no change; the circuit requires the zero
        // sentinel rather than a note for nothing.
        change_commitment: Field::from(0_u64),
    };
    let witness = Witness {
        note_amount: u256::to_limbs(note.amount),
        note_spend_key: note.spend_key,
        leaf_index,
        auth_path,
    };
    let proof =
        ProofGenerator::<OutbeV1, PayNote>::generate(&Barretenberg::default(), &witness, &public)
            .expect("paynote spend proof");
    combined_from(&public, &proof.proof)
}

/// Every `(leafIndex, commitment)` the pool has logged, ordered by leaf index.
fn deposited_leaves(world: &World, port: u16) -> Vec<(u32, Field)> {
    let url = world.rpc.url(port);
    let head = eth::block_number(&url).expect("head block for paynote log scan");
    let topic0 = keccak256(b"NewNote(bytes32,uint32,bytes32,address,uint256)");
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
/// address indexed asset, uint256 noteAmount)` — the commitment is topic 1 and
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
    let commitment =
        field_from_be_bytes_canonical::<Field>(&commitment_bytes, "BN254 field").ok()?;

    let data = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
    if data.len() != 3 * 32 {
        return None;
    }
    let leaf_index = u32::try_from(U256::from_be_slice(&data[..32])).ok()?;
    Some((leaf_index, commitment))
}
