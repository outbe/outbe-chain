use super::*;
use crate::state::{
    read_committee_snapshot, write_committee_snapshot, CommitteeEntry, CommitteeSnapshot,
    COMMITTEE_SNAPSHOT_RETAIN_EPOCHS,
};
use outbe_primitives::error::Result;

const FIRST: Address = address!("0x00000000000000000000000000000000000000A7");
const SECOND: Address = address!("0x00000000000000000000000000000000000000A8");
const EPOCH: u64 = 9;

/// The exact text of a result: `Revert: <message>` or `Fatal: <message>` for a
/// rejection, else the debug form of the result.
fn outcome_text<T: std::fmt::Debug>(result: Result<T>) -> String {
    match result {
        Err(PrecompileError::Revert(message)) => format!("Revert: {message}"),
        Err(PrecompileError::Fatal(message)) => format!("Fatal: {message}"),
        other => format!("{other:?}"),
    }
}

/// A PENDING validator that confirmed readiness.
fn admitted(vs: &mut ValidatorSet<'_>, validator: Address) -> Result<()> {
    let seed = validator.as_slice()[19];
    vs.register_validator(OWNER, validator, &dummy_consensus_pubkey(seed))?;
    vs.mark_pending(validator)?;
    confirm_ready(vs, validator, seed);
    Ok(())
}

fn snapshot_of(validators: &[Address], vrf_key_len: usize) -> CommitteeSnapshot {
    CommitteeSnapshot {
        committee: validators
            .iter()
            .map(|validator| CommitteeEntry {
                address: *validator,
                consensus_pubkey: dummy_consensus_pubkey(validator.as_slice()[19]),
            })
            .collect(),
        vrf_material_version: 3,
        vrf_group_public_key_bytes: (0..vrf_key_len).map(|byte| byte as u8).collect(),
        vrf_public_polynomial_hash: B256::repeat_byte(0xA9),
    }
}

fn snapshot_storage(
    setup: impl FnOnce(&mut ValidatorSet<'_>) -> Result<()>,
) -> Result<HashMapStorageProvider> {
    let mut storage = configured_storage(10);
    storage.enter(|storage| setup(&mut ValidatorSet::new(storage)))?;
    Ok(storage)
}

fn both_admitted(vs: &mut ValidatorSet<'_>) -> Result<()> {
    admitted(vs, FIRST)?;
    admitted(vs, SECOND)
}

/// Requires `call` to fail with exactly `expected` before any storage write.
fn assert_fatal_without_writes<T: std::fmt::Debug>(
    storage: &mut HashMapStorageProvider,
    call: impl FnOnce(StorageHandle) -> Result<T>,
    expected: &str,
) {
    storage.clear_mutation_failure();
    let result = storage.enter(call);
    assert_eq!(outcome_text(result), expected);
    assert_eq!(
        storage.clear_mutation_failure(),
        0,
        "the snapshot store wrote before it failed"
    );
}

#[test]
fn snapshot_replay_rejects_consensus_collision() -> Result<()> {
    let snapshot = snapshot_of(&[FIRST, SECOND], 96);
    let mut storage = snapshot_storage(both_admitted)?;
    let (_, key) = storage.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))?;
    assert_eq!(
        outcome_text(storage.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))),
        format!("Ok(({:?}, {key:?}))", committee_set_hash_of(&snapshot)),
        "an exact replay is accepted",
    );

    // A consensus snapshot collision under the same key is Fatal.
    storage.enter(|storage| {
        ValidatorSet::new(storage)
            .committee_snapshot_vrf_material_version
            .write(&key, snapshot.vrf_material_version + 1)
    })?;
    assert_fatal_without_writes(
        &mut storage,
        |storage| write_committee_snapshot(storage, EPOCH, &snapshot),
        &format!("Fatal: committee snapshot replay mismatch for key {key}"),
    );

    Ok(())
}

fn committee_set_hash_of(snapshot: &CommitteeSnapshot) -> B256 {
    crate::state::committee_set_hash_v2(EPOCH, snapshot)
}

/// The exists flag of the evicted record, the new committee length, the ring
/// slot and the new exists flag.
type WriteOrderView = (bool, u64, B256, bool);

#[test]
fn snapshot_write_evicts_first_and_publishes_ring_then_exists_last() -> Result<()> {
    let old_epoch = EPOCH - COMMITTEE_SNAPSHOT_RETAIN_EPOCHS;
    let old = snapshot_of(&[FIRST], 32);
    let new = snapshot_of(&[FIRST, SECOND], 33);
    let seed = || {
        let mut storage = configured_storage(10);
        let seeded = storage.enter(|storage| -> Result<()> {
            both_admitted(&mut ValidatorSet::new(storage.clone()))?;
            write_committee_snapshot(storage, old_epoch, &old).map(|_| ())
        });
        assert_eq!(outcome_text(seeded), "Ok(())", "fixture setup failed");
        storage
    };
    let old_key = crate::state::snapshot_identity(old_epoch, &old).1;
    let new_key = crate::state::snapshot_identity(EPOCH, &new).1;
    let ring_index = EPOCH % COMMITTEE_SNAPSHOT_RETAIN_EPOCHS;
    let view = |storage: &mut HashMapStorageProvider| -> WriteOrderView {
        let read = storage.enter(|storage| -> Result<WriteOrderView> {
            let vs = ValidatorSet::new(storage);
            Ok((
                vs.committee_snapshot_exists.read(&old_key)?,
                vs.committee_snapshot_len.read(&new_key)?,
                vs.committee_snapshot_key_ring.read(&ring_index)?,
                vs.committee_snapshot_exists.read(&new_key)?,
            ))
        });
        read.unwrap_or((true, u64::MAX, B256::ZERO, true))
    };
    let views = HashMapStorageProvider::mutation_prefix_views(
        seed,
        |storage| write_committee_snapshot(storage, EPOCH, &new),
        view,
    )?;
    assert_eq!(views.complete, (false, 2, new_key, true));
    let last = views.before_mutation.len() - 1;
    for (operation, before) in views.before_mutation.iter().enumerate() {
        let (old_exists, new_len, ring, new_exists) = *before;
        assert!(!new_exists, "exists is the last write");
        assert!(
            new_len == 0 || !old_exists,
            "operation {operation}: eviction precedes the new fields"
        );
        assert_eq!(
            ring == new_key,
            operation == last,
            "operation {operation}: ring precedes exists"
        );
    }
    Ok(())
}

#[test]
fn stored_vrf_key_round_trips_across_chunk_boundaries() -> Result<()> {
    for (epoch, vrf_key_len) in [(1, 0), (2, 31), (3, 32), (4, 33), (5, 96)] {
        let mut storage = snapshot_storage(both_admitted)?;
        let snapshot = snapshot_of(&[FIRST, SECOND], vrf_key_len);
        let read = storage.enter(|storage| -> Result<Option<CommitteeSnapshot>> {
            let (_, key) = write_committee_snapshot(storage.clone(), epoch, &snapshot)?;
            read_committee_snapshot(storage, key)
        })?;
        assert_eq!(
            read.as_ref(),
            Some(&snapshot),
            "VRF key of {vrf_key_len} bytes"
        );
    }
    Ok(())
}
