#[path = "../../../../testing/ocomp_finality_fixture.rs"]
mod shared_finality_fixture;

pub(crate) use shared_finality_fixture::{
    assemble_finalized_intent_proof, awaiting_finality_record, FinalizationCoordinates,
    FinalizedIntentAssembly, FinalizedIntentAssemblyInput,
};

pub(crate) use outbe_ocomp_protocol::test_utils::storage_trie;

use std::collections::BTreeMap;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_trie::{TrieAccount, KECCAK_EMPTY};
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};
use outbe_nod::openings::entry_price_slots;
use outbe_ocomp_protocol::{league_snapshot::league_snapshot_slot, opening::OpeningSubjectsV1};
use outbe_primitives::{addresses::NOD_ADDRESS, time::WorldwideDay};

pub(crate) struct OpeningContractFixture {
    pub(crate) address: Address,
    pub(crate) slots: Vec<(B256, U256)>,
    pub(crate) account: TrieAccount,
    pub(crate) storage_proofs: Vec<Vec<Bytes>>,
}

/// A deterministic, valid Fidelity league for populating a fixture snapshot slot.
///
/// This is NOT the Fidelity league derivation. That derivation lives in
/// `outbe_fidelity` (`league_from_rcfi`, RCFI -> league). These fixtures mock the
/// on-chain state a node would read, so each snapshot slot needs *some* value in
/// the canonical `[MIN_LEAGUE, MAX_LEAGUE]` range. The value is opaque to the
/// tests. The tests assert opening-proof layout and deterministic re-execution,
/// never league semantics. A distinct (but arbitrary) per-owner value just spreads
/// tributes across more than one Lysis per-league group. Owner order carries no
/// meaning.
pub fn fixture_league(owner_index: usize) -> u16 {
    let span = usize::from(MAX_LEAGUE - MIN_LEAGUE) + 1;
    MIN_LEAGUE + u16::try_from(owner_index % span).unwrap_or(0)
}

/// Returns the per-owner Fidelity league snapshot slots plus the standalone Oracle
/// opening contract. The slots live in Metadosis storage. The caller merges them
/// into the storage trie of the intent account.
pub(crate) fn lysis_contracts(
    day: WorldwideDay,
    subjects: &OpeningSubjectsV1,
) -> (Vec<(B256, U256)>, OpeningContractFixture) {
    // Owner order (subjects.owners is strictly ordered) matches the node's
    // `ordered_league_snapshot_slots`, so the opening's slot order is canonical.
    let fidelity_league_slots: Vec<(B256, U256)> = subjects
        .owners
        .iter()
        .enumerate()
        .map(|(owner_index, owner)| {
            (
                league_snapshot_slot(day.value(), *owner),
                U256::from(fixture_league(owner_index)),
            )
        })
        .collect();

    let oracle_values = entry_price_slots(day, &subjects.reference_isos)
        .expect("canonical Nod price subjects")
        .into_iter()
        .enumerate()
        .map(|(index, slot)| (slot, U256::from(index + 1)))
        .collect::<BTreeMap<_, _>>();

    (
        fidelity_league_slots,
        opening_contract(NOD_ADDRESS, oracle_values.into_iter().collect()),
    )
}

fn opening_contract(address: Address, slots: Vec<(B256, U256)>) -> OpeningContractFixture {
    let words = slots
        .iter()
        .map(|(slot, value)| (U256::from_be_bytes(slot.0), *value))
        .collect::<Vec<_>>();
    let (storage_root, storage_proofs) = storage_trie(&words);
    OpeningContractFixture {
        address,
        slots,
        account: TrieAccount {
            nonce: 1,
            balance: U256::from(10),
            storage_root,
            code_hash: KECCAK_EMPTY,
        },
        storage_proofs,
    }
}
