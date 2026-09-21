use alloy_primitives::U256;
use outbe_intex::schema::SeriesId;
use outbe_primitives::time::WorldwideDay;

use super::super::validation::{
    ocomp::{verify_paid_bitmap, verify_series_day},
    Incomplete,
};

#[test]
fn every_bitmap_word_and_only_valid_leaf_bits_determine_unpaid_obligations() {
    for count in [1_u32, 255, 256, 257] {
        let words = (0..u64::from(count).div_ceil(256))
            .map(|index| {
                let remaining = u64::from(count) - index * 256;
                if remaining >= 256 {
                    U256::MAX
                } else {
                    (U256::ONE << remaining) - U256::ONE
                }
            })
            .collect::<Vec<_>>();
        let mut visited = vec![];
        let full = verify_paid_bitmap(count, count, None, |index| {
            visited.push(index);
            Ok(words[index as usize])
        })
        .unwrap();
        assert_eq!(visited.len(), words.len());
        assert_eq!(full.words, words.len() as u64);
        assert_eq!(full.paid, u64::from(count));
        assert_eq!(full.unpaid, 0);
        let mut unpaid = words;
        let last_bit = (count - 1) % 256;
        *unpaid.last_mut().unwrap() &= !(U256::ONE << last_bit);
        let partial =
            verify_paid_bitmap(count, count - 1, None, |index| Ok(unpaid[index as usize])).unwrap();
        assert_eq!(partial.unpaid, 1);
    }
    let non_prefix = verify_paid_bitmap(257, 2, None, |index| {
        Ok(if index == 0 {
            U256::ONE << 7
        } else {
            U256::ONE
        })
    })
    .unwrap();
    assert_eq!(non_prefix.paid, 2);
    assert_eq!(non_prefix.unpaid, 255);
}

#[test]
fn inconsistent_bitmap_counts_and_out_of_range_bits_are_not_complete() {
    assert!(verify_paid_bitmap(1, 2, None, |_| panic!(
        "invalid count must reject before reads"
    ))
    .is_err());
    assert!(verify_paid_bitmap(257, 1, None, |_| Ok(U256::ZERO)).is_err());
    for count in [1_u32, 255, 257] {
        assert!(verify_paid_bitmap(count, 0, None, |index| Ok(
            if u64::from(index) + 1 == u64::from(count).div_ceil(256) {
                U256::ONE << (count % 256)
            } else {
                U256::ZERO
            }
        ))
        .is_err());
    }
}

#[test]
fn bitmap_read_failure_and_scan_budget_never_produce_success() {
    let mut visited = 0;
    let error = verify_paid_bitmap(257, 0, Some(1), |_| {
        visited += 1;
        Ok(U256::ZERO)
    })
    .unwrap_err();
    assert_eq!(visited, 1);
    assert!(error.downcast_ref::<Incomplete>().is_some());
    assert!(error.to_string().contains("1/2"));
    let error = verify_paid_bitmap(257, 0, None, |index| {
        if index == 1 {
            eyre::bail!("missing native word");
        }
        Ok(U256::ZERO)
    })
    .unwrap_err();
    assert!(error.to_string().contains("missing native word"));
}

#[test]
fn canonical_series_identity_is_checked_before_decoding_its_day() {
    for currency in [*b"USD", *b"949"] {
        let day = WorldwideDay::new(20_240_229);
        assert_eq!(
            verify_series_day(SeriesId::pack(day, currency, b'U').unwrap()).unwrap(),
            day
        );
    }
    for malformed in [
        *b"00000000-USD-U",
        *b"20260230-USD-U",
        *b"20260101_USD-U",
        *b"20260101-USD_U",
        *b"20260101-usd-U",
        *b"20260101-USD-!",
        *b"X0260101-USD-U",
        [255; 14],
    ] {
        assert!(verify_series_day(SeriesId::from_bytes(malformed)).is_err());
    }
}

use super::super::{
    native::RethReadOnlyView,
    validation::{
        canonical_state::CanonicalState, evm::verify_current_evm, headers::verify_retained_headers,
        ocomp::CanonicalInventory,
    },
};
use crate::OutbeHeader;
use alloy_consensus::Sealable;
use alloy_primitives::{keccak256, Address, B256};
use outbe_intex::schema::{CertifiedPayoutRound, IntexContract, SeriesRecord};
use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, types::Storable, StorageHandle};
use reth_ethereum::{
    provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    },
    trie::root::{state_root_unhashed, storage_root_unhashed},
};
use reth_primitives_traits::{Account, StorageEntry};
use std::collections::BTreeMap;
const DAY: WorldwideDay = WorldwideDay::new(20_200_101);
fn with_owner_storage(
    version: u32,
    owner: HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    with_prepared_owner_storage(version, 101, |_| owner, check);
}

fn with_prepared_owner_storage(
    version: u32,
    execution_height: u64,
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    let (_source, layout, _) = super::evm::state_fixture(version);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let request = tx
        .get::<tables::Headers<OutbeHeader>>(100)
        .unwrap()
        .unwrap();
    let owner = prepare(&request);
    tx.clear::<tables::PlainAccountState>().unwrap();
    tx.clear::<tables::PlainStorageState>().unwrap();
    tx.clear::<tables::HashedAccounts>().unwrap();
    tx.clear::<tables::HashedStorages>().unwrap();
    let mut accounts: BTreeMap<Address, Vec<(B256, U256)>> = BTreeMap::new();
    for ((address, slot), value) in owner.storage {
        if !value.is_zero() {
            accounts
                .entry(address)
                .or_default()
                .push((B256::from(slot.to_be_bytes::<32>()), value));
        }
    }
    let root = state_root_unhashed(accounts.iter().map(|(address, words)| {
        (
            *address,
            Account::default().into_trie_account(storage_root_unhashed(words.iter().copied())),
        )
    }));
    for (address, words) in accounts {
        if version == 1 {
            tx.put::<tables::PlainAccountState>(address, Account::default())
                .unwrap();
            for (key, value) in words {
                tx.put::<tables::PlainStorageState>(address, StorageEntry { key, value })
                    .unwrap();
            }
        } else {
            tx.put::<tables::HashedAccounts>(keccak256(address), Account::default())
                .unwrap();
            for (key, value) in words {
                tx.put::<tables::HashedStorages>(
                    keccak256(address),
                    StorageEntry {
                        key: keccak256(key),
                        value,
                    },
                )
                .unwrap();
            }
        }
    }
    let mut header = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    header.inner.state_root = root;
    header.inner.timestamp = 1_010;
    header.inner.number = execution_height;
    tx.put::<tables::CanonicalHeaders>(execution_height, header.hash_slow())
        .unwrap();
    tx.put::<tables::Headers<OutbeHeader>>(execution_height, header)
        .unwrap();
    for stage in ["Execution", "Finish"] {
        let mut checkpoint = tx
            .get::<tables::StageCheckpoints>(stage.into())
            .unwrap()
            .unwrap();
        checkpoint.block_number = execution_height;
        tx.put::<tables::StageCheckpoints>(stage.into(), checkpoint)
            .unwrap();
    }
    tx.clear::<tables::AccountChangeSets>().unwrap();
    tx.clear::<tables::StorageChangeSets>().unwrap();
    tx.commit().unwrap();
    drop(db);
    let before = super::headers::fingerprint(_source.path());
    let scratch = tempfile::tempdir().unwrap();
    let source = RethReadOnlyView::open(&layout).unwrap();
    let verified = verify_current_evm(&source, scratch.path()).unwrap();
    let headers = verify_retained_headers(&source, &[99]).unwrap();
    assert_eq!(headers.required_missing, vec![99]);
    let state = CanonicalState::new(&verified, &source, &headers);
    check(&state, &source);
    assert_eq!(super::headers::fingerprint(_source.path()), before);
}

fn seed_nod_generation(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
    sequence: u64,
) -> NodCertifiedGenerationProjection {
    let generation = NodCertifiedGenerationProjection {
        worldwide_day: day,
        generation: sequence,
        job_id: B256::repeat_byte(1),
        program_semantics_hash: B256::repeat_byte(2),
        protocol_bundle_hash: B256::repeat_byte(3),
        nod_root: B256::repeat_byte(4),
        bucket_root: B256::repeat_byte(5),
        output_manifest_root: B256::repeat_byte(6),
        tribute_count: 3,
        nod_count: 3,
        bucket_count: 1,
        nod_amount_total: U256::from(99),
        lysis_allocation_minor: U256::from(77),
        issued_at: 1_000,
        next_nod_ordinal: 1,
        last_progress_height: 90,
    };
    let nod = NodContract::new(storage);
    nod.ocomp_target_generation
        .write(&day, generation.generation)
        .unwrap();
    nod.ocomp_namespace_root
        .write(&day, generation.nod_root)
        .unwrap();
    nod.ocomp_bucket_root
        .write(&day, generation.bucket_root)
        .unwrap();
    nod.ocomp_output_manifest_root
        .write(&day, generation.output_manifest_root)
        .unwrap();
    nod.ocomp_generation_metadata
        .write(&day, generation.metadata_word())
        .unwrap();
    nod.ocomp_nod_amount_total
        .write(&day, generation.nod_amount_total)
        .unwrap();
    nod.ocomp_lysis_allocation_minor
        .write(&day, generation.lysis_allocation_minor)
        .unwrap();
    nod.ocomp_materialization_job_id
        .write(&day, generation.job_id)
        .unwrap();
    nod.ocomp_materialization_protocol_bundle_hash
        .write(&day, generation.protocol_bundle_hash)
        .unwrap();
    nod.ocomp_materialization_program_semantics_hash
        .write(&day, generation.program_semantics_hash)
        .unwrap();
    nod.ocomp_materialization_next_nod_ordinal
        .write(&day, generation.next_nod_ordinal)
        .unwrap();
    nod.ocomp_materialization_last_progress_height
        .write(&day, generation.last_progress_height)
        .unwrap();
    nod.ocomp_materialization_queue_wwd
        .write(&sequence, day)
        .unwrap();
    generation
}

fn series(day: WorldwideDay, currency: [u8; 3]) -> SeriesRecord {
    SeriesRecord {
        series_id: SeriesId::pack(day, currency, b'U').unwrap(),
        issuance_currency: 840,
        reference_currency: 840,
        issued_units: 3,
        promis_load_minor: U256::from(10),
        entry_price_minor: U256::from(11),
        floor_price_minor: U256::from(12),
        call_price_minor: U256::from(13),
        call_window_seconds: 14,
        call_threshold_seconds: 15,
        call_notice_period_seconds: 16,
        issued_at: 1_000,
        called_at: 0,
        state: 0,
        worldwide_day: day,
    }
}

fn queued_owner(count: u64) -> HashMapStorageProvider {
    let mut owner = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut owner, |storage| {
        let nod = NodContract::new(storage.clone());
        nod.ocomp_materialization_head_sequence.write(1).unwrap();
        nod.ocomp_materialization_tail_sequence
            .write(count + 1)
            .unwrap();
        for sequence in 1..=count {
            let day = WorldwideDay::new(DAY.value() + sequence as u32 - 1);
            seed_nod_generation(storage.clone(), day, sequence);
            nod.ocomp_materialization_job_id
                .write(&day, B256::repeat_byte(sequence as u8))
                .unwrap();
        }
    });
    owner
}

#[test]
fn canonical_inventory_discovers_later_nod_jobs_without_any_local_job_files() {
    with_owner_storage(2, queued_owner(2), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        assert_eq!(
            (
                inventory.bounds.nod_head,
                inventory.bounds.nod_tail,
                inventory.bounds.nod_entries
            ),
            (1, 3, 2)
        );
        assert_eq!(inventory.bounds.series, 0);
        let mut jobs = vec![];
        inventory
            .visit_nod(&mut |sequence, projection| {
                jobs.push((sequence, projection.job_id));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            jobs,
            vec![(1, B256::repeat_byte(1)), (2, B256::repeat_byte(2))]
        );
    });
}

#[test]
fn canonical_fifo_rejects_later_holes_duplicates_complete_entries_and_bad_bounds() {
    for damage in [
        "hole",
        "duplicate-day",
        "duplicate-job",
        "complete",
        "tail",
        "bounds",
    ] {
        let mut owner = queued_owner(2);
        StorageHandle::enter(&mut owner, |storage| {
            let nod = NodContract::new(storage);
            let second = WorldwideDay::new(DAY.value() + 1);
            match damage {
                "hole" => nod
                    .ocomp_materialization_queue_wwd
                    .write(&2, WorldwideDay::new(0))
                    .unwrap(),
                "duplicate-day" => nod.ocomp_materialization_queue_wwd.write(&2, DAY).unwrap(),
                "duplicate-job" => nod
                    .ocomp_materialization_job_id
                    .write(&second, B256::repeat_byte(1))
                    .unwrap(),
                "complete" => nod
                    .ocomp_materialization_next_nod_ordinal
                    .write(&second, 3)
                    .unwrap(),
                "tail" => nod.ocomp_materialization_queue_wwd.write(&3, DAY).unwrap(),
                "bounds" => nod.ocomp_materialization_head_sequence.write(4).unwrap(),
                _ => unreachable!(),
            }
        });
        with_owner_storage(2, owner, |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            assert!(
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).is_err(),
                "{damage}"
            );
        });
    }
}

