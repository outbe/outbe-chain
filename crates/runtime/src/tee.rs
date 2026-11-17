use super::*;
mod in_process;
use crate::errors::LaunchError;

pub(crate) async fn prepare(
    config: &LaunchConfig,
    args: &ConsensusArgs,
    node_data_dir: PathBuf,
    initial_tee_policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    cancellation: &tokio_util::sync::CancellationToken,
) -> eyre::Result<(
    Option<Arc<OutbeEvmSigner>>,
    outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    Option<crate::admission::LocalTeeAdmissionAnchorV1>,
    Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
)> {
    let evm_key = args.effective_validator_evm_key()?;
    if args.is_validator && evm_key.is_none() {
        return Err(LaunchError::MissingValidatorEvmSignerKey.into());
    }
    let evm_signer = if let Some(evm_key_path) = evm_key {
        let backend = args.evm_key_backend()?;
        let signer = Arc::new(
            outbe_consensus::bls::load_validator_evm_signer(&evm_key_path, &backend)
                .wrap_err_with(|| {
                    format!(
                        "failed to load validator EVM key from {}",
                        evm_key_path.display()
                    )
                })?,
        );
        info!(
            address = %signer.address(),
            path = %evm_key_path.display(),
            "loaded validator EVM signer"
        );
        Some(signer)
    } else {
        None
    };
    let validator_evm_address = evm_signer.as_ref().map(|signer| signer.address());
    // Keyless replay also needs the process network binding.
    outbe_tee::call_context::set_snapshot(outbe_tee::call_context::EnclaveCallContextV1 {
        chain_id: config.chain.chain().id(),
        genesis_hash: config.chain.genesis_hash(),
        ..Default::default()
    })?;
    if keyless_follower_launch(args) {
        install_keyless_replay_policy()?;
        let (_node_host_signing, reth_p2p_public) =
            load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
        info!("keyless follower: resident admission and offer-key checks skipped");
        return Ok((
            evm_signer,
            outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
                reth_p2p_public,
                expected_enclave_id: None,
                validator: validator_evm_address,
            },
            None,
            None,
        ));
    }
    install_enclave_backed_policy()?;
    let identity = in_process::initialize(config, &node_data_dir, initial_tee_policy)?;
    let (anchor, supervisor) = in_process::prepare_supervisor(
        (config, args),
        (&node_data_dir, initial_tee_policy),
        evm_signer.as_ref(),
        cancellation,
    )
    .await?;
    Ok((
        evm_signer,
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
            validator: validator_evm_address,
            ..identity
        },
        anchor,
        supervisor,
    ))
}

pub(crate) struct TeeRuntimeWorkers {
    pub(crate) finalization_notify: Arc<tokio::sync::Notify>,
    pub(crate) tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
    /// Keeps the keyless armed-watch open for the launcher lifetime. The
    /// enclave path moves its sender into the lease guard, so this is `None`.
    pub(crate) tee_lease_armed_tx: Option<tokio::sync::watch::Sender<bool>>,
    pub(crate) tee_lease_guard_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) tee_canary_handle: Option<tokio::task::JoinHandle<()>>,
}

/// A replay follower is not a validator and has no explicit EVM identity.
pub(crate) fn keyless_follower_launch(args: &ConsensusArgs) -> bool {
    !args.is_validator
        && args
            .effective_validator_evm_key()
            .is_ok_and(|key| key.is_none())
}

/// `None` and `ReplayOnly` are accepted. `None` means the caller installs
/// `ReplayOnly`. Any other installed policy is a launch error.
fn keyless_replay_policy_ok(
    current: Option<outbe_tee::witness::WitnessPolicy>,
) -> eyre::Result<bool> {
    match current {
        Some(outbe_tee::witness::WitnessPolicy::ReplayOnly) => Ok(false),
        None => Ok(true),
        Some(policy) => Err(eyre::eyre!(
            "keyless follower requires WitnessPolicy::ReplayOnly, found {policy:?}"
        )),
    }
}

fn install_keyless_replay_policy() -> eyre::Result<()> {
    if !keyless_replay_policy_ok(outbe_tee::witness::witness_policy())? {
        return Ok(());
    }
    outbe_tee::witness::install_witness_policy(outbe_tee::witness::WitnessPolicy::ReplayOnly)
        .or_else(|_| keyless_replay_policy_ok(outbe_tee::witness::witness_policy()).map(|_| ()))?;
    if outbe_tee::witness::witness_policy() != Some(outbe_tee::witness::WitnessPolicy::ReplayOnly) {
        eyre::bail!("keyless follower failed to install WitnessPolicy::ReplayOnly");
    }
    Ok(())
}

fn enclave_backed_policy_ok(
    current: Option<outbe_tee::witness::WitnessPolicy>,
) -> eyre::Result<bool> {
    match current {
        Some(outbe_tee::witness::WitnessPolicy::EnclaveBacked) => Ok(false),
        None => Ok(true),
        Some(policy) => Err(eyre::eyre!(
            "enclave-backed node requires WitnessPolicy::EnclaveBacked, found {policy:?}"
        )),
    }
}

