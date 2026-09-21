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
    with_prepared_owner_storage_setup(version, execution_height, |_| {}, prepare, check);
}

fn with_prepared_owner_storage_setup(
    version: u32,
    execution_height: u64,
    setup: impl FnOnce(&crate::snapshot::config::NativeLayout),
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    let (_source, layout, _) = super::evm::state_fixture(version);
    setup(&layout);
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
    #[test]
    fn full_canonical_composition_checks_payout_after_independent_inventory() {
        use crate::snapshot::validation::{ocomp::verify_canonical_obligations, Incomplete};
        let leaves = leaves();
        let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
        let active = generation(root);
        for version in [1, 2] {
            for deleted in [false, true] {
                super::with_canonical_frontiers(
                    version,
                    |layout| {
                        write_payout(&layout.ocomp_root, active.job_id, &leaves);
                        if deleted {
                            fs::remove_dir_all(job_root(&layout.ocomp_root, active.job_id))
                                .unwrap();
                        }
                    },
                    |_| native_owner(&active, root),
                    |state, source, layout, scratch| {
                        let result = verify_canonical_obligations(
                            state, source, layout, scratch, None, None,
                        );
                        if deleted {
                            let error = result
                                .err()
                                .expect("whole missing certified payout job cannot pass");
                            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                            assert!(
                                error.to_string().contains("missing payout file"),
                                "{error:#}"
                            );
                        } else {
                            let audit = result.unwrap();
                            assert_eq!(audit.bounds.unpaid_days, 1);
                            assert_eq!(audit.payout_days, 1);
                            assert_eq!(audit.bounds.active_intents, 0);
                        }
                    },
                );
            }
        }
    }

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
    #[test]
    fn full_canonical_composition_cannot_skip_an_entire_later_nod_job() {
        use crate::snapshot::validation::{ocomp::verify_canonical_obligations, Incomplete};
        use std::cell::RefCell;
        for version in [1, 2] {
            for deleted in [false, true] {
                let fixtures = RefCell::new(None);
                super::with_canonical_frontiers(
                    version,
                    |layout| {
                        let first =
                            fixture(&layout.ocomp_root, 0x30, WorldwideDay::new(20_260_725), 10);
                        let second =
                            fixture(&layout.ocomp_root, 0x40, WorldwideDay::new(20_260_726), 10);
                        if deleted {
                            fs::remove_dir_all(
                                layout
                                    .ocomp_root
                                    .join("supervisor-v1/jobs")
                                    .join(hex::encode(second.job_id)),
                            )
                            .unwrap();
                        }
                        *fixtures.borrow_mut() = Some((first, second));
                    },
                    |_| {
                        let fixtures = fixtures.borrow();
                        let (first, second) = fixtures.as_ref().unwrap();
                        canonical_owner(&[(first, 0), (second, 0)], None)
                    },
                    |state, source, layout, scratch| {
                        let result = verify_canonical_obligations(
                            state, source, layout, scratch, None, None,
                        );
                        if deleted {
                            let error = result
                                .err()
                                .expect("whole second canonical NOD job cannot pass");
                            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                            assert!(format!("{error:#}").contains("NOD"), "{error:#}");
                        } else {
                            let audit = result.unwrap();
                            assert_eq!(audit.bounds.nod_entries, 2);
                            assert_eq!(audit.nod.jobs, 2);
                            assert_eq!(audit.nod.actions, 20);
                        }
                    },
                );
            }
        }
    }

    mod present_admissions {
        mod reference_membership {
            use super::*;
            use crate::snapshot::validation::ocomp::ReferenceMembership;
            use outbe_snapshot::layout::ProtectedPaths;

            fn populate(
                root: &Path,
                f: &Fixture,
                expected: &Expected,
                members: &ReferenceMembership,
            ) {
                let counts = run(root, expected, None, &mut |reference| {
                    members.insert(f.job_id, reference)
                })
                .unwrap();
                assert_eq!(counts.0, counts.1);
                assert!(counts.2 > 0);
            }

            fn rejected(result: eyre::Result<()>, incomplete: bool) {
                let error = result.unwrap_err();
                assert_eq!(
                    error.downcast_ref::<Incomplete>().is_some(),
                    incomplete,
                    "{error:#}"
                );
            }

            #[test]
            fn native_artifact_and_result_members_verify_without_source_writes_and_cleanup_on_drop()
            {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                let before = fingerprint(root.path());
                let scratch = tempfile::tempdir().unwrap();
                let scratch_before = fingerprint(scratch.path());
                {
                    let members = ReferenceMembership::create(
                        scratch.path(),
                        &ProtectedPaths(vec![root.path().to_path_buf()]),
                    )
                    .unwrap();
                    populate(root.path(), &f, &expected, &members);
                    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
                    for reference in references(&expected.records) {
                        members.verify(f.job_id, &reference, &cas, true).unwrap();
                        members.verify(f.job_id, &reference, &cas, false).unwrap();
                    }
                    // Explicitly exercise the independently stored result object too.
                    members
                        .verify(f.job_id, &f.result_chunk_refs[0], &cas, true)
                        .unwrap();
                    assert_eq!(fingerprint(root.path()), before);
                }
                assert_eq!(fingerprint(scratch.path()), scratch_before);
                assert_eq!(fingerprint(root.path()), before);
            }

            #[test]
            fn valid_cas_foreign_job_and_foreign_artifact_need_exact_membership() {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let foreign = fixture(root.path(), 0x40, WorldwideDay::new(20_260_726), 10);
                let expected = expected(root.path(), &f);
                let foreign_expected = super::expected(root.path(), &foreign);
                let own_ref = &expected.records[0].artifact_ref;
                let foreign_ref = &foreign_expected.records[0].artifact_ref;
                assert_ne!(own_ref.transport_digest, foreign_ref.transport_digest);
                let before = fingerprint(root.path());
                let scratch = tempfile::tempdir().unwrap();
                let members = ReferenceMembership::create(
                    scratch.path(),
                    &ProtectedPaths(vec![root.path().to_path_buf()]),
                )
                .unwrap();
                populate(root.path(), &f, &expected, &members);
                let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
                for complete in [false, true] {
                    rejected(
                        members.verify(foreign.job_id, own_ref, &cas, complete),
                        !complete,
                    );
                    rejected(
                        members.verify(f.job_id, foreign_ref, &cas, complete),
                        !complete,
                    );
                }
                // The second job's native callback establishes its own membership.
                populate(root.path(), &foreign, &foreign_expected, &members);
                members
                    .verify(foreign.job_id, foreign_ref, &cas, true)
                    .unwrap();
                members.verify(f.job_id, own_ref, &cas, true).unwrap();
                rejected(members.verify(foreign.job_id, own_ref, &cas, true), false);
                rejected(members.verify(f.job_id, foreign_ref, &cas, true), false);
                assert_eq!(fingerprint(root.path()), before);
            }

            #[test]
            fn changed_length_and_wrong_kind_fail_even_when_evidence_is_partial() {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                let before = fingerprint(root.path());
                let scratch = tempfile::tempdir().unwrap();
                let members = ReferenceMembership::create(
                    scratch.path(),
                    &ProtectedPaths(vec![root.path().to_path_buf()]),
                )
                .unwrap();
                populate(root.path(), &f, &expected, &members);
                let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
                let original = &expected.records[0].artifact_ref;
                let mut wrong_length = original.clone();
                wrong_length.encoded_bytes += 1;
                let mut wrong_kind = original.clone();
                wrong_kind.expected_ocb1_kind = f.result_chunk_refs[0].expected_ocb1_kind;
                assert_ne!(wrong_kind.expected_ocb1_kind, original.expected_ocb1_kind);
                for complete in [false, true] {
                    rejected(
                        members.verify(f.job_id, &wrong_length, &cas, complete),
                        false,
                    );
                    rejected(members.verify(f.job_id, &wrong_kind, &cas, complete), false);
                }
                assert_eq!(fingerprint(root.path()), before);
            }

            #[test]
            fn untyped_reference_does_not_alias_a_typed_member() {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                let before = fingerprint(root.path());
                let scratch = tempfile::tempdir().unwrap();
                let members = ReferenceMembership::create(
                    scratch.path(),
                    &ProtectedPaths(vec![root.path().to_path_buf()]),
                )
                .unwrap();
                populate(root.path(), &f, &expected, &members);
                let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
                let mut untyped = expected.records[0].artifact_ref.clone();
                assert!(untyped.expected_ocb1_kind.is_some());
                untyped.expected_ocb1_kind = None;
                rejected(members.verify(f.job_id, &untyped, &cas, true), false);
                rejected(members.verify(f.job_id, &untyped, &cas, false), true);
                assert_eq!(fingerprint(root.path()), before);
            }

            #[test]
            fn partial_native_admissions_allow_present_members_but_leave_valid_unmatched_refs_incomplete(
            ) {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                let first = expected
                    .topology
                    .plan_ordinal_of(PlannedUnitPositionV1::Primary {
                        phase: UnitPhase::Enumerate,
                        ordinal: 0,
                    })
                    .unwrap();
                keep_only(root.path(), &f, &expected, &[first]);
                let absent = expected
                    .records
                    .iter()
                    .find(|record| {
                        record.artifact_ref != expected.records[first as usize].artifact_ref
                    })
                    .unwrap()
                    .artifact_ref
                    .clone();
                let before = fingerprint(root.path());
                let scratch = tempfile::tempdir().unwrap();
                let members = ReferenceMembership::create(
                    scratch.path(),
                    &ProtectedPaths(vec![root.path().to_path_buf()]),
                )
                .unwrap();
                let counts = run(root.path(), &expected, None, &mut |reference| {
                    members.insert(f.job_id, reference)
                })
                .unwrap();
                assert_eq!(counts, (expected.topology.total_unit_count(), 1, 0));
                let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
                members
                    .verify(
                        f.job_id,
                        &expected.records[first as usize].artifact_ref,
                        &cas,
                        false,
                    )
                    .unwrap();
                rejected(members.verify(f.job_id, &absent, &cas, false), true);
                rejected(
                    members.verify(f.job_id, &f.result_chunk_refs[0], &cas, false),
                    true,
                );
                assert_eq!(fingerprint(root.path()), before);
            }

            #[test]
            fn membership_does_not_substitute_for_current_cas_bytes_and_error_paths_cleanup() {
                for missing in [false, true] {
                    let root = tempfile::tempdir().unwrap();
                    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                    let expected = expected(root.path(), &f);
                    let scratch = tempfile::tempdir().unwrap();
                    let scratch_before = fingerprint(scratch.path());
                    let members = ReferenceMembership::create(
                        scratch.path(),
                        &ProtectedPaths(vec![root.path().to_path_buf()]),
                    )
                    .unwrap();
                    populate(root.path(), &f, &expected, &members);
                    let reference = &expected.records[0].artifact_ref;
                    let path = cas_path(&f, reference);
                    if missing {
                        fs::remove_file(path).unwrap();
                    } else {
                        let mut bytes = fs::read(&path).unwrap();
                        *bytes.last_mut().unwrap() ^= 1;
                        fs::write(path, bytes).unwrap();
                    }
                    // Mutation is test setup; verification must not repair or alter it.
                    let before = fingerprint(root.path());
                    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
                    for complete in [false, true] {
                        rejected(members.verify(f.job_id, reference, &cas, complete), missing);
                    }
                    drop(members);
                    assert_eq!(fingerprint(scratch.path()), scratch_before);
                    assert_eq!(fingerprint(root.path()), before);
                }
            }

            #[test]
            fn protected_scratch_equal_nested_or_ancestor_is_rejected_before_writes() {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let before = fingerprint(root.path());
                let protected = ProtectedPaths(vec![root.path().to_path_buf()]);
                assert!(ReferenceMembership::create(root.path(), &protected).is_err());
                let absent = root.path().join("must-not-be-created");
                assert!(ReferenceMembership::create(&absent, &protected).is_err());
                assert!(!absent.exists());
                // Protect a contained native CAS directory, so the candidate parent
                // overlaps it in the opposite direction as well.
                assert!(ReferenceMembership::create(
                    root.path(),
                    &ProtectedPaths(vec![f.cas_root]),
                )
                .is_err());
                assert_eq!(fingerprint(root.path()), before);
            }
        }

        use super::*;
        use crate::snapshot::validation::ocomp::verify_present_admissions;
        use outbe_ocomp::admission_catalog::VerifiedAdmissionRecordV1;

        struct Expected {
            manifest: InputManifestV1,
            lysis_limit: U256,
            evaluation_time: u64,
            topology: LysisPlanTopologyV1,
            records: Vec<VerifiedAdmissionRecordV1>,
        }

        fn admission_root(root: &Path, f: &Fixture) -> PathBuf {
            root.join("supervisor-v1/jobs")
                .join(hex::encode(f.job_id))
                .join("admissions")
        }
        fn record_path(root: &Path, f: &Fixture, ordinal: u32) -> PathBuf {
            admission_root(root, f).join(format!("{ordinal:010}.admission"))
        }
        fn cas_path(f: &Fixture, reference: &CasObjectRefV1) -> PathBuf {
            let digest = hex::encode(reference.transport_digest);
            f.cas_root
                .join("objects")
                .join(&digest[..2])
                .join(&digest[2..])
        }

        fn expected(root: &Path, f: &Fixture) -> Expected {
            let limits = poc_schema_limits();
            let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
            let inputs = VerifiedInputChunkRefCatalog::reopen(
                root.join("exporter-v1/input-refs")
                    .join(hex::encode(f.job_id)),
                &cas,
                limits,
                poc_input_list_limits(),
            )
            .unwrap();
            let admissions =
                AdmissionCatalogReader::open_existing(admission_root(root, f), &cas, limits)
                    .unwrap();
            let audit = LocalLysisPlanAuditV1::open_read_only(
                &admissions,
                &inputs,
                &cas,
                &f.bundle,
                &limits,
            )
            .unwrap();
            let topology = LysisPlanTopologyV1::new(audit.plan().primary_work_unit_count).unwrap();
            Expected {
                manifest: audit.manifest().clone(),
                lysis_limit: audit.plan().lysis_limit_minor,
                evaluation_time: audit.plan().logical_evaluation_time,
                topology,
                records: (0..topology.total_unit_count())
                    .map(|ordinal| admissions.read(ordinal).unwrap())
                    .collect(),
            }
        }

        fn keep_only(root: &Path, f: &Fixture, expected: &Expected, ordinals: &[u32]) {
            for ordinal in 0..expected.topology.total_unit_count() {
                if !ordinals.contains(&ordinal) {
                    fs::remove_file(record_path(root, f, ordinal)).unwrap();
                }
            }
        }

        fn run(
            root: &Path,
            expected: &Expected,
            maximum: Option<u64>,
            visitor: &mut impl FnMut(&CasObjectRefV1) -> eyre::Result<()>,
        ) -> eyre::Result<(u32, u32, u32)> {
            let before = fingerprint(root);
            let result = verify_present_admissions(
                root,
                &expected.manifest,
                expected.lysis_limit,
                expected.evaluation_time,
                CAS_LIMITS,
                maximum,
                visitor,
            )
            .map(|audit| (audit.expected, audit.present, audit.result_chunks));
            assert_eq!(fingerprint(root), before);
            result
        }

        fn sorted(mut refs: Vec<CasObjectRefV1>) -> Vec<CasObjectRefV1> {
            refs.sort_by_key(|reference| {
                (
                    reference.transport_digest,
                    reference.encoded_bytes,
                    reference.expected_ocb1_kind,
                )
            });
            refs
        }

        fn references(records: &[VerifiedAdmissionRecordV1]) -> Vec<CasObjectRefV1> {
            records
                .iter()
                .flat_map(|record| {
                    std::iter::once(record.artifact_ref.clone()).chain(
                        record
                            .result_chunk
                            .as_ref()
                            .map(|chunk| chunk.output_manifest_entry.result_chunk_ref.clone()),
                    )
                })
                .collect()
        }

        #[test]
        fn complete_present_catalog_emits_exact_native_artifact_and_result_references() {
            let root = tempfile::tempdir().unwrap();
            let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
            let expected = expected(root.path(), &f);
            let total = expected.topology.total_unit_count();
            let mut visited = Vec::new();
            let counts = run(
                root.path(),
                &expected,
                Some(u64::from(total)),
                &mut |reference| {
                    visited.push(reference.clone());
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(counts, (total, total, 1));
            assert_eq!(sorted(visited), sorted(references(&expected.records)));
        }

        #[test]
        fn independent_partial_tail_and_empty_catalog_are_valid_without_full_plan_cursor() {
            for keep_first in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                let first = expected
                    .topology
                    .plan_ordinal_of(PlannedUnitPositionV1::Primary {
                        phase: UnitPhase::Enumerate,
                        ordinal: 0,
                    })
                    .unwrap();
                let keep = if keep_first { vec![first] } else { vec![] };
                keep_only(root.path(), &f, &expected, &keep);
                let mut visited = Vec::new();
                let counts = run(
                    root.path(),
                    &expected,
                    Some(keep.len() as u64),
                    &mut |reference| {
                        visited.push(reference.clone());
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(
                    counts,
                    (expected.topology.total_unit_count(), keep.len() as u32, 0)
                );
                let wanted = if keep_first {
                    vec![expected.records[first as usize].artifact_ref.clone()]
                } else {
                    vec![]
                };
                assert_eq!(visited, wanted);
            }
        }

        #[test]
        fn absent_independent_earlier_work_does_not_hide_corrupt_later_present_artifact() {
            let root = tempfile::tempdir().unwrap();
            let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 257);
            let expected = expected(root.path(), &f);
            let later = expected
                .topology
                .plan_ordinal_of(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::Enumerate,
                    ordinal: 1,
                })
                .unwrap();
            assert!(later > 0);
            keep_only(root.path(), &f, &expected, &[later]);
            assert_eq!(
                run(root.path(), &expected, None, &mut |_| Ok(())).unwrap(),
                (expected.topology.total_unit_count(), 1, 0)
            );
            let path = cas_path(&f, &expected.records[later as usize].artifact_ref);
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(path, bytes).unwrap();
            let error = run(root.path(), &expected, None, &mut |_| Ok(())).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        }

        #[test]
        fn present_consumer_without_required_producer_admission_is_incomplete() {
            let root = tempfile::tempdir().unwrap();
            let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
            let expected = expected(root.path(), &f);
            let consumer = expected
                .topology
                .plan_ordinal_of(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::FidelityMap,
                    ordinal: 0,
                })
                .unwrap();
            keep_only(root.path(), &f, &expected, &[consumer]);
            let mut calls = 0;
            let error = run(root.path(), &expected, None, &mut |_| {
                calls += 1;
                Ok(())
            })
            .unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert_eq!(
                calls, 0,
                "unbound consumer must not emit a verified reference"
            );
        }

        #[test]
        fn present_root_leaf_requires_its_exact_result_chunk_and_cas_corruption_fails() {
            for missing in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                let path = cas_path(&f, &f.result_chunk_refs[0]);
                if missing {
                    fs::remove_file(path).unwrap();
                } else {
                    let mut bytes = fs::read(&path).unwrap();
                    *bytes.last_mut().unwrap() ^= 1;
                    fs::write(path, bytes).unwrap();
                }
                let error = run(root.path(), &expected, None, &mut |_| Ok(())).unwrap_err();
                assert_eq!(
                    error.downcast_ref::<Incomplete>().is_some(),
                    missing,
                    "{error:#}"
                );
            }
        }

        #[test]
        fn present_foreign_admission_and_noncanonical_extra_locators_are_failed() {
            for damage in ["foreign", "out_of_range", "malformed"] {
                let root = tempfile::tempdir().unwrap();
                let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
                let expected = expected(root.path(), &f);
                match damage {
                    "foreign" => {
                        let foreign = fixture(root.path(), 0x40, WorldwideDay::new(20_260_726), 10);
                        fs::copy(
                            record_path(root.path(), &foreign, 0),
                            record_path(root.path(), &f, 0),
                        )
                        .unwrap();
                    }
                    "out_of_range" => {
                        fs::copy(
                            record_path(root.path(), &f, 0),
                            record_path(root.path(), &f, expected.topology.total_unit_count()),
                        )
                        .unwrap();
                    }
                    "malformed" => {
                        fs::write(
                            admission_root(root.path(), &f).join("1.admission"),
                            b"foreign entry",
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
                let error = run(root.path(), &expected, None, &mut |_| Ok(())).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{damage}: {error:#}"
                );
            }
        }

        #[test]
        fn canonical_export_manifest_and_frozen_plan_scalar_mismatches_are_failed() {
            let root = tempfile::tempdir().unwrap();
            let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
            for damage in ["manifest_checkpoint", "lysis_limit", "evaluation_time"] {
                let mut expected = expected(root.path(), &f);
                match damage {
                    "manifest_checkpoint" => {
                        expected.manifest.checkpoint.finalized_block_number += 1
                    }
                    "lysis_limit" => expected.lysis_limit += U256::ONE,
                    "evaluation_time" => expected.evaluation_time += 1,
                    _ => unreachable!(),
                }
                let mut calls = 0;
                let error = run(root.path(), &expected, None, &mut |_| {
                    calls += 1;
                    Ok(())
                })
                .unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{damage}: {error:#}"
                );
                assert_eq!(
                    calls, 0,
                    "authority must be checked before reference callbacks"
                );
            }
        }

        #[test]
        fn budget_and_visitor_failures_are_incomplete_not_partial_success() {
            let root = tempfile::tempdir().unwrap();
            let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
            let expected = expected(root.path(), &f);
            for maximum in [0, u64::from(expected.topology.total_unit_count() - 1)] {
                let error =
                    run(root.path(), &expected, Some(maximum), &mut |_| Ok(())).unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            }
            let mut calls = 0;
            let error = run(root.path(), &expected, None, &mut |_| {
                calls += 1;
                Err(Incomplete("reference visitor resource bound".into()).into())
            })
            .unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(
                error
                    .to_string()
                    .contains("reference visitor resource bound"),
                "{error:#}"
            );
            assert_eq!(calls, 1);
        }
    }

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
    mod present_discovery {
        use super::*;
        use crate::snapshot::{
            tests::headers::fingerprint,
            validation::{ocomp::verify_present_discovery, Incomplete},
        };
        use outbe_ocomp::discovery_spool::{DiscoverySpoolRecordV1, DiscoverySpoolV1};
        use outbe_ocomp_protocol::{
            common::BoundedBytes,
            control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
        };
        use std::{
            fs,
            path::{Path, PathBuf},
        };

        fn spec(job: &OcompJobRecordV1) -> FinalizedJobSpecV1 {
            let finality = job.finalized.as_ref().unwrap();
            FinalizedJobSpecV1 {
                summary: FinalizedJobSummaryV1 {
                    cursor: job.intent_height,
                    job_id: finality.job_id,
                    intent_id: job.intent.intent_id(&poc_schema_limits()).unwrap(),
                    finalized_block_hash: finality.finalized_request_block_hash,
                    finalized_state_root: finality.finalized_request_state_root,
                    protocol_bundle_hash: job.intent.protocol_bundle_hash,
                    open_height: finality.open_height,
                    deadline_height: finality.deadline_height,
                },
                canonical_job_intent: BoundedBytes(
                    job.intent.encode_canonical(&poc_schema_limits()).unwrap(),
                ),
            }
        }

        fn write(root: &Path, view: &RethReadOnlyView, spec: &FinalizedJobSpecV1) -> PathBuf {
            let parent = root.join("exporter-v1/discovery");
            fs::create_dir_all(&parent).unwrap();
            let path = parent.join(hex::encode(spec.summary.protocol_bundle_hash));
            let writer = DiscoverySpoolV1::open(
                &path,
                view.chain.chain().id(),
                view.chain.genesis_hash(),
                poc_schema_limits(),
            )
            .unwrap();
            writer.put_offer(1, spec).unwrap();
            drop(writer);
            path
        }

        #[test]
        fn all_present_offers_bind_current_canonical_job_without_export_prerequisite() {
            for version in [1, 2] {
                with_job(version, true, true, |state, view, job| {
                    let root = tempfile::tempdir().unwrap();
                    write(root.path(), view, &spec(&job));
                    let before = fingerprint(root.path());
                    let mut offers = 0;
                    let mut pending = 0;
                    let audit = verify_present_discovery(
                        state,
                        view,
                        root.path(),
                        None,
                        &mut |record, authority| {
                            match record {
                                DiscoverySpoolRecordV1::Offer(_) => {
                                    assert_eq!(authority.as_ref(), Some(&job));
                                    offers += 1;
                                }
                                DiscoverySpoolRecordV1::Pending { .. } => {
                                    assert!(authority.is_none());
                                    pending += 1;
                                }
                                _ => panic!("unexpected fixture record"),
                            }
                            Ok(())
                        },
                    )
                    .unwrap();
                    assert_eq!((audit.spools, audit.records, audit.offers), (1, 2, 1));
                    assert_eq!((offers, pending), (1, 1));
                    assert_eq!(fingerprint(root.path()), before);
                    assert!(!root.path().join("supervisor-v1").exists());
                });
            }
        }

        #[test]
        fn absent_retired_spools_are_optional_and_closure_is_not_a_bundle_spool() {
            with_job(1, true, true, |state, view, _| {
                let root = tempfile::tempdir().unwrap();
                for present_closure in [false, true] {
                    if present_closure {
                        fs::create_dir_all(
                            root.path()
                                .join("exporter-v1/discovery/closure-checkpoint-v1"),
                        )
                        .unwrap();
                    }
                    let before = fingerprint(root.path());
                    let audit =
                        verify_present_discovery(state, view, root.path(), Some(0), &mut |_, _| {
                            panic!("no records")
                        })
                        .unwrap();
                    assert_eq!((audit.spools, audit.records, audit.offers), (0, 0, 0));
                    assert_eq!(fingerprint(root.path()), before);
                }
            });
        }

        #[test]
        fn native_valid_offer_cannot_replace_canonical_cursor_or_voting_window() {
            with_job(1, true, true, |state, view, job| {
                for field in 0..3 {
                    let root = tempfile::tempdir().unwrap();
                    let mut offered = spec(&job);
                    match field {
                        0 => offered.summary.cursor += 1,
                        1 => offered.summary.open_height += 1,
                        _ => offered.summary.deadline_height += 1,
                    };
                    write(root.path(), view, &offered);
                    let before = fingerprint(root.path());
                    let error = verify_present_discovery(
                        state,
                        view,
                        root.path(),
                        None,
                        &mut |_, _| Ok(()),
                    )
                    .unwrap_err();
                    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
                    assert_eq!(fingerprint(root.path()), before);
                }
            });
        }

        #[test]
        fn complete_spool_walk_checks_bundle_location_and_later_native_records() {
            with_job(2, true, true, |state, view, job| {
                for fault in 0..2 {
                    let root = tempfile::tempdir().unwrap();
                    let path = write(root.path(), view, &spec(&job));
                    if fault == 0 {
                        fs::rename(
                            &path,
                            path.parent()
                                .unwrap()
                                .join(hex::encode(B256::repeat_byte(0xee))),
                        )
                        .unwrap();
                    } else {
                        let pending = fs::read_dir(path.join("pending"))
                            .unwrap()
                            .next()
                            .unwrap()
                            .unwrap()
                            .path();
                        fs::write(pending, b"corrupt").unwrap();
                    }
                    let before = fingerprint(root.path());
                    assert!(verify_present_discovery(
                        state,
                        view,
                        root.path(),
                        None,
                        &mut |_, _| Ok(())
                    )
                    .is_err());
                    assert_eq!(fingerprint(root.path()), before);
                }
            });
        }

        #[test]
        fn incomplete_budget_or_callback_cannot_be_replaced_by_partial_spool_success() {
            with_job(1, true, true, |state, view, job| {
                let root = tempfile::tempdir().unwrap();
                write(root.path(), view, &spec(&job));
                let before = fingerprint(root.path());
                for budget in [0, 1] {
                    let error = verify_present_discovery(
                        state,
                        view,
                        root.path(),
                        Some(budget),
                        &mut |_, _| Ok(()),
                    )
                    .unwrap_err();
                    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                }
                let error =
                    verify_present_discovery(state, view, root.path(), None, &mut |_, _| {
                        Err(Incomplete("callback evidence unavailable".into()).into())
                    })
                    .unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                assert_eq!(fingerprint(root.path()), before);
            });
        }
    }

    // This fixture is test setup only.
    // It uses public Registry initialization and native WWD/model capabilities;
    // private Metadosis persistence codecs are reproduced only to seed source words.
    // Every constructed aggregate must pass the real public native getter.
    mod active_canonical {
        mod exported_composition {
            mod all_native {
                use super::*;
                use crate::snapshot::{
                    config::{
                        parse_node_inputs, resolve_layout, resolve_requested_layout, NativeLayout,
                        NativeReadSelection, RequestedLayout,
                    },
                    native::ce_identity,
                    tests::{evm::state_fixture, headers::fingerprint},
                    validation::{
                        report::{CheckName, CheckStatus, ValidationReport},
                        run::{validate_snapshot, ValidationInputs},
                    },
                };
                use alloy_consensus::Header;
                use alloy_primitives::Address;
                use outbe_compressed_entities::{
                    body_commitment, sealed_root, AuthenticatedParentTree, CeMdbx, EntityRef,
                    ExactParentIdentity, FinalLeafMutation, FinalizedMarker, MdbxAuthenticatedTree,
                    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
                };
                use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
                use outbe_offchain_data::{
                    ProjectionCheckpoint, ProjectionState, STORAGE_SCHEMA_VERSION,
                };
                use outbe_offchain_storage::{
                    Key, Namespace, RocksDbStorage, StorageWriter, Value,
                };
                use outbe_primitives::reshare_artifact::{
                    encode_outbe_block_artifacts, CompressedEntitiesRootArtifact,
                    OutbeBlockArtifacts,
                };
                use reth_ethereum::provider::db::{
                    cursor::DbCursorRO,
                    database::Database,
                    init_db,
                    mdbx::DatabaseArguments,
                    models::StoredBlockBodyIndices,
                    table::Table,
                    tables::{self, ChainStateKey},
                    transaction::{DbTx, DbTxMut},
                    DatabaseEnv, DatabaseEnvKind,
                };
                use reth_ethereum::trie::root::{state_root_unhashed, storage_root_unhashed};
                use reth_primitives_traits::{Account, StorageEntry};
                use std::{collections::BTreeMap, ffi::OsString, sync::Arc};

                type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;
                const ACCOUNT: Address = Address::repeat_byte(0xf7);

                struct AllFixture {
                    source: tempfile::TempDir,
                    layout: NativeLayout,
                    job: B256,
                }

                fn arguments(root: &Path) -> Vec<OsString> {
                    vec![
                        "--chain".into(),
                        root.join("genesis.json").into_os_string(),
                        "--datadir".into(),
                        root.join("chain").into_os_string(),
                        "--projection.storage-config".into(),
                        root.join("configuration/offchain.toml").into_os_string(),
                    ]
                }

                impl AllFixture {
                    fn new(version: u32) -> Self {
                        use std::os::unix::fs::PermissionsExt;
                        let (source, _, _) = state_fixture(version);
                        let config = source.path().join("configuration/offchain.toml");
                        fs::write(
                            &config,
                            fs::read_to_string(&config)
                                .unwrap()
                                .replace("start_block = 17", "start_block = 0"),
                        )
                        .unwrap();
                        let inputs = parse_node_inputs(arguments(source.path())).unwrap();
                        let layout = resolve_layout(&inputs).unwrap();
                        let requested = resolve_requested_layout(
                            &inputs,
                            NativeReadSelection { projection: true },
                        )
                        .unwrap();
                        write_source(&requested);
                        let request = seal_source(&layout);
                        // The root is read from the real committed CE marker, not a fixture constant.
                        let marker = outbe_compressed_entities::CeMdbxReadOnly::open(
                            &layout.chain_root,
                            ce_identity(&layout),
                        )
                        .unwrap()
                        .marker()
                        .unwrap();
                        let prepared = fixture_for_identity(
                            &request,
                            Phase::VotingOpen,
                            layout.chain.chain().id(),
                            layout.chain.genesis_hash(),
                            |intent| {
                                bind_source(intent);
                                intent.ce_sealed_root = marker.new_root;
                            },
                        );
                        let export = write_export(&layout.ocomp_root, &prepared);
                        write_exported_pin(
                            &layout.consensus_root.join("ocomp_retention"),
                            &request,
                            &prepared.job,
                            export,
                        );
                        let job = prepared.job.finalized.as_ref().unwrap().job_id;
                        seed_execution(&layout, version, &request, prepared.owner);
                        write_frontiers(&requested, &request);
                        fs::write(
                            source.path().join("snapshot-signing-key.hex"),
                            hex::encode([1u8; 32]),
                        )
                        .unwrap();
                        fs::set_permissions(
                            source.path().join("snapshot-signing-key.hex"),
                            fs::Permissions::from_mode(0o600),
                        )
                        .unwrap();
                        // state_fixture already installs a protected recipient key and config.
                        // Every writer and native read handle is closed before returning.
                        Self {
                            source,
                            layout,
                            job,
                        }
                    }

                    fn validate(&self, inputs: &ValidationInputs) -> ValidationReport {
                        let before = fingerprint(self.source.path());
                        let scratch = tempfile::tempdir().unwrap();
                        let scratch_before = fingerprint(scratch.path());
                        let report = validate_snapshot(
                            inputs,
                            arguments(self.source.path()),
                            scratch.path(),
                        )
                        .unwrap();
                        assert_eq!(
                            fingerprint(self.source.path()),
                            before,
                            "native source was changed"
                        );
                        assert_eq!(
                            fingerprint(scratch.path()),
                            scratch_before,
                            "scratch leaked"
                        );
                        report
                    }

                    fn signed(&self, artifact: &Path) -> ValidationInputs {
                        let before = fingerprint(self.source.path());
                        let archive = artifact.join("snapshot.tar");
                        // This production create path observes current native progress, closes readers,
                        // enumerates and hashes the actual damaged files, then signs NEW manifest bytes.
                        let (_, signer) = crate::snapshot::create::create(
                            &archive,
                            &self.source.path().join("snapshot-signing-key.hex"),
                            Some("all-native fixture".into()),
                            None,
                            arguments(self.source.path()),
                        )
                        .unwrap();
                        assert_eq!(
                            fingerprint(self.source.path()),
                            before,
                            "create changed native source"
                        );
                        ValidationInputs {
                            archive: Some(archive),
                            expected_signer: Some(signer),
                            ..Default::default()
                        }
                    }
                }

                fn seal_source(layout: &NativeLayout) -> OutbeHeader {
                    // Use the same supported test precreation as tests/bodies.rs; do not alter owners.
                    drop(
                        reth_ethereum::provider::db::create_db(
                            layout.chain_root.join("compressed_entities/smt"),
                            DatabaseArguments::test(),
                        )
                        .unwrap(),
                    );
                    let genesis = FinalizedMarker {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        height: 0,
                        block_hash: layout.chain.genesis_hash(),
                        parent_block_hash: B256::ZERO,
                        parent_root: B256::ZERO,
                        new_root: sealed_root(B256::ZERO).unwrap(),
                    };
                    let db = Arc::new(
                        CeMdbx::open(&layout.chain_root, ce_identity(layout), genesis).unwrap(),
                    );
                    let parent = MdbxAuthenticatedTree::open(
                        db.clone(),
                        ExactParentIdentity {
                            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                            block_number: 0,
                            block_hash: genesis.block_hash,
                            root: genesis.new_root,
                        },
                    )
                    .unwrap();
                    let body = source_body();
                    let bytes = encode_tribute_v1(&outbe_tribute::canonical_body(&body)).unwrap();
                    let leaf = body_commitment(
                        ACTIVE_COMMITMENT_SCHEME,
                        BODY_SCHEMA_V1,
                        body.tribute_id,
                        &bytes,
                    )
                    .unwrap();
                    let seal = parent
                        .prepare_seal(
                            1,
                            &[FinalLeafMutation {
                                entity: EntityRef::Tribute(body.tribute_id),
                                final_leaf: Some(leaf),
                            }],
                            &[],
                        )
                        .unwrap();
                    let request = OutbeHeader::new(Header {
                        number: 1,
                        parent_hash: genesis.block_hash,
                        timestamp: 1_000,
                        extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                                r_sealed: seal.new_root(),
                            }),
                            ..Default::default()
                        })
                        .unwrap(),
                        ..Default::default()
                    });
                    db.apply_finalized(&seal.freeze(request.hash_slow()))
                        .unwrap();
                    request
                }

                fn seed_execution(
                    layout: &NativeLayout,
                    version: u32,
                    request: &OutbeHeader,
                    owner: HashMapStorageProvider,
                ) {
                    let mut accounts: BTreeMap<Address, (Account, Vec<(B256, U256)>)> =
                        BTreeMap::new();
                    accounts.insert(
                        ACCOUNT,
                        (
                            Account {
                                nonce: 7,
                                balance: U256::from(900),
                                bytecode_hash: None,
                            },
                            vec![],
                        ),
                    );
                    for ((address, slot), value) in owner.storage {
                        if !value.is_zero() {
                            accounts
                                .entry(address)
                                .or_default()
                                .1
                                .push((B256::from(slot.to_be_bytes::<32>()), value));
                        }
                    }
                    let state_root =
                        state_root_unhashed(accounts.iter().map(|(address, (account, words))| {
                            (
                                *address,
                                (*account).into_trie_account(storage_root_unhashed(
                                    words.iter().copied(),
                                )),
                            )
                        }));
                    let db =
                        init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
                    let tx = db.tx_mut().unwrap();
                    tx.clear::<tables::PlainAccountState>().unwrap();
                    tx.clear::<tables::PlainStorageState>().unwrap();
                    tx.clear::<tables::HashedAccounts>().unwrap();
                    tx.clear::<tables::HashedStorages>().unwrap();
                    tx.clear::<tables::Headers<OutbeHeader>>().unwrap();
                    tx.clear::<tables::CanonicalHeaders>().unwrap();
                    for (address, (account, words)) in accounts {
                        if version == 1 {
                            tx.put::<tables::PlainAccountState>(address, account)
                                .unwrap();
                            for (key, value) in words {
                                tx.put::<tables::PlainStorageState>(
                                    address,
                                    StorageEntry { key, value },
                                )
                                .unwrap();
                            }
                        } else {
                            tx.put::<tables::HashedAccounts>(keccak256(address), account)
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
                    let execution = OutbeHeader::new(Header {
                        number: 400,
                        parent_hash: request.hash_slow(),
                        timestamp: 1_010,
                        state_root,
                        ..Default::default()
                    });
                    for header in [
                        layout.chain.genesis_header().clone(),
                        request.clone(),
                        execution,
                    ] {
                        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
                            .unwrap();
                        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
                            .unwrap();
                    }
                    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 1)
                        .unwrap();
                    for stage in ["Execution", "Finish"] {
                        tx.put::<tables::StageCheckpoints>(stage.into(), StageCheckpoint::new(400))
                            .unwrap();
                    }
                    tx.put::<tables::BlockBodyIndices>(
                        1,
                        StoredBlockBodyIndices {
                            first_tx_num: 0,
                            tx_count: 0,
                        },
                    )
                    .unwrap();
                    tx.clear::<tables::AccountChangeSets>().unwrap();
                    tx.clear::<tables::StorageChangeSets>().unwrap();
                    tx.commit().unwrap();
                }

                fn write_frontiers(layout: &RequestedLayout, request: &OutbeHeader) {
                    let point = ProjectionCheckpoint {
                        block_number: 1,
                        block_hash: request.hash_slow(),
                    };
                    let location = layout.projection.as_ref().unwrap();
                    let projection = RocksDbStorage::open(&location.root).unwrap();
                    let state = ProjectionState {
                        chain_id: layout.chain.chain().id(),
                        genesis_hash: layout.chain.genesis_hash(),
                        storage_schema_version: STORAGE_SCHEMA_VERSION,
                        start_block: location.start_block,
                        checkpoint: Some(point),
                    };
                    projection
                        .put(
                            Namespace::new("projection_state").unwrap(),
                            &Key::new(b"offchain_data".to_vec()).unwrap(),
                            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
                        )
                        .unwrap();
                    drop(projection);
                    let baseline = ProjectionCheckpoint {
                        block_number: 0,
                        block_hash: layout.chain.genesis_hash(),
                    };
                    let closure = ContiguousCheckpointStoreV1::open(
                        layout
                            .ocomp_root
                            .join("exporter-v1/discovery/closure-checkpoint-v1"),
                        baseline,
                    )
                    .unwrap();
                    closure.compare_and_advance_to(baseline, point).unwrap();
                }

                fn assert_native_success(report: &ValidationReport) {
                    for check in [
                        CheckName::Headers,
                        CheckName::Evm,
                        CheckName::Ce,
                        CheckName::Bodies,
                        CheckName::Ocomp,
                    ] {
                        assert_eq!(
                            report.check(check).status,
                            CheckStatus::Passed,
                            "{check:?}: {report:?}"
                        );
                    }
                    assert_eq!(report.observed.h.as_ref().unwrap().number, 1);
                    assert_eq!(report.observed.e.as_ref().unwrap().number, 400);
                    assert_eq!(report.observed.q.as_ref().unwrap().number, 1);
                    assert_eq!(report.observed.p.as_ref().unwrap().number, 1);
                    assert_eq!(report.observed.c_current.as_ref().unwrap().number, 1);
                    assert!(report.required_missing.is_empty());
                    assert_eq!(report.active_ocomp.len(), 1);
                    assert!(report.active_ocomp[0].export_verified);
                    for name in [
                        "ce_leaves",
                        "live_projection_bodies",
                        "verified_source_leases",
                        "verified_complete_exports",
                    ] {
                        assert!(
                            report
                                .inventory_bounds
                                .iter()
                                .any(|bound| bound.name == name && bound.visited > 0),
                            "{name}: {report:?}"
                        );
                    }
                    assert!(report.success(), "{report:?}");
                }

                #[test]
                fn all_native_checks_real_nonzero_ce_body_and_active_export_at_independent_frontiers(
                ) {
                    for version in [1, 2] {
                        let fixture = AllFixture::new(version);
                        let report = fixture.validate(&ValidationInputs::default());
                        assert_native_success(&report);
                        for check in [CheckName::Files, CheckName::Provenance] {
                            assert_eq!(report.check(check).status, CheckStatus::NotRequested);
                        }
                    }
                }

                #[derive(Clone, Copy, Debug)]
                enum Damage {
                    None,
                    Evm,
                    Ce,
                    Body,
                    Receipt,
                    MissingCatalog,
                }

                fn damage(fixture: &AllFixture, damage: Damage) {
                    match damage {
                        Damage::None => {}
                        Damage::Evm => {
                            let db = init_db(
                                fixture.layout.chain_root.join("db"),
                                DatabaseArguments::test(),
                            )
                            .unwrap();
                            let tx = db.tx_mut().unwrap();
                            let mut account = tx
                                .get::<tables::HashedAccounts>(keccak256(ACCOUNT))
                                .unwrap()
                                .unwrap();
                            account.balance += U256::ONE;
                            tx.put::<tables::HashedAccounts>(keccak256(ACCOUNT), account)
                                .unwrap();
                            tx.commit().unwrap();
                        }
                        Damage::Ce => {
                            #[derive(Debug)]
                            struct TestCeLeaves;
                            impl Table for TestCeLeaves {
                                const NAME: &'static str = "OutbeCompressedEntitiesLeavesV3";
                                const DUPSORT: bool = false;
                                type Key = Vec<u8>;
                                type Value = Vec<u8>;
                            }
                            let db = DatabaseEnv::open(
                                &fixture.layout.chain_root.join("compressed_entities/smt"),
                                DatabaseEnvKind::RW,
                                DatabaseArguments::test(),
                            )
                            .unwrap();
                            let tx = db.tx_mut().unwrap();
                            let (key, previous) = tx
                                .cursor_read::<TestCeLeaves>()
                                .unwrap()
                                .seek(vec![1])
                                .unwrap()
                                .unwrap();
                            assert_eq!(key[0], 1);
                            let wrong = B256::with_last_byte(42).to_vec();
                            assert_ne!(previous, wrong);
                            tx.put::<TestCeLeaves>(key, wrong).unwrap();
                            tx.commit().unwrap();
                        }
                        Damage::Body => {
                            let storage = Arc::new(
                                RocksDbStorage::open(&fixture.layout.offchain_root).unwrap(),
                            );
                            let mut body = source_body();
                            body.nominal_amount_minor += U256::ONE;
                            outbe_tribute::TributeRepositoryWriter::new(storage.clone(), storage)
                                .put(&body)
                                .unwrap();
                        }
                        Damage::Receipt => {
                            let file = fixture
                                .layout
                                .ocomp_root
                                .join("exporter-v1/receipts")
                                .join(hex::encode(fixture.job))
                                .join("receipt.ref");
                            assert!(file.is_file());
                            fs::write(file, b"malformed native receipt reference").unwrap();
                        }
                        Damage::MissingCatalog => {
                            fs::remove_dir_all(
                                fixture
                                    .layout
                                    .ocomp_root
                                    .join("exporter-v1/input-refs")
                                    .join(hex::encode(fixture.job)),
                            )
                            .unwrap();
                        }
                    }
                }

                #[test]
                fn freshly_signed_all_distinguishes_native_semantic_damage_from_files_and_provenance(
                ) {
                    for fault in [
                        Damage::None,
                        Damage::Evm,
                        Damage::Ce,
                        Damage::Body,
                        Damage::Receipt,
                        Damage::MissingCatalog,
                    ] {
                        let fixture = AllFixture::new(2);
                        damage(&fixture, fault);
                        let artifact = tempfile::tempdir().unwrap();
                        let inputs = fixture.signed(artifact.path());
                        let before = fingerprint(artifact.path());
                        let report = fixture.validate(&inputs);
                        assert_eq!(fingerprint(artifact.path()), before);
                        for check in [CheckName::Files, CheckName::Provenance, CheckName::Headers] {
                            assert_eq!(
                                report.check(check).status,
                                CheckStatus::Passed,
                                "{fault:?}, {check:?}: {report:?}"
                            );
                        }
                        assert_eq!(report.provenance.signature_valid, Some(true));
                        assert_eq!(report.provenance.expected_signer_match, Some(true));
                        let expected = match fault {
                            Damage::None => {
                                assert_native_success(&report);
                                continue;
                            }
                            Damage::Evm => (CheckName::Evm, CheckStatus::Failed),
                            Damage::Ce => (CheckName::Ce, CheckStatus::Failed),
                            Damage::Body => (CheckName::Bodies, CheckStatus::Failed),
                            Damage::Receipt => (CheckName::Ocomp, CheckStatus::Failed),
                            Damage::MissingCatalog => (CheckName::Ocomp, CheckStatus::Incomplete),
                        };
                        assert_eq!(
                            report.check(expected.0).status,
                            expected.1,
                            "{fault:?}: {report:?}"
                        );
                        assert!(report.check(expected.0).diagnostic.is_some());
                        assert!(!report.success());
                    }
                }
            }

            mod final_join {
                fn install_active_result_request_frame(
                    layout: &crate::snapshot::config::RequestedLayout,
                ) -> OutbeHeader {
                    use alloy_consensus::{SignableTransaction, TxLegacy};
                    use alloy_primitives::{Log, Signature};
                    use alloy_sol_types::SolEvent;
                    use outbe_metadosis::precompile::IMetadosis;
                    use outbe_offchain_data::{ProjectionState, STORAGE_SCHEMA_VERSION};
                    use outbe_offchain_storage::{
                        Key, Namespace, RocksDbStorage, StorageWriter, Value,
                    };
                    use outbe_primitives::{
                        addresses::METADOSIS_ADDRESS, projection::ProjectionCheckpoint,
                        OutbePrimitives, OutbeReceipt, OutbeTxEnvelope,
                    };
                    use reth_ethereum::provider::db::{
                        models::StoredBlockBodyIndices, transaction::DbTxMut,
                    };
                    use reth_provider::{
                        providers::StaticFileProviderBuilder, StaticFileSegment, StaticFileWriter,
                    };
                    let db =
                        init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
                    let tx = db.tx_mut().unwrap();
                    let mut header = tx
                        .get::<tables::Headers<OutbeHeader>>(100)
                        .unwrap()
                        .unwrap();
                    // The native planner requires a nonzero frozen logical time.
                    // Set the request header before deriving the event and canonical owner.
                    header.inner.timestamp = 1_000;
                    let prepared = fixture_for_identity(
                        &header,
                        Phase::VotingOpen,
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                        bind_source,
                    );
                    let intent = &prepared.job.intent;
                    let event = IMetadosis::OffchainJobRequested {
                        intentId: intent.intent_id(&poc_schema_limits()).unwrap(),
                        wwd: intent.wwd,
                        pendingNonce: intent.pending_nonce,
                        attempt: intent.attempt,
                        activationPreconditionsHash: intent
                            .activation_preconditions
                            .activation_preconditions_hash(&poc_schema_limits())
                            .unwrap(),
                    };
                    let receipts = vec![OutbeReceipt {
                        success: true,
                        cumulative_gas_used: 21_000,
                        logs: vec![Log {
                            address: METADOSIS_ADDRESS,
                            data: event.encode_log_data(),
                        }],
                        ..Default::default()
                    }];
                    let transactions: Vec<OutbeTxEnvelope> = vec![TxLegacy {
                        gas_limit: 21_000,
                        ..Default::default()
                    }
                    .into_signed(Signature::new(U256::ONE, U256::ONE, false))
                    .into()];
                    header.inner.gas_limit = 30_000_000;
                    header.inner.gas_used = 21_000;
                    header.inner.transactions_root =
                        alloy_consensus::proofs::calculate_transaction_root(&transactions);
                    header.inner.receipts_root =
                        reth_ethereum::calculate_receipt_root_no_memo(&receipts);
                    let hash = header.hash_slow();
                    tx.put::<tables::Headers<OutbeHeader>>(100, header.clone())
                        .unwrap();
                    tx.put::<tables::CanonicalHeaders>(100, hash).unwrap();
                    let mut next = tx
                        .get::<tables::Headers<OutbeHeader>>(101)
                        .unwrap()
                        .unwrap();
                    next.inner.parent_hash = hash;
                    next.inner.timestamp = 1_001;
                    tx.put::<tables::CanonicalHeaders>(101, next.hash_slow())
                        .unwrap();
                    tx.put::<tables::Headers<OutbeHeader>>(101, next).unwrap();
                    tx.put::<tables::BlockBodyIndices>(
                        100,
                        StoredBlockBodyIndices {
                            first_tx_num: 0,
                            tx_count: 1,
                        },
                    )
                    .unwrap();
                    tx.put::<tables::Receipts<OutbeReceipt>>(0, receipts[0].clone())
                        .unwrap();
                    tx.commit().unwrap();
                    drop(db);
                    let files = StaticFileProviderBuilder::read_write(&layout.static_files_root)
                        .with_blocks_per_file(1_000)
                        .build::<OutbePrimitives>()
                        .unwrap();
                    {
                        let mut writer = files
                            .get_writer(0, StaticFileSegment::Transactions)
                            .unwrap();
                        for height in 0..=100 {
                            writer.increment_block(height).unwrap();
                        }
                        writer.append_transaction(0, &transactions[0]).unwrap();
                    }
                    files.commit().unwrap();
                    drop(files);
                    // Keep native setup frontiers attached to the now-complete request header.
                    let location = layout.projection.as_ref().unwrap();
                    let projection = RocksDbStorage::open(&location.root).unwrap();
                    let point = ProjectionCheckpoint {
                        block_number: 100,
                        block_hash: hash,
                    };
                    let state = ProjectionState {
                        chain_id: layout.chain.chain().id(),
                        genesis_hash: layout.chain.genesis_hash(),
                        storage_schema_version: STORAGE_SCHEMA_VERSION,
                        start_block: location.start_block,
                        checkpoint: Some(point),
                    };
                    projection
                        .put(
                            Namespace::new("projection_state").unwrap(),
                            &Key::new(b"offchain_data".to_vec()).unwrap(),
                            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
                        )
                        .unwrap();
                    drop(projection);
                    let closure_root = layout
                        .ocomp_root
                        .join("exporter-v1/discovery/closure-checkpoint-v1");
                    fs::remove_dir_all(&closure_root).unwrap();
                    let baseline = ProjectionCheckpoint {
                        block_number: 0,
                        block_hash: layout.chain.genesis_hash(),
                    };
                    let closure = outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1::open(
                        &closure_root,
                        baseline,
                    )
                    .unwrap();
                    closure.compare_and_advance_to(baseline, point).unwrap();
                    drop(closure);
                    header
                }

                fn write_empty_native_plan(root: &Path, prepared: &ActiveFixture) -> (B256, B256) {
                    use outbe_lysis::program_v1::planner::{
                        LysisPlannerBindingsV1, LysisPlannerV1,
                    };
                    use outbe_ocomp::{
                        admission_catalog::VerifiedAdmissionCatalog,
                        export_receipt::ExportReceiptReader,
                    };
                    let limits = poc_schema_limits();
                    let job = &prepared.job;
                    let id = job.finalized.as_ref().unwrap().job_id;
                    let cas = FilesystemCas::open(
                        root.join("cas-v1"),
                        CasWriterRole::Supervisor,
                        CAS_LIMITS,
                    )
                    .unwrap();
                    let reader =
                        FilesystemCasReader::open(root.join("cas-v1"), CAS_LIMITS).unwrap();
                    let receipt =
                        ExportReceiptReader::open(root.join("exporter-v1/receipts"), id, limits)
                            .unwrap()
                            .load_exact(&reader)
                            .unwrap();
                    let manifest = receipt.manifest();
                    let manifest_ref = receipt.manifest_ref();
                    let inputs = VerifiedInputChunkRefCatalog::reopen(
                        root.join("exporter-v1/input-refs").join(hex::encode(id)),
                        &reader,
                        limits,
                        OrderedListLimits::new(16, 4096, 4096),
                    )
                    .unwrap();
                    let refs = inputs
                        .exact_cursor()
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap();
                    let bundle = &prepared.bundle;
                    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
                        protocol_bundle_hash: job.intent.protocol_bundle_hash,
                        job_id: id,
                        attempt: job.intent.attempt,
                        input_manifest_hash: receipt.manifest_hash(),
                        input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
                        fidelity_opening_root: manifest.fidelity_opening_root,
                        oracle_opening_root: manifest.oracle_opening_root,
                        wwd: job.intent.wwd,
                        lysis_limit_minor: job.intent.frozen_metadosis_values.lysis_limit_minor,
                        logical_evaluation_time: job.intent.logical_evaluation_time,
                        tribute_count: manifest.tribute_count,
                        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
                        planner_spec_version: bundle.planner_spec_version,
                        reducer_spec_version: bundle.reducer_spec_version,
                    })
                    .unwrap();
                    let plan = planner.commit_primary_catalog(refs, &limits).unwrap();
                    let plan_ref = cas
                        .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
                        .unwrap();
                    let admission_root = root
                        .join("supervisor-v1/jobs")
                        .join(hex::encode(id))
                        .join("admissions");
                    drop(
                        VerifiedAdmissionCatalog::open(
                            &admission_root,
                            &cas,
                            &plan_ref,
                            &manifest_ref,
                            limits,
                        )
                        .unwrap(),
                    );
                    (receipt.manifest_hash(), plan.plan_hash(&limits).unwrap())
                }

                #[test]
                fn present_local_result_matches_surviving_manifest_and_plan_without_requiring_retired_plan(
                ) {
                    use crate::snapshot::tests::ocomp::pin_authority::local_result::{
                        refresh_arithmetic, result_for, write_result,
                    };
                    for version in [1, 2] {
                        for damage in ["none", "manifest", "plan", "plan_absent"] {
                            let identity = std::cell::Cell::new(None);
                            crate::snapshot::tests::ocomp::with_canonical_frontiers(
                                version,
                                |layout| {
                                    identity.set(Some((
                                        layout.chain.chain().id(),
                                        layout.chain.genesis_hash(),
                                    )));
                                    write_source(layout);
                                    let request = install_active_result_request_frame(layout);
                                    let prepared = fixture_for_identity(
                                        &request,
                                        Phase::VotingOpen,
                                        layout.chain.chain().id(),
                                        layout.chain.genesis_hash(),
                                        bind_source,
                                    );
                                    let export = write_export(&layout.ocomp_root, &prepared);
                                    write_exported_pin(
                                        &layout.consensus_root.join("ocomp_retention"),
                                        &request,
                                        &prepared.job,
                                        export,
                                    );
                                    let (manifest_hash, plan_hash) =
                                        write_empty_native_plan(&layout.ocomp_root, &prepared);
                                    let mut result = result_for(&prepared.job);
                                    result.input_manifest_hash = manifest_hash;
                                    result.plan_hash = plan_hash;
                                    match damage {
                                        "manifest" => {
                                            result.input_manifest_hash = B256::repeat_byte(0xf1)
                                        }
                                        "plan" | "plan_absent" => {
                                            result.plan_hash = B256::repeat_byte(0xf2)
                                        }
                                        _ => {}
                                    }
                                    refresh_arithmetic(&mut result);
                                    write_result(&layout.ocomp_root, &result);
                                    if damage == "plan_absent" {
                                        fs::remove_dir_all(
                                            layout
                                                .ocomp_root
                                                .join("supervisor-v1/jobs")
                                                .join(hex::encode(result.job_id)),
                                        )
                                        .unwrap();
                                    }
                                },
                                |request| {
                                    let (chain_id, genesis_hash) = identity.get().unwrap();
                                    fixture_for_identity(
                                        request,
                                        Phase::VotingOpen,
                                        chain_id,
                                        genesis_hash,
                                        bind_source,
                                    )
                                    .owner
                                },
                                |state, source, layout, scratch| {
                                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                                    let result = verify_ocomp_relations(
                                        state,
                                        source,
                                        layout,
                                        scratch,
                                        &mut report,
                                    );
                                    if matches!(damage, "none" | "plan_absent") {
                                        result.unwrap();
                                    } else {
                                        let error = result
                            .expect_err("surviving result/manifest/plan disagreement cannot pass");
                                        assert!(
                                            error.downcast_ref::<Incomplete>().is_none(),
                                            "{damage}: {error:#}"
                                        );
                                        assert!(
                                            format!("{error:#}").contains("surviving local result"),
                                            "{damage}: {error:#}"
                                        );
                                    }
                                },
                            );
                        }
                    }
                }

                #[test]
                fn retained_discovery_ack_must_match_surviving_export_but_retired_records_stay_optional(
                ) {
                    use outbe_ocomp::{
                        discovery_spool::{
                            DiscoverySpoolReaderV1, DiscoverySpoolRecordV1, DiscoverySpoolV1,
                        },
                        export_receipt::ExportReceiptReader,
                    };
                    for version in [1, 2] {
                        for damage in [
                            "none",
                            "lease",
                            "manifest",
                            "record",
                            "receipt_digest",
                            "retired",
                        ] {
                            let identity = std::cell::Cell::new(None);
                            crate::snapshot::tests::ocomp::with_canonical_frontiers(
                                version,
                                |layout| {
                                    identity.set(Some((
                                        layout.chain.chain().id(),
                                        layout.chain.genesis_hash(),
                                    )));
                                    write_source(layout);
                                    let db = init_db(
                                        layout.chain_root.join("db"),
                                        DatabaseArguments::test(),
                                    )
                                    .unwrap();
                                    let tx = db.tx().unwrap();
                                    let request = tx
                                        .get::<tables::Headers<OutbeHeader>>(100)
                                        .unwrap()
                                        .unwrap();
                                    drop(tx);
                                    drop(db);
                                    let prepared = fixture_for_identity(
                                        &request,
                                        Phase::VotingOpen,
                                        layout.chain.chain().id(),
                                        layout.chain.genesis_hash(),
                                        bind_source,
                                    );
                                    let export = write_export(&layout.ocomp_root, &prepared);
                                    write_exported_pin(
                                        &layout.consensus_root.join("ocomp_retention"),
                                        &request,
                                        &prepared.job,
                                        export,
                                    );
                                    let job = &prepared.job;
                                    let finalized = job.finalized.as_ref().unwrap();
                                    let limits = poc_schema_limits();
                                    let spec = FinalizedJobSpecV1 {
                                        summary: FinalizedJobSummaryV1 {
                                            cursor: job.intent_height,
                                            job_id: finalized.job_id,
                                            intent_id: job.intent.intent_id(&limits).unwrap(),
                                            finalized_block_hash: finalized
                                                .finalized_request_block_hash,
                                            finalized_state_root: finalized
                                                .finalized_request_state_root,
                                            protocol_bundle_hash: job.intent.protocol_bundle_hash,
                                            open_height: finalized.open_height,
                                            deadline_height: finalized.deadline_height,
                                        },
                                        canonical_job_intent: BoundedBytes(
                                            job.intent.encode_canonical(&limits).unwrap(),
                                        ),
                                    };
                                    let spool_root = layout
                                        .ocomp_root
                                        .join("exporter-v1/discovery")
                                        .join(hex::encode(job.intent.protocol_bundle_hash));
                                    let spool = DiscoverySpoolV1::open(
                                        &spool_root,
                                        job.intent.chain_id,
                                        job.intent.genesis_hash,
                                        limits,
                                    )
                                    .unwrap();
                                    let (offer, _) =
                                        spool.put_offer(export.source_generation, &spec).unwrap();
                                    let cas = FilesystemCasReader::open(
                                        layout.ocomp_root.join("cas-v1"),
                                        CAS_LIMITS,
                                    )
                                    .unwrap();
                                    let receipt = ExportReceiptReader::open(
                                        layout.ocomp_root.join("exporter-v1/receipts"),
                                        finalized.job_id,
                                        limits,
                                    )
                                    .unwrap()
                                    .load_exact(&cas)
                                    .unwrap();
                                    spool.put_ack(&offer, &receipt, &prepared.bundle).unwrap();
                                    if damage == "retired" {
                                        spool.prepare_retirement(&offer, 100).unwrap();
                                        assert_eq!(
                                            spool
                                                .complete_retirements_through(100)
                                                .unwrap()
                                                .completed,
                                            1
                                        );
                                        assert!(spool
                                            .ack(&offer.observation_id)
                                            .unwrap()
                                            .is_none());
                                    } else if damage != "none" {
                                        let mut ack =
                                            spool.ack(&offer.observation_id).unwrap().unwrap();
                                        match damage {
                                            "lease" => ack.lease_generation += 1,
                                            "manifest" => {
                                                ack.manifest_hash = B256::repeat_byte(0xe1)
                                            }
                                            "record" => {
                                                ack.committed.record_hash = B256::repeat_byte(0xe2)
                                            }
                                            "receipt_digest" => {
                                                ack.reference.export_receipt_digest =
                                                    B256::repeat_byte(0xe3)
                                            }
                                            _ => unreachable!(),
                                        }
                                        // Exact native ACK envelope in test setup only. Recompute its
                                        // checksum so the existing native decoder accepts the fixture;
                                        // the failure must come from the missing cross-record relation.
                                        let canonical = ack.committed.encode_body(&limits).unwrap();
                                        let mut bytes = b"OUTBDSA2".to_vec();
                                        bytes.extend_from_slice(&ack.reference.encode_fixed());
                                        bytes
                                            .extend_from_slice(&ack.lease_generation.to_be_bytes());
                                        bytes.extend_from_slice(ack.manifest_hash.as_slice());
                                        bytes.extend_from_slice(
                                            &u64::try_from(canonical.len()).unwrap().to_be_bytes(),
                                        );
                                        bytes.extend_from_slice(&canonical);
                                        bytes.extend_from_slice(keccak256(&bytes).as_slice());
                                        fs::write(
                                            spool_root.join("acks").join(format!(
                                                "{}.ack",
                                                hex::encode(offer.observation_id)
                                            )),
                                            bytes,
                                        )
                                        .unwrap();
                                    }
                                    drop(spool);
                                    let reader = DiscoverySpoolReaderV1::open_existing(
                                        &spool_root,
                                        job.intent.chain_id,
                                        job.intent.genesis_hash,
                                        limits,
                                    )
                                    .unwrap();
                                    let mut acknowledgements = 0;
                                    reader
                                        .visit_records(&mut |record| {
                                            if matches!(record, DiscoverySpoolRecordV1::Ack(_)) {
                                                acknowledgements += 1;
                                            }
                                            Ok(())
                                        })
                                        .unwrap();
                                    assert_eq!(acknowledgements, usize::from(damage != "retired"));
                                },
                                |request| {
                                    let (chain_id, genesis_hash) = identity.get().unwrap();
                                    fixture_for_identity(
                                        request,
                                        Phase::VotingOpen,
                                        chain_id,
                                        genesis_hash,
                                        bind_source,
                                    )
                                    .owner
                                },
                                |state, source, layout, scratch| {
                                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                                    let result = verify_ocomp_relations(
                                        state,
                                        source,
                                        layout,
                                        scratch,
                                        &mut report,
                                    );
                                    if matches!(damage, "none" | "retired") {
                                        result.unwrap();
                                    } else {
                                        let error = result
                            .expect_err("native-valid ACK disagreement cannot pass the final join");
                                        assert!(
                                            error.downcast_ref::<Incomplete>().is_none(),
                                            "{damage}: {error:#}"
                                        );
                                        assert!(
                                            format!("{error:#}").contains("ACK"),
                                            "{damage}: {error:#}"
                                        );
                                    }
                                },
                            );
                        }
                    }
                }

                use super::*;
                use crate::snapshot::validation::{
                    ocomp::verify_ocomp_relations,
                    report::{CheckName, ValidationReport},
                };
                use reth_ethereum::provider::db::{
                    database::Database, init_db, mdbx::DatabaseArguments, tables, transaction::DbTx,
                };

                #[test]
                fn final_join_accepts_real_exported_active_job_and_detects_deleted_required_catalog(
                ) {
                    for deleted in [false, true] {
                        crate::snapshot::tests::ocomp::with_canonical_frontiers(
                            2,
                            |layout| {
                                write_source(layout);
                                let db = init_db(
                                    layout.chain_root.join("db"),
                                    DatabaseArguments::test(),
                                )
                                .unwrap();
                                let tx = db.tx().unwrap();
                                let request = tx
                                    .get::<tables::Headers<OutbeHeader>>(100)
                                    .unwrap()
                                    .unwrap();
                                drop(tx);
                                drop(db);
                                let prepared = fixture(&request, Phase::VotingOpen, bind_source);
                                let export = write_export(&layout.ocomp_root, &prepared);
                                write_exported_pin(
                                    &layout.consensus_root.join("ocomp_retention"),
                                    &request,
                                    &prepared.job,
                                    export,
                                );
                                if deleted {
                                    fs::remove_dir_all(
                                        layout.ocomp_root.join("exporter-v1/input-refs").join(
                                            hex::encode(
                                                prepared.job.finalized.as_ref().unwrap().job_id,
                                            ),
                                        ),
                                    )
                                    .unwrap();
                                }
                            },
                            |request| fixture(request, Phase::VotingOpen, bind_source).owner,
                            |state, source, layout, scratch| {
                                let mut report = ValidationReport::new([CheckName::Ocomp]);
                                let result = verify_ocomp_relations(
                                    state,
                                    source,
                                    layout,
                                    scratch,
                                    &mut report,
                                );
                                if deleted {
                                    assert!(result
                                        .unwrap_err()
                                        .downcast_ref::<Incomplete>()
                                        .is_some());
                                } else {
                                    result.unwrap();
                                    assert_eq!(report.active_ocomp.len(), 1);
                                    assert!(report.active_ocomp[0].export_verified);
                                    assert!(report
                                        .inventory_bounds
                                        .iter()
                                        .any(|bound| bound.name == "present_receipts"
                                            && bound.visited == 1));
                                }
                            },
                        );
                    }
                }
            }
            use super::*;
            use crate::snapshot::validation::{
                ocomp::{verify_canonical_obligations, CanonicalLocalPinStage},
                Incomplete,
            };
            use alloy_primitives::{keccak256, B256};
            use outbe_compressed_entities::encode_tribute_v1;
            use outbe_node::ocomp::retention::{
                inspect_retention_journal, CandidatePinV1, ExportAuthorityV1, PinRecordV1,
                PinStateV1,
            };
            use outbe_ocomp::{
                cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
                export_binding::{ExportBindingCandidate, ExportedManifestBindingStore},
                export_receipt::{ExportReceiptCandidate, ExportReceiptStore},
                input_artifacts::derive_input_chunk_ref,
                input_ref_catalog::VerifiedInputChunkRefCatalog,
                supervisor::DiscoveryRecord,
            };
            use outbe_ocomp_protocol::{
                common::BoundedBytes,
                control::{FinalizedJobSpecV1, FinalizedJobSummaryV1, SnapshotHandoffV1},
                input::{
                    AuthenticatedInputChunkV1, CheckpointIdentityV1, Compression, InputChunkKind,
                    InputManifestV1,
                },
                ListKind, ObjectKind, OrderedListLimits, SnapshotExportCommittedV1,
            };
            use std::{fs, path::Path};
            const CAS_LIMITS: CasLimits = CasLimits {
                max_object_bytes: 1_048_576,
                max_total_bytes: u64::MAX,
            };

            // Actual native writers close the manifest, reference catalog, binding and receipt.
            // This fixture exercises a Tribute-only closure, not worker opening-proof E2E.
            fn write_export(root: &Path, prepared: &ActiveFixture) -> ExportAuthorityV1 {
                let limits = poc_schema_limits();
                let list_limits = OrderedListLimits::new(16, 4096, 4096);
                let job = &prepared.job;
                let intent = &job.intent;
                let bundle = &prepared.bundle;
                let finalized = job.finalized.as_ref().unwrap();
                let spec = FinalizedJobSpecV1 {
                    summary: FinalizedJobSummaryV1 {
                        cursor: job.intent_height,
                        job_id: finalized.job_id,
                        intent_id: intent.intent_id(&limits).unwrap(),
                        finalized_block_hash: finalized.finalized_request_block_hash,
                        finalized_state_root: finalized.finalized_request_state_root,
                        protocol_bundle_hash: intent.protocol_bundle_hash,
                        open_height: finalized.open_height,
                        deadline_height: finalized.deadline_height,
                    },
                    canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
                };
                let cas_root = root.join("cas-v1");
                let job_hex = hex::encode(spec.summary.job_id);
                let binding_root = root.join("supervisor-v1/export-bindings").join(&job_hex);
                let catalog_root = root.join("exporter-v1/input-refs").join(&job_hex);
                let receipt_base = root.join("exporter-v1/receipts");
                let bundles = root.join("protocol-bundles-v1");
                fs::create_dir_all(&bundles).unwrap();
                fs::write(
                    bundles.join(format!(
                        "{}.ocb1",
                        hex::encode(spec.summary.protocol_bundle_hash)
                    )),
                    bundle.encode_canonical(&limits).unwrap(),
                )
                .unwrap();
                let cas_limits = CAS_LIMITS;
                let cas =
                    FilesystemCas::open(&cas_root, CasWriterRole::SnapshotExporter, cas_limits)
                        .unwrap();
                let reader = FilesystemCasReader::open(&cas_root, cas_limits).unwrap();
                let tribute = outbe_tribute::canonical_body(&super::source_body());
                let chunk = AuthenticatedInputChunkV1 {
                    protocol_bundle_hash: spec.summary.protocol_bundle_hash,
                    job_id: spec.summary.job_id,
                    kind: InputChunkKind::Tribute,
                    ordinal: 0,
                    canonical_records_or_openings: vec![BoundedBytes(
                        encode_tribute_v1(&tribute).unwrap(),
                    )],
                };
                let mut chunk_ref = cas
                    .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
                    .unwrap();
                chunk_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
                let input_ref = derive_input_chunk_ref(
                    &reader.read_verified(&chunk_ref).unwrap(),
                    bundle,
                    &limits,
                )
                .unwrap()
                .reference;
                let manifest = InputManifestV1 {
                    protocol_bundle_hash: spec.summary.protocol_bundle_hash,
                    job_id: spec.summary.job_id,
                    attempt: intent.attempt,
                    checkpoint: CheckpointIdentityV1 {
                        finalized_block_number: spec.summary.cursor,
                        finalized_block_hash: spec.summary.finalized_block_hash,
                        finalized_state_root: spec.summary.finalized_state_root,
                        finalized_ce_root: intent.ce_sealed_root,
                        ce_schema_version: u16::try_from(
                            outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
                        )
                        .unwrap(),
                    },
                    wwd: intent.wwd,
                    sealed_tribute_collection_key: intent.sealed_tribute_collection_key,
                    sealed_tribute_collection_root: intent.sealed_tribute_collection_root,
                    tribute_count: intent.authenticated_day_count,
                    tribute_nominal_total: intent.authenticated_day_nominal,
                    input_chunk_count: 1,
                    input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
                        ListKind::InputChunkReferences,
                        &[input_ref.encode_canonical_record(&limits).unwrap()],
                        list_limits,
                    )
                    .unwrap(),
                    fidelity_opening_root: B256::repeat_byte(201),
                    oracle_opening_root: B256::repeat_byte(202),
                    exact_encoded_bytes: input_ref.encoded_bytes,
                    exact_record_count: input_ref.record_count,
                    body_codec_id: bundle.tribute_body_codec_id,
                    opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
                    compression: Compression::None,
                };
                let mut manifest_ref = cas
                    .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
                    .unwrap();
                manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
                let mut catalog = VerifiedInputChunkRefCatalog::open(
                    &catalog_root,
                    &cas,
                    &manifest_ref,
                    limits,
                    list_limits,
                )
                .unwrap();
                catalog.admit(&input_ref).unwrap();
                let committed = SnapshotExportCommittedV1 {
                    job_id: spec.summary.job_id,
                    pin_generation: 12,
                    record_hash: B256::repeat_byte(203),
                };
                // Only the native producer uses the legacy discovery record. The offline
                // consumer below retains the authenticated immutable spec, not this record.
                let discovery = DiscoveryRecord {
                    generation: 7,
                    cursor: spec.summary.cursor,
                    spec: spec.clone(),
                };
                let _binding_ref = {
                    let mut store =
                        ExportedManifestBindingStore::open(&binding_root, limits).unwrap();
                    store
                        .seal(
                            &cas,
                            &reader,
                            ExportBindingCandidate {
                                discovery: &discovery,
                                job_id: spec.summary.job_id,
                                source_pin_generation: 11,
                                lease_generation: 17,
                                checkpoint: &manifest.checkpoint,
                                manifest_ref: &manifest_ref,
                                committed: &committed,
                                bundle,
                                input_refs: &catalog,
                            },
                        )
                        .unwrap()
                        .1
                        .binding_ref()
                };
                let receipt_source = 11;
                let receipt_lease = 17;
                let receipt_manifest = manifest.clone();
                let receipt_manifest_ref = manifest_ref.clone();
                let receipt_committed = committed.clone();
                let handoff = SnapshotHandoffV1 {
                    job_id: spec.summary.job_id,
                    input_lease_id: intent.input_lease_id().unwrap(),
                    pin_generation: receipt_source,
                    lease_generation: receipt_lease,
                    checkpoint: receipt_manifest.checkpoint.clone(),
                    canonical_lease_offer: BoundedBytes(vec![1]),
                };
                let _receipt_ref = {
                    let mut store =
                        ExportReceiptStore::open(&receipt_base, spec.summary.job_id, limits)
                            .unwrap();
                    store
                        .record(
                            &cas,
                            &reader,
                            ExportReceiptCandidate {
                                handoff: &handoff,
                                manifest_ref: &receipt_manifest_ref,
                                manifest_hash: receipt_manifest.manifest_hash(&limits).unwrap(),
                                committed: &receipt_committed,
                            },
                        )
                        .unwrap()
                        .1
                        .receipt_ref()
                };

                ExportAuthorityV1 {
                    source_generation: 11,
                    lease_generation: 17,
                    manifest_hash: manifest.manifest_hash(&limits).unwrap(),
                }
            }
            // The owner does not expose a public journal writer independent of live frame
            // ingestion. Confine the exact native v6 fixture codec to test setup and decode
            // it immediately through the owner's public read-only inspector.
            fn write_exported_pin(
                root: &Path,
                request: &OutbeHeader,
                job: &OcompJobRecordV1,
                export: ExportAuthorityV1,
            ) {
                let finalized = job.finalized.as_ref().unwrap();
                let candidate = CandidatePinV1 {
                    block_number: request.inner.number,
                    block_hash: request.hash_slow(),
                    state_root: request.inner.state_root,
                    intent_id: job.intent.intent_id(&poc_schema_limits()).unwrap(),
                    wwd: job.intent.wwd,
                    ce_sealed_root: job.intent.ce_sealed_root,
                    protocol_bundle_hash: job.intent.protocol_bundle_hash,
                    input_lease_id: job.intent.input_lease_id().unwrap(),
                };
                let generation = 12_u64;
                let record = PinRecordV1 {
                    generation,
                    state: PinStateV1::Exported {
                        candidate,
                        job_id: finalized.job_id,
                        finality_recorded_height: finalized.finality_recorded_height,
                        open_height: finalized.open_height,
                        deadline_height: finalized.deadline_height,
                        export,
                    },
                };
                let mut bytes = b"OUTBPIN1".to_vec();
                bytes.extend_from_slice(&6_u16.to_be_bytes());
                bytes.extend_from_slice(&generation.to_be_bytes());
                bytes.push(3);
                bytes.extend_from_slice(&candidate.block_number.to_be_bytes());
                bytes.extend_from_slice(candidate.block_hash.as_slice());
                bytes.extend_from_slice(candidate.state_root.as_slice());
                bytes.extend_from_slice(candidate.intent_id.as_slice());
                bytes.extend_from_slice(&candidate.wwd.to_be_bytes());
                bytes.extend_from_slice(candidate.ce_sealed_root.as_slice());
                bytes.extend_from_slice(candidate.protocol_bundle_hash.as_slice());
                bytes.extend_from_slice(candidate.input_lease_id.as_slice());
                bytes.extend_from_slice(finalized.job_id.as_slice());
                bytes.extend_from_slice(&finalized.finality_recorded_height.to_be_bytes());
                bytes.extend_from_slice(&finalized.open_height.to_be_bytes());
                bytes.extend_from_slice(&finalized.deadline_height.to_be_bytes());
                bytes.extend_from_slice(&export.source_generation.to_be_bytes());
                bytes.extend_from_slice(&export.lease_generation.to_be_bytes());
                bytes.extend_from_slice(export.manifest_hash.as_slice());
                bytes.extend_from_slice(keccak256(&bytes).as_slice());
                let mut registry = b"OUTBPIN1".to_vec();
                registry.extend_from_slice(&6_u16.to_be_bytes());
                registry.extend_from_slice(&generation.to_be_bytes());
                registry.extend_from_slice(candidate.block_hash.as_slice());
                registry.extend_from_slice(&1_u16.to_be_bytes());
                registry.extend_from_slice(candidate.block_hash.as_slice());
                registry.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_be_bytes());
                registry.extend_from_slice(&bytes);
                registry.extend_from_slice(keccak256(&registry).as_slice());
                fs::create_dir_all(root).unwrap();
                fs::write(root.join("pin.v1"), registry).unwrap();
                let decoded = inspect_retention_journal(root).unwrap();
                assert_eq!(decoded.records, vec![(candidate.block_hash, record)]);
            }

            #[test]
            fn recorded_exported_requires_complete_native_export_even_when_entire_job_directory_disappears(
            ) {
                use reth_ethereum::provider::db::{
                    database::Database, init_db, mdbx::DatabaseArguments, tables, transaction::DbTx,
                };
                for version in [1, 2] {
                    for deleted in [
                        None,
                        Some("supervisor-v1/export-bindings"),
                        Some("exporter-v1/receipts"),
                        Some("exporter-v1/input-refs"),
                    ] {
                        super::super::super::with_canonical_frontiers(
                            version,
                            |layout| {
                                write_source(layout);
                                let db = init_db(
                                    layout.chain_root.join("db"),
                                    DatabaseArguments::test(),
                                )
                                .unwrap();
                                let tx = db.tx().unwrap();
                                let request = tx
                                    .get::<tables::Headers<OutbeHeader>>(100)
                                    .unwrap()
                                    .unwrap();
                                drop(tx);
                                drop(db);
                                let prepared = fixture(&request, Phase::VotingOpen, bind_source);
                                let export = write_export(&layout.ocomp_root, &prepared);
                                // Baseline is validated through the native read-only composition
                                // before deleting any whole per-job public directory.
                                crate::snapshot::validation::ocomp::verify_export_inputs(
                                    &layout.ocomp_root,
                                    &prepared.job,
                                    Some(export),
                                    CAS_LIMITS,
                                )
                                .unwrap();
                                write_exported_pin(
                                    &layout.consensus_root.join("ocomp_retention"),
                                    &request,
                                    &prepared.job,
                                    export,
                                );
                                if let Some(prefix) = deleted {
                                    let job_id = prepared.job.finalized.as_ref().unwrap().job_id;
                                    fs::remove_dir_all(
                                        layout.ocomp_root.join(prefix).join(hex::encode(job_id)),
                                    )
                                    .unwrap();
                                }
                            },
                            |request| fixture(request, Phase::VotingOpen, bind_source).owner,
                            |state, source, layout, scratch| {
                                let result = verify_canonical_obligations(
                                    state, source, layout, scratch, None, None,
                                );
                                if let Some(deleted) = deleted {
                                    let error = result.err().expect("recorded exported obligation cannot disappear with its entire public directory");
                                    assert!(
                                        error.downcast_ref::<Incomplete>().is_some(),
                                        "{deleted}: {error:#}"
                                    );
                                    assert!(format!("{error:#}").contains("export"), "must reach required export after complete source verification: {error:#}");
                                } else {
                                    let audit = result.unwrap();
                                    assert_eq!(audit.bounds.active_intents, 1);
                                    assert_eq!(audit.pins.len(), 1);
                                    assert!(matches!(
                                        audit.pins[0].record.state,
                                        PinStateV1::Exported { .. }
                                    ));
                                    assert!(audit.pins[0].authority.export.is_some());
                                    assert_eq!(audit.active.len(), 1);
                                    assert_eq!(
                                        audit.active[0].pin_stage,
                                        CanonicalLocalPinStage::Exported
                                    );
                                    assert!(audit.active[0].source_verified);
                                    assert!(audit.active[0].export_verified);
                                    assert!(!audit.active[0].projection_before_request);
                                    assert_eq!(
                                        audit.source_leases, 1,
                                        "pin and active authority share one source lease"
                                    );
                                    assert_eq!(
                                        audit.complete_exports, 1,
                                        "pin and active authority share one export"
                                    );
                                    assert_eq!(audit.input_chunks, 1);
                                }
                            },
                        );
                    }
                }
            }
        }

        fn source_body() -> outbe_tribute::TributeData {
            let owner = alloy_primitives::Address::repeat_byte(1);
            outbe_tribute::TributeData {
                tribute_id: outbe_compressed_entities::derive_poseidon_entity_id(owner, DAY)
                    .unwrap(),
                owner,
                worldwide_day: DAY,
                issuance_amount_minor: U256::from(1000),
                issuance_currency: 840,
                nominal_amount_minor: U256::from(700),
                reference_currency: 840,
                tribute_price_minor: U256::from(2),
                exclude_from_intex_issuance: false,
            }
        }

        fn bind_source(intent: &mut JobIntentV1) {
            use outbe_compressed_entities::{
                body_commitment, encode_tribute_v1, partition_collection_key,
                tribute_partition_root_from_leaves, PartitionRef, ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
            };
            let body = source_body();
            let bytes = encode_tribute_v1(&outbe_tribute::canonical_body(&body)).unwrap();
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
                body.tribute_id,
                &bytes,
            )
            .unwrap();
            let root = tribute_partition_root_from_leaves(DAY, vec![(body.tribute_id, commitment)])
                .unwrap();
            let key = alloy_primitives::B256::from(
                *partition_collection_key(PartitionRef::TributeWwd(DAY))
                    .unwrap()
                    .1
                    .as_bytes(),
            );
            intent.sealed_tribute_collection_key = key;
            intent.sealed_tribute_collection_root = root;
            intent.authenticated_day_count = 1;
            intent.authenticated_day_nominal = body.nominal_amount_minor;
            let tribute = &mut intent.activation_preconditions.tribute;
            tribute.collection_key = key;
            tribute.sealed_collection_root = root;
            tribute.exact_count = 1;
            tribute.exact_nominal_total = body.nominal_amount_minor;
            intent
                .activation_preconditions
                .contributors
                .max_eligible_nominal_total = body.nominal_amount_minor;
        }

        fn write_source(layout: &crate::snapshot::config::RequestedLayout) {
            use outbe_offchain_storage::{
                RocksDbStorage, StorageReaderHandle, StorageWriterHandle,
            };
            let storage = std::sync::Arc::new(
                RocksDbStorage::open(&layout.projection.as_ref().unwrap().root).unwrap(),
            );
            let reader: StorageReaderHandle = storage.clone();
            let writer: StorageWriterHandle = storage;
            outbe_tribute::TributeRepositoryWriter::new(reader, writer)
                .put(&source_body())
                .unwrap();
        }

        #[test]
        fn source_complete_active_before_projection_request_passes_without_local_pin_or_export() {
            use crate::snapshot::validation::ocomp::{
                verify_canonical_obligations, CanonicalLocalPinStage,
            };
            use reth_ethereum::provider::db::{
                database::Database,
                init_db,
                mdbx::DatabaseArguments,
                models::StoredBlockBodyIndices,
                tables,
                transaction::{DbTx, DbTxMut},
            };
            for version in [1, 2] {
                let request = std::cell::RefCell::new(None);
                super::super::with_canonical_frontiers(
                    version,
                    |layout| {
                        write_source(layout);
                        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test())
                            .unwrap();
                        let tx = db.tx_mut().unwrap();
                        *request.borrow_mut() = Some(
                            tx.get::<tables::Headers<OutbeHeader>>(101)
                                .unwrap()
                                .unwrap(),
                        );
                        tx.put::<tables::BlockBodyIndices>(
                            101,
                            StoredBlockBodyIndices {
                                first_tx_num: 0,
                                tx_count: 0,
                            },
                        )
                        .unwrap();
                        tx.commit().unwrap();
                    },
                    |_| {
                        fixture(
                            request.borrow().as_ref().unwrap(),
                            Phase::AwaitingFinality,
                            bind_source,
                        )
                        .owner
                    },
                    |state, source, layout, scratch| {
                        let expected = fixture(
                            request.borrow().as_ref().unwrap(),
                            Phase::AwaitingFinality,
                            bind_source,
                        )
                        .job;
                        let audit = verify_canonical_obligations(
                            state, source, layout, scratch, None, None,
                        )
                        .unwrap();
                        assert_eq!(audit.projection.block_number, 100);
                        assert_eq!(audit.closure.checkpoint.current.block_number, 100);
                        assert_eq!(audit.bounds.active_intents, 1);
                        assert_eq!(audit.active.len(), 1);
                        let active = &audit.active[0];
                        assert_eq!(
                            active.intent_id,
                            expected.intent.intent_id(&poc_schema_limits()).unwrap()
                        );
                        assert_eq!(active.job, expected);
                        assert_eq!(active.job.intent_height, 101);
                        assert_eq!(active.job.status, OcompJobStatus::AwaitingFinality);
                        assert!(active.job.finalized.is_none());
                        assert_eq!(active.pin_stage, CanonicalLocalPinStage::Absent);
                        assert!(active.projection_before_request);
                        assert!(active.source_verified);
                        assert!(!active.export_verified);
                        assert!(audit.pins.is_empty());
                        assert_eq!(audit.source_leases, 1);
                        assert_eq!(audit.complete_exports, 0);
                        assert_eq!(audit.input_chunks, 0);
                        assert_eq!(audit.nod.jobs, 0);
                        assert_eq!(audit.payout_days, 0);
                        assert!(!layout.consensus_root.join("ocomp_retention").exists());
                        assert!(!layout.ocomp_root.join("supervisor-v1/jobs").exists());
                    },
                );
            }
        }

        #[test]
        fn later_series_or_bitmap_budget_preserves_reached_bounds_and_active_identity() {
            use crate::snapshot::validation::{
                ocomp::verify_canonical_obligations,
                report::{CheckName, CheckStatus, ValidationReport},
                Incomplete,
            };
            for version in [1, 2] {
                for with_round in [false, true] {
                    super::super::with_canonical_frontiers(
                        version,
                        |_| {},
                        |request| {
                            let mut prepared = fixture(request, Phase::AwaitingFinality, |_| {});
                            prepared
                                .owner
                                .storage
                                .extend(super::super::payout_owner(true, with_round).storage);
                            prepared.owner
                        },
                        |state, source, layout, scratch| {
                            let expected = state.live_ocomp_jobs().unwrap();
                            assert_eq!(expected.len(), 1);
                            let mut report = ValidationReport::new([CheckName::Ocomp]);
                            let error = verify_canonical_obligations(
                                state,
                                source,
                                layout,
                                scratch,
                                Some(1),
                                Some(&mut report),
                            )
                            .err()
                            .expect("later series or bitmap word exceeds the selected cap");
                            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                            let expected_diagnostic = if with_round {
                                "payout bitmap scan stopped at 1/2 words"
                            } else {
                                "Intex series scan stopped at 1/3"
                            };
                            assert!(error.to_string().contains(expected_diagnostic), "{error:#}");
                            assert_eq!(report.active_ocomp.len(), 1);
                            let active = &report.active_ocomp[0];
                            assert_eq!(active.intent_id, hex::encode(expected[0].0));
                            assert_eq!(active.job_id, None);
                            assert_eq!(active.pin_stage, "NotInspected");
                            assert_eq!(active.projection_before_request, None);
                            assert!(!active.source_verified);
                            assert!(!active.export_verified);
                            let series = report
                                .inventory_bounds
                                .iter()
                                .find(|b| b.name == "intex_series")
                                .expect("reached permanent series interval");
                            assert_eq!(
                                (series.start, series.end_exclusive, series.visited),
                                (0, 3, 1)
                            );
                            let fifo = report
                                .inventory_bounds
                                .iter()
                                .find(|b| b.name == "nod_fifo")
                                .expect("completed empty FIFO interval");
                            assert_eq!((fifo.start, fifo.end_exclusive, fifo.visited), (1, 1, 0));
                            if with_round {
                                let bitmap = report
                                    .inventory_bounds
                                    .iter()
                                    .find(|b| b.name == "payout_bitmap_words")
                                    .expect("reached contributor bitmap interval");
                                assert_eq!(
                                    (bitmap.start, bitmap.end_exclusive, bitmap.visited),
                                    (0, 2, 1)
                                );
                            }
                            assert!(report.observed.p.is_none());
                            assert!(report.observed.c_current.is_none());
                            assert_eq!(
                                report.check(CheckName::Ocomp).status,
                                CheckStatus::Incomplete
                            );
                            assert!(!report.success());
                        },
                    );
                }
            }
        }

        #[test]
        fn later_fifo_budget_preserves_discovered_active_identity_and_partial_scan_bounds() {
            use crate::snapshot::validation::{
                ocomp::verify_canonical_obligations,
                report::{CheckName, CheckStatus, ValidationReport},
                Incomplete,
            };
            for version in [1, 2] {
                super::super::with_canonical_frontiers(
                    version,
                    |_| {},
                    |request| {
                        let mut prepared = fixture(request, Phase::AwaitingFinality, |_| {});
                        // Replace only the fixture's empty NOD inventory with two native
                        // queued generations; Metadosis/Registry authority stays intact.
                        prepared.owner.storage.extend(queued_owner(2).storage);
                        prepared.owner
                    },
                    |state, source, layout, scratch| {
                        let expected = state.live_ocomp_jobs().unwrap();
                        assert_eq!(expected.len(), 1);
                        let mut report = ValidationReport::new([CheckName::Ocomp]);
                        let error = verify_canonical_obligations(
                            state,
                            source,
                            layout,
                            scratch,
                            Some(1),
                            Some(&mut report),
                        )
                        .err()
                        .expect("the second FIFO entry exceeds the selected cap");
                        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                        assert!(
                            error.to_string().contains("NOD FIFO scan stopped at 1/2"),
                            "{error:#}"
                        );
                        assert_eq!(report.active_ocomp.len(), 1, "later inventory interruption must not erase the independently discovered active intent");
                        let active = &report.active_ocomp[0];
                        assert_eq!(active.intent_id, hex::encode(expected[0].0));
                        assert_eq!(active.job_id, None);
                        assert_eq!(active.request_height, expected[0].1.intent_height);
                        assert_eq!(active.canonical_status, "AwaitingFinality");
                        assert_eq!(active.pin_stage, "NotInspected");
                        assert_eq!(active.projection_before_request, None);
                        assert!(!active.source_verified);
                        assert!(!active.export_verified);
                        let active_bounds = report
                            .inventory_bounds
                            .iter()
                            .find(|b| b.name == "active_intents")
                            .expect("completed active inventory bound");
                        assert_eq!(
                            (
                                active_bounds.start,
                                active_bounds.end_exclusive,
                                active_bounds.visited
                            ),
                            (0, 1, 1)
                        );
                        let fifo = report
                            .inventory_bounds
                            .iter()
                            .find(|b| b.name == "nod_fifo")
                            .expect("interrupted FIFO bound");
                        assert_eq!((fifo.start, fifo.end_exclusive, fifo.visited), (1, 3, 1));
                        assert!(report.observed.p.is_none());
                        assert!(report.observed.c_current.is_none());
                        assert_eq!(
                            report.check(CheckName::Ocomp).status,
                            CheckStatus::Incomplete
                        );
                        assert!(!report.success());
                    },
                );
            }
        }

        #[test]
        fn missing_projection_preserves_active_identity_with_unknown_frontier_relationship() {
            use crate::snapshot::validation::{
                ocomp::verify_canonical_obligations,
                report::{CheckName, CheckStatus, ValidationReport},
                Incomplete,
            };
            for version in [1, 2] {
                super::super::with_canonical_frontiers(
                    version,
                    |layout| {
                        std::fs::remove_dir_all(&layout.projection.as_ref().unwrap().root).unwrap()
                    },
                    |request| fixture(request, Phase::AwaitingFinality, |_| {}).owner,
                    |state, source, layout, scratch| {
                        let mut report = ValidationReport::new([CheckName::Ocomp]);
                        let error = verify_canonical_obligations(
                            state,
                            source,
                            layout,
                            scratch,
                            None,
                            Some(&mut report),
                        )
                        .err()
                        .expect("selected projection inputs are absent");
                        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                        assert!(error.to_string().contains("projection"), "{error:#}");
                        let expected = state.live_ocomp_jobs().unwrap();
                        assert_eq!(report.active_ocomp.len(), 1);
                        assert_eq!(report.active_ocomp[0].intent_id, hex::encode(expected[0].0));
                        assert_eq!(report.active_ocomp[0].pin_stage, "NotInspected");
                        assert_eq!(report.active_ocomp[0].projection_before_request, None);
                        assert!(!report.active_ocomp[0].source_verified);
                        assert!(!report.active_ocomp[0].export_verified);
                        assert!(report.observed.p.is_none());
                        assert!(report.observed.c_current.is_none());
                        assert_eq!(
                            report.check(CheckName::Ocomp).status,
                            CheckStatus::Incomplete
                        );
                        assert!(!report.success());
                        let json = serde_json::to_value(&report).unwrap();
                        assert!(json["active_ocomp"][0]["projection_before_request"].is_null());
                    },
                );
            }
        }

        #[test]
        fn active_report_preserves_identity_and_distinguishes_unverified_local_capabilities() {
            use crate::snapshot::validation::{
                ocomp::{CanonicalActiveAudit, CanonicalLocalPinStage},
                report::{ActiveOcompObservation, CheckName, ValidationReport},
            };
            with_prepared_owner_storage(
                1,
                400,
                |request| fixture(request, Phase::AwaitingFinality, |_| {}).owner,
                |state, source| {
                    let (intent_id, job) = state.live_ocomp_jobs().unwrap().pop().unwrap();
                    let request_height = job.intent_height;
                    let day = job.intent.wwd;
                    assert!(job.finalized.is_none());
                    let audit = CanonicalActiveAudit {
                        intent_id,
                        job,
                        pin_stage: CanonicalLocalPinStage::Absent,
                        projection_before_request: true,
                        source_verified: false,
                        export_verified: false,
                    };
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    report
                        .active_ocomp
                        .push(ActiveOcompObservation::from(&audit));
                    let json = serde_json::to_value(report).unwrap();
                    let observed = &json["active_ocomp"][0];
                    assert_eq!(observed["intent_id"], hex::encode(intent_id));
                    assert_eq!(observed["job_id"], serde_json::Value::Null);
                    assert_eq!(observed["request_height"], request_height);
                    assert_eq!(observed["worldwide_day"], day);
                    assert_eq!(observed["canonical_status"], "AwaitingFinality");
                    assert_eq!(observed["pin_stage"], "Absent");
                    assert_eq!(observed["projection_before_request"], true);
                    assert_eq!(observed["source_verified"], false);
                    assert_eq!(observed["export_verified"], false);
                    assert!(source.header(request_height).unwrap().is_some());
                },
            );
        }

        #[test]
        fn full_canonical_composition_finds_active_obligation_without_any_local_pin_or_job() {
            use crate::snapshot::validation::{
                ocomp::verify_canonical_obligations,
                report::{CheckName, ValidationReport},
                Incomplete,
            };
            for version in [1, 2] {
                for phase in [Phase::AwaitingFinality, Phase::VotingOpen] {
                    super::super::with_canonical_frontiers(
                        version,
                        |_| {},
                        |request| fixture(request, phase, |_| {}).owner,
                        |state, source, layout, scratch| {
                            assert!(!layout.consensus_root.join("ocomp_retention").exists());
                            assert!(!layout.ocomp_root.join("supervisor-v1/jobs").exists());
                            let mut report = ValidationReport::new([CheckName::Ocomp]);
                            let error = verify_canonical_obligations(
                                state,
                                source,
                                layout,
                                scratch,
                                None,
                                Some(&mut report),
                            )
                            .err()
                            .expect("missing body for independently found active job cannot pass");
                            assert!(
                                error.downcast_ref::<Incomplete>().is_some(),
                                "{phase:?}: {error:#}"
                            );
                            assert!(
                                format!("{error:#}").contains("Tribute"),
                                "must reach active source requirement: {error:#}"
                            );
                            let (intent_id, job) = state.live_ocomp_jobs().unwrap().pop().unwrap();
                            assert_eq!(report.active_ocomp.len(), 1);
                            let observation = &report.active_ocomp[0];
                            assert_eq!(observation.intent_id, hex::encode(intent_id));
                            assert_eq!(
                                observation.job_id,
                                job.finalized.as_ref().map(|f| hex::encode(f.job_id))
                            );
                            assert_eq!(observation.request_height, job.intent_height);
                            assert_eq!(observation.worldwide_day, job.intent.wwd);
                            assert_eq!(observation.pin_stage, "Absent");
                            assert!(!observation.source_verified);
                            assert!(!observation.export_verified);
                            assert_eq!(report.observed.p.as_ref().unwrap().number, 100);
                            assert_eq!(report.observed.c_current.as_ref().unwrap().number, 100);
                            assert!(report
                                .inventory_bounds
                                .iter()
                                .any(|b| b.name == "active_intents" && b.visited == 1));
                            assert!(!report.success());
                        },
                    );
                }
            }
        }

        #[test]
        fn canonical_active_budget_is_incomplete_before_missing_local_inputs() {
            use crate::snapshot::validation::{ocomp::verify_canonical_obligations, Incomplete};
            super::super::with_canonical_frontiers(
                1,
                |_| {},
                |request| fixture(request, Phase::VotingOpen, |_| {}).owner,
                |state, source, layout, scratch| {
                    let error =
                        verify_canonical_obligations(state, source, layout, scratch, Some(0), None)
                            .err()
                            .expect("active inventory cannot truncate to zero success");
                    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                    assert!(
                        error.to_string().contains("active intent scan requires 1"),
                        "{error:#}"
                    );
                },
            );
        }

        use super::super::{queued_owner, with_prepared_owner_storage, CanonicalInventory};
        use super::{canonical_job, DAY};
        use alloy_consensus::Sealable;
        use alloy_primitives::U256;
        use outbe_metadosis::{
            api::read_live_ocomp_jobs,
            model::{JobFsmCommand, JobFsmState},
            test_support::{seed_ready_worldwide_days_for_capacity, ForkInstallScenario},
            WwdStatus,
        };
        use outbe_ocomp_protocol::{
            intent::JobIntentV1,
            profile::{poc_schema_limits, ProtocolBundleV1},
            receipts::{
                desis_request_brief_hash, LimitSplitDestination, RequestLimitSplitReceiptV1,
            },
            state::{OcompJobRecordV1, OcompJobStatus},
        };
        use outbe_ocompregistry::{OcompProtocolAuthorityV1, OcompRegistry};
        use outbe_primitives::{
            addresses::METADOSIS_ADDRESS,
            storage::{
                hashmap::HashMapStorageProvider,
                types::{StorageBytes, StorageKey},
                StorageHandle,
            },
            OutbeHeader,
        };

        #[derive(Clone, Copy, Debug)]
        enum Phase {
            AwaitingFinality,
            VotingOpen,
        }

        struct ActiveFixture {
            owner: HashMapStorageProvider,
            job: OcompJobRecordV1,
            bundle: ProtocolBundleV1,
        }

        fn fixture(
            request: &OutbeHeader,
            phase: Phase,
            configure_input: impl FnOnce(&mut JobIntentV1),
        ) -> ActiveFixture {
            fixture_for_identity(
                request,
                phase,
                1,
                alloy_primitives::B256::repeat_byte(11),
                configure_input,
            )
        }

        // Test-only variant binding the native integration fixture to its real chain.
        fn fixture_for_identity(
            request: &OutbeHeader,
            phase: Phase,
            chain_id: u64,
            genesis_hash: alloy_primitives::B256,
            configure_input: impl FnOnce(&mut JobIntentV1),
        ) -> ActiveFixture {
            let limits = poc_schema_limits();
            let mut job = canonical_job(request, false);
            job.intent.chain_id = chain_id;
            job.intent.genesis_hash = genesis_hash;
            let install = ForkInstallScenario::final_at(
                outbe_ocompregistry::OCOMP_POC_FINAL_ACTIVATION_HEIGHT,
                job.intent.chain_id,
                job.intent.genesis_hash,
            )
            .unwrap()
            .into_install();
            let profile = &install.request_profile;
            job.intent.fork_id = profile.fork_id;
            job.intent.protocol_bundle_hash = profile.protocol_bundle_hash;
            job.intent.source_availability_policy_id = profile.source_availability_policy_id;
            job.intent_height = request.inner.number;
            job.intent.logical_evaluation_height = request.inner.number;
            job.intent.logical_evaluation_time = request.inner.timestamp;
            configure_input(&mut job.intent);
            assert_eq!(job.intent.wwd, DAY.value());

            let frozen = &job.intent.frozen_metadosis_values;
            let receipt = RequestLimitSplitReceiptV1 {
                protocol_bundle_hash: job.intent.protocol_bundle_hash,
                wwd: job.intent.wwd,
                pending_nonce: 0,
                day_type: frozen.day_type,
                day_limit: frozen.day_limit,
                lysis_limit_minor: frozen.lysis_limit_minor,
                desis_limit_minor: frozen.desis_limit_minor,
                destination: LimitSplitDestination::DesisAuction,
                desis_brief_hash: Some(
                    desis_request_brief_hash(
                        job.intent.protocol_bundle_hash,
                        job.intent.wwd,
                        frozen.desis_limit_minor,
                        &frozen.auction_entry_prices,
                        job.intent.logical_evaluation_time,
                    )
                    .unwrap(),
                ),
                carry_over_credit: U256::ZERO,
                auction_entry_prices: frozen.auction_entry_prices.clone(),
                logical_anchor: job.intent.logical_evaluation_time,
            };
            let receipt_hash = receipt.receipt_hash(&limits).unwrap();
            job.intent
                .frozen_metadosis_values
                .request_limit_split_receipt_hash = receipt_hash;
            let intent_id = job.intent.intent_id(&limits).unwrap();
            let request_deadline = job.intent_height.checked_add(64).unwrap();
            // 64 is native OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS, currently private
            // behind the owner module. Keep this constant confined to the fixture.
            let mut fsm = JobFsmState::initial_ready(DAY, job.intent_height);
            fsm.apply(JobFsmCommand::Request {
                at_height: job.intent_height,
                deadline_height: request_deadline,
                intent_id,
                lysis_limit_minor: receipt.lysis_limit_minor,
                request_limit_receipt_hash: receipt_hash,
            })
            .unwrap();
            match phase {
                Phase::AwaitingFinality => {
                    job.status = OcompJobStatus::AwaitingFinality;
                    job.finalized = None;
                }
                Phase::VotingOpen => {
                    job.status = OcompJobStatus::VotingOpen;
                    let finalized = job.finalized.as_mut().unwrap();
                    finalized.job_id = job
                        .intent
                        .job_id(request.hash_slow(), request.inner.state_root, &limits)
                        .unwrap();
                    finalized.finality_recorded_height = job.intent_height;
                    finalized.open_height = job.intent_height.checked_add(4).unwrap();
                    finalized.deadline_height = job.intent_height.checked_add(100).unwrap();
                    fsm.apply(JobFsmCommand::OpenVoting {
                        at_height: finalized.open_height,
                        deadline_height: finalized.deadline_height,
                    })
                    .unwrap();
                }
            }
            job.validate_semantics(&limits).unwrap();

            let mut owner = HashMapStorageProvider::new_with_chain_identity(
                job.intent.chain_id,
                job.intent.genesis_hash,
            );
            owner.set_block_number(install.activation_height);
            StorageHandle::enter(&mut owner, |storage| {
                OcompRegistry::new(storage.clone())
                    .initialize_genesis_authority(
                        &OcompProtocolAuthorityV1 {
                            request_profile: profile.clone(),
                            protocol_bundle: install.protocol_bundle.clone(),
                        },
                        install.install_hash(&limits).unwrap(),
                        install.activation_height,
                        install.activation_height,
                        &limits,
                    )
                    .unwrap();
                seed_ready_worldwide_days_for_capacity(storage, &[DAY]).unwrap();
            });
            // Native schema scalar mappings use DAY.mapping_slot(base+field_offset).
            // Ready aggregate membership was created by the public owner fixture.
            let status_slot = DAY.mapping_slot(U256::from(1));
            assert_eq!(
                owner.storage.get(&(METADOSIS_ADDRESS, status_slot)),
                Some(&U256::from(WwdStatus::Ready.as_u8()))
            );
            owner.storage.insert(
                (METADOSIS_ADDRESS, status_slot),
                U256::from(WwdStatus::OffchainPending.as_u8()),
            );
            for (base, value) in [
                (8_u64, receipt.day_limit),
                (9, job.intent.frozen_metadosis_values.previous_vwap),
                (10, job.intent.frozen_metadosis_values.current_vwap),
            ] {
                owner.storage.insert(
                    (METADOSIS_ADDRESS, DAY.mapping_slot(U256::from(base))),
                    value,
                );
            }

            // Exact bounded persistence of the public model's valid snapshot.
            // Current owner codec.rs: OMJS/v1, pending=2, fixed-width big endian.
            let snapshot = fsm.snapshot();
            let live = snapshot.live.unwrap();
            let mut scheduler = b"OMJS".to_vec();
            scheduler.extend_from_slice(&1_u16.to_be_bytes());
            scheduler.push(2);
            scheduler.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
            scheduler.extend_from_slice(&live.pending_nonce.to_be_bytes());
            scheduler.extend_from_slice(&0_u64.to_be_bytes());
            scheduler.extend_from_slice(live.intent_id.as_slice());
            scheduler.extend_from_slice(&live.requested_height.to_be_bytes());
            scheduler.extend_from_slice(&live.deadline_height.unwrap().to_be_bytes());
            scheduler.push(1);
            scheduler.extend_from_slice(&live.retained_effect.effect_nonce.to_be_bytes());
            scheduler
                .extend_from_slice(&live.retained_effect.lysis_limit_minor.to_be_bytes::<32>());
            scheduler.extend_from_slice(live.retained_effect.receipt_hash.as_slice());
            assert_eq!(scheduler.len(), 148);
            let mut live_index = b"OMLI".to_vec();
            live_index.extend_from_slice(&1_u16.to_be_bytes());
            live_index.extend_from_slice(&1_u16.to_be_bytes());
            live_index.extend_from_slice(&scheduler);
            StorageHandle::enter(&mut owner, |storage| {
                let write = |slot, bytes: &[u8]| {
                    StorageBytes::new(slot, METADOSIS_ADDRESS, storage.clone())
                        .write(bytes)
                        .unwrap();
                };
                write(U256::from(20), &live_index);
                write(
                    DAY.mapping_slot(U256::from(22)),
                    &receipt.encode_canonical(&limits).unwrap(),
                );
                write(DAY.mapping_slot(U256::from(25)), &scheduler);
                write(
                    outbe_ocomp_protocol::intent::intent_storage_key(intent_id)
                        .unwrap()
                        .mapping_slot(U256::from(21)),
                    &job.encode_canonical(&limits).unwrap(),
                );
                if let Some(finalized) = &job.finalized {
                    let mut response = b"OMDI".to_vec();
                    response.extend_from_slice(&1_u16.to_be_bytes());
                    response.extend_from_slice(&1_u16.to_be_bytes());
                    response.extend_from_slice(&finalized.deadline_height.to_be_bytes());
                    response.extend_from_slice(finalized.job_id.as_slice());
                    response.extend_from_slice(intent_id.as_slice());
                    write(U256::from(33), &response);
                }
                assert_eq!(
                    read_live_ocomp_jobs(storage).unwrap(),
                    vec![(intent_id, job.clone())]
                );
            });
            // Complete the independent empty NOD inventory without overriding owner words.
            for (key, value) in queued_owner(0).storage {
                assert!(owner.storage.insert(key, value).is_none());
            }
            ActiveFixture {
                owner,
                job,
                bundle: install.protocol_bundle,
            }
        }

        #[test]
        fn genuine_active_owner_is_discovered_without_any_local_job_population() {
            for version in [1, 2] {
                for phase in [Phase::AwaitingFinality, Phase::VotingOpen] {
                    with_prepared_owner_storage(
                        version,
                        400,
                        |request| {
                            let prepared = fixture(request, phase, |_| {});
                            assert_eq!(
                                prepared
                                    .bundle
                                    .protocol_bundle_hash(&poc_schema_limits())
                                    .unwrap(),
                                prepared.job.intent.protocol_bundle_hash
                            );
                            prepared.owner
                        },
                        |state, source| {
                            let request = source.header(100).unwrap().unwrap();
                            let expected = fixture(&request, phase, |_| {}).job;
                            let scratch = tempfile::tempdir().unwrap();
                            let inventory = CanonicalInventory::scan(
                                state,
                                scratch.path(),
                                &source.protected,
                                None,
                            )
                            .unwrap();
                            assert_eq!(inventory.bounds.active_intents, 1);
                            assert_eq!(
                                inventory.active_jobs(),
                                &[(
                                    expected.intent.intent_id(&poc_schema_limits()).unwrap(),
                                    expected
                                )]
                            );
                            assert_eq!(inventory.bounds.nod_entries, 0);
                            assert_eq!(inventory.bounds.unpaid_days, 0);
                        },
                    );
                }
            }
        }
    }

    mod request_locator {
        mod present_results {
            use super::super::local_result::{authority, result_for, write_result};
            use super::*;
            use crate::snapshot::{
                tests::headers::fingerprint, validation::ocomp::verify_present_local_results,
            };
            use std::{fs, path::Path};

            fn fixture(
                version: u32,
                backend: Receipts,
                case: Case,
                completed: bool,
                inspect: impl FnOnce(
                    &crate::snapshot::validation::canonical_state::CanonicalState<'_>,
                    &crate::snapshot::native::RethReadOnlyView,
                    OcompJobRecordV1,
                ),
            ) {
                with_prepared_owner_storage_setup(
                    version,
                    400,
                    |layout| setup_frame(layout, backend, case),
                    |request| {
                        let job = authority(request, completed);
                        let limits = poc_schema_limits();
                        stored_job(
                            job.intent.intent_id(&limits).unwrap(),
                            &job.encode_canonical(&limits).unwrap(),
                        )
                    },
                    |state, view| {
                        inspect(
                            state,
                            view,
                            authority(&view.header(B).unwrap().unwrap(), completed),
                        );
                    },
                );
            }

            fn inspect(
                state: &crate::snapshot::validation::canonical_state::CanonicalState<'_>,
                view: &crate::snapshot::native::RethReadOnlyView,
                root: &Path,
                maximum: Option<u64>,
            ) -> eyre::Result<(u64, u64)> {
                let before = fingerprint(root);
                let mut visited = 0;
                let result =
                    verify_present_local_results(state, view, root, maximum, &mut |job, audit| {
                        assert_eq!(job.finalized.as_ref().unwrap().job_id, audit.result.job_id);
                        visited += 1;
                        Ok(())
                    });
                assert_eq!(fingerprint(root), before);
                result.map(|audit| {
                    assert_eq!(audit.results, visited);
                    (audit.results, audit.terminal_digests)
                })
            }

            #[test]
            fn bare_results_use_exact_request_frames_without_retired_export_or_spool() {
                for version in [1, 2] {
                    for backend in [Receipts::Mdbx, Receipts::Static] {
                        for completed in [false, true] {
                            fixture(
                                version,
                                backend,
                                Case::Valid,
                                completed,
                                |state, view, job| {
                                    let root = tempfile::tempdir().unwrap();
                                    write_result(root.path(), &result_for(&job));
                                    assert_eq!(
                                        inspect(state, view, root.path(), None).unwrap(),
                                        (1, u64::from(completed))
                                    );
                                    assert!(!root.path().join("exporter-v1").exists());
                                    assert!(!root.path().join("supervisor-v1").exists());
                                },
                            );
                        }
                    }
                }
            }

            #[test]
            fn absent_optional_results_do_not_create_a_store_or_spend_budget() {
                fixture(1, Receipts::Mdbx, Case::Valid, true, |state, view, _| {
                    let root = tempfile::tempdir().unwrap();
                    assert_eq!(inspect(state, view, root.path(), Some(0)).unwrap(), (0, 0));
                    assert!(!root.path().join("node-v1").exists());
                    fs::create_dir(root.path().join("node-v1")).unwrap();
                    drop(
                        outbe_node::ocomp::local_result::LocalLysisResultStore::open(
                            root.path().join("node-v1/local-results"),
                            poc_schema_limits(),
                        )
                        .unwrap(),
                    );
                    assert_eq!(inspect(state, view, root.path(), Some(0)).unwrap(), (0, 0));
                });
            }

            #[test]
            fn result_inventory_budget_and_missing_request_receipt_are_incomplete() {
                for (case, budget) in [(Case::Valid, Some(0)), (Case::MissingReceipt, None)] {
                    fixture(2, Receipts::Mdbx, case, true, |state, view, job| {
                        let root = tempfile::tempdir().unwrap();
                        write_result(root.path(), &result_for(&job));
                        let error = inspect(state, view, root.path(), budget).unwrap_err();
                        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                    });
                }
            }

            #[test]
            fn semantically_valid_changed_result_still_fails_canonical_terminal_digest() {
                fixture(1, Receipts::Mdbx, Case::Valid, true, |state, view, job| {
                    let root = tempfile::tempdir().unwrap();
                    let mut result = result_for(&job);
                    result.input_manifest_hash = B256::repeat_byte(0xea);
                    super::super::local_result::refresh_arithmetic(&mut result);
                    write_result(root.path(), &result);
                    let error = inspect(state, view, root.path(), None).unwrap_err();
                    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
                });
            }

            #[test]
            fn every_present_result_is_examined_even_without_canonical_active_jobs() {
                fixture(1, Receipts::Mdbx, Case::Valid, true, |state, view, job| {
                    assert!(state.live_ocomp_jobs().unwrap().is_empty());
                    let root = tempfile::tempdir().unwrap();
                    write_result(root.path(), &result_for(&job));
                    let mut foreign = result_for(&job);
                    foreign.job_id = B256::repeat_byte(0xed);
                    super::super::local_result::refresh_arithmetic(&mut foreign);
                    write_result(root.path(), &foreign);
                    let error = inspect(state, view, root.path(), None).unwrap_err();
                    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
                });
            }
        }

        use super::super::with_prepared_owner_storage_setup;
        use super::{canonical_job, stored_job, DAY};
        use crate::snapshot::{
            config::NativeLayout,
            validation::{ocomp::locate_request_job, Incomplete},
        };
        use alloy_consensus::{Sealable, SignableTransaction, TxLegacy};
        use alloy_primitives::{Address, Log, LogData, Signature, B256, U256};
        use alloy_sol_types::SolEvent;
        use outbe_metadosis::precompile::IMetadosis;
        use outbe_ocomp_protocol::{profile::poc_schema_limits, state::OcompJobRecordV1};
        use outbe_primitives::{
            addresses::METADOSIS_ADDRESS, time::WorldwideDay, OutbeHeader, OutbePrimitives,
            OutbeReceipt, OutbeTxEnvelope,
        };
        use reth_ethereum::provider::db::{
            database::Database,
            init_db,
            mdbx::DatabaseArguments,
            models::StoredBlockBodyIndices,
            tables,
            transaction::{DbTx, DbTxMut},
        };
        use reth_provider::{
            providers::StaticFileProviderBuilder, StaticFileSegment, StaticFileWriter,
        };

        const B: u64 = 100;

        #[derive(Clone, Copy, Debug)]
        enum Receipts {
            Mdbx,
            Static,
        }

        #[derive(Clone, Copy, Debug)]
        enum Case {
            Valid,
            IgnoredDecoys,
            NoEvent,
            ForeignAddressOnly,
            ForeignTopicOnly,
            FailedReceiptOnly,
            Duplicate,
            DuplicateForeignDay,
            Malformed,
            WrongIntent,
            WrongDay,
            WrongAttempt,
            WrongNonce,
            WrongActivation,
            WrongFrozenHeight,
            WrongFrozenTime,
            WrongExpectedJob,
            WrongExpectedDay,
            MissingIndex,
            MissingTransaction,
            MissingReceipt,
            MissingSelectedFrame,
        }

        fn job_for(request: &OutbeHeader, case: Case) -> OcompJobRecordV1 {
            let mut job = canonical_job(request, true);
            match case {
                Case::WrongFrozenHeight => job.intent.logical_evaluation_height -= 1,
                Case::WrongFrozenTime => job.intent.logical_evaluation_time -= 1,
                _ => {}
            }
            // Rebind every identity after intentional frozen-field changes. These
            // fixtures remain native-codec-valid and fail only at locator authority.
            let limits = poc_schema_limits();
            let intent_id = job.intent.intent_id(&limits).unwrap();
            let job_id = job
                .intent
                .job_id(request.hash_slow(), request.inner.state_root, &limits)
                .unwrap();
            job.finalized.as_mut().unwrap().job_id = job_id;
            let binding = job
                .terminal
                .as_mut()
                .unwrap()
                .completed_binding
                .as_mut()
                .unwrap();
            binding.job_id = job_id;
            binding.terminal_receipt.binding.intent_id = intent_id;
            binding.terminal_receipt.binding.job_id = job_id;
            binding.terminal_receipt_hash = binding
                .terminal_receipt
                .terminal_receipt_hash(&limits)
                .unwrap();
            job.validate_semantics(&limits).unwrap();
            job
        }

        fn request_event(job: &OcompJobRecordV1) -> IMetadosis::OffchainJobRequested {
            let limits = poc_schema_limits();
            IMetadosis::OffchainJobRequested {
                intentId: job.intent.intent_id(&limits).unwrap(),
                wwd: job.intent.wwd,
                pendingNonce: job.intent.pending_nonce,
                attempt: job.intent.attempt,
                activationPreconditionsHash: job
                    .intent
                    .activation_preconditions
                    .activation_preconditions_hash(&limits)
                    .unwrap(),
            }
        }

        fn setup_frame(layout: &NativeLayout, backend: Receipts, case: Case) {
            let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            let mut request = tx.get::<tables::Headers<OutbeHeader>>(B).unwrap().unwrap();
            request.inner.timestamp = 1_000;
            // IntentId does not contain B's hash. Construct the event first, then
            // finalize receipt/transaction roots and only then derive final JobId.
            let job = job_for(&request, case);
            let mut event = request_event(&job);
            match case {
                Case::WrongIntent => event.intentId = B256::repeat_byte(0xee),
                Case::WrongDay => event.wwd += 1,
                Case::WrongAttempt => event.attempt += 1,
                Case::WrongNonce => event.pendingNonce += 1,
                Case::WrongActivation => {
                    event.activationPreconditionsHash = B256::repeat_byte(0xdd)
                }
                _ => {}
            }
            let native_log = Log {
                address: METADOSIS_ADDRESS,
                data: event.encode_log_data(),
            };
            let mut receipts = vec![
                OutbeReceipt {
                    success: true,
                    cumulative_gas_used: 21_000,
                    logs: vec![native_log.clone()],
                    ..Default::default()
                },
                OutbeReceipt {
                    success: true,
                    cumulative_gas_used: 42_000,
                    ..Default::default()
                },
            ];
            match case {
                Case::NoEvent => receipts[0].logs.clear(),
                Case::ForeignAddressOnly => {
                    receipts[0].logs[0].address = Address::repeat_byte(0xee)
                }
                Case::ForeignTopicOnly => {
                    receipts[0].logs[0].data = LogData::new_unchecked(
                        vec![B256::repeat_byte(0xee)],
                        Vec::<u8>::new().into(),
                    )
                }
                Case::FailedReceiptOnly => receipts[0].success = false,
                Case::Duplicate => receipts[1].logs.push(native_log.clone()),
                Case::DuplicateForeignDay => {
                    let mut foreign = request_event(&job);
                    foreign.wwd += 1;
                    receipts[1].logs.push(Log {
                        address: METADOSIS_ADDRESS,
                        data: foreign.encode_log_data(),
                    });
                }
                Case::Malformed => {
                    receipts[0].logs[0].data = LogData::new_unchecked(
                        vec![IMetadosis::OffchainJobRequested::SIGNATURE_HASH],
                        Vec::<u8>::new().into(),
                    )
                }
                Case::IgnoredDecoys => {
                    let mut foreign_address = native_log.clone();
                    foreign_address.address = Address::repeat_byte(0xee);
                    receipts[0].logs.push(foreign_address);
                    receipts[0].logs.push(Log {
                        address: METADOSIS_ADDRESS,
                        data: LogData::new_unchecked(
                            vec![B256::repeat_byte(0xee)],
                            Vec::<u8>::new().into(),
                        ),
                    });
                    receipts[1].success = false;
                    receipts[1].logs.push(native_log);
                }
                _ => {}
            }
            let transactions: Vec<OutbeTxEnvelope> = (0..2)
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
            request.inner.gas_limit = 30_000_000;
            request.inner.gas_used = 42_000;
            request.inner.transactions_root =
                alloy_consensus::proofs::calculate_transaction_root(&transactions);
            request.inner.receipts_root = reth_ethereum::calculate_receipt_root_no_memo(&receipts);
            let request_hash = request.hash_slow();
            tx.put::<tables::Headers<OutbeHeader>>(B, request).unwrap();
            tx.put::<tables::CanonicalHeaders>(B, request_hash).unwrap();
            // Preserve the preexisting B+1 header linkage used by retained headers.
            let mut next = tx
                .get::<tables::Headers<OutbeHeader>>(B + 1)
                .unwrap()
                .unwrap();
            next.inner.parent_hash = request_hash;
            tx.put::<tables::CanonicalHeaders>(B + 1, next.hash_slow())
                .unwrap();
            tx.put::<tables::Headers<OutbeHeader>>(B + 1, next).unwrap();
            if !matches!(case, Case::MissingIndex) {
                tx.put::<tables::BlockBodyIndices>(
                    B,
                    StoredBlockBodyIndices {
                        first_tx_num: 0,
                        tx_count: 2,
                    },
                )
                .unwrap();
            }
            if matches!(backend, Receipts::Mdbx) {
                for (ordinal, receipt) in receipts.iter().enumerate() {
                    if ordinal == 1 && matches!(case, Case::MissingReceipt) {
                        continue;
                    }
                    tx.put::<tables::Receipts<OutbeReceipt>>(ordinal as u64, receipt.clone())
                        .unwrap();
                }
            }
            tx.commit().unwrap();
            drop(db);

            let files = StaticFileProviderBuilder::read_write(&layout.static_files_root)
                .with_blocks_per_file(1_000)
                .build::<OutbePrimitives>()
                .unwrap();
            {
                let mut writer = files
                    .get_writer(0, StaticFileSegment::Transactions)
                    .unwrap();
                for height in 0..=B {
                    writer.increment_block(height).unwrap();
                }
                for (ordinal, transaction) in transactions.iter().enumerate() {
                    if ordinal == 1 && matches!(case, Case::MissingTransaction) {
                        continue;
                    }
                    writer
                        .append_transaction(ordinal as u64, transaction)
                        .unwrap();
                }
            }
            if matches!(backend, Receipts::Static) {
                let mut writer = files.get_writer(0, StaticFileSegment::Receipts).unwrap();
                for height in 0..=B {
                    writer.increment_block(height).unwrap();
                }
                for (ordinal, receipt) in receipts.iter().enumerate() {
                    if ordinal == 1 && matches!(case, Case::MissingReceipt) {
                        continue;
                    }
                    writer.append_receipt(ordinal as u64, receipt).unwrap();
                }
            }
            files.commit().unwrap();
        }

        fn check(
            version: u32,
            backend: Receipts,
            case: Case,
            maximum_transactions: Option<u64>,
            inspect: impl FnOnce(eyre::Result<OcompJobRecordV1>, OcompJobRecordV1),
        ) {
            with_prepared_owner_storage_setup(
                version,
                400,
                |layout| setup_frame(layout, backend, case),
                |request| {
                    let job = job_for(request, case);
                    let limits = poc_schema_limits();
                    stored_job(
                        job.intent.intent_id(&limits).unwrap(),
                        &job.encode_canonical(&limits).unwrap(),
                    )
                },
                |state, view| {
                    let request = view.header(B).unwrap().unwrap();
                    let job = job_for(&request, case);
                    let expected_job = if matches!(case, Case::WrongExpectedJob) {
                        B256::repeat_byte(0xcc)
                    } else {
                        job.finalized.as_ref().unwrap().job_id
                    };
                    let expected_day = if matches!(case, Case::WrongExpectedDay) {
                        WorldwideDay::new(DAY.value() + 1)
                    } else {
                        DAY
                    };
                    let height = if matches!(case, Case::MissingSelectedFrame) {
                        B - 1
                    } else {
                        B
                    };
                    inspect(
                        locate_request_job(
                            state,
                            view,
                            height,
                            expected_job,
                            expected_day,
                            maximum_transactions,
                        ),
                        job,
                    );
                },
            );
            // The shared helper fingerprints MDBX, static files, config and keys
            // before/after each check, including every error path above.
        }

        #[test]
        fn exact_request_frame_locates_retired_completed_job_in_both_storage_layouts() {
            for version in [1, 2] {
                for backend in [Receipts::Mdbx, Receipts::Static] {
                    check(
                        version,
                        backend,
                        Case::Valid,
                        Some(2),
                        |result, expected| {
                            assert_eq!(result.unwrap(), expected);
                        },
                    );
                }
            }
        }

        #[test]
        fn request_locator_ignores_failed_receipts_foreign_addresses_and_foreign_topics() {
            for version in [1, 2] {
                check(
                    version,
                    Receipts::Static,
                    Case::IgnoredDecoys,
                    None,
                    |result, expected| {
                        assert_eq!(result.unwrap(), expected);
                    },
                );
            }
        }

        #[test]
        fn complete_frame_without_one_unique_decodable_native_request_is_failed() {
            for case in [
                Case::NoEvent,
                Case::ForeignAddressOnly,
                Case::ForeignTopicOnly,
                Case::FailedReceiptOnly,
                Case::Duplicate,
                Case::DuplicateForeignDay,
                Case::Malformed,
            ] {
                check(1, Receipts::Mdbx, case, None, |result, _| {
                    let error = result.expect_err("complete contradictory request frame must fail");
                    assert!(
                        error.downcast_ref::<Incomplete>().is_none(),
                        "{case:?}: {error:#}"
                    );
                });
            }
        }

        #[test]
        fn request_event_and_frozen_identity_must_match_current_execution_authority() {
            for case in [
                Case::WrongIntent,
                Case::WrongDay,
                Case::WrongAttempt,
                Case::WrongNonce,
                Case::WrongActivation,
                Case::WrongFrozenHeight,
                Case::WrongFrozenTime,
                Case::WrongExpectedJob,
                Case::WrongExpectedDay,
            ] {
                for version in [1, 2] {
                    check(version, Receipts::Mdbx, case, None, |result, _| {
                        let error = result.expect_err("foreign request authority must fail");
                        assert!(
                            error.downcast_ref::<Incomplete>().is_none(),
                            "{case:?}: {error:#}"
                        );
                    });
                }
            }
        }

        #[test]
        fn missing_selected_frame_or_native_transaction_receipt_evidence_is_incomplete() {
            for backend in [Receipts::Mdbx, Receipts::Static] {
                for case in [
                    Case::MissingSelectedFrame,
                    Case::MissingIndex,
                    Case::MissingTransaction,
                    Case::MissingReceipt,
                ] {
                    check(2, backend, case, None, |result, _| {
                        let error =
                            result.expect_err("missing required frame evidence must be incomplete");
                        assert!(
                            error.downcast_ref::<Incomplete>().is_some(),
                            "{backend:?} {case:?}: {error:#}"
                        );
                    });
                }
            }
        }

        #[test]
        fn request_locator_does_not_accept_first_event_before_finishing_bounded_frame() {
            for maximum in [0, 1] {
                check(
                    1,
                    Receipts::Mdbx,
                    Case::Valid,
                    Some(maximum),
                    |result, _| {
                        let error = result
                            .expect_err("request found before budget exhaustion is not complete");
                        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                    },
                );
            }
            check(
                1,
                Receipts::Mdbx,
                Case::Valid,
                Some(2),
                |result, expected| {
                    assert_eq!(result.unwrap(), expected);
                },
            );
        }
    }

    mod local_result {
        use super::super::with_prepared_owner_storage;
        use super::{canonical_job, stored_job};
        use crate::{
            snapshot::{tests::headers::fingerprint, validation::ocomp::verify_local_result},
            OutbeHeader,
        };
        use alloy_primitives::{B256, U256};
        use outbe_node::ocomp::local_result::LocalLysisResultStore;
        use outbe_ocomp_protocol::{
            hash::hash_framed,
            profile::poc_schema_limits,
            registry::HashDomain,
            result::{
                lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
                CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisResultV1,
                MetadosisCompletionSummaryV1, ResultRootsV1,
            },
            state::{OcompJobRecordV1, OcompJobStatus},
        };
        use outbe_primitives::time::WorldwideDay;
        use std::{
            fs,
            path::{Path, PathBuf},
        };

        pub(super) fn refresh_arithmetic(result: &mut LysisResultV1) {
            result.arithmetic_commitment = hash_framed(
                HashDomain::LysisArithmetic,
                &result
                    .arithmetic_summary()
                    .encode_canonical(&poc_schema_limits())
                    .unwrap(),
            )
            .unwrap();
            result.encode_canonical(&poc_schema_limits()).unwrap();
        }

        // Compact native-result fixture, bound to the actual canonical JobIntent
        // and B-derived JobId. This proves stored evidence, not worker execution.
        pub(super) fn result_for(job: &OcompJobRecordV1) -> LysisResultV1 {
            let intent = &job.intent;
            let frozen = &intent.frozen_metadosis_values;
            let unused = frozen.lysis_limit_minor;
            let conservation = ConservationTotalsV1 {
                tribute_nominal_total: intent.authenticated_day_nominal,
                eligible_nominal_total: U256::ZERO,
                day_limit: frozen.day_limit,
                gratis_demand: frozen.gratis_demand,
                day_gratis_limit_minor: frozen.day_gratis_limit_minor,
                lysis_limit_minor: frozen.lysis_limit_minor,
                desis_limit_minor: frozen.desis_limit_minor,
                lysis_allocation_minor: U256::ZERO,
                unused_lysis_limit_minor: unused,
                carry_over_credit: unused,
                nod_cost_total: U256::ZERO,
            };
            let mut result = LysisResultV1 {
                protocol_bundle_hash: intent.protocol_bundle_hash,
                job_id: job.finalized.as_ref().unwrap().job_id,
                attempt: intent.attempt,
                input_manifest_hash: B256::repeat_byte(0x35),
                plan_hash: B256::repeat_byte(0x36),
                unit_artifact_root: B256::repeat_byte(0x37),
                fidelity_fraction_root: B256::repeat_byte(0x38),
                gratis_prefix_root: B256::repeat_byte(0x39),
                result_chunk_count: 1,
                result_chunk_list_root: B256::repeat_byte(0x3a),
                carry_over_credit: CarryOverCreditActionV1 {
                    source_wwd: intent.wwd,
                    reason: CarryOverReason::UnusedLysis,
                    amount: unused,
                },
                metadosis_completion_summary: MetadosisCompletionSummaryV1 {
                    wwd: intent.wwd,
                    pending_nonce: intent.pending_nonce,
                    day_type: frozen.day_type,
                    tribute_nominal_total: intent.authenticated_day_nominal,
                    day_limit: frozen.day_limit,
                    gratis_demand: frozen.gratis_demand,
                    day_gratis_limit_minor: frozen.day_gratis_limit_minor,
                    lysis_limit_minor: frozen.lysis_limit_minor,
                    desis_limit_minor: frozen.desis_limit_minor,
                    lysis_allocation_minor: U256::ZERO,
                    unused_lysis_limit_minor: unused,
                    carry_over_credit: unused,
                    status: CompletionStatus::Completed,
                    logical_evaluation_height: intent.logical_evaluation_height,
                    logical_evaluation_time: intent.logical_evaluation_time,
                },
                tribute_count: intent.authenticated_day_count,
                tribute_nominal_total: intent.authenticated_day_nominal,
                unused_lysis_limit_minor: unused,
                roots: ResultRootsV1 {
                    nod_root: B256::repeat_byte(0x31),
                    bucket_root: B256::repeat_byte(0x32),
                    contributor_root: B256::repeat_byte(0x33),
                    output_manifest_root: B256::repeat_byte(0x34),
                },
                counts: ExactCountsV1 {
                    tribute_count: intent.authenticated_day_count,
                    nod_count: intent.authenticated_day_count,
                    bucket_count: 0,
                    contributor_count: 0,
                    semantic_event_count: 0,
                },
                conservation,
                arithmetic_commitment: B256::ZERO,
                event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
            };
            refresh_arithmetic(&mut result);
            result.validate_finalized_intent(intent).unwrap();
            result
        }

        pub(super) fn authority(request: &OutbeHeader, completed: bool) -> OcompJobRecordV1 {
            let limits = poc_schema_limits();
            let mut job = canonical_job(request, completed);
            if completed {
                let result = result_for(&job);
                let digest = result.result_digest(&limits).unwrap();
                job.finalized
                    .as_mut()
                    .unwrap()
                    .quorum
                    .as_mut()
                    .unwrap()
                    .result_digest = digest;
                let binding = job
                    .terminal
                    .as_mut()
                    .unwrap()
                    .completed_binding
                    .as_mut()
                    .unwrap();
                binding.result_digest = digest;
                binding.result_evidence_hash = result.result_evidence_hash(&limits).unwrap();
                binding.terminal_receipt.binding.result_digest = digest;
                binding.terminal_receipt.event_summary_hash = result.event_summary_hash;
                binding.terminal_receipt_hash = binding
                    .terminal_receipt
                    .terminal_receipt_hash(&limits)
                    .unwrap();
            } else {
                job.status = OcompJobStatus::VotingOpen;
            }
            job.validate_semantics(&limits).unwrap();
            job
        }

        fn with_authority(version: u32, completed: bool, check: impl FnOnce(OcompJobRecordV1)) {
            with_prepared_owner_storage(
                version,
                400,
                |request| {
                    let job = authority(request, completed);
                    stored_job(
                        job.intent.intent_id(&poc_schema_limits()).unwrap(),
                        &job.encode_canonical(&poc_schema_limits()).unwrap(),
                    )
                },
                |state, view| {
                    let expected = authority(&view.header(100).unwrap().unwrap(), completed);
                    let actual = state
                        .metadosis_job(
                            expected.intent.intent_id(&poc_schema_limits()).unwrap(),
                            WorldwideDay::new(expected.intent.wwd),
                            Some(expected.finalized.as_ref().unwrap().job_id),
                        )
                        .unwrap();
                    assert_eq!(actual, expected);
                    check(actual);
                },
            );
        }

        fn result_root(root: &Path) -> PathBuf {
            root.join("node-v1/local-results")
        }

        pub(super) fn write_result(root: &Path, result: &LysisResultV1) -> PathBuf {
            fs::create_dir_all(root.join("node-v1")).unwrap();
            let directory = result_root(root);
            let writer = LocalLysisResultStore::open(&directory, poc_schema_limits()).unwrap();
            writer
                .commit(
                    result.job_id,
                    &result.encode_canonical(&poc_schema_limits()).unwrap(),
                )
                .unwrap();
            drop(writer);
            directory.join(format!(
                "{}.lysis-result-v1.ocb1",
                hex::encode(result.job_id)
            ))
        }

        fn inspect(
            root: &Path,
            job: &OcompJobRecordV1,
        ) -> eyre::Result<Option<(LysisResultV1, bool)>> {
            let before = fingerprint(root);
            let result = verify_local_result(root, job).map(|observation| {
                observation.map(|audit| (audit.result, audit.terminal_digest_checked))
            });
            assert_eq!(
                fingerprint(root),
                before,
                "local result inspection mutated source"
            );
            result
        }

        #[test]
        fn voting_open_accepts_native_local_result_without_terminal_or_old_intermediates() {
            for version in [1, 2] {
                with_authority(version, false, |job| {
                    assert_eq!(job.status, OcompJobStatus::VotingOpen);
                    assert!(job.terminal.is_none());
                    let public = tempfile::tempdir().unwrap();
                    let result = result_for(&job);
                    write_result(public.path(), &result);
                    assert_eq!(inspect(public.path(), &job).unwrap(), Some((result, false)));
                    assert!(!public.path().join("exporter-v1").exists());
                    assert!(!public.path().join("supervisor-v1").exists());
                });
            }
        }

        #[test]
        fn completed_binding_compares_exact_native_digest_without_plan_or_admissions() {
            for version in [1, 2] {
                with_authority(version, true, |job| {
                    let public = tempfile::tempdir().unwrap();
                    let result = result_for(&job);
                    write_result(public.path(), &result);
                    assert_eq!(inspect(public.path(), &job).unwrap(), Some((result, true)));
                    assert!(!public.path().join("exporter-v1").exists());
                    assert!(!public.path().join("supervisor-v1").exists());
                });
            }
        }

        #[test]
        fn absent_optional_root_or_job_is_none_even_after_canonical_completion() {
            for completed in [false, true] {
                with_authority(1, completed, |job| {
                    let public = tempfile::tempdir().unwrap();
                    assert_eq!(inspect(public.path(), &job).unwrap(), None);
                    assert!(!public.path().join("node-v1").exists());
                    fs::create_dir(public.path().join("node-v1")).unwrap();
                    let writer = LocalLysisResultStore::open(
                        result_root(public.path()),
                        poc_schema_limits(),
                    )
                    .unwrap();
                    drop(writer);
                    assert_eq!(inspect(public.path(), &job).unwrap(), None);
                    // A valid result for a different job does not fabricate this
                    // job's missing result, and is not a filename corruption.
                    let mut unrelated = result_for(&job);
                    unrelated.job_id = B256::repeat_byte(0x91);
                    refresh_arithmetic(&mut unrelated);
                    write_result(public.path(), &unrelated);
                    assert_eq!(inspect(public.path(), &job).unwrap(), None);
                });
            }
        }

        #[test]
        fn semantically_valid_local_result_with_different_terminal_digest_fails() {
            with_authority(1, true, |job| {
                let public = tempfile::tempdir().unwrap();
                let mut changed = result_for(&job);
                changed.input_manifest_hash = B256::repeat_byte(0x92);
                refresh_arithmetic(&mut changed);
                changed.validate_finalized_intent(&job.intent).unwrap();
                assert_ne!(
                    changed.result_digest(&poc_schema_limits()).unwrap(),
                    job.terminal
                        .as_ref()
                        .unwrap()
                        .completed_binding
                        .as_ref()
                        .unwrap()
                        .result_digest
                );
                write_result(public.path(), &changed);
                assert!(inspect(public.path(), &job).is_err());
            });
        }

        #[test]
        fn matching_job_id_does_not_authorize_foreign_intent_fields() {
            with_authority(1, false, |job| {
                for field in 0..5 {
                    let public = tempfile::tempdir().unwrap();
                    let mut changed = result_for(&job);
                    match field {
                        0 => changed.protocol_bundle_hash = B256::repeat_byte(0x93),
                        1 => changed.attempt += 1,
                        2 => changed.metadosis_completion_summary.pending_nonce += 1,
                        3 => changed.metadosis_completion_summary.logical_evaluation_time += 1,
                        _ => {
                            changed.metadosis_completion_summary.wwd += 1;
                            changed.carry_over_credit.source_wwd += 1;
                        }
                    }
                    refresh_arithmetic(&mut changed);
                    assert!(changed.validate_finalized_intent(&job.intent).is_err());
                    write_result(public.path(), &changed);
                    assert!(
                        inspect(public.path(), &job).is_err(),
                        "accepted foreign field {field}"
                    );
                }
            });
        }

        #[test]
        fn pending_publication_corrupt_bytes_and_foreign_filename_are_not_repaired() {
            with_authority(1, false, |job| {
                for damage in ["pending", "linked-pending", "foreign", "corrupt"] {
                    let public = tempfile::tempdir().unwrap();
                    let result = result_for(&job);
                    let path = write_result(public.path(), &result);
                    let pending = result_root(public.path())
                        .join(format!(".{}.pending", hex::encode(result.job_id)));
                    match damage {
                        "pending" => fs::rename(&path, &pending).unwrap(),
                        "linked-pending" => fs::hard_link(&path, &pending).unwrap(),
                        "foreign" => {
                            let foreign = result_root(public.path()).join(format!(
                                "{}.lysis-result-v1.ocb1",
                                hex::encode(B256::repeat_byte(0x94))
                            ));
                            fs::rename(&path, foreign).unwrap();
                        }
                        _ => fs::write(&path, b"invalid canonical result").unwrap(),
                    }
                    assert!(inspect(public.path(), &job).is_err(), "accepted {damage}");
                    if damage == "pending" || damage == "linked-pending" {
                        assert!(pending.exists());
                    }
                }
            });
        }
    }

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
            logical_evaluation_height: request.inner.number,
            logical_evaluation_time: request.inner.timestamp,
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
            finality_recorded_height: request.inner.number,
            open_height: request.inner.number + 4,
            deadline_height: request.inner.number + 100,
            quorum: None,
        };
        let terminal = if completed {
            let quorum = OcompQuorumV1 {
                member_count: 4,
                quorum_threshold: 3,
                result_digest: hash,
                quorum_height: request.inner.number + 5,
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
                activated_at_height: request.inner.number + 5,
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
                terminal_height: request.inner.number + 5,
                terminal_time: 1_005,
                completed_binding: Some(binding),
            })
        } else {
            None
        };
        let record = OcompJobRecordV1 {
            intent,
            intent_height: request.inner.number,
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

mod export_inventory {
    mod receipt_only {
        use super::*;
        use crate::snapshot::validation::ocomp::verify_present_receipt;
        use outbe_ocomp::export_receipt::VerifiedExportReceipt;

        fn check(
            f: &Fixture,
            job: &OcompJobRecordV1,
            expected: Option<ExportAuthorityV1>,
        ) -> eyre::Result<VerifiedExportReceipt> {
            let before = fingerprint(f.directory.path());
            let result = verify_present_receipt(f.directory.path(), job, expected, CAS_LIMITS);
            assert_eq!(fingerprint(f.directory.path()), before);
            result
        }

        fn remove_historical_siblings(f: &Fixture) {
            fs::remove_dir_all(&f.binding_root).unwrap();
            fs::remove_dir_all(&f.catalog_root).unwrap();
            // A receipt is not proof of surviving input chunks or binding CAS.
            fs::remove_file(f.cas_path(&f.binding_ref)).unwrap();
            fs::remove_file(f.cas_path(&f.chunk_ref)).unwrap();
        }

        #[test]
        fn receipt_survives_pruned_binding_catalog_and_their_cas_objects() {
            let f = fixture(20, None);
            remove_historical_siblings(&f);
            for expected in [None, Some(f.expected())] {
                let receipt = check(&f, &f.job, expected).unwrap();
                assert_eq!(receipt.receipt_ref(), f.receipt_ref);
                assert_eq!(receipt.manifest_ref(), f.manifest_ref);
                assert_eq!(receipt.manifest_hash(), f.manifest_hash);
                assert_eq!(receipt.committed(), f.committed);
                assert!(!f.binding_root.exists());
                assert!(!f.catalog_root.exists());
            }
            // The weaker receipt observation cannot satisfy a complete-export obligation.
            assert!(f
                .check(Some(f.expected()))
                .unwrap_err()
                .downcast_ref::<Incomplete>()
                .is_some());
        }

        #[test]
        fn canonical_manifest_fields_and_request_checkpoint_cannot_be_substituted() {
            for field in [
                "bundle",
                "attempt",
                "day",
                "collection_key",
                "collection_root",
                "count",
                "nominal",
                "ce_root",
                "height",
                "block_hash",
                "state_root",
            ] {
                let f = fixture(20, None);
                remove_historical_siblings(&f);
                let mut job = f.job.clone();
                match field {
                    "bundle" => job.intent.protocol_bundle_hash = hash(0xee),
                    "attempt" => job.intent.attempt += 1,
                    "day" => job.intent.wwd += 1,
                    "collection_key" => job.intent.sealed_tribute_collection_key = hash(0xee),
                    "collection_root" => job.intent.sealed_tribute_collection_root = hash(0xee),
                    "count" => job.intent.authenticated_day_count += 1,
                    "nominal" => job.intent.authenticated_day_nominal += U256::from(1),
                    "ce_root" => job.intent.ce_sealed_root = hash(0xee),
                    "height" => job.intent_height += 1,
                    "block_hash" => {
                        job.finalized.as_mut().unwrap().finalized_request_block_hash = hash(0xee)
                    }
                    "state_root" => {
                        job.finalized.as_mut().unwrap().finalized_request_state_root = hash(0xee)
                    }
                    _ => unreachable!(),
                }
                let error = check(&f, &job, None).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{field}: {error:#}"
                );
            }
        }

        #[test]
        fn native_consistent_receipt_must_match_canonical_checkpoint_height_and_ce_schema() {
            for damage in ["checkpoint_height", "checkpoint_schema"] {
                let f = fixture(20, Some(damage));
                remove_historical_siblings(&f);
                let error = check(&f, &f.job, None).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{damage}: {error:#}"
                );
            }
        }

        #[test]
        fn optional_saved_export_authority_binds_source_lease_and_manifest() {
            let f = fixture(20, None);
            remove_historical_siblings(&f);
            let expected = f.expected();
            for changed in [
                ExportAuthorityV1 {
                    source_generation: 12,
                    ..expected
                },
                ExportAuthorityV1 {
                    lease_generation: 18,
                    ..expected
                },
                ExportAuthorityV1 {
                    manifest_hash: hash(0xee),
                    ..expected
                },
            ] {
                let error = check(&f, &f.job, Some(changed)).unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            }
            check(&f, &f.job, Some(expected)).unwrap();
        }

        #[test]
        fn complete_receipt_still_requires_its_own_prepared_manifest_and_cas_evidence() {
            for missing in [
                "receipt_locator",
                "prepared_locator",
                "receipt_cas",
                "manifest_cas",
            ] {
                let f = fixture(20, None);
                remove_historical_siblings(&f);
                let path = match missing {
                    "receipt_locator" => f.receipt_root.join("receipt.ref"),
                    "prepared_locator" => f.receipt_root.join("prepared.ref"),
                    "receipt_cas" => f.cas_path(&f.receipt_ref),
                    "manifest_cas" => f.cas_path(&f.manifest_ref),
                    _ => unreachable!(),
                };
                fs::remove_file(&path).unwrap();
                let error = check(&f, &f.job, None).unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_some(),
                    "{missing}: {error:#}"
                );
                assert!(!path.exists());
            }
        }

        #[test]
        fn malformed_receipt_cas_fails_without_requiring_pruned_siblings() {
            let f = fixture(20, None);
            remove_historical_siblings(&f);
            let path = f.cas_path(&f.receipt_ref);
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(path, bytes).unwrap();
            let error = check(&f, &f.job, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        }
    }
    use crate::snapshot::{
        tests::headers::fingerprint,
        validation::{ocomp::verify_export_inputs, Incomplete},
    };
    use alloy_primitives::{Address, B256, U256};
    use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
    use outbe_node::ocomp::retention::ExportAuthorityV1;
    use outbe_ocomp::{
        cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
        control::poc_schema_limits,
        export_binding::{ExportBindingCandidate, ExportedManifestBindingStore},
        export_receipt::{ExportReceiptCandidate, ExportReceiptStore},
        input_artifacts::derive_input_chunk_ref,
        input_ref_catalog::VerifiedInputChunkRefCatalog,
        supervisor::DiscoveryRecord,
    };
    use outbe_ocomp_protocol::{
        common::BoundedBytes,
        control::{FinalizedJobSpecV1, FinalizedJobSummaryV1, SnapshotHandoffV1},
        input::{
            AuthenticatedInputChunkV1, CheckpointIdentityV1, Compression, InputChunkKind,
            InputManifestV1,
        },
        intent::{
            ActivationPreconditionsV1, AuctionEntryPriceSource, ContributorTargetPreconditionV1,
            DayType, FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
            MetadosisExpectedStatus, NodTargetPreconditionV1, ReferenceEntryPriceV1,
            TributeInputBindingV1,
        },
        profile::ProtocolBundleV1,
        registry::{FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID},
        state::{OcompFinalizedJobV1, OcompJobRecordV1, OcompJobStatus},
        CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits, SnapshotExportCommittedV1,
    };
    use outbe_primitives::time::WorldwideDay;
    use std::{fs, path::PathBuf};
    const CAS_LIMITS: CasLimits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: 8_388_608,
    };
    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(if byte == 0 { 0xff } else { byte })
    }
    fn protocol_bundle() -> ProtocolBundleV1 {
        ProtocolBundleV1 {
            protocol_version: 1,
            fork_id: hash(1),
            intent_codec_id: hash(2),
            finalized_intent_proof_codec_id: hash(3),
            tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
            fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
            oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
            result_codec_id: hash(4),
            action_codec_id: hash(5),
            activation_codec_id: hash(6),
            evidence_codec_id: hash(7),
            request_semantics_version: 1,
            lysis_program_semantics_hash: hash(8),
            planner_spec_version: 1,
            reducer_spec_version: 1,
            activation_apply_semantics_hash: hash(9),
            effect_contract_registry_hash: hash(10),
            object_codec_registry_hash: hash(11),
            correctness_profile_id: hash(12),
            capacity_profile_id: hash(13),
            result_signature_profile_id: hash(14),
            finality_verifier_and_vote_domain_id: hash(15),
            consensus_committee_history_schema_version: 1,
            ocomp_committee_schema_version: 1,
            proof_system_and_verifier_key_id: None,
            da_codec_and_binding_verifier_id: None,
            anti_equivocation_journal_schema_hash: hash(16),
            mode_pause_revocation_semantics_hash: hash(17),
            upgrade_fsm_semantics_hash: hash(18),
            release_requirement_catalog_sequence: 1,
            release_requirement_catalog_hash: hash(19),
            release_requirement_catalog_parent_hash: hash(20),
            release_gate_authority_envelope_hash: hash(21),
            release_approval_policy_hash: hash(22),
            release_validator_command_artifact_hash: hash(23),
            consensus_state_schema_version: 1,
            migration_manifest_hash: hash(24),
            required_upgrade_handler_set_hash: hash(25),
        }
    }
    fn finalized_job_spec(
        seed: u8,
        cursor: u64,
        chain_id: u64,
        genesis_hash: B256,
    ) -> FinalizedJobSpecV1 {
        let limits = poc_schema_limits();
        let day = 20_260_901_u32;
        let bundle = protocol_bundle();
        let protocol_bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let collection_key = hash(seed.wrapping_add(2));
        let collection_root = hash(seed.wrapping_add(3));
        let nominal = U256::from(1);
        let intent = JobIntentV1 {
            chain_id,
            genesis_hash,
            fork_id: bundle.fork_id,
            wwd: day,
            pending_nonce: 0,
            attempt: 0,
            protocol_bundle_hash,
            ce_sealed_root: hash(seed.wrapping_add(5)),
            sealed_tribute_collection_key: collection_key,
            sealed_tribute_collection_root: collection_root,
            authenticated_day_count: 1,
            authenticated_day_nominal: nominal,
            pre_admission_envelope_hash: hash(seed.wrapping_add(6)),
            source_availability_policy_id: hash(seed.wrapping_add(7)),
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
                request_limit_split_receipt_hash: hash(seed.wrapping_add(8)),
            },
            logical_evaluation_height: cursor,
            logical_evaluation_time: cursor,
            activation_preconditions: ActivationPreconditionsV1 {
                tribute: TributeInputBindingV1 {
                    wwd: day,
                    source_generation: 1,
                    collection_key,
                    sealed_collection_root: collection_root,
                    exact_count: 1,
                    exact_nominal_total: nominal,
                },
                nod: NodTargetPreconditionV1 {
                    wwd: day,
                    target_generation: 1,
                    namespace_root_before: hash(seed.wrapping_add(9)),
                    max_nod_count: 1,
                },
                contributors: ContributorTargetPreconditionV1 {
                    worldwide_day: day,
                    expected_series_version: 1,
                    max_contributor_count: 1,
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
            result_committee_set_hash: hash(seed.wrapping_add(10)),
            result_ocomp_binding_hash: hash(seed.wrapping_add(11)),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        };
        let finalized_block_hash = hash(seed.wrapping_add(12));
        let finalized_state_root = hash(seed.wrapping_add(13));
        FinalizedJobSpecV1 {
            summary: FinalizedJobSummaryV1 {
                cursor,
                job_id: intent
                    .job_id(finalized_block_hash, finalized_state_root, &limits)
                    .unwrap(),
                intent_id: intent.intent_id(&limits).unwrap(),
                finalized_block_hash,
                finalized_state_root,
                protocol_bundle_hash,
                open_height: cursor + 1,
                deadline_height: cursor + 1_801,
            },
            canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
        }
    }
    fn job_from_spec(spec: &FinalizedJobSpecV1) -> OcompJobRecordV1 {
        let limits = poc_schema_limits();
        let job = OcompJobRecordV1 {
            intent: JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap(),
            intent_height: spec.summary.cursor,
            status: OcompJobStatus::VotingOpen,
            finalized: Some(OcompFinalizedJobV1 {
                job_id: spec.summary.job_id,
                finalized_request_block_hash: spec.summary.finalized_block_hash,
                finalized_request_state_root: spec.summary.finalized_state_root,
                finality_recorded_height: spec.summary.open_height - 4,
                open_height: spec.summary.open_height,
                deadline_height: spec.summary.deadline_height,
                quorum: None,
            }),
            terminal: None,
        };
        job.validate_semantics(&limits).unwrap();
        job
    }
    struct Fixture {
        directory: tempfile::TempDir,
        binding_root: PathBuf,
        receipt_root: PathBuf,
        catalog_root: PathBuf,
        cas_root: PathBuf,
        job: OcompJobRecordV1,
        binding_ref: CasObjectRefV1,
        receipt_ref: CasObjectRefV1,
        manifest_ref: CasObjectRefV1,
        chunk_ref: CasObjectRefV1,
        manifest_hash: B256,
        committed: SnapshotExportCommittedV1,
    }
    fn fixture(seed: u8, damage: Option<&str>) -> Fixture {
        let limits = poc_schema_limits();
        let list_limits = OrderedListLimits::new(16, 4096, 4096);
        let bundle = protocol_bundle();
        let mut spec = finalized_job_spec(seed, 90, 1, B256::repeat_byte(250));
        spec.summary.open_height = 94;
        let intent = JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let cas_root = directory.path().join("cas-v1");
        let job_hex = hex::encode(spec.summary.job_id);
        let binding_root = directory
            .path()
            .join("supervisor-v1/export-bindings")
            .join(&job_hex);
        let catalog_root = directory
            .path()
            .join("exporter-v1/input-refs")
            .join(&job_hex);
        let receipt_base = directory.path().join("exporter-v1/receipts");
        let receipt_root = receipt_base.join(&job_hex);
        let bundles = directory.path().join("protocol-bundles-v1");
        fs::create_dir_all(&bundles).unwrap();
        fs::write(
            bundles.join(format!(
                "{}.ocb1",
                hex::encode(spec.summary.protocol_bundle_hash)
            )),
            bundle.encode_canonical(&limits).unwrap(),
        )
        .unwrap();
        let cas_limits = CAS_LIMITS;
        let cas =
            FilesystemCas::open(&cas_root, CasWriterRole::SnapshotExporter, cas_limits).unwrap();
        let reader = FilesystemCasReader::open(&cas_root, cas_limits).unwrap();
        let day = WorldwideDay::new(intent.wwd);
        let owner = Address::repeat_byte(1);
        let tribute = TributeBodyV1 {
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            issuance_amount_minor: U256::from(1),
            issuance_currency: 840,
            nominal_amount_minor: U256::from(1),
            reference_currency: 978,
            tribute_price_minor: U256::from(1),
            exclude_from_intex_issuance: false,
        };
        let chunk = AuthenticatedInputChunkV1 {
            protocol_bundle_hash: spec.summary.protocol_bundle_hash,
            job_id: spec.summary.job_id,
            kind: InputChunkKind::Tribute,
            ordinal: 0,
            canonical_records_or_openings: vec![BoundedBytes(encode_tribute_v1(&tribute).unwrap())],
        };
        let mut chunk_ref = cas
            .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
            .unwrap();
        chunk_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
        let input_ref =
            derive_input_chunk_ref(&reader.read_verified(&chunk_ref).unwrap(), &bundle, &limits)
                .unwrap()
                .reference;
        let manifest = InputManifestV1 {
            protocol_bundle_hash: spec.summary.protocol_bundle_hash,
            job_id: spec.summary.job_id,
            attempt: intent.attempt,
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: spec.summary.cursor
                    + u64::from(damage == Some("checkpoint_height")),
                finalized_block_hash: spec.summary.finalized_block_hash,
                finalized_state_root: spec.summary.finalized_state_root,
                finalized_ce_root: intent.ce_sealed_root,
                ce_schema_version: u16::try_from(
                    outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
                )
                .unwrap()
                    + u16::from(damage == Some("checkpoint_schema")),
            },
            wwd: intent.wwd,
            sealed_tribute_collection_key: intent.sealed_tribute_collection_key,
            sealed_tribute_collection_root: intent.sealed_tribute_collection_root,
            tribute_count: intent.authenticated_day_count,
            tribute_nominal_total: intent.authenticated_day_nominal,
            input_chunk_count: 1,
            input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
                ListKind::InputChunkReferences,
                &[input_ref.encode_canonical_record(&limits).unwrap()],
                list_limits,
            )
            .unwrap(),
            fidelity_opening_root: B256::repeat_byte(201),
            oracle_opening_root: B256::repeat_byte(202),
            exact_encoded_bytes: input_ref.encoded_bytes,
            exact_record_count: input_ref.record_count,
            body_codec_id: bundle.tribute_body_codec_id,
            opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
            compression: Compression::None,
        };
        let mut manifest_ref = cas
            .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
            .unwrap();
        manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
        let mut catalog = VerifiedInputChunkRefCatalog::open(
            &catalog_root,
            &cas,
            &manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        catalog.admit(&input_ref).unwrap();
        let committed = SnapshotExportCommittedV1 {
            job_id: spec.summary.job_id,
            pin_generation: 12,
            record_hash: B256::repeat_byte(203),
        };
        // Only the native producer uses the legacy discovery record. The offline
        // consumer below retains the authenticated immutable spec, not this record.
        let discovery = DiscoveryRecord {
            generation: 7,
            cursor: spec.summary.cursor,
            spec: spec.clone(),
        };
        let binding_ref = {
            let mut store = ExportedManifestBindingStore::open(&binding_root, limits).unwrap();
            store
                .seal(
                    &cas,
                    &reader,
                    ExportBindingCandidate {
                        discovery: &discovery,
                        job_id: spec.summary.job_id,
                        source_pin_generation: 11,
                        lease_generation: 17,
                        checkpoint: &manifest.checkpoint,
                        manifest_ref: &manifest_ref,
                        committed: &committed,
                        bundle: &bundle,
                        input_refs: &catalog,
                    },
                )
                .unwrap()
                .1
                .binding_ref()
        };
        // Bind a native receipt independently so mismatch cases remain well-formed
        // in each owner and fail only when their public authorities are composed.
        let receipt_source = if damage == Some("receipt_source") {
            21
        } else {
            11
        };
        let receipt_lease = if damage == Some("receipt_lease") {
            18
        } else {
            17
        };
        let receipt_committed = SnapshotExportCommittedV1 {
            job_id: spec.summary.job_id,
            pin_generation: receipt_source + 1,
            record_hash: if damage == Some("receipt_record") {
                B256::repeat_byte(204)
            } else {
                committed.record_hash
            },
        };
        let mut receipt_manifest = manifest.clone();
        let receipt_manifest_ref = if damage == Some("receipt_manifest") {
            receipt_manifest.fidelity_opening_root = B256::repeat_byte(205);
            let mut reference = cas
                .publish_bytes(&receipt_manifest.encode_canonical(&limits).unwrap())
                .unwrap();
            reference.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
            reference
        } else {
            manifest_ref.clone()
        };
        let handoff = SnapshotHandoffV1 {
            job_id: spec.summary.job_id,
            input_lease_id: intent.input_lease_id().unwrap(),
            pin_generation: receipt_source,
            lease_generation: receipt_lease,
            checkpoint: receipt_manifest.checkpoint.clone(),
            canonical_lease_offer: BoundedBytes(vec![1]),
        };
        let receipt_ref = {
            let mut store =
                ExportReceiptStore::open(&receipt_base, spec.summary.job_id, limits).unwrap();
            store
                .record(
                    &cas,
                    &reader,
                    ExportReceiptCandidate {
                        handoff: &handoff,
                        manifest_ref: &receipt_manifest_ref,
                        manifest_hash: receipt_manifest.manifest_hash(&limits).unwrap(),
                        committed: &receipt_committed,
                    },
                )
                .unwrap()
                .1
                .receipt_ref()
        };
        // Native seal/load closes the actual catalog. This minimal Tribute-only
        // fixture does not claim opening-proof or full worker-pipeline E2E coverage.
        let job = job_from_spec(&spec);
        let manifest_hash = manifest.manifest_hash(&limits).unwrap();
        drop(catalog);
        drop(reader);
        drop(cas);
        Fixture {
            directory,
            binding_root,
            receipt_root,
            catalog_root,
            cas_root,
            job,
            binding_ref,
            receipt_ref,
            manifest_ref,
            chunk_ref,
            manifest_hash,
            committed,
        }
    }

    impl Fixture {
        fn expected(&self) -> ExportAuthorityV1 {
            ExportAuthorityV1 {
                source_generation: 11,
                lease_generation: 17,
                manifest_hash: self.manifest_hash,
            }
        }
        fn cas_path(&self, reference: &CasObjectRefV1) -> PathBuf {
            let digest = hex::encode(reference.transport_digest);
            self.cas_root
                .join("objects")
                .join(&digest[..2])
                .join(&digest[2..])
        }
        fn check(&self, expected: Option<ExportAuthorityV1>) -> eyre::Result<(B256, u64)> {
            let before = fingerprint(self.directory.path());
            let result =
                verify_export_inputs(self.directory.path(), &self.job, expected, CAS_LIMITS)
                    .map(|audit| (audit.receipt.manifest_hash(), audit.input_chunks));
            assert_eq!(fingerprint(self.directory.path()), before);
            result
        }
    }

    #[test]
    fn complete_native_export_returns_owned_receipt_binding_without_discovery_or_private_journals()
    {
        let f = fixture(20, None);
        assert!(!f.directory.path().join("node-v1").exists());
        assert!(!f.directory.path().join("supervisor-v1/discovery").exists());
        for expected in [None, Some(f.expected())] {
            let before = fingerprint(f.directory.path());
            let audit =
                verify_export_inputs(f.directory.path(), &f.job, expected, CAS_LIMITS).unwrap();
            assert_eq!(audit.receipt.manifest_hash(), f.manifest_hash);
            assert_eq!(audit.input_chunks, 1);
            assert_eq!(audit.receipt.receipt_ref(), f.receipt_ref);
            assert_eq!(audit.binding.binding_ref(), f.binding_ref);
            assert_eq!(audit.receipt.manifest_ref(), f.manifest_ref);
            assert_eq!(audit.binding.manifest_ref(), f.manifest_ref);
            assert_eq!(audit.receipt.committed(), f.committed);
            assert_eq!(
                audit.binding.commit_replay_request(),
                audit.receipt.commit_replay_request()
            );
            assert_eq!(fingerprint(f.directory.path()), before);
        }
    }

    #[test]
    fn absent_required_receipt_binding_catalog_or_cas_is_incomplete() {
        for missing in [
            "receipt_directory",
            "binding_directory",
            "catalog_directory",
            "prepared_locator",
            "receipt_locator",
            "binding_locator",
            "catalog_header",
            "input_ref",
            "receipt_cas",
            "binding_cas",
            "manifest_cas",
            "chunk_cas",
        ] {
            let f = fixture(20, None);
            let path = match missing {
                "receipt_directory" => f.receipt_root.clone(),
                "binding_directory" => f.binding_root.clone(),
                "catalog_directory" => f.catalog_root.clone(),
                "prepared_locator" => f.receipt_root.join("prepared.ref"),
                "receipt_locator" => f.receipt_root.join("receipt.ref"),
                "binding_locator" => f.binding_root.join("binding.ref"),
                "catalog_header" => f.catalog_root.join("catalog.header"),
                "input_ref" => f.catalog_root.join("0000000000.input-ref"),
                "receipt_cas" => f.cas_path(&f.receipt_ref),
                "binding_cas" => f.cas_path(&f.binding_ref),
                "manifest_cas" => f.cas_path(&f.manifest_ref),
                "chunk_cas" => f.cas_path(&f.chunk_ref),
                _ => unreachable!(),
            };
            if path.is_dir() {
                fs::remove_dir_all(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
            let error = f.check(Some(f.expected())).unwrap_err();
            assert!(
                error.downcast_ref::<Incomplete>().is_some(),
                "{missing}: {error:#}"
            );
            assert!(!path.exists(), "reader must not recreate {missing}");
        }
    }

    #[test]
    fn changed_cas_bytes_are_failed_and_never_missing_input() {
        for object in ["chunk", "receipt", "binding", "manifest"] {
            let f = fixture(20, None);
            let reference = match object {
                "chunk" => &f.chunk_ref,
                "receipt" => &f.receipt_ref,
                "binding" => &f.binding_ref,
                "manifest" => &f.manifest_ref,
                _ => unreachable!(),
            };
            let path = f.cas_path(reference);
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(&path, bytes).unwrap();
            let error = f.check(None).unwrap_err();
            assert!(
                error.downcast_ref::<Incomplete>().is_none(),
                "{object}: {error:#}"
            );
        }
    }

    #[test]
    fn native_consistent_checkpoint_height_and_schema_still_bind_to_canonical_request() {
        for damage in ["checkpoint_height", "checkpoint_schema"] {
            // Native writers accept internally consistent checkpoint descriptors;
            // the local composition must compare height/schema with B/native CE.
            let f = fixture(20, Some(damage));
            let error = f.check(None).unwrap_err();
            assert!(
                error.downcast_ref::<Incomplete>().is_none(),
                "{damage}: {error:#}"
            );
        }
    }

    #[test]
    fn independently_valid_receipt_and_binding_must_describe_same_export() {
        for damage in [
            "receipt_source",
            "receipt_lease",
            "receipt_record",
            "receipt_manifest",
        ] {
            let f = fixture(20, Some(damage));
            // No pin export supplied: the two local native authorities must
            // still agree on request generations, manifest and committed record.
            let error = f.check(None).unwrap_err();
            assert!(
                error.downcast_ref::<Incomplete>().is_none(),
                "{damage}: {error:#}"
            );
        }
    }

    #[test]
    fn canonical_export_source_lease_and_manifest_must_match_receipt() {
        let f = fixture(20, None);
        let expected = f.expected();
        for changed in [
            ExportAuthorityV1 {
                source_generation: 12,
                ..expected
            },
            ExportAuthorityV1 {
                lease_generation: 18,
                ..expected
            },
            ExportAuthorityV1 {
                manifest_hash: hash(0xee),
                ..expected
            },
        ] {
            let error = f.check(Some(changed)).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        }
        assert_eq!(f.check(Some(expected)).unwrap(), (f.manifest_hash, 1));
    }

    #[test]
    fn existing_foreign_job_artifacts_cannot_satisfy_another_canonical_job() {
        let mut f = fixture(20, None);
        let mut other = finalized_job_spec(21, 90, 1, B256::repeat_byte(250));
        other.summary.open_height = 94;
        let foreign = job_from_spec(&other);
        let job_hex = hex::encode(other.summary.job_id);
        // Every required path and CAS object still exists. This must be a binding
        // failure, not a missing-directory classification under the new job key.
        for old in [&f.receipt_root, &f.binding_root, &f.catalog_root] {
            fs::rename(old, old.parent().unwrap().join(&job_hex)).unwrap();
        }
        f.job = foreign;
        let error = f.check(None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }
}

// The existing owner fixture supplies current-E root verification and
// whole-source fingerprints.
fn with_canonical_frontiers(
    version: u32,
    setup_public: impl FnOnce(&crate::snapshot::config::RequestedLayout),
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(
        &CanonicalState<'_>,
        &RethReadOnlyView,
        &crate::snapshot::config::RequestedLayout,
        &std::path::Path,
    ),
) {
    use crate::snapshot::config::{ProjectionLocation, RequestedLayout};
    use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
    use outbe_offchain_data::{ProjectionCheckpoint, ProjectionState, STORAGE_SCHEMA_VERSION};
    use outbe_offchain_storage::{Key, Namespace, RocksDbStorage, StorageWriter, Value};
    use reth_ethereum::provider::db::models::StoredBlockBodyIndices;
    use std::cell::RefCell;
    let requested = RefCell::new(None);
    with_prepared_owner_storage_setup(
        version,
        400,
        |native| {
            let layout = RequestedLayout {
                chain: native.chain.clone(),
                chain_root: native.chain_root.clone(),
                consensus_root: native.consensus_root.clone(),
                ocomp_root: native.ocomp_root.clone(),
                static_files_root: native.static_files_root.clone(),
                execution_rocksdb_root: native.execution_rocksdb_root.clone(),
                projection: Some(ProjectionLocation {
                    root: native.offchain_root.clone(),
                    start_block: native.projection_start_block,
                }),
                protected: native.protected.clone(),
            };
            let db = init_db(native.chain_root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            let header = tx
                .get::<tables::Headers<OutbeHeader>>(100)
                .unwrap()
                .unwrap();
            // Empty native frame: canonical active authority is read at E;
            // this tests availability, not reconstructed execution history.
            tx.put::<tables::BlockBodyIndices>(
                100,
                StoredBlockBodyIndices {
                    first_tx_num: 0,
                    tx_count: 0,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            drop(db);
            let point = ProjectionCheckpoint {
                block_number: 100,
                block_hash: header.hash_slow(),
            };
            let location = layout.projection.as_ref().unwrap();
            let projection = RocksDbStorage::open(&location.root).unwrap();
            let saved = ProjectionState {
                chain_id: layout.chain.chain().id(),
                genesis_hash: layout.chain.genesis_hash(),
                storage_schema_version: STORAGE_SCHEMA_VERSION,
                start_block: location.start_block,
                checkpoint: Some(point),
            };
            projection
                .put(
                    Namespace::new("projection_state").unwrap(),
                    &Key::new(b"offchain_data".to_vec()).unwrap(),
                    &Value::new(postcard::to_stdvec(&saved).unwrap()).unwrap(),
                )
                .unwrap();
            drop(projection);
            let baseline = ProjectionCheckpoint {
                block_number: 0,
                block_hash: layout.chain.genesis_hash(),
            };
            let root = layout
                .ocomp_root
                .join("exporter-v1/discovery/closure-checkpoint-v1");
            let closure = ContiguousCheckpointStoreV1::open(&root, baseline).unwrap();
            closure.compare_and_advance_to(baseline, point).unwrap();
            drop(closure);
            setup_public(&layout);
            *requested.borrow_mut() = Some(layout);
        },
        prepare,
        |state, source| {
            let requested = requested.borrow();
            let layout = requested.as_ref().unwrap();
            let scratch = tempfile::tempdir().unwrap();
            check(state, source, layout, scratch.path());
            assert_eq!(
                std::fs::read_dir(scratch.path()).unwrap().count(),
                0,
                "canonical composition must release its scratch on success and error"
            );
        },
    );
}

mod canonical_composition {
    use super::*;
    use crate::snapshot::validation::ocomp::verify_canonical_obligations;

    #[test]
    fn empty_canonical_composition_needs_no_job_cas_or_payout_directories() {
        for version in [1, 2] {
            with_canonical_frontiers(
                version,
                |_| {},
                |_| queued_owner(0),
                |state, source, layout, scratch| {
                    for maximum in [None, Some(0)] {
                        let audit = verify_canonical_obligations(
                            state, source, layout, scratch, maximum, None,
                        )
                        .unwrap();
                        assert_eq!(audit.projection.block_number, 100);
                        assert_eq!(audit.closure.checkpoint.current.block_number, 100);
                        assert_eq!(audit.closure.replay.blocks, 0);
                        assert_eq!(audit.bounds.active_intents, 0);
                        assert_eq!(audit.bounds.nod_entries, 0);
                        assert_eq!(audit.bounds.unpaid_days, 0);
                        assert!(audit.active.is_empty());
                        assert!(audit.pins.is_empty());
                        assert_eq!(audit.source_leases, 0);
                        assert_eq!(audit.complete_exports, 0);
                        assert_eq!(audit.input_chunks, 0);
                        assert_eq!(audit.nod.jobs, 0);
                        assert_eq!(audit.payout_days, 0);
                    }
                    for absent in [
                        "cas-v1",
                        "supervisor-v1/jobs",
                        "node-v1/local-results",
                        "supervisor-v1/materialization-references",
                    ] {
                        assert!(!layout.ocomp_root.join(absent).exists());
                    }
                    assert!(!layout.consensus_root.join("ocomp_retention").exists());
                },
            );
        }
    }

    #[test]
    fn canonical_inventory_failure_precedes_missing_projection_and_local_population() {
        with_canonical_frontiers(
            2,
            |layout| {
                std::fs::remove_dir_all(&layout.projection.as_ref().unwrap().root).unwrap();
            },
            |_| {
                let mut owner = queued_owner(0);
                // Exact corruption already exercised by active_inventory.
                StorageHandle::enter(&mut owner, |storage| {
                    outbe_primitives::storage::types::StorageBytes::new(
                        U256::from(20),
                        outbe_primitives::addresses::METADOSIS_ADDRESS,
                        storage,
                    )
                    .write(&[0; 8])
                    .unwrap();
                });
                owner
            },
            |state, source, layout, scratch| {
                let error =
                    verify_canonical_obligations(state, source, layout, scratch, None, None)
                        .err()
                        .expect("invalid canonical inventory cannot be skipped");
                assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
                assert!(
                    error
                        .to_string()
                        .contains("OCOMP live index magic/version mismatch"),
                    "{error:#}"
                );
            },
        );
    }
}

mod present_cas {
    use crate::snapshot::tests::headers::fingerprint;
    use crate::snapshot::validation::{ocomp::verify_present_cas, Incomplete};
    use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    const LIMITS: CasLimits = CasLimits {
        max_object_bytes: 1024,
        max_total_bytes: 4096,
    };

    fn publish(root: &Path, bytes: &[u8]) -> PathBuf {
        let cas =
            FilesystemCas::open(root.join("cas-v1"), CasWriterRole::Supervisor, LIMITS).unwrap();
        let object = cas.publish_bytes(bytes).unwrap();
        let hash = hex::encode(object.transport_digest);
        root.join("cas-v1/objects")
            .join(&hash[..2])
            .join(&hash[2..])
    }

    #[test]
    fn absent_cas_stays_absent_and_unreferenced_native_objects_are_verified() {
        let root = tempfile::tempdir().unwrap();
        let absent = verify_present_cas(root.path(), LIMITS, None).unwrap();
        assert_eq!((absent.objects, absent.bytes), (0, 0));
        assert!(!root.path().join("cas-v1").exists());
        publish(root.path(), b"historical unreferenced bytes");
        publish(root.path(), b"another object");
        let before = fingerprint(root.path());
        let result = verify_present_cas(root.path(), LIMITS, None).unwrap();
        assert_eq!(result.objects, 2);
        assert_eq!(result.bytes, 43);
        assert_eq!(fingerprint(root.path()), before);
        assert!(!root.path().join("supervisor-v1").exists());
    }

    #[test]
    fn changed_unreferenced_object_is_detected_without_job_or_catalog() {
        for replacement in [b"different bytes".as_slice(), b"short".as_slice()] {
            let root = tempfile::tempdir().unwrap();
            let path = publish(root.path(), b"unchanged bytes");
            fs::write(path, replacement).unwrap();
            let before = fingerprint(root.path());
            let error = verify_present_cas(root.path(), LIMITS, None).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            assert!(error.to_string().contains("digest mismatch"), "{error:#}");
            assert_eq!(fingerprint(root.path()), before);
        }
    }

    #[test]
    fn object_count_and_total_byte_budgets_cannot_pass_a_successful_prefix() {
        let root = tempfile::tempdir().unwrap();
        publish(root.path(), &[1; 700]);
        publish(root.path(), &[2; 700]);
        for (limits, maximum) in [
            (LIMITS, Some(1)),
            (
                CasLimits {
                    max_total_bytes: 1024,
                    ..LIMITS
                },
                None,
            ),
        ] {
            let before = fingerprint(root.path());
            let error = verify_present_cas(root.path(), limits, maximum).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert_eq!(fingerprint(root.path()), before);
        }
    }
}

mod present_join {
    use super::*;
    use crate::snapshot::validation::{
        ocomp::verify_ocomp_relations,
        report::{CheckName, ValidationReport},
    };
    use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
    use std::fs;

    const LIMITS: CasLimits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: u64::MAX,
    };

    #[test]
    fn empty_native_join_preserves_independent_frontiers_and_creates_no_job_stores() {
        for version in [1, 2] {
            with_canonical_frontiers(
                version,
                |_| {},
                |_| queued_owner(0),
                |state, source, layout, scratch| {
                    let before = crate::snapshot::tests::headers::fingerprint(&layout.ocomp_root);
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap();
                    assert_eq!(report.observed.p.as_ref().unwrap().number, 100);
                    assert_eq!(report.observed.c_current.as_ref().unwrap().number, 100);
                    assert!(report.required_missing.is_empty());
                    assert!(report.active_ocomp.is_empty());
                    assert_eq!(
                        crate::snapshot::tests::headers::fingerprint(&layout.ocomp_root),
                        before
                    );
                    for absent in [
                        "cas-v1",
                        "supervisor-v1/jobs",
                        "exporter-v1/receipts",
                        "exporter-v1/input-refs",
                        "supervisor-v1/export-bindings",
                    ] {
                        assert!(!layout.ocomp_root.join(absent).exists());
                    }
                },
            );
        }
    }

    #[test]
    fn every_present_population_is_enumerated_even_without_live_canonical_jobs() {
        for prefix in [
            "exporter-v1/receipts",
            "supervisor-v1/export-bindings",
            "exporter-v1/input-refs",
            "supervisor-v1/jobs",
            "supervisor-v1/materialization-references",
        ] {
            with_canonical_frontiers(
                2,
                |layout| {
                    fs::create_dir_all(layout.ocomp_root.join(prefix).join("not-a-native-job"))
                        .unwrap();
                },
                |_| queued_owner(0),
                |state, source, layout, scratch| {
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    let error = verify_ocomp_relations(state, source, layout, scratch, &mut report)
                        .unwrap_err();
                    assert!(
                        error.downcast_ref::<Incomplete>().is_none(),
                        "{prefix}: {error:#}"
                    );
                    assert_eq!(report.observed.p.as_ref().unwrap().number, 100);
                },
            );
        }
    }

    #[test]
    fn empty_historical_job_and_nested_reference_directories_do_not_require_old_evidence() {
        with_canonical_frontiers(
            2,
            |layout| {
                let job = hex::encode(B256::repeat_byte(71));
                for prefix in [
                    "exporter-v1/receipts",
                    "supervisor-v1/export-bindings",
                    "exporter-v1/input-refs",
                    "supervisor-v1/jobs",
                ] {
                    fs::create_dir_all(layout.ocomp_root.join(prefix).join(&job)).unwrap();
                }
                fs::create_dir_all(
                    layout
                        .ocomp_root
                        .join("supervisor-v1/materialization-references")
                        .join(&job)
                        .join("0"),
                )
                .unwrap();
            },
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap();
            },
        );
    }

    #[test]
    fn full_join_runs_orphan_cas_check_after_canonical_success() {
        with_canonical_frontiers(
            2,
            |layout| {
                let cas = FilesystemCas::open(
                    layout.ocomp_root.join("cas-v1"),
                    CasWriterRole::Supervisor,
                    LIMITS,
                )
                .unwrap();
                let reference = cas.publish_bytes(b"native orphan bytes").unwrap();
                drop(cas);
                let digest = hex::encode(reference.transport_digest);
                fs::write(
                    layout
                        .ocomp_root
                        .join("cas-v1/objects")
                        .join(&digest[..2])
                        .join(&digest[2..]),
                    b"native broken bytes",
                )
                .unwrap();
            },
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                let error = verify_ocomp_relations(state, source, layout, scratch, &mut report)
                    .unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
                assert_eq!(report.observed.c_current.as_ref().unwrap().number, 100);
            },
        );
    }

    #[test]
    fn nested_materialization_reference_with_unavailable_job_evidence_is_incomplete() {
        use outbe_ocomp::nod_materialization::MaterializationReferenceStoreV1;
        with_canonical_frontiers(
            2,
            |layout| {
                let cas = FilesystemCas::open(
                    layout.ocomp_root.join("cas-v1"),
                    CasWriterRole::Supervisor,
                    LIMITS,
                )
                .unwrap();
                let reference = cas.publish_bytes(b"retained dependency").unwrap();
                let job = B256::repeat_byte(72);
                let path = layout
                    .ocomp_root
                    .join("supervisor-v1/materialization-references")
                    .join(hex::encode(job))
                    .join("9");
                MaterializationReferenceStoreV1::open(path)
                    .unwrap()
                    .pin_exact(job, &[reference])
                    .unwrap();
            },
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                let error = verify_ocomp_relations(state, source, layout, scratch, &mut report)
                    .unwrap_err();
                assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                assert!(format!("{error:#}").contains("job"));
            },
        );
    }
}
