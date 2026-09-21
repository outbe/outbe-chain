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
