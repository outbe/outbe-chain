use alloy_consensus::{Header, Sealable};
use alloy_primitives::{keccak256, Address, B256, U256};
use reth_ethereum::{
    provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        table::Table,
        tables::{self, ChainStateKey},
        transaction::{DbTx, DbTxMut},
    },
    trie::root::{state_root_unhashed, storage_root_unhashed},
};
use reth_primitives_traits::{Account, StorageEntry};

use super::super::{
    config::{parse_node_inputs, resolve_layout, NativeLayout},
    native::RethReadOnlyView,
    validation::evm::VerifiedState,
};
use crate::OutbeHeader;

type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;

fn verify_current_evm(
    view: &RethReadOnlyView,
    scratch: &std::path::Path,
) -> eyre::Result<VerifiedState> {
    // Every fixture uses native_arguments with this genesis at its source root.
    // Check all files, config and keys on both success and error, not only MDBX.
    let root = view
        .protected
        .0
        .iter()
        .find(|path| path.file_name().is_some_and(|name| name == "genesis.json"))
        .unwrap()
        .parent()
        .unwrap();
    let before = super::headers::fingerprint(root);
    let result = super::super::validation::evm::verify_current_evm(view, scratch);
    assert_eq!(super::headers::fingerprint(root), before);
    result
}

