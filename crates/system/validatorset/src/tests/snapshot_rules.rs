//! Characterization of the committee snapshot store: the exact rejection of a
//! committee without admitted OCOMP material, the replay comparison, the
//! write order that keeps a partial snapshot unreadable, and the strict
//! epoch lookup of the OCOMP extension.

use super::*;
use crate::state::{
    read_committee_snapshot, read_ocomp_snapshot_extension_at_epoch, read_ocomp_snapshot_member_at,
    write_committee_snapshot, CommitteeEntry, CommitteeSnapshot, COMMITTEE_SNAPSHOT_RETAIN_EPOCHS,
};
use outbe_primitives::error::Result;

const FIRST: Address = address!("0x00000000000000000000000000000000000000A7");
const SECOND: Address = address!("0x00000000000000000000000000000000000000A8");
const EPOCH: u64 = 9;

/// The exact text of a result: `Fatal: <message>` for a fatal rejection, else
/// the debug form of the result.
fn outcome_text<T: std::fmt::Debug>(result: Result<T>) -> String {
    match result {
        Err(PrecompileError::Fatal(message)) => format!("Fatal: {message}"),
        other => format!("{other:?}"),
    }
}

/// A PENDING validator with an admitted OCOMP key.
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
fn snapshot_write_requires_bound_ocomp_material_for_each_member_in_order() -> Result<()> {
    let mut storage = snapshot_storage(|vs| {
        vs.register_validator(OWNER, FIRST, &dummy_consensus_pubkey(0xA7))?;
        vs.register_validator(OWNER, SECOND, &dummy_consensus_pubkey(0xA8))
    })?;
    let snapshot = snapshot_of(&[FIRST, SECOND], 96);
    assert_fatal_without_writes(
        &mut storage,
        |storage| write_committee_snapshot(storage, EPOCH, &snapshot),
        &format!("Fatal: active validator {FIRST} has no admitted OCOMP registration"),
    );

    let mut storage = snapshot_storage(both_admitted)?;
    let mut rekeyed = snapshot_of(&[FIRST, SECOND], 96);
    rekeyed.committee[1].consensus_pubkey = dummy_consensus_pubkey(0x01);
    assert_fatal_without_writes(
        &mut storage,
        |storage| write_committee_snapshot(storage, EPOCH, &rekeyed),
        &format!("Fatal: active validator {SECOND} has stale or invalid OCOMP registration"),
    );
    Ok(())
}

#[test]
fn snapshot_replay_compares_extension_then_members() -> Result<()> {
    let snapshot = snapshot_of(&[FIRST, SECOND], 96);
    let corruptions: [fn(&ValidatorSet<'_>, B256) -> Result<()>; 2] = [
        |vs, key| {
            vs.committee_snapshot_ocomp_binding_hash
                .write(&key, B256::repeat_byte(0x01))
        },
        |vs, key| {
            vs.committee_snapshot_ocomp_key_lo_at
                .get_nested(&key)
                .write(&1, B256::repeat_byte(0x01))
        },
    ];
    for corrupt in corruptions {
        let mut storage = snapshot_storage(both_admitted)?;
        let (_, key) =
            storage.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))?;
        assert_eq!(
            outcome_text(
                storage.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))
            ),
            format!("Ok(({:?}, {key:?}))", committee_set_hash_of(&snapshot)),
            "an exact replay is accepted",
        );
        storage.enter(|storage| corrupt(&ValidatorSet::new(storage), key))?;
        assert_fatal_without_writes(
            &mut storage,
            |storage| write_committee_snapshot(storage, EPOCH, &snapshot),
            &format!("Fatal: committee snapshot replay mismatch for key {key}"),
        );
    }
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

#[test]
fn epoch_extension_lookup_is_strict() -> Result<()> {
    let snapshot = snapshot_of(&[FIRST, SECOND], 96);
    let mut storage = snapshot_storage(both_admitted)?;
    let (_, key) = storage.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))?;
    let found = storage.enter(|storage| read_ocomp_snapshot_extension_at_epoch(storage, EPOCH))?;
    assert_eq!(found.as_ref().map(|(found_key, _)| *found_key), Some(key));
    let lookup_after = |storage: &mut HashMapStorageProvider,
                        corrupt: fn(&ValidatorSet<'_>, B256) -> Result<()>| {
        storage.enter(|storage| -> Result<bool> {
            corrupt(&ValidatorSet::new(storage.clone()), key)?;
            Ok(read_ocomp_snapshot_extension_at_epoch(storage, EPOCH)?.is_some())
        })
    };
    let mut wrong_count = snapshot_storage(both_admitted)?;
    wrong_count.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))?;
    assert!(!lookup_after(&mut wrong_count, |vs, key| vs
        .committee_snapshot_ocomp_member_count
        .write(&key, 3))?);
    let mut wrong_member = snapshot_storage(both_admitted)?;
    wrong_member.enter(|storage| write_committee_snapshot(storage, EPOCH, &snapshot))?;
    assert!(!lookup_after(&mut wrong_member, |vs, key| {
        vs.committee_snapshot_ocomp_key_epoch_at
            .get_nested(&key)
            .write(&0, 7)
    })?);
    let colliding = EPOCH + COMMITTEE_SNAPSHOT_RETAIN_EPOCHS;
    storage.enter(|storage| write_committee_snapshot(storage, colliding, &snapshot))?;
    let (old_epoch, new_epoch) = storage.enter(|storage| -> Result<(bool, bool)> {
        Ok((
            read_ocomp_snapshot_extension_at_epoch(storage.clone(), EPOCH)?.is_some(),
            read_ocomp_snapshot_extension_at_epoch(storage, colliding)?.is_some(),
        ))
    })?;
    assert_eq!((old_epoch, new_epoch), (false, true));
    Ok(())
}

#[test]
fn stored_ocomp_key_padding_must_be_zero() -> Result<()> {
    let snapshot = snapshot_of(&[FIRST], 96);
    let mut storage = snapshot_storage(|vs| admitted(vs, FIRST))?;
    let result = storage.enter(|storage| {
        let (_, key) = write_committee_snapshot(storage.clone(), EPOCH, &snapshot)?;
        let mut padded = [0u8; 32];
        padded[1] = 1;
        ValidatorSet::new(storage.clone())
            .committee_snapshot_ocomp_key_hi_at
            .get_nested(&key)
            .write(&0, B256::from(padded))?;
        read_ocomp_snapshot_member_at(storage, key, 0)
    });
    assert_eq!(
        outcome_text(result),
        "Fatal: stored OCOMP snapshot public-key padding is non-zero"
    );
    Ok(())
}