fn payout_owner(unpaid: bool, with_round: bool) -> HashMapStorageProvider {
    let mut owner = queued_owner(0);
    StorageHandle::enter(&mut owner, |storage| {
        let intex = IntexContract::new(storage);
        let records = [
            series(DAY, *b"USD"),
            series(DAY, *b"EUR"),
            series(WorldwideDay::new(DAY.value() + 1), *b"USD"),
        ];
        intex.total_series.write(3).unwrap();
        for (index, record) in records.iter().enumerate() {
            intex.series.create(record).unwrap();
            intex
                .series_id_at_index
                .write(&(index as u64), record.series_id.to_word())
                .unwrap();
        }
        intex
            .ocomp_contributor_root
            .write(&DAY, B256::repeat_byte(9))
            .unwrap();
        intex
            .ocomp_contributor_metadata
            .write(&DAY, U256::ONE | (U256::from(257) << 64))
            .unwrap();
        intex
            .ocomp_eligible_nominal_total
            .write(&DAY, U256::from(1000))
            .unwrap();
        if with_round {
            intex
                .ocomp_payout_round
                .create(&CertifiedPayoutRound {
                    wwd: DAY.value(),
                    amount: U256::from(100),
                    paid_so_far: U256::from(10),
                    paid_leaf_count: if unpaid { 256 } else { 257 },
                    active: 1,
                })
                .unwrap();
            intex
                .ocomp_paid_leaves
                .write(&IntexContract::paid_bitmap_key(DAY.value(), 0), U256::MAX)
                .unwrap();
            intex
                .ocomp_paid_leaves
                .write(
                    &IntexContract::paid_bitmap_key(DAY.value(), 1),
                    if unpaid { U256::ZERO } else { U256::ONE },
                )
                .unwrap();
        }
    });
    owner
}

#[test]
fn permanent_series_index_deduplicates_days_and_finds_old_later_word_obligation() {
    with_owner_storage(2, payout_owner(true, true), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        assert_eq!(
            (
                inventory.bounds.series,
                inventory.bounds.days,
                inventory.bounds.unpaid_days,
                inventory.bounds.bitmap_words
            ),
            (3, 2, 1, 2)
        );
        let mut days = vec![];
        inventory
            .visit_payouts(&mut |day, certified| {
                assert_eq!(certified.contributor_count, 257);
                days.push(day);
                Ok(())
            })
            .unwrap();
        assert_eq!(days, vec![DAY]);
    });
    for (unpaid, round) in [(false, true), (true, false)] {
        with_owner_storage(2, payout_owner(unpaid, round), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            assert_eq!(inventory.bounds.unpaid_days, 0);
            inventory
                .visit_payouts(&mut |_, _| panic!("no current payout obligation"))
                .unwrap();
        });
    }
}

#[test]
fn canonical_index_duplicates_and_inconsistent_rounds_cannot_hide_required_data() {
    for damage in [
        "duplicate-series",
        "round-without-generation",
        "paid-count",
        "paid-amount",
        "tail-bit",
        "series-record-day",
        "index-hole",
        "missing-series-record",
        "zero-count-open-round",
    ] {
        let mut owner = payout_owner(true, true);
        StorageHandle::enter(&mut owner, |storage| {
            let intex = IntexContract::new(storage);
            match damage {
                "index-hole" => intex.series_id_at_index.write(&1, U256::ZERO).unwrap(),
                "missing-series-record" => intex
                    .series_id_at_index
                    .write(&1, SeriesId::pack(DAY, *b"GBP", b'U').unwrap().to_word())
                    .unwrap(),
                "zero-count-open-round" => {
                    intex
                        .ocomp_contributor_metadata
                        .write(&DAY, U256::ONE)
                        .unwrap();
                    intex
                        .ocomp_eligible_nominal_total
                        .write(&DAY, U256::ZERO)
                        .unwrap();
                    intex
                        .ocomp_payout_round
                        .update(&CertifiedPayoutRound {
                            wwd: DAY.value(),
                            amount: U256::from(100),
                            paid_so_far: U256::ZERO,
                            paid_leaf_count: 0,
                            active: 1,
                        })
                        .unwrap();
                }
                "duplicate-series" => intex
                    .series_id_at_index
                    .write(&1, series(DAY, *b"USD").series_id.to_word())
                    .unwrap(),
                "round-without-generation" => {
                    intex
                        .ocomp_contributor_root
                        .write(&DAY, B256::ZERO)
                        .unwrap();
                    intex
                        .ocomp_contributor_metadata
                        .write(&DAY, U256::ZERO)
                        .unwrap();
                    intex
                        .ocomp_eligible_nominal_total
                        .write(&DAY, U256::ZERO)
                        .unwrap();
                }
                "paid-count" | "paid-amount" => {
                    intex
                        .ocomp_payout_round
                        .update(&CertifiedPayoutRound {
                            wwd: DAY.value(),
                            amount: U256::from(100),
                            paid_so_far: U256::from(if damage == "paid-amount" { 101 } else { 10 }),
                            paid_leaf_count: if damage == "paid-count" { 258 } else { 256 },
                            active: 1,
                        })
                        .unwrap();
                }
                "tail-bit" => intex
                    .ocomp_paid_leaves
                    .write(
                        &IntexContract::paid_bitmap_key(DAY.value(), 1),
                        U256::from(2),
                    )
                    .unwrap(),
                "series-record-day" => {
                    let mut record = series(DAY, *b"USD");
                    record.worldwide_day = WorldwideDay::new(DAY.value() + 1);
                    intex.series.update(&record).unwrap();
                }
                _ => unreachable!(),
            }
        });
        with_owner_storage(2, owner, |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            assert!(
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).is_err(),
                "{damage}"
            );
        });
    }
}

#[test]
fn canonical_scan_budget_and_scratch_overlap_fail_without_source_changes() {
    with_owner_storage(2, queued_owner(2), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let error = CanonicalInventory::scan(state, scratch.path(), &source.protected, Some(1))
            .err()
            .unwrap();
        assert!(error.downcast_ref::<Incomplete>().is_some());
        assert!(error.to_string().contains("1/2"));
        let protected = outbe_snapshot::layout::ProtectedPaths(vec![scratch.path().to_path_buf()]);
        assert!(CanonicalInventory::scan(state, scratch.path(), &protected, None).is_err());
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    });
    with_owner_storage(1, payout_owner(true, false), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let error = CanonicalInventory::scan(state, scratch.path(), &source.protected, Some(1))
            .err()
            .unwrap();
        assert!(error.downcast_ref::<Incomplete>().is_some());
        assert!(error
            .to_string()
            .contains("Intex series scan stopped at 1/3"));
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    });
}

mod active_inventory {
    use super::{queued_owner, with_owner_storage, CanonicalInventory};
    use alloy_primitives::U256;
    use outbe_primitives::{
        addresses::METADOSIS_ADDRESS,
        storage::{types::StorageBytes, StorageHandle},
    };

    #[test]
    fn empty_native_aggregate_has_no_active_intents_without_local_job_files() {
        for version in [1, 2] {
            with_owner_storage(version, queued_owner(0), |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                for maximum in [None, Some(0)] {
                    let inventory =
                        CanonicalInventory::scan(state, scratch.path(), &source.protected, maximum)
                            .unwrap();
                    assert!(inventory.active_jobs().is_empty());
                    assert_eq!(inventory.bounds.active_intents, 0);
                    drop(inventory);
                    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
                }
                // with_owner_storage also compares the complete source fingerprint.
            });
        }
    }

    #[test]
    fn malformed_native_scheduler_propagates_through_inventory_without_source_writes() {
        // Current Metadosis schema places the one-slot scheduler StorageBytes
        // immediately before the fixed OCOMP job-records mapping base slot 21.
        // Raw corruption is test setup only; the inventory calls the owner view.
        let scheduler_slot = U256::from(20);
        for version in [1, 2] {
            for oversized in [false, true] {
                let mut owner = queued_owner(0);
                if oversized {
                    // Solidity long-bytes marker: 2 * length + 1. This length
                    // exceeds the native u16 live-index capacity before payload reads.
                    owner.storage.insert(
                        (METADOSIS_ADDRESS, scheduler_slot),
                        U256::from(40_000_001_u64),
                    );
                } else {
                    StorageHandle::enter(&mut owner, |storage| {
                        StorageBytes::new(scheduler_slot, METADOSIS_ADDRESS, storage)
                            .write(&[0; 8])
                            .unwrap();
                    });
                }
                with_owner_storage(version, owner, |state, source| {
                    let scratch = tempfile::tempdir().unwrap();
                    let error =
                        CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                            .err()
                            .expect("malformed live index cannot become an empty inventory");
                    let expected = if oversized {
                        "OCOMP live scheduler exceeds native byte cap"
                    } else {
                        "OCOMP live index magic/version mismatch"
                    };
                    assert!(error.to_string().contains(expected), "{error:#}");
                    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
                });
            }
        }
    }
}

mod payout_inventory {
    use super::super::headers::fingerprint;
    use super::{payout_owner, with_owner_storage, CanonicalInventory, Incomplete, DAY};
    use alloy_primitives::{Address, B256, U256};
    use outbe_intex::{
        payout::{contributor_list_root, encode_contributor_leaf, ContributorLeafData},
        schema::IntexContract,
    };
    use outbe_ocomp::payout_artifact::CONTRIBUTOR_PAYOUT_ARTIFACT_FILE;
    use outbe_ocomp_protocol::{
        profile::poc_schema_limits, result::ExactCountsV1, state::ActiveGenerationV1,
    };
    use outbe_primitives::{
        addresses::METADOSIS_ADDRESS,
        error::{PrecompileError, Result},
        storage::{
            hashmap::HashMapStorageProvider,
            readonly::{ReadOnlyStorageProvider, StorageReader},
            types::StorageBytes,
            StorageHandle,
        },
    };
    use std::{
        cell::Cell,
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        rc::Rc,
    };

    fn leaves() -> Vec<ContributorLeafData> {
        (0..257_u32)
            .map(|index| {
                let mut owner = [0; 20];
                owner[16..].copy_from_slice(&(index + 1).to_be_bytes());
                ContributorLeafData {
                    owner: Address::from(owner),
                    source_tribute_id: (U256::from(DAY.value()) << 224) | U256::from(index + 1),
                    nominal: U256::from(if index == 0 { 744 } else { 1 }),
                }
            })
            .collect()
    }

    fn generation(root: B256) -> ActiveGenerationV1 {
        ActiveGenerationV1 {
            job_id: B256::repeat_byte(0x71),
            program_semantics_hash: B256::repeat_byte(2),
            nod_root: B256::repeat_byte(3),
            bucket_root: B256::repeat_byte(4),
            contributor_root: root,
            output_manifest_root: B256::repeat_byte(6),
            exact_counts: ExactCountsV1 {
                tribute_count: 257,
                nod_count: 257,
                bucket_count: 1,
                contributor_count: 257,
                semantic_event_count: 3,
            },
            result_evidence_hash: B256::repeat_byte(7),
            availability_certificate_hash: None,
        }
    }

    fn native_owner(active: &ActiveGenerationV1, root: B256) -> HashMapStorageProvider {
        struct AbsentGenerationReader(Rc<Cell<Option<U256>>>);
        impl StorageReader for AbsentGenerationReader {
            fn read_storage(&self, address: Address, key: B256) -> Result<U256> {
                assert_eq!(address, METADOSIS_ADDRESS);
                assert!(self.0.replace(Some(U256::from_be_bytes(key.0))).is_none());
                Ok(U256::ZERO)
            }
        }
        // Observe the public getter's native length slot, as canonical_state tests do.
        let observed = Rc::new(Cell::new(None));
        let mut reader = ReadOnlyStorageProvider::new(AbsentGenerationReader(observed.clone()));
        let absent =
            outbe_metadosis::api::get_active_lysis_generation(StorageHandle::new(&mut reader), DAY);
        assert!(
            matches!(absent,Err(PrecompileError::Revert(message)) if message=="ActiveGenerationV1 not found")
        );
        let mut owner = payout_owner(true, true);
        StorageHandle::enter(&mut owner, |storage| {
            IntexContract::new(storage.clone())
                .ocomp_contributor_root
                .write(&DAY, root)
                .unwrap();
            StorageBytes::new(observed.get().unwrap(), METADOSIS_ADDRESS, storage)
                .write(&active.encode_canonical(&poc_schema_limits()).unwrap())
                .unwrap();
        });
        owner
    }

    fn job_root(root: &Path, job: B256) -> PathBuf {
        root.join("supervisor-v1")
            .join("jobs")
            .join(format!("{job:x}"))
    }

    fn write_payout(root: &Path, job: B256, leaves: &[ContributorLeafData]) -> PathBuf {
        let directory = job_root(root, job);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
        let bytes = leaves
            .iter()
            .flat_map(encode_contributor_leaf)
            .collect::<Vec<_>>();
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn unpaid_old_day_uses_exact_canonical_job_without_intermediate_catalogs() {
        let leaves = leaves();
        let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
        let active = generation(root);
        for version in [1, 2] {
            with_owner_storage(version, native_owner(&active, root), |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                assert_eq!(inventory.bounds.unpaid_days, 1);
                let public = tempfile::tempdir().unwrap();
                let path = write_payout(public.path(), active.job_id, &leaves);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
                assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
                let before = fingerprint(public.path());
                assert_eq!(inventory.verify_payout_files(public.path()).unwrap(), 1);
                assert_eq!(fingerprint(public.path()), before);
                // Existing with_owner_storage fingerprints canonical source bytes/modes too.
            });
        }
    }