pub(super) fn state_fixture(version: u32) -> (tempfile::TempDir, NativeLayout, B256) {
    let root = tempfile::tempdir().unwrap();
    let inputs = parse_node_inputs(super::layout::native_arguments(root.path())).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    std::fs::create_dir_all(layout.chain_root.join("keys")).unwrap();
    std::fs::write(
        layout.chain_root.join("keys/recipient.key"),
        b"recipient key sentinel",
    )
    .unwrap();
    std::fs::create_dir_all(&layout.static_files_root).unwrap();
    let address = Address::repeat_byte(0x11);
    let slot = B256::repeat_byte(0x22);
    let value = U256::from(123);
    let account = Account {
        nonce: 7,
        balance: U256::from(900),
        bytecode_hash: None,
    };
    let expected = state_root_unhashed([(
        address,
        account.into_trie_account(storage_root_unhashed([(slot, value)])),
    )]);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let h = OutbeHeader::new(Header {
        number: 100,
        ..Default::default()
    });
    let e = OutbeHeader::new(Header {
        number: 101,
        parent_hash: h.hash_slow(),
        state_root: expected,
        ..Default::default()
    });
    for header in [layout.chain.genesis_header().clone(), h, e] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
            .unwrap();
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 100)
        .unwrap();
    for stage in ["Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), StageCheckpoint::new(101))
            .unwrap();
    }
    if version == 1 {
        tx.put::<tables::PlainAccountState>(address, account)
            .unwrap();
        tx.put::<tables::PlainStorageState>(address, StorageEntry { key: slot, value })
            .unwrap();
        // This is a derived v1 cache: it cannot replace the authoritative plain state.
        tx.put::<tables::HashedAccounts>(keccak256(address), Account::default())
            .unwrap();
    } else {
        tx.put::<tables::Metadata>(
            "storage_settings".into(),
            br#"{"storage_v2":true}"#.to_vec(),
        )
        .unwrap();
        tx.put::<tables::HashedAccounts>(keccak256(address), account)
            .unwrap();
        tx.put::<tables::HashedStorages>(
            keccak256(address),
            StorageEntry {
                key: keccak256(slot),
                value,
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    (root, layout, expected)
}

#[test]
fn complete_current_evm_tail_uses_authoritative_v1_and_v2_without_history() {
    for version in [1, 2] {
        let (_source, layout, expected) = state_fixture(version);
        let scratch = tempfile::tempdir().unwrap();
        let view = RethReadOnlyView::open(&layout).unwrap();
        let verified = verify_current_evm(&view, scratch.path()).unwrap();
        assert_eq!(verified.state_root, expected);
        assert_eq!(verified.header.inner.number, 101);
        assert_eq!(view.progress.finalized.number, 100);
        assert_eq!(
            verified
                .db
                .tx()
                .unwrap()
                .get::<tables::HashedAccounts>(keccak256(Address::repeat_byte(0x11)))
                .unwrap()
                .unwrap()
                .balance,
            U256::from(900)
        );
        assert!(!layout.offchain_root.exists());
        assert!(!layout.ocomp_root.exists());
        assert!(!layout.chain_root.join("rocksdb-secondary-tmp").exists());
    }
}

#[test]
fn changed_authoritative_accounts_and_slots_fail_without_rewriting_sources() {
    for version in [1, 2] {
        for storage in [false, true] {
            let (_source, layout, _) = state_fixture(version);
            let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            let address = Address::repeat_byte(0x11);
            if storage {
                if version == 1 {
                    tx.clear::<tables::PlainStorageState>().unwrap();
                    tx.put::<tables::PlainStorageState>(
                        address,
                        StorageEntry {
                            key: B256::repeat_byte(0x22),
                            value: U256::from(124),
                        },
                    )
                    .unwrap();
                } else {
                    tx.clear::<tables::HashedStorages>().unwrap();
                    tx.put::<tables::HashedStorages>(
                        keccak256(address),
                        StorageEntry {
                            key: keccak256(B256::repeat_byte(0x22)),
                            value: U256::from(124),
                        },
                    )
                    .unwrap();
                }
            } else {
                let account = Account {
                    nonce: 7,
                    balance: U256::from(901),
                    bytecode_hash: None,
                };
                if version == 1 {
                    tx.put::<tables::PlainAccountState>(address, account)
                        .unwrap();
                } else {
                    tx.put::<tables::HashedAccounts>(keccak256(address), account)
                        .unwrap();
                }
            }
            tx.commit().unwrap();
            drop(db);
            let before = std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap();
            let scratch = tempfile::tempdir().unwrap();
            let view = RethReadOnlyView::open(&layout).unwrap();
            let error = verify_current_evm(&view, scratch.path()).err().unwrap();
            assert!(
                error.to_string().contains("state root differs"),
                "{error:#}"
            );
            assert_eq!(
                std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap(),
                before
            );
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn derived_source_tries_do_not_replace_authoritative_state() {
    use reth_ethereum::trie::BranchNodeCompact;
    for version in [1, 2] {
        let (_source, layout, expected) = state_fixture(version);
        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        tx.put::<tables::AccountsTrie>(
            Default::default(),
            BranchNodeCompact {
                root_hash: Some(B256::repeat_byte(99)),
                ..Default::default()
            },
        )
        .unwrap();
        tx.commit().unwrap();
        drop(db);
        let scratch = tempfile::tempdir().unwrap();
        let verified =
            verify_current_evm(&RethReadOnlyView::open(&layout).unwrap(), scratch.path()).unwrap();
        assert_eq!(verified.state_root, expected);
        assert_eq!(
            verified
                .db
                .tx()
                .unwrap()
                .entries::<tables::AccountsTrie>()
                .unwrap(),
            0
        );
    }
}

#[test]
fn referenced_code_is_verified_even_when_the_account_root_matches() {
    use reth_primitives_traits::Bytecode;
    let code = [0x60, 0x01, 0x00];
    for variant in [0, 1, 2] {
        let (_source, layout, _) = state_fixture(2);
        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        let address = Address::repeat_byte(0x11);
        let account = Account {
            nonce: 7,
            balance: U256::from(900),
            bytecode_hash: Some(keccak256(code)),
        };
        let expected = state_root_unhashed([(
            address,
            account.into_trie_account(storage_root_unhashed([(
                B256::repeat_byte(0x22),
                U256::from(123),
            )])),
        )]);
        tx.put::<tables::HashedAccounts>(keccak256(address), account)
            .unwrap();
        let mut header = tx
            .get::<tables::Headers<OutbeHeader>>(101)
            .unwrap()
            .unwrap();
        header.inner.state_root = expected;
        tx.put::<tables::CanonicalHeaders>(101, header.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(101, header).unwrap();
        if variant != 1 {
            let bytes = if variant == 0 {
                code.to_vec()
            } else {
                vec![0x60, 0x02, 0x00]
            };
            tx.put::<tables::Bytecodes>(keccak256(code), Bytecode::new_raw(bytes.into()))
                .unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        let scratch = tempfile::tempdir().unwrap();
        let result = verify_current_evm(&RethReadOnlyView::open(&layout).unwrap(), scratch.path());
        if variant == 0 {
            assert_eq!(result.unwrap().state_root, expected);
        } else {
            let error = result.err().unwrap();
            assert!(error.to_string().contains("bytecode"), "{error:#}");
        }
    }
}

#[test]
fn persisted_unwind_and_partial_observations_never_claim_a_complete_root() {
    use super::super::validation::Incomplete;
    let (_source, layout, _) = state_fixture(2);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    // A later suffix touches A again, masking its H account/slot updates in
    // the persisted image. B retains its H update: this is neither H nor E.
    let address_a = Address::repeat_byte(0x11);
    let address_b = Address::repeat_byte(0x33);
    let account_b = Account {
        nonce: 1,
        balance: U256::from(44),
        bytecode_hash: None,
    };
    tx.put::<tables::HashedAccounts>(keccak256(address_b), account_b)
        .unwrap();
    let root_for = |balance, value| {
        state_root_unhashed([
            (
                address_a,
                Account {
                    nonce: 7,
                    balance: U256::from(balance),
                    bytecode_hash: None,
                }
                .into_trie_account(storage_root_unhashed([(
                    B256::repeat_byte(0x22),
                    U256::from(value),
                )])),
            ),
            (
                address_b,
                account_b.into_trie_account(storage_root_unhashed([])),
            ),
        ])
    };
    let masked_root = root_for(900_u64, 123_u64);
    let mut h = tx
        .get::<tables::Headers<OutbeHeader>>(100)
        .unwrap()
        .unwrap();
    h.inner.state_root = root_for(902, 124);
    let mut e = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    e.inner.parent_hash = h.hash_slow();
    e.inner.state_root = root_for(903, 125);
    assert_ne!(masked_root, h.inner.state_root);
    assert_ne!(masked_root, e.inner.state_root);
    for header in [h, e] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
            .unwrap();
    }
    tx.put::<tables::Metadata>(
        "partial_state_trie_unwind".into(),
        br#"{"finish_block_number":101,"partial_state_trie":100}"#.to_vec(),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let before = std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut view = RethReadOnlyView::open(&layout).unwrap();
    let error = verify_current_evm(&view, scratch.path()).err().unwrap();
    assert!(error.downcast_ref::<Incomplete>().is_some());
    // This build's pinned Reth disables FinishCheckpoint serialization. Exercise
    // the observation decision independently; the persisted unwind above is real.
    view.progress.unwind = None;
    view.progress.partial_state_trie = Some(100);
    let error = verify_current_evm(&view, scratch.path()).err().unwrap();
    assert!(error.downcast_ref::<Incomplete>().is_some());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap(),
        before
    );
}

#[test]
fn read_only_inspection_does_not_initialize_a_missing_static_directory() {
    let (_source, layout, _) = state_fixture(2);
    std::fs::remove_dir(&layout.static_files_root).unwrap();
    assert!(super::super::native::inspect_reth(&layout).is_err());
    assert!(!layout.static_files_root.exists());
}

#[test]
fn execution_selection_does_not_require_projection_configuration_or_ocomp() {
    use super::super::config::{resolve_requested_layout, NativeReadSelection};
    let (source, layout, expected) = state_fixture(2);
    std::fs::remove_file(source.path().join("configuration/offchain.toml")).unwrap();
    for config in [
        None,
        Some(source.path().join("configuration/offchain.toml")),
    ] {
        let mut arguments = vec![
            "--chain".into(),
            source.path().join("genesis.json").into_os_string(),
            "--datadir".into(),
            layout.chain_root.clone().into_os_string(),
        ];
        if let Some(path) = config {
            arguments.extend(["--projection.storage-config".into(), path.into_os_string()]);
        }
        let inputs = parse_node_inputs(arguments).unwrap();
        assert!(resolve_layout(&inputs).is_err());
        let requested =
            resolve_requested_layout(&inputs, NativeReadSelection { projection: false }).unwrap();
        assert!(requested.projection.is_none());
        let view = RethReadOnlyView::open_requested(&requested).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        assert_eq!(
            verify_current_evm(&view, scratch.path())
                .unwrap()
                .state_root,
            expected
        );
        assert!(!layout.offchain_root.exists());
        assert!(!layout.ocomp_root.exists());
    }
}

#[test]
fn orphan_and_duplicate_storage_are_not_silently_ignored() {
    for duplicate in [false, true] {
        let (_source, layout, _) = state_fixture(2);
        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        let address = if duplicate {
            keccak256(Address::repeat_byte(0x11))
        } else {
            B256::repeat_byte(77)
        };
        tx.put::<tables::HashedStorages>(
            address,
            StorageEntry {
                key: keccak256(B256::repeat_byte(0x22)),
                value: U256::from(124),
            },
        )
        .unwrap();
        assert_eq!(tx.entries::<tables::HashedStorages>().unwrap(), 2);
        tx.commit().unwrap();
        drop(db);
        let scratch = tempfile::tempdir().unwrap();
        let error = verify_current_evm(&RethReadOnlyView::open(&layout).unwrap(), scratch.path())
            .err()
            .unwrap();
        assert!(
            error.to_string().contains(if duplicate {
                "duplicate storage"
            } else {
                "orphan storage"
            }),
            "{error:#}"
        );
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}

#[test]
fn malformed_native_account_rows_return_an_error_without_source_repair() {
    use reth_ethereum::provider::db::tables::{RawKey, RawTable, RawValue};
    let (_source, layout, _) = state_fixture(2);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    tx.put::<RawTable<tables::HashedAccounts>>(
        RawKey::new(keccak256(Address::repeat_byte(0x11))),
        RawValue::from_vec(Vec::new()),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let before = std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    assert!(verify_current_evm(&RethReadOnlyView::open(&layout).unwrap(), scratch.path()).is_err());
    assert_eq!(
        std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap(),
        before
    );
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn trailing_native_account_bytes_are_not_hidden_by_the_state_root() {
    use reth_ethereum::provider::db::tables::{RawKey, RawTable, RawValue};
    for version in [1, 2] {
        let (_source, layout, _) = state_fixture(version);
        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        let address = Address::repeat_byte(0x11);
        let account = Account {
            nonce: 7,
            balance: U256::from(900),
            bytecode_hash: None,
        };
        let mut bytes = RawValue::new(account).raw_value().to_vec();
        bytes.push(0xff);
        if version == 1 {
            tx.put::<RawTable<tables::PlainAccountState>>(
                RawKey::new(address),
                RawValue::from_vec(bytes),
            )
            .unwrap();
        } else {
            tx.put::<RawTable<tables::HashedAccounts>>(
                RawKey::new(keccak256(address)),
                RawValue::from_vec(bytes),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        let before = std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let result = verify_current_evm(&RethReadOnlyView::open(&layout).unwrap(), scratch.path());
        assert!(
            result.is_err(),
            "ignored trailing bytes in v{version} account"
        );
        assert_eq!(
            std::fs::read(layout.chain_root.join("db/mdbx.dat")).unwrap(),
            before
        );
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}

#[test]
fn evm_scratch_cannot_overlap_native_data_or_configuration() {
    let (source, layout, _) = state_fixture(2);
    let view = RethReadOnlyView::open(&layout).unwrap();
    for scratch in [&layout.chain_root, &layout.static_files_root, source.path()] {
        assert!(verify_current_evm(&view, scratch).is_err());
    }
    assert_eq!(
        std::fs::read_dir(&layout.static_files_root)
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn stopped_audit_transaction_survives_the_normal_reader_timeout() {
    use reth_ethereum::provider::db::{mdbx::MaxReadTransactionDuration, open_db_read_only};
    use std::time::{Duration, Instant};
    let (_source, layout, _) = state_fixture(2);
    let RethReadOnlyView {
        db,
        static_files,
        chain,
        progress,
        protected,
    } = RethReadOnlyView::open(&layout).unwrap();
    drop(db);
    let view = RethReadOnlyView {
        db: open_db_read_only(
            layout.chain_root.join("db"),
            DatabaseArguments::default().with_max_read_transaction_duration(Some(
                MaxReadTransactionDuration::Set(Duration::from_millis(100)),
            )),
        )
        .unwrap(),
        static_files,
        chain,
        progress,
        protected,
    };
    let normal = view.db.tx().unwrap();
    let audit = view.read_transaction().unwrap();
    let key = keccak256(Address::repeat_byte(0x11));
    let deadline = Instant::now() + Duration::from_secs(5);
    while normal.get::<tables::HashedAccounts>(key).is_ok() {
        assert!(
            Instant::now() < deadline,
            "short-lived control reader did not expire"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        audit
            .get::<tables::HashedAccounts>(key)
            .unwrap()
            .unwrap()
            .balance,
        U256::from(900)
    );
}
