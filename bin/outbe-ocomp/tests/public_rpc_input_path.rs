//! Manual full-path check for the production public-RPC OCOMP input pipeline.
//!
//! This test is intentionally ignored: it requires a finalized OCOMP request and
//! a transaction-capable MongoDB deployment. CI only compiles it.

use std::{env, fs};

use alloy_primitives::B256;
use outbe_ocomp::{
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, FilesystemCasReader},
    control::{poc_schema_limits, EndpointIdentity},
    export_receipt::ExportReceiptReader,
    rpc_discovery::{
        FinalizedRpcDiscoveryConfigV1, FinalizedRpcDiscoveryPurposeV1, FinalizedRpcDiscoveryV1,
    },
    rpc_input_exporter::{RpcInputExporterConfigV1, RpcInputExporterV1},
    rpc_projection::RpcProjectionConfigV1,
};
use outbe_offchain_storage::MongoStorageConfig;
use tempfile::tempdir;

const RPC_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
};

#[test]
#[ignore = "requires a finalized public-RPC OCOMP request and transaction-capable MongoDB"]
fn finalized_public_rpc_path_exports_restart_safe_inputs_and_rejects_identity_mismatch() {
    let rpc_url = required("OUTBE_OCOMP_TEST_RPC_URL");
    let mongo_uri = required("OUTBE_OCOMP_TEST_MONGODB_URI");
    let mongo_database = required("OUTBE_OCOMP_TEST_MONGODB_DATABASE");
    let chain_id = required("OUTBE_OCOMP_TEST_CHAIN_ID")
        .parse::<u64>()
        .unwrap();
    let genesis_hash = required("OUTBE_OCOMP_TEST_GENESIS_HASH")
        .parse::<B256>()
        .unwrap();
    let protocol_bundle_hash = required("OUTBE_OCOMP_TEST_PROTOCOL_BUNDLE_HASH")
        .parse::<B256>()
        .unwrap();
    let limits = poc_schema_limits();
    let protocol_bundle = PinnedProtocolBundle::decode(
        &fs::read(required("OUTBE_OCOMP_TEST_PROTOCOL_BUNDLE_PATH")).unwrap(),
        protocol_bundle_hash,
        &limits,
    )
    .unwrap();
    let fork_id = protocol_bundle.bundle().fork_id;
    let identity = EndpointIdentity {
        chain_id,
        genesis_hash,
        boot_nonce: B256::repeat_byte(0x42),
        protocol_bundle_hash,
    };
    let root = tempdir().unwrap();
    let journal_root = root.path().join("discovery");
    let cas_root = root.path().join("cas");
    let input_ref_root = root.path().join("input-refs");
    let receipt_root = root.path().join("receipts");

    let mut discovery = FinalizedRpcDiscoveryV1::open(FinalizedRpcDiscoveryConfigV1 {
        rpc_url: rpc_url.clone(),
        rpc_max_response_bytes: RPC_MAX_RESPONSE_BYTES,
        journal_root,
        projection_start_block: 1,
        identity,
        fork_id,
        limits,
        purpose: FinalizedRpcDiscoveryPurposeV1::InputReplay,
    })
    .unwrap();
    discovery.reconcile_once().unwrap();
    let record = discovery.current_record().expect(
        "the configured public RPC must expose one finalized, currently voting OCOMP request",
    );

    let exporter_config = RpcInputExporterConfigV1 {
        rpc_url: rpc_url.clone(),
        rpc_max_response_bytes: RPC_MAX_RESPONSE_BYTES,
        projection: RpcProjectionConfigV1 {
            rpc_url: rpc_url.clone(),
            rpc_max_response_bytes: RPC_MAX_RESPONSE_BYTES,
            mongo: MongoStorageConfig {
                uri: mongo_uri,
                database: mongo_database,
            },
            chain_id,
            genesis_hash,
            start_block: 1,
            tribute_page_limit: 256,
        },
        chain_id,
        genesis_hash,
        fork_id,
        protocol_bundle_hash,
        cas_root: cas_root.clone(),
        cas_limits: CAS_LIMITS,
        input_ref_root,
        receipt_root: receipt_root.clone(),
        protocol_bundle,
        limits,
    };

    let mut exporter = RpcInputExporterV1::open(exporter_config.clone()).unwrap();
    exporter.export(&record).unwrap();
    drop(exporter);

    let cas_reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
    let first = ExportReceiptReader::open(&receipt_root, record.spec.summary.job_id, limits)
        .unwrap()
        .load_exact(&cas_reader)
        .unwrap();

    let receipt_job_root = receipt_root.join(hex::encode(record.spec.summary.job_id.as_slice()));
    fs::remove_file(receipt_job_root.join("prepared.ref")).unwrap();
    fs::remove_file(receipt_job_root.join("receipt.ref")).unwrap();
    let mut empty_directory_restart = RpcInputExporterV1::open(exporter_config.clone()).unwrap();
    empty_directory_restart.export(&record).unwrap();
    drop(empty_directory_restart);
    let after_empty_directory =
        ExportReceiptReader::open(&receipt_root, record.spec.summary.job_id, limits)
            .unwrap()
            .load_exact(&cas_reader)
            .unwrap();
    assert_eq!(first.receipt_ref(), after_empty_directory.receipt_ref());
    assert_eq!(first.manifest(), after_empty_directory.manifest());

    fs::remove_file(receipt_job_root.join("receipt.ref")).unwrap();
    let mut prepared_only_restart = RpcInputExporterV1::open(exporter_config.clone()).unwrap();
    prepared_only_restart.export(&record).unwrap();
    drop(prepared_only_restart);
    let after_prepared_only =
        ExportReceiptReader::open(&receipt_root, record.spec.summary.job_id, limits)
            .unwrap()
            .load_exact(&cas_reader)
            .unwrap();
    assert_eq!(first.receipt_ref(), after_prepared_only.receipt_ref());
    assert_eq!(first.manifest(), after_prepared_only.manifest());

    let mut restarted = RpcInputExporterV1::open(exporter_config.clone()).unwrap();
    restarted.export(&record).unwrap();
    drop(restarted);
    let second = ExportReceiptReader::open(&receipt_root, record.spec.summary.job_id, limits)
        .unwrap()
        .load_exact(&cas_reader)
        .unwrap();
    assert_eq!(first.receipt_ref(), second.receipt_ref());
    assert_eq!(first.manifest(), second.manifest());

    let mismatch_root = root.path().join("identity-mismatch");
    let mut mismatch_config = exporter_config;
    mismatch_config.chain_id = chain_id.checked_add(1).unwrap();
    mismatch_config.cas_root = mismatch_root.join("cas");
    mismatch_config.input_ref_root = mismatch_root.join("input-refs");
    mismatch_config.receipt_root = mismatch_root.join("receipts");
    let mut mismatch = RpcInputExporterV1::open(mismatch_config).unwrap();
    assert!(mismatch.export(&record).is_err());
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("missing required environment variable {name}"))
}
