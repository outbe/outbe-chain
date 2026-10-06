mod present_results;

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
    addresses::METADOSIS_ADDRESS, time::WorldwideDay, OutbeHeader, OutbePrimitives, OutbeReceipt,
    OutbeTxEnvelope,
};
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    models::StoredBlockBodyIndices,
    tables,
    transaction::{DbTx, DbTxMut},
};
use reth_provider::{providers::StaticFileProviderBuilder, StaticFileSegment, StaticFileWriter};

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
    // IntentId does not contain B's hash. Construct the event first. Then
    // finalize receipt/transaction roots. Only then derive the final JobId.
    let job = job_for(&request, case);
    let mut event = request_event(&job);
    match case {
        Case::WrongIntent => event.intentId = B256::repeat_byte(0xee),
        Case::WrongDay => event.wwd += 1,
        Case::WrongAttempt => event.attempt += 1,
        Case::WrongNonce => event.pendingNonce += 1,
        Case::WrongActivation => event.activationPreconditionsHash = B256::repeat_byte(0xdd),
        _ => {}
    }
    let native_log = Log {
        address: METADOSIS_ADDRESS,
        data: event.encode_log_data(),
    };
    let mut receipts = [
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
        Case::ForeignAddressOnly => receipts[0].logs[0].address = Address::repeat_byte(0xee),
        Case::ForeignTopicOnly => {
            receipts[0].logs[0].data =
                LogData::new_unchecked(vec![B256::repeat_byte(0xee)], Vec::<u8>::new().into())
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
    request.inner.receipts_root = alloy_consensus::proofs::calculate_receipt_root(
        &receipts
            .iter()
            .map(alloy_consensus::TxReceipt::with_bloom_ref)
            .collect::<Vec<_>>(),
    );
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
                let error = result.expect_err("missing required frame evidence must be incomplete");
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
                let error =
                    result.expect_err("request found before budget exhaustion is not complete");
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
