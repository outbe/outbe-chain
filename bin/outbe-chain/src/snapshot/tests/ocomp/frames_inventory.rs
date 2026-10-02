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
                receipts_root: alloy_consensus::proofs::calculate_receipt_root(&[
                    alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
                ]),
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
mod closure_inventory;
