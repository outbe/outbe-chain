use super::*;

static STACK_MARSHAL_TEST_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
struct AckingMarshalReporter;

impl Reporter for AckingMarshalReporter {
    type Activity = Update<ConsensusBlock, commonware_utils::acknowledgement::Exact>;

    // `report` is now SYNC and returns `Feedback` (commonware 2026.5.0). The
    // body does unchanged work (acknowledge delivered blocks). We always return
    // `Feedback::Ok` because this test reporter has no downstream mailbox that
    // can close.
    fn report(&mut self, activity: Self::Activity) -> Feedback {
        if let Update::Block(_, ack) = activity {
            ack.acknowledge();
        }
        Feedback::Ok
    }
}

pub(super) async fn start_recovery_marshal(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
) -> (
    outbe_consensus::marshal_types::MarshalMailbox,
    handler::Handler<outbe_consensus::digest::Digest>,
    commonware_runtime::Handle<()>,
) {
    start_recovery_marshal_with_reporter(context, provider, AckingMarshalReporter).await
}

pub(super) async fn start_recovery_marshal_with_reporter<R>(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
    reporter: R,
) -> (
    outbe_consensus::marshal_types::MarshalMailbox,
    handler::Handler<outbe_consensus::digest::Digest>,
    commonware_runtime::Handle<()>,
)
where
    R: Reporter<Activity = outbe_consensus::marshal_types::MarshalUpdate> + Send + 'static,
{
    let test_id = STACK_MARSHAL_TEST_ID.fetch_add(1, Ordering::SeqCst);
    crate::stack::recovery_marshal::start_recovery_marshal_in_partition(
        context,
        provider,
        reporter,
        format!("stack-finalized-round-recovery-{test_id}"),
        recovery_block(0),
    )
    .await
}

/// Independent unbound-validator configuration for signer and restart scenarios.
pub(super) fn validator_signer_args(
    signing_key: std::path::PathBuf,
    evm_key: std::path::PathBuf,
) -> crate::args::ConsensusArgs {
    crate::args::ConsensusArgs {
        is_validator: true,
        signing_key: Some(signing_key),
        validator_evm_key: Some(evm_key),
        listen_address: std::net::SocketAddr::from(([127, 0, 0, 1], 30400)),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        evm_key_backend: "plaintext".to_string(),
        evm_passphrase: None,
        reth_key_backend: "plaintext".to_string(),
        reth_passphrase: None,
        tee_enclave_socket: None,
        tee_session_mode: crate::args::TeeSessionMode::PolicyDefault,
        tee_bootstrap_timeout_secs: 60,
        tee_canary_interval_secs: 30,
        tee_canary_failure_threshold: 3,
        txpool_pending_staleness_secs: 600,
        radicle_control_socket: None,
        radicle_status_address: None,
        upstream: None,
        upstream_nocertify: false,
    }
}