    #[test]
    fn absent_entire_job_or_file_and_wrong_job_substitute_are_incomplete() {
        let leaves = leaves();
        let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
        let active = generation(root);
        with_owner_storage(2, native_owner(&active, root), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let public = tempfile::tempdir().unwrap();
            let missing = public.path().join("absent-ocomp");
            let before = fingerprint(public.path());
            let error = inventory.verify_payout_files(&missing).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(!missing.exists());
            assert_eq!(fingerprint(public.path()), before);
            for stage in ["no-job", "wrong-job", "no-file"] {
                if stage == "wrong-job" {
                    write_payout(public.path(), B256::repeat_byte(0x72), &leaves);
                } else if stage == "no-file" {
                    fs::create_dir_all(job_root(public.path(), active.job_id)).unwrap();
                }
                let before = fingerprint(public.path());
                let error = inventory.verify_payout_files(public.path()).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_some(),
                    "{stage}: {error:#}"
                );
                assert_eq!(fingerprint(public.path()), before);
            }
        });
    }

    #[test]
    fn corrupt_canonical_payout_leaf_is_failed_and_not_missing_input() {
        let leaves = leaves();
        let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
        let active = generation(root);
        with_owner_storage(2, native_owner(&active, root), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let public = tempfile::tempdir().unwrap();
            let path = write_payout(public.path(), active.job_id, &leaves);
            let mut bytes = fs::read(&path).unwrap();
            bytes[83] ^= 1;
            fs::write(path, bytes).unwrap();
            let before = fingerprint(public.path());
            let error = inventory.verify_payout_files(public.path()).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            assert_eq!(fingerprint(public.path()), before);
        });
    }

    #[test]
    fn active_generation_requires_job_and_matching_certified_contributors() {
        let leaves = leaves();
        let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
        for damage in ["root", "count", "zero-job"] {
            let mut active = generation(root);
            match damage {
                "count" => active.exact_counts.contributor_count = 256,
                "zero-job" => active.job_id = B256::ZERO,
                _ => active.contributor_root = B256::repeat_byte(0x99),
            }
            with_owner_storage(2, native_owner(&active, root), |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                let public = tempfile::tempdir().unwrap();
                write_payout(public.path(), active.job_id, &leaves);
                let before = fingerprint(public.path());
                let error = inventory.verify_payout_files(public.path()).unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
                assert_eq!(fingerprint(public.path()), before);
            });
        }
    }

    #[test]
    fn fully_paid_and_no_round_days_require_no_files_or_active_generation() {
        for (unpaid, round) in [(false, true), (true, false)] {
            with_owner_storage(2, payout_owner(unpaid, round), |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                assert_eq!(inventory.bounds.unpaid_days, 0);
                let public = tempfile::tempdir().unwrap();
                let missing = public.path().join("absent-ocomp");
                let before = fingerprint(public.path());
                assert_eq!(inventory.verify_payout_files(&missing).unwrap(), 0);
                assert!(!missing.exists());
                assert_eq!(fingerprint(public.path()), before);
            });
        }
    }
}