fn install_enclave_backed_policy() -> eyre::Result<()> {
    if !enclave_backed_policy_ok(outbe_tee::witness::witness_policy())? {
        return Ok(());
    }
    outbe_tee::witness::install_witness_policy(outbe_tee::witness::WitnessPolicy::EnclaveBacked)
        .or_else(|_| enclave_backed_policy_ok(outbe_tee::witness::witness_policy()).map(|_| ()))?;
    if outbe_tee::witness::witness_policy()
        != Some(outbe_tee::witness::WitnessPolicy::EnclaveBacked)
    {
        eyre::bail!("enclave-backed node failed to install WitnessPolicy::EnclaveBacked");
    }
    Ok(())
}

fn keyless_runtime_workers() -> TeeRuntimeWorkers {
    let (tee_lease_armed_tx, tee_lease_armed_rx) = tokio::sync::watch::channel(false);
    TeeRuntimeWorkers {
        finalization_notify: Arc::new(tokio::sync::Notify::new()),
        tee_lease_armed_rx,
        tee_lease_armed_tx: Some(tee_lease_armed_tx),
        tee_lease_guard_handle: None,
        tee_canary_handle: None,
    }
}

pub(crate) fn start_runtime_workers(
    (protocol, supervisor): (
        Arc<crate::OutbeRuntime>,
        Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
    ),
    (node, args, evm_signer): (&OutbeFullNode, &ConsensusArgs, Option<&Arc<OutbeEvmSigner>>),
    (proof_chain_id, genesis_hash, node_data_dir): (u64, alloy_primitives::B256, &Path),
    (local_tee_identity, tee_admission_anchor, initial_tee_policy): (
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
        Option<crate::admission::LocalTeeAdmissionAnchorV1>,
        &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    ),
    (tee_canary_status, authority, shutdown_token, shutdown): (
        outbe_tee::TeeEnclaveHealthChannel,
        outbe_tee::TeeAuthorityChannel,
        tokio_util::sync::CancellationToken,
        outbe_execution::shutdown::NodeShutdown,
    ),
) -> eyre::Result<TeeRuntimeWorkers> {
    if keyless_follower_launch(args) {
        info!("keyless follower: skipping the TEE lease guard, lifecycle supervisor, and canary");
        return Ok(keyless_runtime_workers());
    }
    let chain = outbe_execution::tee_remote_session::RegistryChainIdentity {
        chain_id: proof_chain_id,
        genesis_hash,
    };
    let context = outbe_tee::in_process_tee_context()
        .ok_or(in_process::AutonomousStartupError::MissingContext)?;
    if args.is_validator
        && args.upstream.is_none()
        && node.rpc_server_handles.rpc.http_local_addr().is_none()
    {
        tracing::warn!(
            "validator has no upstream or local HTTP RPC endpoint; autonomous founder lease renewal is unavailable. Enable local HTTP RPC (--http) to allow the lifecycle supervisor to renew"
        );
    }
    // Founding validators acquire their key at block one, after local RPC starts.
    let supervisor = if supervisor.is_none() {
        in_process::local_lifecycle_endpoint(
            args,
            evm_signer.is_some(),
            node.rpc_server_handles.rpc.http_local_addr(),
        )
        .zip(evm_signer)
        .map(|(endpoint, signer)| {
            in_process::new_supervisor(
                &node.config,
                node_data_dir,
                initial_tee_policy,
                signer,
                &endpoint,
            )
        })
        .transpose()?
    } else {
        supervisor
    };
    let gate = if args.upstream.is_none()
        && args.is_validator
        && node
            .provider
            .finalized_block_num_hash()?
            .is_none_or(|head| head.number == 0)
    {
        in_process::founder_lease_gate(protocol.as_ref(), node, args, genesis_hash)?
    } else {
        tee_admission_anchor.map_or(
            TeeLeaseGuardGateV1::AwaitingAnchor,
            TeeLeaseGuardGateV1::Pending,
        )
    };
    let finalization_notify = Arc::new(tokio::sync::Notify::new());
    let (tee_lease_armed_tx, tee_lease_armed_rx) = tokio::sync::watch::channel(false);
    let owner = Arc::new(crate::admission_authority::live::LiveTeeRuntimeV1::new(
        crate::admission_authority::live::LiveTeeSnapshotV1 {
            context: Arc::clone(&context),
            identity: local_tee_identity,
            gate,
        },
        authority,
    )?);
    let guard_config = crate::admission::TeeLeaseGuardConfigV1 {
        chain,
        finalization_notify: finalization_notify.clone(),
        armed: tee_lease_armed_tx,
    };
    in_process::bind_activation_refresh(
        &protocol,
        node.provider.clone(),
        guard_config.clone(),
        owner.clone(),
    )?;
    outbe_consensus::protocol_state::EngineProtocolState::refresh_tee_authority(protocol.as_ref())?;
    let supervisor_owner = owner.clone();
    let lease_check = crate::admission_authority::guard::run_lease_guard(
        node.provider.clone(),
        Arc::clone(&protocol) as Arc<dyn outbe_consensus::protocol_state::EngineProtocolState>,
        guard_config,
        owner,
        shutdown_token.clone(),
    );
    let supervisor_provider = node.provider.clone();
    let supervisor_protocol = protocol.clone();
    let supervisor_shutdown = shutdown_token.clone();
    let supervisor_finalized = finalization_notify.clone();
    let tee_lease_guard_handle = tokio::spawn(shutdown.track_task(
        "TEE lease guard and lifecycle",
        async move {
            let lifecycle = async move {
                if let Some(supervisor) = supervisor {
                    supervisor
                        .run(
                            supervisor_owner,
                            (supervisor_provider, supervisor_protocol),
                            supervisor_shutdown,
                            supervisor_finalized,
                        )
                        .await;
                }
            };
            let (guard, ()) = tokio::join!(lease_check, lifecycle);
            if let Err(error) = guard {
                tracing::error!(%error, "TEE lease guard failed closed");
            }
        },
    ));

    // Periodic resident canary is diagnostic only. `0` disables it.
    let canary_config = outbe_execution::tee_canary::TeeCanaryConfig {
        interval: std::time::Duration::from_secs(args.tee_canary_interval_secs),
        failure_threshold: args.tee_canary_failure_threshold,
    };
    let tee_canary_handle = (args.tee_canary_interval_secs > 0).then(|| {
        tokio::spawn(shutdown.track_task("TEE canary worker", async move {
            crate::tee_canary_probe::TributeCanaryProbe::run_in_process_worker(
                context,
                canary_config,
                tee_canary_status,
                shutdown_token,
            )
            .await;
        }))
    });

    Ok(TeeRuntimeWorkers {
        finalization_notify,
        tee_lease_armed_rx,
        tee_lease_armed_tx: None,
        tee_lease_guard_handle: Some(tee_lease_guard_handle),
        tee_canary_handle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct ArgsCli {
        #[command(flatten)]
        consensus: ConsensusArgs,
    }

    fn parsed_args(extra: &[&str]) -> ConsensusArgs {
        let mut command = vec!["test"];
        command.extend_from_slice(extra);
        ArgsCli::try_parse_from(command)
            .expect("consensus args parse")
            .consensus
    }

    #[test]
    fn keyless_launch_requires_no_validator_key() {
        assert!(keyless_follower_launch(&parsed_args(&[])));
        assert!(!keyless_follower_launch(&parsed_args(&[
            "--validator.evm-key",
            "/tmp/key.json"
        ])));
        let mut validator = parsed_args(&[]);
        validator.is_validator = true;
        assert!(!keyless_follower_launch(&validator));
    }

    #[test]
    fn keyless_replay_policy_accepts_only_replay_only() {
        use outbe_tee::witness::WitnessPolicy;
        assert!(keyless_replay_policy_ok(None).unwrap());
        assert!(!keyless_replay_policy_ok(Some(WitnessPolicy::ReplayOnly)).unwrap());
        let err = keyless_replay_policy_ok(Some(WitnessPolicy::EnclaveBacked))
            .expect_err("enclave policy");
        assert!(err.to_string().contains("ReplayOnly"));
    }

    #[test]
    fn enclave_backed_policy_accepts_only_enclave_backed() {
        use outbe_tee::witness::WitnessPolicy;
        assert!(enclave_backed_policy_ok(None).unwrap());
        assert!(!enclave_backed_policy_ok(Some(WitnessPolicy::EnclaveBacked)).unwrap());
        let err =
            enclave_backed_policy_ok(Some(WitnessPolicy::ReplayOnly)).expect_err("replay policy");
        assert!(err.to_string().contains("EnclaveBacked"));
    }

    #[test]
    fn founding_lifecycle_uses_local_http_only_for_resident_keyed_launch() {
        let local = "127.0.0.1:8545".parse().unwrap();
        let mut args = parsed_args(&[]);
        args.is_validator = true;
        assert_eq!(
            in_process::local_lifecycle_endpoint(&args, true, Some(local)),
            Some("http://127.0.0.1:8545".into())
        );
        assert_eq!(
            in_process::local_lifecycle_endpoint(&args, true, None),
            None
        );
        assert_eq!(
            in_process::local_lifecycle_endpoint(&args, false, Some(local)),
            None
        );
        args.upstream = Some("http://upstream:8545".into());
        assert_eq!(
            in_process::local_lifecycle_endpoint(&args, true, Some(local)),
            None
        );
    }

    #[test]
    fn keyless_workers_have_no_lease_guard() {
        let workers = keyless_runtime_workers();
        assert!(workers.tee_lease_guard_handle.is_none());
        assert!(workers.tee_canary_handle.is_none());
        assert!(!*workers.tee_lease_armed_rx.borrow());
        assert!(workers.tee_lease_armed_tx.is_some());
    }
}
