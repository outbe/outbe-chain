mod support;

use alloy_primitives::B256;
use outbe_ocomp::{
    bundle::PinnedProtocolBundle,
    cas::CasLimits,
    control::{poc_schema_limits, EndpointIdentity},
    embedded_runtime::{EmbeddedNodePolicyV1, EmbeddedOcompDomainConfigV1, EmbeddedOcompDomainV1},
    inbox::WorkerInboxLimits,
    supervisor_job::{SupervisorJobRunnerConfigV1, SupervisorJobRunnerV1},
    worker_transport::SupervisorWorkerServerV1,
};

#[test]
fn fresh_full_node_domain_creates_its_node_local_storage_parent() {
    let temporary = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let bundle = support::protocol_bundle();
    let canonical_bundle = bundle.encode_canonical(&limits).unwrap();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let worker_address = probe.local_addr().unwrap();
    drop(probe);
    let domain_root = temporary.path().join("domain-v1");

    let domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
        domain_root: domain_root.clone(),
        worker_address,
        identity: EndpointIdentity {
            chain_id: 54_322_345,
            genesis_hash: B256::repeat_byte(0x21),
            boot_nonce: B256::repeat_byte(0x22),
            protocol_bundle_hash: bundle_hash,
        },
        registry_generation: 1,
        protocol_bundle: PinnedProtocolBundle::decode(&canonical_bundle, bundle_hash, &limits)
            .unwrap(),
        policy: EmbeddedNodePolicyV1::FullNode,
        validator_rpc_url: None,
        limits,
    })
    .unwrap();

    assert!(domain_root.join("node-v1/local-results").is_dir());
    assert!(domain_root.join("supervisor-v1").is_dir());
    assert!(domain_root.join("exporter-v1").is_dir());
    drop(domain);
}

#[test]
fn node_owned_worker_server_outlives_embedded_runner() {
    let temporary = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let bundle = support::protocol_bundle();
    let canonical_bundle = bundle.encode_canonical(&limits).unwrap();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let identity = EndpointIdentity {
        chain_id: 54_322_345,
        genesis_hash: B256::repeat_byte(0x31),
        boot_nonce: B256::repeat_byte(0x32),
        protocol_bundle_hash: bundle_hash,
    };
    let server =
        SupervisorWorkerServerV1::start("127.0.0.1:0".parse().unwrap(), identity, 1, limits)
            .unwrap();

    let runner = SupervisorJobRunnerV1::open(
        SupervisorJobRunnerConfigV1 {
            cas_root: temporary.path().join("cas"),
            cas_limits: CasLimits {
                max_object_bytes: 1 << 20,
                max_total_bytes: 1 << 24,
            },
            input_ref_root: temporary.path().join("input-refs"),
            job_root: temporary.path().join("jobs"),
            worker_inbox_root: temporary.path().join("worker-inbox"),
            worker_inbox_limits: WorkerInboxLimits {
                max_artifact_bytes: 1 << 20,
                max_total_bytes: 1 << 24,
            },
            protocol_bundle: PinnedProtocolBundle::decode(&canonical_bundle, bundle_hash, &limits)
                .unwrap(),
            limits,
        },
        server.dispatcher(),
    )
    .unwrap();

    drop(runner);
    assert_eq!(server.registered_workers().unwrap(), 0);
}