mod nod_inventory {
    use super::super::headers::fingerprint;
    use super::{
        queued_owner, seed_nod_generation, with_owner_storage, CanonicalInventory, Incomplete,
    };
    use alloy_primitives::{Address, B256, U256};
    use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
    use outbe_lysis::program_v1::{
        planner::{
            LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedUnitPositionV1,
        },
        result::{
            encode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1,
            RootReduceSummaryV1,
        },
    };
    use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
    use outbe_ocomp::nod_materialization::build_nod_materialization_batch_with_references;
    use outbe_ocomp::{
        admission_catalog::{
            AdmissionCatalogReader, AdmissionPositionV1, VerifiedAdmissionCatalog,
        },
        bundle::PinnedProtocolBundle,
        cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
        control::poc_schema_limits,
        input_artifacts::{
            poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
            InputArtifactIdentity,
        },
        input_ref_catalog::VerifiedInputChunkRefCatalog,
        lysis_plan_audit::LocalLysisPlanAuditV1,
    };
    use outbe_ocomp_protocol::{
        common::{BoundedBytes, ProofBytes},
        input::{
            materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind,
            InputManifestV1,
        },
        opening::{
            partition_lysis_opening_subjects, LysisOpeningsProofV1, RawContractOpeningProofV1,
            RawStorageSlotV1,
        },
        registry::{
            ObjectKind, FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID,
        },
        result::{ContributorActionV1, NodActionV1, OutputManifestEntryV1, ResultChunkV1},
        unit::{UnitArtifactV1, UnitPhase, WorkOutputHeaderV1},
        ListKind,
    };
    use outbe_ocomp_protocol::{
        nod_materialization::NodMaterializationHeadV1, profile::ProtocolBundleV1, CasObjectRefV1,
        StreamingOrderedListRoot,
    };
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_primitives::time::WorldwideDay;
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    const CAS_LIMITS: CasLimits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: 64 * 1_048_576,
    };
    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }
    struct Fixture {
        cas_root: PathBuf,
        job_id: B256,
        day: WorldwideDay,
        bundle: PinnedProtocolBundle,
        nod_root: B256,
        bucket_root: B256,
        output_manifest_root: B256,
        nod_count: u32,
        result_chunk_refs: Vec<CasObjectRefV1>,
    }
    // Protocol-shaped native CAS/planner fixture. Minimal non-root phase payloads
    // support structural/proof tests; this is not real worker-pipeline E2E evidence.
    fn protocol_bundle() -> ProtocolBundleV1 {
        ProtocolBundleV1 {
            protocol_version: 1,
            fork_id: B256::repeat_byte(1),
            intent_codec_id: B256::repeat_byte(2),
            finalized_intent_proof_codec_id: B256::repeat_byte(3),
            tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
            fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
            oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
            result_codec_id: B256::repeat_byte(4),
            action_codec_id: B256::repeat_byte(5),
            activation_codec_id: B256::repeat_byte(6),
            evidence_codec_id: B256::repeat_byte(7),
            request_semantics_version: 1,
            lysis_program_semantics_hash: B256::repeat_byte(8),
            planner_spec_version: 1,
            reducer_spec_version: 1,
            activation_apply_semantics_hash: B256::repeat_byte(9),
            effect_contract_registry_hash: B256::repeat_byte(10),
            object_codec_registry_hash: B256::repeat_byte(11),
            correctness_profile_id: B256::repeat_byte(12),
            capacity_profile_id: B256::repeat_byte(13),
            result_signature_profile_id: B256::repeat_byte(14),
            finality_verifier_and_vote_domain_id: B256::repeat_byte(15),
            consensus_committee_history_schema_version: 1,
            ocomp_committee_schema_version: 1,
            proof_system_and_verifier_key_id: None,
            da_codec_and_binding_verifier_id: None,
            anti_equivocation_journal_schema_hash: B256::repeat_byte(16),
            mode_pause_revocation_semantics_hash: B256::repeat_byte(17),
            upgrade_fsm_semantics_hash: B256::repeat_byte(18),
            release_requirement_catalog_sequence: 1,
            release_requirement_catalog_hash: B256::repeat_byte(19),
            release_requirement_catalog_parent_hash: B256::repeat_byte(20),
            release_gate_authority_envelope_hash: B256::repeat_byte(21),
            release_approval_policy_hash: B256::repeat_byte(22),
            release_validator_command_artifact_hash: B256::repeat_byte(23),
            consensus_state_schema_version: 1,
            migration_manifest_hash: B256::repeat_byte(24),
            required_upgrade_handler_set_hash: B256::repeat_byte(25),
        }
    }
    fn fixture(root: &Path, job_seed: u8, day: WorldwideDay, tribute_count: u32) -> Fixture {
        let limits = poc_schema_limits();
        let list_limits = poc_input_list_limits();
        let bundle = protocol_bundle();
        let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let pinned_bundle = PinnedProtocolBundle::decode(
            &bundle.encode_canonical(&limits).unwrap(),
            bundle_hash,
            &limits,
        )
        .unwrap();
        let job_id = hash(job_seed);

        let mut tributes = (0..tribute_count)
            .map(|index| {
                let mut owner_bytes = [0_u8; 20];
                owner_bytes[16..].copy_from_slice(&(index + 1).to_be_bytes());
                let owner = Address::from(owner_bytes);
                TributeBodyV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_amount_minor: U256::from(1),
                    issuance_currency: if index % 2 == 0 { 840 } else { 826 },
                    nominal_amount_minor: U256::from((index % 7) + 1),
                    reference_currency: if index % 3 == 0 { 978 } else { 392 },
                    tribute_price_minor: U256::from(1),
                    exclude_from_intex_issuance: false,
                }
            })
            .collect::<Vec<_>>();
        tributes.sort_by_key(|tribute| tribute.tribute_id);
        let mut contributors_by_owner = tributes
            .iter()
            .map(|tribute| ContributorActionV1 {
                owner: tribute.owner,
                source_tribute_id: *tribute.tribute_id,
                nominal_amount_minor: tribute.nominal_amount_minor,
            })
            .collect::<Vec<_>>();
        contributors_by_owner
            .sort_by_key(|contributor| (contributor.owner, contributor.source_tribute_id));
        let nod_action_tributes = tributes.clone();
        let owners = tributes
            .iter()
            .map(|tribute| tribute.owner)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut reference_isos = tributes
            .iter()
            .map(|tribute| tribute.reference_currency)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        reference_isos.push(840);
        reference_isos.sort_unstable();
        reference_isos.dedup();
        let finalized_state_root = hash(0x32);
        let raw_opening = |address, slot_byte| RawContractOpeningProofV1 {
            contract_address: address,
            state_root: finalized_state_root,
            ordered_slots: vec![RawStorageSlotV1 {
                slot: hash(slot_byte),
                value: U256::from(1),
            }],
            account_proof: ProofBytes(vec![0xa1]),
            storage_proof: ProofBytes(vec![0xb1]),
        };
        let mut fidelity_openings = Vec::new();
        let mut oracle_opening = None;
        for subjects in partition_lysis_opening_subjects(&owners, &reference_isos, &limits).unwrap()
        {
            let openings = materialize_authenticated_openings(
                &LysisOpeningsProofV1 {
                    protocol_bundle_hash: bundle_hash,
                    job_id,
                    finalized_block_hash: hash(0x31),
                    finalized_state_root,
                    wwd: day.value(),
                    subjects,
                    fidelity: raw_opening(Address::repeat_byte(0x63), 0x64),
                    oracle: raw_opening(Address::repeat_byte(0x65), 0x66),
                },
                &bundle,
                &limits,
            )
            .unwrap();
            fidelity_openings.push(openings.fidelity);
            match &oracle_opening {
                None => oracle_opening = Some(openings.oracle),
                Some(existing) => assert_eq!(existing, &openings.oracle),
            }
        }

        let job = hex::encode(job_id);
        let cas_root = root.join("cas-v1");
        let input_ref_root = root.join("exporter-v1/input-refs").join(&job);
        let admission_root = root
            .join("supervisor-v1/jobs")
            .join(&job)
            .join("admissions");
        let bundle_dir = root.join("protocol-bundles-v1");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(
            bundle_dir.join(format!("{}.ocb1", hex::encode(bundle_hash))),
            bundle.encode_canonical(&limits).unwrap(),
        )
        .unwrap();
        let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
        let published = publish_input_artifact_set(
            &cas,
            &input_ref_root,
            &bundle,
            InputArtifactContents {
                identity: InputArtifactIdentity {
                    job_id,
                    attempt: 0,
                    checkpoint: CheckpointIdentityV1 {
                        finalized_block_number: 90,
                        finalized_block_hash: hash(0x31),
                        finalized_state_root,
                        finalized_ce_root: hash(0x33),
                        ce_schema_version: 1,
                    },
                    wwd: day.value(),
                    sealed_tribute_collection_key: hash(0x34),
                    sealed_tribute_collection_root: hash(0x35),
                },
                canonical_tributes: tributes
                    .iter()
                    .map(|tribute| encode_tribute_v1(tribute).unwrap())
                    .collect(),
                fidelity_openings,
                oracle_opening: oracle_opening.unwrap(),
            },
            &limits,
            list_limits,
        )
        .unwrap();
        let manifest = InputManifestV1::decode_canonical(
            cas.read_verified(&published.manifest_ref).unwrap().bytes(),
            &limits,
        )
        .unwrap();
        let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
            &input_ref_root,
            &cas,
            &published.manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        let all_input_refs = input_refs_for_plan
            .exact_cursor()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let tribute_refs = all_input_refs
            .iter()
            .filter(|reference| reference.kind == InputChunkKind::Tribute)
            .cloned()
            .collect::<Vec<_>>();
        drop(input_refs_for_plan);
        let manifest_ref = published.manifest_ref;

        let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            attempt: 0,
            input_manifest_hash: manifest.manifest_hash(&limits).unwrap(),
            input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
            fidelity_opening_root: manifest.fidelity_opening_root,
            oracle_opening_root: manifest.oracle_opening_root,
            wwd: manifest.wwd,
            lysis_limit_minor: U256::from(200),
            logical_evaluation_time: 1_784_765_900,
            tribute_count: manifest.tribute_count,
            lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
            planner_spec_version: bundle.planner_spec_version,
            reducer_spec_version: bundle.reducer_spec_version,
        })
        .unwrap();
        let plan = planner
            .commit_primary_catalog(tribute_refs.clone(), &limits)
            .unwrap();
        let plan_ref = cas
            .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
            .unwrap();

        let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
        let input_refs =
            VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
                .unwrap();
        let mut admissions =
            VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
                .unwrap();
        let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
        let plan_hash = plan.plan_hash(&limits).unwrap();
        let mut nod_root =
            StreamingOrderedListRoot::new(ListKind::NodActions, tribute_count).unwrap();
        let mut result_chunk_refs = Vec::new();
        let mut output_manifest_root = StreamingOrderedListRoot::new(
            ListKind::CompleteOutputManifest,
            plan.primary_work_unit_count,
        )
        .unwrap();
        let mut bucket_root =
            StreamingOrderedListRoot::new(ListKind::BucketRecords, tribute_count).unwrap();

        for plan_ordinal in 0..topology.total_unit_count() {
            let spec = {
                let audit = LocalLysisPlanAuditV1::open(
                    &admissions,
                    &input_refs,
                    &reader,
                    &pinned_bundle,
                    &limits,
                )
                .unwrap();
                audit.candidate_spec_at(plan_ordinal).unwrap()
            };
            let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
                PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    level: 0,
                    index,
                } => {
                    let start = usize::try_from(index * 256).unwrap();
                    let end = (start + 256).min(tributes.len());
                    let actions = nod_action_tributes[start..end]
                        .iter()
                        .enumerate()
                        .map(|(local, tribute)| {
                            let tribute_id = *tribute.tribute_id;
                            NodActionV1 {
                                raw_ordinal: u32::try_from(start + local).unwrap(),
                                tribute_id,
                                nod_id: tribute_id,
                                owner: tribute.owner,
                                wwd: day.value(),
                                league_id: 1,
                                floor_price_minor: U256::ZERO,
                                gratis_load_minor: U256::from(1),
                                entry_price_minor: U256::ZERO,
                                settlement_cost_minor: U256::from(2),
                                issuance_currency: tribute.issuance_currency,
                                reference_currency: tribute.reference_currency,
                                issued_at: 1_784_765_900,
                                bucket_key: hash(u8::try_from(local % 251).unwrap()),
                            }
                        })
                        .collect::<Vec<_>>();
                    for action in &actions {
                        nod_root
                            .push(
                                &action.encode_canonical_record(&limits).unwrap(),
                                limits.max_bounded_bytes,
                            )
                            .unwrap();
                    }
                    let contributors = contributors_by_owner[start..end].to_vec();
                    let chunk = ResultChunkV1 {
                        protocol_bundle_hash: bundle_hash,
                        job_id,
                        attempt: 0,
                        chunk_ordinal: index,
                        first_nod_ordinal: u32::try_from(start).unwrap(),
                        ordered_nod_actions: actions.clone(),
                        ordered_eligible_contributors: contributors.clone(),
                    };
                    let chunk_hash = chunk.result_chunk_hash(&limits).unwrap();
                    let mut chunk_ref = cas
                        .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
                        .unwrap();
                    chunk_ref.expected_ocb1_kind = Some(ObjectKind::ResultChunkV1.tag());
                    result_chunk_refs.push(chunk_ref.clone());
                    let entry = OutputManifestEntryV1 {
                        chunk_ordinal: index,
                        result_chunk_hash: chunk_hash,
                        result_chunk_ref: chunk_ref,
                    };
                    output_manifest_root
                        .push(
                            &entry.encode_canonical_record(&limits).unwrap(),
                            limits.max_bounded_bytes,
                        )
                        .unwrap();
                    let nod_records = actions
                        .iter()
                        .map(|action| action.encode_canonical_record(&limits).unwrap())
                        .collect::<Vec<_>>();
                    let bucket_records = (start..end)
                        .map(|ordinal| ordinal.to_be_bytes().to_vec())
                        .collect::<Vec<_>>();
                    for record in &bucket_records {
                        bucket_root.push(record, limits.max_bounded_bytes).unwrap();
                    }
                    let contributor_records = contributors
                        .iter()
                        .map(|contributor| contributor.encode_canonical_record(&limits).unwrap())
                        .collect::<Vec<_>>();
                    let manifest_records = vec![entry.encode_canonical_record(&limits).unwrap()];
                    let chunk_hash_records = vec![chunk_hash.as_slice().to_vec()];
                    let count = u32::try_from(end - start).unwrap();
                    let raw_nominal_total = tributes[start..end]
                        .iter()
                        .fold(U256::ZERO, |total, tribute| {
                            total.checked_add(tribute.nominal_amount_minor).unwrap()
                        });
                    let nod_cost_total = actions.iter().fold(U256::ZERO, |total, action| {
                        total.checked_add(action.settlement_cost_minor).unwrap()
                    });
                    let summary = RootReduceSummaryV1 {
                        protocol_bundle_hash: bundle_hash,
                        job_id,
                        attempt: 0,
                        plan_hash,
                        covered_primary_start: index,
                        covered_primary_count: 1,
                        nod_actions: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::NodActions,
                            index,
                            &nod_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        bucket_records: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::BucketRecords,
                            index,
                            &bucket_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        contributor_actions: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::ContributorActions,
                            index,
                            &contributor_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        output_manifest_entries: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::CompleteOutputManifest,
                            index,
                            &manifest_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::ResultChunkHashes,
                            index,
                            &chunk_hash_records,
                            B256::len_bytes(),
                        )
                        .unwrap(),
                        tribute_count: count,
                        nod_count: count,
                        bucket_count: count,
                        contributor_count: u32::try_from(contributors.len()).unwrap(),
                        tribute_nominal_total: raw_nominal_total,
                        eligible_nominal_total: raw_nominal_total,
                        lysis_allocation_minor: U256::from(count),
                        nod_cost_total,
                        first_error_ordinal: None,
                    };
                    let coverage_root = summary.result_chunk_hashes.tree_root;
                    let output_coverage_root = coverage_root;
                    (
                        UnitArtifactV1::from_canonical_output(
                            &spec,
                            WorkOutputHeaderV1 {
                                source_coverage_root: coverage_root,
                                output_coverage_root,
                                source_coverage_count: 1,
                                output_coverage_count: 1,
                            },
                            BoundedBytes(
                                encode_root_reduce_output(
                                    &RootReduceOutputV1::Leaf {
                                        summary,
                                        output_manifest_entry: entry.clone(),
                                    },
                                    &limits,
                                )
                                .unwrap(),
                            ),
                            &limits,
                        )
                        .unwrap(),
                        Some(entry),
                    )
                }
                _ => (
                    UnitArtifactV1::from_canonical_output(
                        &spec,
                        WorkOutputHeaderV1 {
                            source_coverage_root: hash(0xa1),
                            output_coverage_root: hash(0xa2),
                            source_coverage_count: 1,
                            output_coverage_count: 1,
                        },
                        BoundedBytes(vec![0x42]),
                        &limits,
                    )
                    .unwrap(),
                    None,
                ),
            };
            let mut artifact_ref = cas
                .publish_bytes(&artifact.encode_canonical(&limits).unwrap())
                .unwrap();
            artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
            admissions
                .admit_verified_unit(
                    AdmissionPositionV1 { plan_ordinal },
                    &spec,
                    artifact_ref,
                    result_entry,
                )
                .unwrap();
        }
        drop(admissions);
        drop(input_refs);
        drop(reader);
        drop(cas);

        Fixture {
            cas_root,
            job_id,
            day,
            bundle: pinned_bundle,
            nod_root: nod_root.finish().unwrap(),
            bucket_root: bucket_root.finish().unwrap(),
            output_manifest_root: output_manifest_root.finish().unwrap(),
            nod_count: tribute_count,
            result_chunk_refs,
        }
    }

    fn canonical_owner(jobs: &[(&Fixture, u32)], damage: Option<&str>) -> HashMapStorageProvider {
        let mut owner = queued_owner(0);
        StorageHandle::enter(&mut owner, |storage| {
            let nod = NodContract::new(storage.clone());
            nod.ocomp_materialization_tail_sequence
                .write(jobs.len() as u64 + 1)
                .unwrap();
            for (index, (f, start)) in jobs.iter().enumerate() {
                let sequence = index as u64 + 1;
                let mut p = seed_nod_generation(storage.clone(), f.day, sequence);
                p.job_id = f.job_id;
                p.program_semantics_hash = f.bundle.bundle().lysis_program_semantics_hash;
                p.protocol_bundle_hash = f.bundle.hash();
                p.nod_root = f.nod_root;
                p.bucket_root = f.bucket_root;
                p.output_manifest_root = f.output_manifest_root;
                p.tribute_count = f.nod_count;
                p.nod_count = f.nod_count;
                p.bucket_count = f.nod_count;
                p.nod_amount_total = U256::from(f.nod_count) * U256::from(2);
                p.lysis_allocation_minor = U256::from(f.nod_count);
                p.next_nod_ordinal = *start;
                if let Some(damage) = damage {
                    match damage {
                        "root" => p.nod_root = hash(0x99),
                        "job" => p.job_id = hash(0x99),
                        "bundle" => p.protocol_bundle_hash = hash(0x99),
                        _ => unreachable!(),
                    }
                }
                write_projection(&nod, &p);
            }
        });
        owner
    }

    fn write_projection(nod: &NodContract<'_>, p: &NodCertifiedGenerationProjection) {
        let day = p.worldwide_day;
        nod.ocomp_namespace_root.write(&day, p.nod_root).unwrap();
        nod.ocomp_bucket_root.write(&day, p.bucket_root).unwrap();
        nod.ocomp_output_manifest_root
            .write(&day, p.output_manifest_root)
            .unwrap();
        nod.ocomp_generation_metadata
            .write(&day, p.metadata_word())
            .unwrap();
        nod.ocomp_nod_amount_total
            .write(&day, p.nod_amount_total)
            .unwrap();
        nod.ocomp_lysis_allocation_minor
            .write(&day, p.lysis_allocation_minor)
            .unwrap();
        nod.ocomp_materialization_job_id
            .write(&day, p.job_id)
            .unwrap();
        nod.ocomp_materialization_protocol_bundle_hash
            .write(&day, p.protocol_bundle_hash)
            .unwrap();
        nod.ocomp_materialization_program_semantics_hash
            .write(&day, p.program_semantics_hash)
            .unwrap();
        nod.ocomp_materialization_next_nod_ordinal
            .write(&day, p.next_nod_ordinal)
            .unwrap();
    }

    fn object_path(f: &Fixture, chunk: usize) -> PathBuf {
        let digest = hex::encode(f.result_chunk_refs[chunk].transport_digest);
        f.cas_root
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..])
    }

    fn first_native_batch(root: &Path, f: &Fixture) -> usize {
        let limits = poc_schema_limits();
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let job = hex::encode(f.job_id);
        let inputs = VerifiedInputChunkRefCatalog::reopen(
            root.join("exporter-v1/input-refs").join(&job),
            &cas,
            limits,
            poc_input_list_limits(),
        )
        .unwrap();
        let admissions = AdmissionCatalogReader::open_existing(
            root.join("supervisor-v1/jobs")
                .join(&job)
                .join("admissions"),
            &cas,
            limits,
        )
        .unwrap();
        let audit =
            LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &limits)
                .unwrap();
        let head = NodMaterializationHeadV1 {
            queue_sequence: 1,
            job_id: f.job_id,
            program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
            worldwide_day: f.day.value(),
            generation: 1,
            nod_root: f.nod_root,
            nod_count: f.nod_count,
            next_nod_ordinal: 0,
            last_progress_height: 90,
        };
        build_nod_materialization_batch_with_references(&audit, &head, 3)
            .unwrap()
            .batch
            .actions
            .len()
    }

    #[test]
    fn canonical_two_job_queue_checks_every_remaining_batch_without_historical_exports() {
        let public = tempfile::tempdir().unwrap();
        let first = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let second = fixture(public.path(), 0x40, WorldwideDay::new(20_260_726), 10);
        assert!(!public.path().join("node-v1").exists());
        assert!(!public.path().join("exporter-v1/receipts").exists());
        assert!(!public.path().join("supervisor-v1/export-bindings").exists());
        let before = fingerprint(public.path());
        with_owner_storage(
            2,
            canonical_owner(&[(&first, 0), (&second, 0)], None),
            |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                let audit = inventory
                    .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                    .unwrap();
                assert_eq!((audit.jobs, audit.batches, audit.actions), (2, 4, 20));
            },
        );
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn missing_entire_second_job_is_incomplete_even_when_head_files_are_complete() {
        let public = tempfile::tempdir().unwrap();
        let first = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let second = fixture(public.path(), 0x40, WorldwideDay::new(20_260_726), 10);
        fs::remove_dir_all(
            public
                .path()
                .join("supervisor-v1/jobs")
                .join(hex::encode(second.job_id)),
        )
        .unwrap();
        let before = fingerprint(public.path());
        with_owner_storage(
            2,
            canonical_owner(&[(&first, 0), (&second, 0)], None),
            |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                let error = inventory
                    .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                    .expect_err("later job cannot be skipped");
                assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            },
        );
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn first_batch_success_does_not_hide_missing_later_required_result_chunk() {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 257);
        fs::remove_file(object_path(&f, 1)).unwrap();
        // Addressable native first batch only needs the later producer artifact,
        // not the deleted later ResultChunk. Later traversal must find the gap.
        assert_eq!(first_native_batch(public.path(), &f), 8);
        let before = fingerprint(public.path());
        with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .expect_err("all remaining batches required");
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        });
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn nonzero_start_uses_addressable_native_proofs_without_consumed_result_chunk() {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 257);
        fs::remove_file(object_path(&f, 0)).unwrap();
        let before = fingerprint(public.path());
        with_owner_storage(1, canonical_owner(&[(&f, 256)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let audit = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .unwrap();
            assert_eq!((audit.jobs, audit.batches, audit.actions), (1, 1, 1));
        });
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn canonical_root_job_and_bundle_mismatches_are_failed_not_missing() {
        for damage in ["root", "job", "bundle"] {
            let public = tempfile::tempdir().unwrap();
            let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
            if damage == "job" {
                // Put the foreign native files at the required path: existence
                // cannot turn their different plan JobId into canonical authority.
                for base in ["supervisor-v1/jobs", "exporter-v1/input-refs"] {
                    fs::rename(
                        public.path().join(base).join(hex::encode(f.job_id)),
                        public.path().join(base).join(hex::encode(hash(0x99))),
                    )
                    .unwrap();
                }
            } else if damage == "bundle" {
                fs::copy(
                    public
                        .path()
                        .join("protocol-bundles-v1")
                        .join(format!("{}.ocb1", hex::encode(f.bundle.hash()))),
                    public
                        .path()
                        .join("protocol-bundles-v1")
                        .join(format!("{}.ocb1", hex::encode(hash(0x99)))),
                )
                .unwrap();
            }
            let before = fingerprint(public.path());
            with_owner_storage(
                2,
                canonical_owner(&[(&f, 0)], Some(damage)),
                |state, source| {
                    let scratch = tempfile::tempdir().unwrap();
                    let inventory =
                        CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                            .unwrap();
                    let error = inventory
                        .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                        .expect_err("canonical authority mismatch");
                    assert!(
                        error.downcast_ref::<Incomplete>().is_none(),
                        "{damage}: {error:#}"
                    );
                },
            );
            assert_eq!(fingerprint(public.path()), before);
        }
    }

    #[test]
    fn corrupt_required_cas_bytes_fail_without_becoming_missing_input() {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let path = object_path(&f, 0);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(path, bytes).unwrap();
        let before = fingerprint(public.path());
        with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .expect_err("CAS digest corruption");
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        });
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn batch_budget_reports_incomplete_and_empty_fifo_needs_no_public_data() {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let before = fingerprint(public.path());
        with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, Some(1))
                .expect_err("second batch exceeds budget");
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(
                error.to_string().contains("8/10"),
                "exact ordinal bounds: {error:#}"
            );
        });
        assert_eq!(fingerprint(public.path()), before);
        with_owner_storage(2, queued_owner(0), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let missing = public.path().join("absent-root");
            let audit = inventory
                .verify_nod_inputs(&missing, CAS_LIMITS, 3, Some(0))
                .unwrap();
            assert_eq!((audit.jobs, audit.batches, audit.actions), (0, 0, 0));
            assert!(!missing.exists());
        });
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn missing_middle_native_admission_is_incomplete_through_transparent_plan_error() {
        use outbe_ocomp::{
            admission_catalog::AdmissionCatalogError, lysis_plan_audit::ExactLysisPlanError,
            nod_materialization::NodMaterializationBuildErrorV1,
        };

        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 257);
        let topology = LysisPlanTopologyV1::new(2).unwrap();
        let missing_ordinal = topology
            .plan_ordinal_of(PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level: 0,
                index: 1,
            })
            .unwrap();
        // The second leaf is a proof sibling of the first batch, with the final
        // reducer and all CAS objects still present. Only its admission is absent.
        assert!(missing_ordinal > 0 && missing_ordinal + 1 < topology.total_unit_count());
        fs::remove_file(
            public
                .path()
                .join("supervisor-v1/jobs")
                .join(hex::encode(f.job_id))
                .join("admissions")
                .join(format!("{missing_ordinal:010}.admission")),
        )
        .unwrap();
        let before = fingerprint(public.path());
        with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .expect_err("missing proof-sibling admission must be incomplete");
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(
                matches!(
                    error.downcast_ref::<NodMaterializationBuildErrorV1>(),
                    Some(NodMaterializationBuildErrorV1::Plan(
                        ExactLysisPlanError::Admission(AdmissionCatalogError::MissingAdmission {
                            plan_ordinal
                        })
                    )) if *plan_ordinal == missing_ordinal
                ),
                "retain the native transparent missing-record error: {error:#}"
            );
        });
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn absent_catalog_bundle_uses_valid_hash_pinned_fallback() {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let catalog = public
            .path()
            .join("protocol-bundles-v1")
            .join(format!("{}.ocb1", hex::encode(f.bundle.hash())));
        fs::rename(&catalog, public.path().join("protocol-bundle-v1.ocb1")).unwrap();
        let before = fingerprint(public.path());
        with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let audit = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .unwrap();
            assert_eq!((audit.jobs, audit.batches, audit.actions), (1, 2, 10));
        });
        assert!(!catalog.exists());
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn corrupt_present_catalog_bundle_does_not_fall_back_to_valid_single_bundle() {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let catalog = public
            .path()
            .join("protocol-bundles-v1")
            .join(format!("{}.ocb1", hex::encode(f.bundle.hash())));
        let fallback = public.path().join("protocol-bundle-v1.ocb1");
        fs::copy(&catalog, &fallback).unwrap();
        assert_eq!(
            PinnedProtocolBundle::decode(
                &fs::read(&fallback).unwrap(),
                f.bundle.hash(),
                &poc_schema_limits()
            )
            .unwrap(),
            f.bundle
        );
        fs::write(&catalog, b"invalid OCB1 bundle").unwrap();
        let before = fingerprint(public.path());
        with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .expect_err("present corrupt catalog must not be hidden by fallback");
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        });
        assert_eq!(fingerprint(public.path()), before);
    }

    #[test]
    fn canonical_tribute_count_and_program_semantics_mismatches_are_failed() {
        for damage in ["tribute_count", "program_semantics"] {
            let public = tempfile::tempdir().unwrap();
            let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
            let mut owner = canonical_owner(&[(&f, 0)], None);
            StorageHandle::enter(&mut owner, |storage| {
                let nod = NodContract::new(storage);
                if damage == "tribute_count" {
                    // Keep the canonical NOD/tribute counts mutually consistent,
                    // but different from the native plan's ten tributes.
                    let mut projection = nod.ocomp_certified_generation(f.day).unwrap().unwrap();
                    projection.tribute_count = 11;
                    projection.nod_count = 11;
                    write_projection(&nod, &projection);
                } else {
                    nod.ocomp_materialization_program_semantics_hash
                        .write(&f.day, hash(0x99))
                        .unwrap();
                }
            });
            let before = fingerprint(public.path());
            with_owner_storage(2, owner, |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                let error = inventory
                    .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                    .expect_err("canonical authority mismatch must fail");
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{damage}: {error:#}"
                );
            });
            assert_eq!(fingerprint(public.path()), before);
        }
    }
}
mod frames_inventory {
    use std::fs;

    use alloy_consensus::{Header, Sealable, SignableTransaction, TxLegacy};
    use alloy_primitives::{Signature, B256, U256};
    use outbe_primitives::{OutbeHeader, OutbePrimitives, OutbeReceipt, OutbeTxEnvelope};
    use outbe_snapshot::manifest::BlockIdentity;
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        models::StoredBlockBodyIndices,
        table::Table,
        tables::{self, ChainStateKey},
        transaction::{DbTx, DbTxMut},
    };
    use reth_provider::{
        providers::StaticFileProviderBuilder, ReceiptProvider, StaticFileSegment, StaticFileWriter,
    };

    use crate::snapshot::{
        config::{parse_node_inputs, resolve_layout, NativeLayout},
        native::RethReadOnlyView,
        tests::{headers::fingerprint, layout::native_arguments},
        validation::{ocomp::verify_retained_frames, Incomplete},
    };

    type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;

    #[derive(Clone, Copy)]
    enum Receipts {
        Mdbx,
        Static,
        StaticHoleWithMdbxCopy,
    }

    #[derive(Clone, Copy)]
    enum Fault {
        None,
        MissingIndex,
        MissingTransaction,
        MissingReceipt,
        MissingHeader,
        IndexOverflow,
        CanonicalHash,
    }

    struct Fixture {
        root: tempfile::TempDir,
        layout: NativeLayout,
        end: BlockIdentity,
    }

    impl Fixture {
        fn new(receipts: Receipts, fault: Fault) -> Self {
            let root = tempfile::tempdir().unwrap();
            let inputs = parse_node_inputs(native_arguments(root.path())).unwrap();
            let layout = resolve_layout(&inputs).unwrap();
            fs::create_dir_all(&layout.static_files_root).unwrap();
            let transactions: Vec<OutbeTxEnvelope> = (0..3)
                .map(|nonce| {
                    TxLegacy {
                        nonce,
                        gas_limit: 21_000,
                        ..Default::default()
                    }
                    .into_signed(Signature::new(U256::ONE, U256::ONE, false))
                    .into()
                })
                .collect();
            let receipt = OutbeReceipt {
                cumulative_gas_used: 21_000,
                ..Default::default()
            };
            let mut headers = vec![layout.chain.genesis_header().clone()];
            for number in 1..=3_u64 {
                headers.push(OutbeHeader::new(Header {
                    number,
                    parent_hash: headers.last().unwrap().hash_slow(),
                    gas_limit: 30_000_000,
                    gas_used: 21_000,
                    transactions_root: alloy_consensus::proofs::calculate_transaction_root(
                        &transactions[(number - 1) as usize..number as usize],
                    ),
                    receipts_root: reth_ethereum::calculate_receipt_root_no_memo(
                        std::slice::from_ref(&receipt),
                    ),
                    ..Default::default()
                }));
            }
            let static_files = StaticFileProviderBuilder::read_write(&layout.static_files_root)
                .with_blocks_per_file(1)
                .build::<OutbePrimitives>()
                .unwrap();
            {
                let mut writer = static_files
                    .get_writer(0, StaticFileSegment::Transactions)
                    .unwrap();
                for number in 0..=3_u64 {
                    writer.increment_block(number).unwrap();
                    if number != 0 && !(number == 3 && matches!(fault, Fault::MissingTransaction)) {
                        writer
                            .append_transaction(number - 1, &transactions[(number - 1) as usize])
                            .unwrap();
                    }
                }
            }
            if !matches!(receipts, Receipts::Mdbx) {
                let mut writer = static_files
                    .get_writer(0, StaticFileSegment::Receipts)
                    .unwrap();
                for number in 0..=3_u64 {
                    writer.increment_block(number).unwrap();
                    if number != 0 && !(number == 3 && matches!(fault, Fault::MissingReceipt)) {
                        writer.append_receipt(number - 1, &receipt).unwrap();
                    }
                }
            }
            static_files.commit().unwrap();
            if matches!(receipts, Receipts::StaticHoleWithMdbxCopy) {
                let deleted = static_files
                    .delete_segment_below_block(StaticFileSegment::Receipts, 3)
                    .unwrap();
                assert!(!deleted.is_empty());
            }
            drop(static_files);

            let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            for header in &headers {
                let number = header.inner.number;
                if !(number == 2 && matches!(fault, Fault::MissingHeader)) {
                    tx.put::<tables::Headers<OutbeHeader>>(number, header.clone())
                        .unwrap();
                }
                let hash = if number == 2 && matches!(fault, Fault::CanonicalHash) {
                    B256::repeat_byte(200)
                } else {
                    header.hash_slow()
                };
                tx.put::<tables::CanonicalHeaders>(number, hash).unwrap();
                if !(number == 3 && matches!(fault, Fault::MissingIndex)) {
                    let indices = if number == 2 && matches!(fault, Fault::IndexOverflow) {
                        StoredBlockBodyIndices {
                            first_tx_num: u64::MAX,
                            tx_count: 1,
                        }
                    } else {
                        StoredBlockBodyIndices {
                            first_tx_num: number.saturating_sub(1),
                            tx_count: u64::from(number != 0),
                        }
                    };
                    tx.put::<tables::BlockBodyIndices>(number, indices).unwrap();
                }
                if number != 0
                    && !matches!(receipts, Receipts::Static)
                    && !(number == 3 && matches!(fault, Fault::MissingReceipt))
                {
                    tx.put::<tables::Receipts<OutbeReceipt>>(number - 1, receipt.clone())
                        .unwrap();
                }
            }
            tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 3)
                .unwrap();
            for stage in ["Execution", "Finish"] {
                tx.put::<tables::StageCheckpoints>(stage.into(), StageCheckpoint::new(3))
                    .unwrap();
            }
            tx.commit().unwrap();
            drop(db);
            let end = BlockIdentity {
                number: 3,
                hash: hex::encode(headers[3].hash_slow()),
            };
            Self { root, layout, end }
        }

        fn check(&self, start: u64, maximum_transactions: Option<u64>) -> eyre::Result<(u64, u64)> {
            let before = fingerprint(self.root.path());
            let result = {
                let view = RethReadOnlyView::open(&self.layout).unwrap();
                verify_retained_frames(&view, start, self.end.clone(), maximum_transactions)
                    .map(|available| (available.blocks, available.transactions))
            };
            assert_eq!(fingerprint(self.root.path()), before);
            result
        }
    }

    #[test]
    fn retained_frames_cover_every_height_with_mdbx_or_static_receipts() {
        for receipts in [Receipts::Mdbx, Receipts::Static] {
            let fixture = Fixture::new(receipts, Fault::None);
            assert_eq!(fixture.check(1, None).unwrap(), (3, 3));
            // Sparse closure C=1 needs only frames2..=H, not a fabricated C-1 witness.
            assert_eq!(fixture.check(2, Some(2)).unwrap(), (2, 2));
            // A selected old quorum height can be audited independently of C+1.
            let mut old = fixture;
            let view = RethReadOnlyView::open(&old.layout).unwrap();
            old.end = BlockIdentity {
                number: 1,
                hash: hex::encode(view.header(1).unwrap().unwrap().hash_slow()),
            };
            drop(view);
            assert_eq!(old.check(1, Some(1)).unwrap(), (1, 1));
        }
    }

    #[test]
    fn empty_interval_and_existing_empty_block_need_no_transactions() {
        let mut fixture = Fixture::new(Receipts::Mdbx, Fault::MissingIndex);
        assert_eq!(fixture.check(4, Some(0)).unwrap(), (0, 0));
        fixture.end = BlockIdentity {
            number: 0,
            hash: hex::encode(fixture.layout.chain.genesis_hash()),
        };
        assert_eq!(fixture.check(0, Some(0)).unwrap(), (1, 0));
    }

    #[test]
    fn missing_required_header_index_transaction_and_receipt_are_incomplete() {
        for fault in [
            Fault::MissingHeader,
            Fault::MissingIndex,
            Fault::MissingTransaction,
            Fault::MissingReceipt,
        ] {
            let fixture = Fixture::new(Receipts::Mdbx, fault);
            let error = fixture.check(1, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }
        let fixture = Fixture::new(Receipts::Static, Fault::MissingReceipt);
        let error = fixture.check(1, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }

    #[test]
    fn static_receipt_hole_is_not_filled_from_a_surviving_mdbx_copy() {
        let fixture = Fixture::new(Receipts::StaticHoleWithMdbxCopy, Fault::None);
        let before = fingerprint(fixture.root.path());
        {
            let view = RethReadOnlyView::open(&fixture.layout).unwrap();
            assert!(view
                .read_transaction()
                .unwrap()
                .get::<tables::Receipts<OutbeReceipt>>(0)
                .unwrap()
                .is_some());
            assert!(view.static_files.receipt(0).unwrap().is_none());
            assert!(view.static_files.receipt(2).unwrap().is_some());
        }
        assert_eq!(fingerprint(fixture.root.path()), before);
        let error = fixture.check(1, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        assert_eq!(fixture.check(3, None).unwrap(), (1, 1));
    }

    #[test]
    fn index_overflow_and_conflicting_canonical_identity_are_failed() {
        for fault in [Fault::IndexOverflow, Fault::CanonicalHash] {
            let fixture = Fixture::new(Receipts::Mdbx, fault);
            let error = fixture.check(1, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        }
        let mut fixture = Fixture::new(Receipts::Static, Fault::None);
        fixture.end.hash = "cc".repeat(32);
        let error = fixture.check(1, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }

    #[test]
    fn transaction_budget_cannot_report_a_partial_interval_as_available() {
        let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
        for budget in [0, 1, 2] {
            let error = fixture.check(1, Some(budget)).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }
        assert_eq!(fixture.check(1, Some(3)).unwrap(), (3, 3));
    }
    // Insert this module inside frames_inventory; it reuses its private native fixture.
    mod closure_inventory {
        use super::*;
        use crate::snapshot::validation::ocomp::verify_closure;
        use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
        use outbe_primitives::projection::ProjectionCheckpoint;
        use std::path::{Path, PathBuf};

        type Observed = (
            ProjectionCheckpoint,
            ProjectionCheckpoint,
            ProjectionCheckpoint,
            u64,
            u64,
        );

        fn point(fixture: &Fixture, number: u64) -> ProjectionCheckpoint {
            let view = RethReadOnlyView::open(&fixture.layout).unwrap();
            ProjectionCheckpoint {
                block_number: number,
                block_hash: view.header(number).unwrap().unwrap().hash_slow(),
            }
        }

        fn store(fixture: &Fixture, advances: &[ProjectionCheckpoint]) -> PathBuf {
            let root = fixture.root.path().join("closure-checkpoint-v1");
            let baseline = ProjectionCheckpoint {
                block_number: 0,
                block_hash: fixture.layout.chain.genesis_hash(),
            };
            let writer = ContiguousCheckpointStoreV1::open(&root, baseline).unwrap();
            let mut previous = baseline;
            for next in advances {
                writer.compare_and_advance_to(previous, *next).unwrap();
                previous = *next;
            }
            drop(writer);
            root
        }

        fn check(
            fixture: &Fixture,
            root: &Path,
            projection: Option<ProjectionCheckpoint>,
            maximum_transactions: Option<u64>,
        ) -> eyre::Result<Observed> {
            let before = fingerprint(fixture.root.path());
            let result = {
                let view = RethReadOnlyView::open(&fixture.layout).unwrap();
                verify_closure(&view, root, projection, maximum_transactions).map(|audit| {
                    (
                        audit.checkpoint.baseline,
                        audit.checkpoint.previous,
                        audit.checkpoint.current,
                        audit.replay.blocks,
                        audit.replay.transactions,
                    )
                })
            };
            assert_eq!(fingerprint(fixture.root.path()), before);
            result
        }

        #[test]
        fn baseline_only_closure_allows_no_projection_and_requires_every_replay_frame() {
            let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
            let baseline = point(&fixture, 0);
            let root = store(&fixture, &[]);
            assert_eq!(
                check(&fixture, &root, None, Some(3)).unwrap(),
                (baseline, baseline, baseline, 3, 3)
            );
            let error = check(&fixture, &root, None, Some(2)).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");

            let missing = Fixture::new(Receipts::Mdbx, Fault::MissingReceipt);
            let root = store(&missing, &[]);
            let error = check(&missing, &root, None, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }

        #[test]
        fn sparse_closure_below_finalized_accepts_equal_or_later_projection_and_replays_suffix() {
            let fixture = Fixture::new(Receipts::Static, Fault::None);
            let baseline = point(&fixture, 0);
            let closed = point(&fixture, 2);
            let root = store(&fixture, &[closed]);
            for projected in [closed, point(&fixture, 3)] {
                assert_eq!(
                    check(&fixture, &root, Some(projected), Some(1)).unwrap(),
                    (baseline, baseline, closed, 1, 1)
                );
            }
            let error = check(&fixture, &root, Some(closed), Some(0)).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }

        #[test]
        fn sparse_previous_is_its_stored_height_not_current_minus_one() {
            let fixture = Fixture::new(Receipts::Mdbx, Fault::MissingHeader);
            let baseline = point(&fixture, 0);
            let previous = point(&fixture, 1);
            let closed = point(&fixture, 3);
            let root = store(&fixture, &[previous, closed]);
            // Header2 is absent, but it is neither stored previous1 nor current3.
            // Chain-wide retained-header validation is a separate audit.
            assert_eq!(
                check(&fixture, &root, Some(closed), Some(0)).unwrap(),
                (baseline, previous, closed, 0, 0)
            );
        }

        #[test]
        fn closure_requires_projection_at_or_beyond_current_with_equal_height_hash_match() {
            let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
            let closed = point(&fixture, 2);
            let root = store(&fixture, &[closed]);
            let wrong_hash = ProjectionCheckpoint {
                block_hash: B256::repeat_byte(0xee),
                ..closed
            };
            for projected in [None, Some(point(&fixture, 1)), Some(wrong_hash)] {
                let error = check(&fixture, &root, projected, None).unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            }
        }

        #[test]
        fn baseline_previous_and_current_hash_conflicts_are_failed() {
            for corrupted in ["baseline", "previous", "current"] {
                let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
                let mut baseline = point(&fixture, 0);
                let mut previous = point(&fixture, 1);
                let mut current = point(&fixture, 3);
                match corrupted {
                    "baseline" => baseline.block_hash = B256::repeat_byte(0xee),
                    "previous" => previous.block_hash = B256::repeat_byte(0xee),
                    "current" => current.block_hash = B256::repeat_byte(0xee),
                    _ => unreachable!(),
                }
                let root = fixture.root.path().join("closure-checkpoint-v1");
                let writer = ContiguousCheckpointStoreV1::open(&root, baseline).unwrap();
                writer.compare_and_advance_to(baseline, previous).unwrap();
                writer.compare_and_advance_to(previous, current).unwrap();
                drop(writer);
                let error = check(&fixture, &root, Some(current), None).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{corrupted}: {error:#}"
                );
            }
        }

        #[test]
        fn missing_stored_baseline_previous_or_current_header_is_incomplete() {
            for missing in [0_u64, 1, 2] {
                let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
                let previous = point(&fixture, 1);
                let current = point(&fixture, 2);
                let root = store(&fixture, &[previous, current]);
                // Fixture mutation finishes before opening the read-only snapshot.
                let db = init_db(
                    fixture.layout.chain_root.join("db"),
                    DatabaseArguments::test(),
                )
                .unwrap();
                let tx = db.tx_mut().unwrap();
                assert!(tx
                    .delete::<tables::Headers<OutbeHeader>>(missing, None)
                    .unwrap());
                tx.commit().unwrap();
                drop(db);
                let error = check(&fixture, &root, Some(current), None).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_some(),
                    "header{missing}: {error:#}"
                );
            }
        }

        #[test]
        fn absent_checkpoint_is_incomplete_without_initializing_store_and_corruption_is_failed() {
            let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
            let root = fixture.root.path().join("closure-checkpoint-v1");
            let error = check(&fixture, &root, None, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(!root.exists());
            let root = store(&fixture, &[]);
            let path = root.join("checkpoint.v1");
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(&path, bytes).unwrap();
            let error = check(&fixture, &root, None, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            fs::remove_file(&path).unwrap();
            let error = check(&fixture, &root, None, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(!path.exists());
        }

        #[test]
        fn closure_ahead_of_finalized_keeps_independent_frontier_and_avoids_max_height_overflow() {
            for number in [4_u64, u64::MAX] {
                let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
                let baseline = point(&fixture, 0);
                let header = OutbeHeader::new(Header {
                    number,
                    parent_hash: point(&fixture, 3).block_hash,
                    ..Default::default()
                });
                let closed = ProjectionCheckpoint {
                    block_number: number,
                    block_hash: header.hash_slow(),
                };
                // Install only the named canonical checkpoint identity above H=3.
                // This isolated audit does not impose equality of independent frontiers.
                let db = init_db(
                    fixture.layout.chain_root.join("db"),
                    DatabaseArguments::test(),
                )
                .unwrap();
                let tx = db.tx_mut().unwrap();
                tx.put::<tables::Headers<OutbeHeader>>(number, header)
                    .unwrap();
                tx.put::<tables::CanonicalHeaders>(number, closed.block_hash)
                    .unwrap();
                tx.commit().unwrap();
                drop(db);
                let root = store(&fixture, &[closed]);
                assert_eq!(
                    check(&fixture, &root, Some(closed), Some(0)).unwrap(),
                    (baseline, baseline, closed, 0, 0)
                );
            }
        }
    }
}
// Append as a child module of snapshot::tests::ocomp.
mod pin_authority {
    use super::{with_prepared_owner_storage, CanonicalState, RethReadOnlyView, DAY};
    use crate::{snapshot::validation::ocomp::verify_pin_authority, OutbeHeader};
    use alloy_consensus::Sealable;
    use alloy_primitives::{B256, U256};
    use outbe_node::ocomp::retention::{
        CandidatePinV1, ExportAuthorityV1, PinRecordV1, PinStateV1,
    };
    use outbe_ocomp_protocol::state::OcompJobRecordV1;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    use outbe_ocomp_protocol::{
        hash::hash_framed,
        intent::{
            intent_storage_key, ActivationPreconditionsV1, ContributorTargetPreconditionV1,
            DayType, FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
            MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
        },
        profile::poc_schema_limits,
        receipts::{ActivationOutcome, AggregateActivationReceiptV1, EffectBindingV1},
        registry::HashDomain,
        state::{
            LysisTerminalV1, OcompCompletedBindingV1, OcompFinalizedJobV1, OcompJobStatus,
            OcompTerminalOutcome,
        },
        vote::OcompQuorumV1,
    };
    use outbe_primitives::{
        addresses::METADOSIS_ADDRESS,
        storage::types::{StorageBytes, StorageKey},
    };

    fn canonical_job(request: &OutbeHeader, completed: bool) -> OcompJobRecordV1 {
        let hash = B256::repeat_byte(11);
        let intent = JobIntentV1 {
            chain_id: 1,
            genesis_hash: hash,
            fork_id: hash,
            wwd: DAY.value(),
            pending_nonce: 0,
            attempt: 0,
            protocol_bundle_hash: hash,
            ce_sealed_root: hash,
            sealed_tribute_collection_key: hash,
            sealed_tribute_collection_root: hash,
            authenticated_day_count: 1,
            authenticated_day_nominal: U256::from(10),
            pre_admission_envelope_hash: hash,
            source_availability_policy_id: hash,
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: U256::from(10),
                previous_vwap: U256::ONE,
                current_vwap: U256::ONE,
                gratis_demand: U256::from(10),
                day_gratis_limit_minor: U256::from(10),
                lysis_limit_minor: U256::from(10),
                desis_limit_minor: U256::ZERO,
                auction_entry_prices: Vec::new(),
                request_limit_split_receipt_hash: hash,
            },
            logical_evaluation_height: 90,
            logical_evaluation_time: 900,
            activation_preconditions: ActivationPreconditionsV1 {
                tribute: TributeInputBindingV1 {
                    wwd: DAY.value(),
                    source_generation: 0,
                    collection_key: hash,
                    sealed_collection_root: hash,
                    exact_count: 1,
                    exact_nominal_total: U256::from(10),
                },
                nod: NodTargetPreconditionV1 {
                    wwd: DAY.value(),
                    target_generation: 0,
                    namespace_root_before: B256::ZERO,
                    max_nod_count: 1,
                },
                contributors: ContributorTargetPreconditionV1 {
                    worldwide_day: DAY.value(),
                    expected_series_version: 0,
                    max_contributor_count: 1,
                    max_eligible_nominal_total: U256::from(10),
                },
                metadosis: MetadosisAttemptPreconditionV1 {
                    wwd: DAY.value(),
                    pending_nonce: 0,
                    expected_status: MetadosisExpectedStatus::OffchainPending,
                    state_version: 2,
                },
            },
            result_validator_set_epoch: 1,
            result_committee_set_hash: hash,
            result_ocomp_binding_hash: hash,
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        };
        let limits = poc_schema_limits();
        let intent_id = intent.intent_id(&limits).unwrap();
        let job_id = intent
            .job_id(request.hash_slow(), request.inner.state_root, &limits)
            .unwrap();
        let mut finalized = OcompFinalizedJobV1 {
            job_id,
            finalized_request_block_hash: request.hash_slow(),
            finalized_request_state_root: request.inner.state_root,
            finality_recorded_height: 100,
            open_height: 104,
            deadline_height: 200,
            quorum: None,
        };
        let terminal = if completed {
            let quorum = OcompQuorumV1 {
                member_count: 4,
                quorum_threshold: 3,
                result_digest: hash,
                quorum_height: 105,
                signer_bitmap: vec![7],
                evidence_hash: hash,
            };
            let receipt = AggregateActivationReceiptV1 {
                binding: EffectBindingV1 {
                    intent_id,
                    job_id,
                    attempt: 0,
                    protocol_bundle_hash: hash,
                    result_digest: hash,
                    activation_preconditions_hash: intent
                        .activation_preconditions
                        .activation_preconditions_hash(&limits)
                        .unwrap(),
                    activation_call_id: hash,
                },
                outcome: ActivationOutcome::Applied,
                nod_receipt_hash: Some(hash),
                contributor_receipt_hash: Some(hash),
                tribute_receipt_hash: Some(hash),
                carry_over_receipt_hash: Some(hash),
                request_limit_split_receipt_hash: hash,
                active_generation_hash: Some(hash),
                effect_commitment: hash_framed(HashDomain::Effects, &hash.as_slice().repeat(4))
                    .unwrap(),
                event_summary_hash: hash,
                activated_at_height: 105,
                activated_at_time: 1_005,
            };
            let binding = OcompCompletedBindingV1 {
                job_id,
                activation_call_id: hash,
                result_digest: hash,
                quorum_height: quorum.quorum_height,
                quorum_signer_bitmap: quorum.signer_bitmap.clone(),
                quorum_evidence_hash: quorum.evidence_hash,
                result_evidence_hash: hash,
                terminal_receipt_hash: receipt.terminal_receipt_hash(&limits).unwrap(),
                terminal_receipt: receipt,
            };
            finalized.quorum = Some(quorum);
            Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Completed,
                terminal_height: 105,
                terminal_time: 1_005,
                completed_binding: Some(binding),
            })
        } else {
            None
        };
        let record = OcompJobRecordV1 {
            intent,
            intent_height: 100,
            status: if completed {
                OcompJobStatus::Completed
            } else {
                OcompJobStatus::AwaitingFinality
            },
            finalized: Some(finalized),
            terminal,
        };
        record.validate_semantics(&limits).unwrap();
        record
    }

    fn stored_job(intent_id: B256, encoded: &[u8]) -> HashMapStorageProvider {
        let mut owner = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut owner, |storage| {
            // Fixed OCM proof mapping base. The snapshot adapter itself never reads
            // Metadosis slots directly or imports its private schema.
            let slot = intent_storage_key(intent_id)
                .unwrap()
                .mapping_slot(U256::from(21));
            StorageBytes::new(slot, METADOSIS_ADDRESS, storage)
                .write(encoded)
                .unwrap();
        });
        owner
    }

    fn with_job(
        version: u32,
        completed: bool,
        finalized: bool,
        check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView, OcompJobRecordV1),
    ) {
        with_prepared_owner_storage(
            version,
            400,
            |request| {
                let mut job = canonical_job(request, completed);
                if !finalized {
                    assert!(!completed);
                    job.finalized = None;
                }
                stored_job(
                    job.intent.intent_id(&poc_schema_limits()).unwrap(),
                    &job.encode_canonical(&poc_schema_limits()).unwrap(),
                )
            },
            |state, view| {
                let request = view.header(100).unwrap().unwrap();
                let mut job = canonical_job(&request, completed);
                if !finalized {
                    job.finalized = None;
                }
                // The native getter reads the job from E, whose state root is
                // independent of the retained request header B.
                assert_ne!(
                    request.inner.state_root,
                    view.header(400).unwrap().unwrap().inner.state_root
                );
                assert_eq!(
                    state
                        .metadosis_job(
                            job.intent.intent_id(&poc_schema_limits()).unwrap(),
                            DAY,
                            None
                        )
                        .unwrap(),
                    job
                );
                check(state, view, job);
            },
        );
    }

    fn candidate(view: &RethReadOnlyView, job: &OcompJobRecordV1) -> CandidatePinV1 {
        let request = view.header(100).unwrap().unwrap();
        CandidatePinV1 {
            block_number: 100,
            block_hash: request.hash_slow(),
            state_root: request.inner.state_root,
            intent_id: job.intent.intent_id(&poc_schema_limits()).unwrap(),
            wwd: job.intent.wwd,
            ce_sealed_root: job.intent.ce_sealed_root,
            protocol_bundle_hash: job.intent.protocol_bundle_hash,
            input_lease_id: job.intent.input_lease_id().unwrap(),
        }
    }

    fn export() -> ExportAuthorityV1 {
        ExportAuthorityV1 {
            source_generation: 3,
            lease_generation: 4,
            manifest_hash: B256::repeat_byte(55),
        }
    }

    fn stages(
        candidate: CandidatePinV1,
        job: &OcompJobRecordV1,
    ) -> Vec<(PinRecordV1, bool, Option<ExportAuthorityV1>)> {
        let finalized = job.finalized.as_ref().unwrap();
        let job_id = finalized.job_id;
        let finality_recorded_height = finalized.finality_recorded_height;
        let open_height = finalized.open_height;
        let deadline_height = finalized.deadline_height;
        let mut states = vec![
            (
                PinStateV1::AwaitingJobFinalization { candidate },
                true,
                None,
            ),
            (
                PinStateV1::Finalized {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                true,
                None,
            ),
            (
                PinStateV1::Exported {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    export: export(),
                },
                true,
                Some(export()),
            ),
        ];
        for authority in [None, Some(export())] {
            // Canonical completion was at 105. Retention observed terminality
            // at 250, after the response deadline of 200; its native evidence
            // window is 64 blocks. This lag does not change job authority.
            states.push((
                PinStateV1::Terminal {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    source_generation: 3,
                    export: authority,
                    terminal_height: 250,
                    release_height: 314,
                },
                true,
                authority,
            ));
            states.push((
                PinStateV1::GcPending {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    source_generation: 3,
                    export: authority,
                    terminal_height: 250,
                    release_height: 314,
                },
                false,
                authority,
            ));
            // Released has no response-window fields. Its observation height
            // is independent of both canonical completion and terminal height.
            states.push((
                PinStateV1::Released {
                    candidate,
                    job_id,
                    source_generation: 3,
                    observed_height: 400,
                    export: authority,
                },
                false,
                authority,
            ));
        }
        states
            .into_iter()
            .map(|(state, source, authority)| {
                (
                    PinRecordV1 {
                        generation: 12,
                        state,
                    },
                    source,
                    authority,
                )
            })
            .collect()
    }

    #[test]
    fn awaiting_pin_binds_current_e_before_and_after_canonical_finalization() {
        for version in [1, 2] {
            for finalized in [false, true] {
                with_job(version, false, finalized, |state, view, job| {
                    let candidate = candidate(view, &job);
                    let record = PinRecordV1 {
                        generation: 1,
                        state: PinStateV1::AwaitingJobFinalization { candidate },
                    };
                    let verified =
                        verify_pin_authority(state, view, candidate.block_hash, &record).unwrap();
                    assert_eq!(verified.candidate, candidate);
                    assert_eq!(verified.job, job);
                    assert!(verified.requires_source);
                    assert_eq!(verified.export, None);
                });
            }
        }
    }

    #[test]
    fn all_pin_stages_allow_completed_authority_and_preserve_source_obligations() {
        for version in [1, 2] {
            with_job(version, true, true, |state, view, job| {
                assert_eq!(job.status, OcompJobStatus::Completed);
                assert_eq!(job.terminal.as_ref().unwrap().terminal_height, 105);
                let candidate = candidate(view, &job);
                for (record, requires_source, export) in stages(candidate, &job) {
                    let verified = verify_pin_authority(state, view, candidate.block_hash, &record)
                        .unwrap_or_else(|error| panic!("{:?}: {error:#}", record.state));
                    assert_eq!(verified.candidate, candidate);
                    assert_eq!(verified.job, job);
                    assert_eq!(
                        verified.requires_source, requires_source,
                        "{:?}",
                        record.state
                    );
                    assert_eq!(verified.export, export);
                }
            });
        }
    }

    #[test]
    fn every_candidate_identity_component_and_registry_key_bind_exactly() {
        for version in [1, 2] {
            // An unfinalized canonical record forces the verifier to bind B
            // explicitly; the getter's finalized-request branch cannot do it.
            with_job(version, false, false, |state, view, job| {
                let valid = candidate(view, &job);
                let bad_hash = B256::repeat_byte(77);
                let mutations = [
                    CandidatePinV1 {
                        block_number: 101,
                        ..valid
                    },
                    CandidatePinV1 {
                        block_hash: bad_hash,
                        ..valid
                    },
                    CandidatePinV1 {
                        state_root: bad_hash,
                        ..valid
                    },
                    CandidatePinV1 {
                        intent_id: bad_hash,
                        ..valid
                    },
                    CandidatePinV1 {
                        wwd: valid.wwd + 1,
                        ..valid
                    },
                    CandidatePinV1 {
                        ce_sealed_root: bad_hash,
                        ..valid
                    },
                    CandidatePinV1 {
                        protocol_bundle_hash: bad_hash,
                        ..valid
                    },
                    CandidatePinV1 {
                        input_lease_id: bad_hash,
                        ..valid
                    },
                ];
                for changed in mutations {
                    let record = PinRecordV1 {
                        generation: 1,
                        state: PinStateV1::AwaitingJobFinalization { candidate: changed },
                    };
                    // Follow a changed block hash with the registry key so the
                    // actual canonical-header comparison is also exercised.
                    assert!(
                        verify_pin_authority(state, view, changed.block_hash, &record).is_err(),
                        "accepted {changed:?}"
                    );
                }
                let record = PinRecordV1 {
                    generation: 1,
                    state: PinStateV1::AwaitingJobFinalization { candidate: valid },
                };
                assert!(verify_pin_authority(state, view, bad_hash, &record).is_err());
            });
        }
    }

    #[test]
    fn finalized_job_and_each_persisted_response_window_field_must_match_current_e() {
        for version in [1, 2] {
            with_job(version, true, true, |state, view, job| {
                let candidate = candidate(view, &job);
                for (valid, _, _) in stages(candidate, &job) {
                    if matches!(valid.state, PinStateV1::AwaitingJobFinalization { .. }) {
                        continue;
                    }
                    for field in 0..4 {
                        let mut changed = valid;
                        match &mut changed.state {
                            PinStateV1::Finalized {
                                job_id,
                                finality_recorded_height,
                                open_height,
                                deadline_height,
                                ..
                            }
                            | PinStateV1::Exported {
                                job_id,
                                finality_recorded_height,
                                open_height,
                                deadline_height,
                                ..
                            }
                            | PinStateV1::Terminal {
                                job_id,
                                finality_recorded_height,
                                open_height,
                                deadline_height,
                                ..
                            }
                            | PinStateV1::GcPending {
                                job_id,
                                finality_recorded_height,
                                open_height,
                                deadline_height,
                                ..
                            } => match field {
                                0 => *job_id = B256::repeat_byte(88),
                                1 => *finality_recorded_height += 1,
                                2 => *open_height += 1,
                                _ => *deadline_height += 1,
                            },
                            PinStateV1::Released { job_id, .. } => {
                                if field != 0 {
                                    continue;
                                }
                                *job_id = B256::repeat_byte(88);
                            }
                            PinStateV1::AwaitingJobFinalization { .. } => unreachable!(),
                        }
                        assert!(
                            verify_pin_authority(state, view, candidate.block_hash, &changed)
                                .is_err(),
                            "accepted {changed:?}"
                        );
                    }
                }
            });
        }
    }

    #[test]
    fn local_finalized_pin_cannot_supply_finalization_absent_from_current_e() {
        for version in [1, 2] {
            with_job(version, false, false, |state, view, job| {
                let candidate = candidate(view, &job);
                let finalized_job = canonical_job(&view.header(100).unwrap().unwrap(), false);
                for (record, _, _) in stages(candidate, &finalized_job) {
                    if !matches!(record.state, PinStateV1::AwaitingJobFinalization { .. }) {
                        assert!(
                            verify_pin_authority(state, view, candidate.block_hash, &record)
                                .is_err(),
                            "accepted {:?}",
                            record.state
                        );
                    }
                }
            });
        }
    }

    #[test]
    fn canonical_finalized_job_must_bind_the_retained_native_request_header() {
        for version in [1, 2] {
            with_prepared_owner_storage(
                version,
                400,
                |request| {
                    let mut other_request = request.clone();
                    other_request.inner.timestamp += 1;
                    let job = canonical_job(&other_request, false);
                    stored_job(
                        job.intent.intent_id(&poc_schema_limits()).unwrap(),
                        &job.encode_canonical(&poc_schema_limits()).unwrap(),
                    )
                },
                |state, view| {
                    let job = canonical_job(&view.header(100).unwrap().unwrap(), false);
                    let candidate = candidate(view, &job);
                    let record = PinRecordV1 {
                        generation: 1,
                        state: PinStateV1::AwaitingJobFinalization { candidate },
                    };
                    assert!(
                        verify_pin_authority(state, view, candidate.block_hash, &record).is_err()
                    );
                },
            );
        }
    }
    #[test]
    fn unfinalized_canonical_intent_height_must_equal_candidate_height() {
        for version in [1, 2] {
            with_prepared_owner_storage(
                version,
                400,
                |request| {
                    let mut job = canonical_job(request, false);
                    job.finalized = None;
                    job.intent_height = 101;
                    stored_job(
                        job.intent.intent_id(&poc_schema_limits()).unwrap(),
                        &job.encode_canonical(&poc_schema_limits()).unwrap(),
                    )
                },
                |state, view| {
                    let job = canonical_job(&view.header(100).unwrap().unwrap(), false);
                    let candidate = candidate(view, &job);
                    assert_eq!(
                        state
                            .metadosis_job(candidate.intent_id, DAY, None)
                            .unwrap()
                            .intent_height,
                        101
                    );
                    let record = PinRecordV1 {
                        generation: 1,
                        state: PinStateV1::AwaitingJobFinalization { candidate },
                    };
                    assert!(
                        verify_pin_authority(state, view, candidate.block_hash, &record).is_err()
                    );
                },
            );
        }
    }
}
mod lease_inventory {
    use crate::snapshot::{
        tests::headers::fingerprint,
        validation::{ocomp::verify_lease_inputs, Incomplete},
    };
    use alloy_primitives::{Address, B256, U256};
    use outbe_compressed_entities::{
        body_commitment, derive_poseidon_entity_id, encode_tribute_v1, partition_collection_key,
        tribute_partition_root_from_leaves, PartitionRef, StoredBody, ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
    };
    use outbe_ocomp::{control::poc_schema_limits, exporter::TributeStreamSummary};
    use outbe_ocomp_protocol::intent::{
        ActivationPreconditionsV1, AuctionEntryPriceSource, ContributorTargetPreconditionV1,
        DayType, FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, ReferenceEntryPriceV1,
        TributeInputBindingV1,
    };
    use outbe_offchain_storage::{
        AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, RocksDbReader, RocksDbStorage,
        StorageReaderHandle, StorageWriterHandle, Value,
    };
    use outbe_primitives::time::WorldwideDay;
    use outbe_snapshot::layout::ProtectedPaths;
    use outbe_tribute::{
        canonical_body, RetainedTributePin, RetainedTributeReader, TributeData,
        TributeRepositoryReader, TributeRepositoryWriter, OCOMP_RETAINED_TRIBUTES_NAMESPACE,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
    };

    const DAY: WorldwideDay = WorldwideDay::new(20_260_901);
    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn intent(collection_root: B256, count: u32, nominal: U256) -> JobIntentV1 {
        let day = DAY.value();
        let collection_key = B256::from(
            *partition_collection_key(PartitionRef::TributeWwd(DAY))
                .unwrap()
                .1
                .as_bytes(),
        );
        let intent = JobIntentV1 {
            chain_id: 54322345,
            genesis_hash: hash(1),
            fork_id: hash(2),
            wwd: day,
            pending_nonce: 0,
            attempt: 0,
            protocol_bundle_hash: hash(3),
            ce_sealed_root: hash(5),
            sealed_tribute_collection_key: collection_key,
            sealed_tribute_collection_root: collection_root,
            authenticated_day_count: count,
            authenticated_day_nominal: nominal,
            pre_admission_envelope_hash: hash(6),
            source_availability_policy_id: hash(7),
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: nominal,
                previous_vwap: nominal,
                current_vwap: nominal,
                gratis_demand: U256::ZERO,
                day_gratis_limit_minor: U256::ZERO,
                lysis_limit_minor: nominal,
                desis_limit_minor: U256::ZERO,
                auction_entry_prices: vec![ReferenceEntryPriceV1 {
                    reference_currency: 840,
                    entry_price_minor: nominal,
                    source: AuctionEntryPriceSource::LastClosedDayVwap,
                    source_day: day - 1,
                }],
                request_limit_split_receipt_hash: hash(8),
            },
            logical_evaluation_height: 100,
            logical_evaluation_time: 1000,
            activation_preconditions: ActivationPreconditionsV1 {
                tribute: TributeInputBindingV1 {
                    wwd: day,
                    source_generation: 1,
                    collection_key,
                    sealed_collection_root: collection_root,
                    exact_count: count,
                    exact_nominal_total: nominal,
                },
                nod: NodTargetPreconditionV1 {
                    wwd: day,
                    target_generation: 1,
                    namespace_root_before: hash(9),
                    max_nod_count: count,
                },
                contributors: ContributorTargetPreconditionV1 {
                    worldwide_day: day,
                    expected_series_version: 1,
                    max_contributor_count: count,
                    max_eligible_nominal_total: nominal,
                },
                metadosis: MetadosisAttemptPreconditionV1 {
                    wwd: day,
                    pending_nonce: 0,
                    expected_status: MetadosisExpectedStatus::OffchainPending,
                    state_version: 1,
                },
            },
            result_validator_set_epoch: 1,
            result_committee_set_hash: hash(10),
            result_ocomp_binding_hash: hash(11),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        };
        intent.encode_canonical(&poc_schema_limits()).unwrap();
        intent
    }

    fn body(index: u8, day: WorldwideDay) -> TributeData {
        let owner = Address::repeat_byte(index);
        TributeData {
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            issuance_amount_minor: U256::from(1000),
            issuance_currency: 840,
            nominal_amount_minor: U256::from(700),
            reference_currency: 840,
            tribute_price_minor: U256::from(2),
            exclude_from_intex_issuance: false,
        }
    }

    #[derive(Clone, Copy)]
    enum Placement {
        Live,
        Retained,
        Union,
        Absent,
        OtherLease,
        OtherDay,
    }
    #[derive(Clone, Copy)]
    enum Fault {
        None,
        MissingBody,
        CorruptBody,
        CrossDayBody,
    }

    struct Fixture {
        reader: StorageReaderHandle,
        root: tempfile::TempDir,
        source: PathBuf,
        scratch: PathBuf,
        intent: JobIntentV1,
        exact_body_bytes: u64,
    }

    impl Fixture {
        fn new(placement: Placement, fault: Fault) -> Self {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("primary");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).unwrap();
            let bodies: Vec<_> = (1..=5).map(|index| body(index, DAY)).collect();
            let mut exact_body_bytes = 0;
            let leaves: Vec<_> = bodies
                .iter()
                .map(|body| {
                    let bytes = encode_tribute_v1(&canonical_body(body)).unwrap();
                    exact_body_bytes += bytes.len() as u64;
                    (
                        body.tribute_id,
                        body_commitment(
                            ACTIVE_COMMITMENT_SCHEME,
                            BODY_SCHEMA_V1,
                            body.tribute_id,
                            &bytes,
                        )
                        .unwrap(),
                    )
                })
                .collect();
            let collection_root = tribute_partition_root_from_leaves(DAY, leaves).unwrap();
            let intent = intent(collection_root, 5, U256::from(3500));
            {
                let storage = Arc::new(RocksDbStorage::open(&source).unwrap());
                let reader: StorageReaderHandle = storage.clone();
                let writer: StorageWriterHandle = storage.clone();
                let current = TributeRepositoryReader::new(reader.clone());
                let repository = TributeRepositoryWriter::new(reader.clone(), writer.clone());
                let retained = RetainedTributeReader::new(reader);
                let pin = RetainedTributePin {
                    input_lease_id: if matches!(placement, Placement::OtherLease) {
                        hash(0xee)
                    } else {
                        intent.input_lease_id().unwrap()
                    },
                    worldwide_day: DAY,
                };
                for (index, data) in bodies.iter().enumerate() {
                    if matches!(placement, Placement::Absent) {
                        continue;
                    }
                    if matches!(placement, Placement::OtherDay) {
                        repository
                            .put(&body(index as u8 + 1, WorldwideDay::new(20_260_902)))
                            .unwrap();
                        continue;
                    }
                    repository.put(data).unwrap();
                    let retain = matches!(placement, Placement::Retained | Placement::OtherLease)
                        || matches!(placement, Placement::Union) && index <= 2;
                    if retain {
                        let retained_batch =
                            retained.plan_retain_current(pin, data.tribute_id).unwrap();
                        let mut batch = AtomicWriteBatch::new();
                        batch.extend(retained_batch.operations().iter().cloned());
                        if !matches!(placement, Placement::Union) || index < 2 {
                            batch.extend(
                                current
                                    .projection_session(&[data.tribute_id])
                                    .unwrap()
                                    .delete(data.tribute_id)
                                    .unwrap()
                                    .operations()
                                    .iter()
                                    .cloned(),
                            );
                        }
                        writer.apply_atomic(&batch).unwrap();
                        if index == 0 && matches!(fault, Fault::MissingBody) {
                            // Remove only the native body put, leaving its real retained index.
                            for operation in retained_batch.operations() {
                                if let AtomicWriteOperation::Put { namespace, key, .. } = operation
                                {
                                    if namespace.as_str() == OCOMP_RETAINED_TRIBUTES_NAMESPACE {
                                        writer.delete(namespace.clone(), key).unwrap();
                                    }
                                }
                            }
                        }
                    } else if index == 0 {
                        let namespace = Namespace::new("tributes").unwrap();
                        let key = Key::new(data.tribute_id.to_vec()).unwrap();
                        match fault {
                            Fault::None => {}
                            Fault::MissingBody => writer.delete(namespace, &key).unwrap(),
                            Fault::CorruptBody => writer
                                .put(
                                    namespace,
                                    &key,
                                    &Value::new(b"invalid stored body".to_vec()).unwrap(),
                                )
                                .unwrap(),
                            Fault::CrossDayBody => {
                                // A valid foreign-day body under the selected native primary key.
                                let bytes = encode_tribute_v1(&canonical_body(&body(
                                    1,
                                    WorldwideDay::new(20_260_902),
                                )))
                                .unwrap();
                                let value = Value::new(StoredBody::new_v1(bytes).unwrap().encode())
                                    .unwrap();
                                writer.put(namespace, &key, &value).unwrap();
                            }
                        }
                    }
                }
            }
            // The primary writer is stopped. Session creation may write secondary
            // metadata, so open it before the measured read-only adapter call.
            let reader: StorageReaderHandle =
                Arc::new(RocksDbReader::open(&source, &root.path().join("secondary")).unwrap());
            Self {
                reader,
                root,
                source,
                scratch,
                intent,
                exact_body_bytes,
            }
        }

        fn check(
            &self,
            authority: &JobIntentV1,
            maximum: Option<u64>,
        ) -> eyre::Result<TributeStreamSummary> {
            self.check_at(authority, &self.scratch, maximum)
        }

        fn check_at(
            &self,
            authority: &JobIntentV1,
            scratch: &Path,
            maximum: Option<u64>,
        ) -> eyre::Result<TributeStreamSummary> {
            let before = fingerprint(&self.source);
            let scratch_before = fingerprint(scratch);
            let result = verify_lease_inputs(
                self.reader.clone(),
                authority,
                scratch,
                &ProtectedPaths(vec![self.source.clone()]),
                maximum,
            );
            assert_eq!(fingerprint(&self.source), before);
            assert_eq!(
                fingerprint(scratch),
                scratch_before,
                "temporary verification files must be cleaned on every exit"
            );
            result
        }
    }

    #[test]
    fn live_retained_and_deduplicated_union_close_actual_native_root_and_nominal() {
        for placement in [Placement::Live, Placement::Retained, Placement::Union] {
            let fixture = Fixture::new(placement, Fault::None);
            let summary = fixture.check(&fixture.intent, Some(5)).unwrap();
            assert_eq!(
                summary,
                TributeStreamSummary {
                    record_count: 5,
                    nominal_total: U256::from(3500),
                    exact_body_bytes: fixture.exact_body_bytes
                }
            );
        }
    }

    #[test]
    fn absent_required_partition_and_missing_selected_live_or_retained_body_are_incomplete() {
        for (placement, fault) in [
            (Placement::Absent, Fault::None),
            (Placement::Live, Fault::MissingBody),
            (Placement::Retained, Fault::MissingBody),
        ] {
            let fixture = Fixture::new(placement, fault);
            let error = fixture.check(&fixture.intent, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }
    }

    #[test]
    fn bodies_under_other_lease_or_day_do_not_substitute_required_partition() {
        for placement in [Placement::OtherLease, Placement::OtherDay] {
            let fixture = Fixture::new(placement, Fault::None);
            let error = fixture.check(&fixture.intent, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }
    }

    #[test]
    fn corrupt_or_foreign_day_body_under_selected_key_is_failed() {
        for fault in [Fault::CorruptBody, Fault::CrossDayBody] {
            let fixture = Fixture::new(Placement::Live, fault);
            let error = fixture.check(&fixture.intent, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        }
    }

    #[test]
    fn full_count_wrong_root_count_overrun_and_nominal_contradictions_are_failed() {
        let fixture = Fixture::new(Placement::Live, Fault::None);
        for authority in [
            intent(hash(0x99), 5, U256::from(3500)),
            intent(
                fixture.intent.sealed_tribute_collection_root,
                4,
                U256::from(3500),
            ),
            intent(
                fixture.intent.sealed_tribute_collection_root,
                5,
                U256::from(3499),
            ),
            intent(
                fixture.intent.sealed_tribute_collection_root,
                5,
                U256::from(3501),
            ),
        ] {
            let error = fixture.check(&authority, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        }
        let incomplete = intent(
            fixture.intent.sealed_tribute_collection_root,
            6,
            U256::from(4200),
        );
        let error = fixture.check(&incomplete, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }

    #[test]
    fn resource_limit_reports_incomplete_and_exact_budget_can_finish() {
        let fixture = Fixture::new(Placement::Union, Fault::None);
        for maximum in [0, 1, 4] {
            let error = fixture.check(&fixture.intent, Some(maximum)).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }
        assert_eq!(
            fixture
                .check(&fixture.intent, Some(5))
                .unwrap()
                .record_count,
            5
        );
    }

    #[test]
    fn overlapping_scratch_is_rejected_before_source_mutation() {
        let fixture = Fixture::new(Placement::Union, Fault::None);
        let error = fixture
            .check_at(&fixture.intent, &fixture.source, None)
            .unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        let missing = fixture.source.join("new-scratch");
        let before = fingerprint(&fixture.source);
        let error = verify_lease_inputs(
            fixture.reader.clone(),
            &fixture.intent,
            &missing,
            &ProtectedPaths(vec![fixture.source.clone()]),
            None,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        assert!(!missing.exists());
        assert_eq!(fingerprint(&fixture.source), before);
        assert!(fixture.root.path().is_dir());
    }
}
