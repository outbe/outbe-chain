use super::*;
mod in_process;
use crate::errors::LaunchError;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
pub(crate) static PREPARE_ENTRIES: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static REGISTER_ENCLAVE_SENDS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_register_enclave_payload(raw: &[u8]) {
    #[cfg(test)]
    {
        use alloy_sol_types::SolCall;
        let selector =
            outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::registerEnclaveCall::SELECTOR;
        if raw.windows(selector.len()).any(|window| window == selector) {
            REGISTER_ENCLAVE_SENDS.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[cfg(not(test))]
    {
        let _ = raw;
    }
}

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
    #[cfg(test)]
    PREPARE_ENTRIES.fetch_add(1, Ordering::SeqCst);
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
    let mut identity = identity;
    if let Some(context) = outbe_tee::in_process_tee_context() {
        identity.expected_enclave_id = Some(context.manifest().enclave_id()?);
    }
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

pub(crate) struct FoundingSlotRequest<'a> {
    pub rpc: &'a reth_node_core::args::RpcServerArgs,
    pub upstream: Option<&'a str>,
    pub evm_signer: Option<&'a Arc<OutbeEvmSigner>>,
    pub is_validator: bool,
}

/// Choose the founding supervisor from the launch slot.
///
/// Production calls this from [`start_runtime_workers`] with `node.config.rpc`
/// (`rpc.http == false`), `args.upstream == None`, and `evm_signer == Some`.
pub(crate) fn founding_supervisor_from_launch_slot<F>(
    existing: Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
    founding_slot: &std::sync::Mutex<Option<jsonrpsee::Methods>>,
    request: FoundingSlotRequest<'_>,
    build: F,
) -> eyre::Result<Option<crate::tee_supervisor::TeeLifecycleSupervisor>>
where
    F: FnOnce(
        jsonrpsee::Methods,
        &Arc<OutbeEvmSigner>,
    ) -> eyre::Result<crate::tee_supervisor::TeeLifecycleSupervisor>,
{
    if let Some(supervisor) = existing {
        return Ok(Some(supervisor));
    }
    let methods = founding_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let FoundingSlotRequest {
        rpc,
        upstream,
        evm_signer,
        is_validator,
    } = request;
    // Operator HTTP off, no upstream, and an EVM signer: the launch slot is the module.
    if !rpc.http && upstream.is_none() {
        if let (Some(methods), Some(signer)) = (methods, evm_signer) {
            return Ok(Some(build(methods, signer)?));
        }
    } else if let (Some(methods), Some(signer)) = (methods, evm_signer) {
        return Ok(Some(build(methods, signer)?));
    }
    if is_validator && upstream.is_none() && evm_signer.is_some() {
        return Err(in_process::AutonomousStartupError::MissingLocalLifecycleRpc.into());
    }
    Ok(None)
}

pub(crate) fn start_runtime_workers(
    (protocol, supervisor, founding_slot): (
        Arc<crate::OutbeRuntime>,
        Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
        &std::sync::Mutex<Option<jsonrpsee::Methods>>,
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
    // Founding renewal uses the in-process module for every HTTP, JWT, and instance setting.
    let fresh_supervisor = supervisor.is_none();
    let http_off = !node.config.rpc.http;
    let no_upstream = args.upstream.is_none();
    let launch_slot = fresh_supervisor && http_off && no_upstream;
    let validator_signer = args.is_validator && evm_signer.is_some();
    let direct_founding = launch_slot && validator_signer;
    let slot_ready = founding_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some();
    if direct_founding && !slot_ready {
        return Err(in_process::AutonomousStartupError::MissingLocalLifecycleRpc.into());
    }
    let founding_resident = if direct_founding {
        Some(
            in_process::founding_resident(&node.config, node_data_dir, initial_tee_policy)
                .map_err(|error| eyre::Report::new(*error))?,
        )
    } else {
        None
    };
    let supervisor = if founding_resident.is_some() {
        None
    } else {
        founding_supervisor_from_launch_slot(
            supervisor,
            founding_slot,
            FoundingSlotRequest {
                rpc: &node.config.rpc,
                upstream: args.upstream.as_deref(),
                evm_signer,
                is_validator: args.is_validator,
            },
            |methods, signer| {
                in_process::new_local_supervisor(
                    &node.config,
                    node_data_dir,
                    initial_tee_policy,
                    signer,
                    methods,
                )
                .map_err(|error| eyre::Report::new(*error))
            },
        )?
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
    let lifecycle = if let Some(resident) = founding_resident {
        spawn_founding_validator_supervisor(
            founding_slot,
            FoundingSlotRequest {
                rpc: &node.config.rpc,
                upstream: None,
                evm_signer: Some(evm_signer.expect("direct founding carries the EVM signer")),
                is_validator: true,
            },
            resident,
            FoundingRun {
                owner: supervisor_owner,
                provider: supervisor_provider,
                protocol: supervisor_protocol,
                shutdown: supervisor_shutdown,
                finalized: supervisor_finalized,
            },
            &shutdown,
        )?
    } else {
        spawn_supervisor_run(
            supervisor,
            FoundingRun {
                owner: supervisor_owner,
                provider: supervisor_provider,
                protocol: supervisor_protocol,
                shutdown: supervisor_shutdown,
                finalized: supervisor_finalized,
            },
            &shutdown,
        )
    };
    let tee_lease_guard_handle = tokio::spawn(shutdown.track_task(
        "TEE lease guard and lifecycle",
        async move {
            let lifecycle = async move {
                if let Err(join_err) = lifecycle.await {
                    if join_err.is_panic() {
                        std::panic::resume_unwind(join_err.into_panic());
                    }
                    tracing::error!(%join_err, "TEE lifecycle task failed");
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

pub(crate) struct FoundingResident {
    pub node_signing: k256::ecdsa::SigningKey,
    pub live: Arc<outbe_tee::in_process::InProcessTeeContext>,
    pub node_data_dir: PathBuf,
    pub descriptor: outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1,
}

pub(crate) struct FoundingRun<P> {
    pub owner: Arc<crate::admission_authority::live::LiveTeeRuntimeV1>,
    pub provider: P,
    pub protocol: Arc<crate::OutbeRuntime>,
    pub shutdown: tokio_util::sync::CancellationToken,
    pub finalized: Arc<tokio::sync::Notify>,
}

/// Build the founding supervisor from the launch slot and spawn `supervisor.run`.
///
/// Production startup uses the same constructor and the same spawn. The slot is
/// the in-process module when operator HTTP is off, upstream is absent, and the
/// node is a validator.
pub(crate) fn spawn_founding_validator_supervisor<P>(
    founding_slot: &std::sync::Mutex<Option<jsonrpsee::Methods>>,
    request: FoundingSlotRequest<'_>,
    resident: FoundingResident,
    run: FoundingRun<P>,
    shutdown: &outbe_execution::shutdown::NodeShutdown,
) -> eyre::Result<tokio::task::JoinHandle<()>>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    let FoundingResident {
        node_signing,
        live,
        node_data_dir,
        descriptor,
    } = resident;
    let supervisor =
        founding_supervisor_from_launch_slot(None, founding_slot, request, |methods, signer| {
            in_process::construct_in_process_supervisor(
                methods,
                signer,
                FoundingResident {
                    node_signing,
                    live,
                    node_data_dir,
                    descriptor,
                },
            )
            .map_err(|error| eyre::Report::new(*error))
        })?
        .ok_or(in_process::AutonomousStartupError::MissingLocalLifecycleRpc)?;
    Ok(spawn_supervisor_run(Some(supervisor), run, shutdown))
}

fn spawn_supervisor_run<P>(
    supervisor: Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
    run: FoundingRun<P>,
    shutdown: &outbe_execution::shutdown::NodeShutdown,
) -> tokio::task::JoinHandle<()>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    tokio::spawn(shutdown.track_task("TEE lifecycle supervisor", async move {
        if let Some(supervisor) = supervisor {
            supervisor
                .run(
                    run.owner,
                    (run.provider, run.protocol),
                    run.shutdown,
                    run.finalized,
                )
                .await;
        }
    }))
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
    fn founding_validator_without_http_spawns_in_process_supervisor() {
        let context = crate::admission_authority::test_support::context(4);
        let descriptor = outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1 {
            network_binding: context.manifest().network_binding(),
            genesis_consensus_keys: vec![],
        };
        let root = tempfile::tempdir().unwrap();
        crate::tee_supervisor::TeeLifecycleSupervisor::new_in_process(
            jsonrpsee::Methods::new(),
            (
                Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x11; 32]).unwrap()),
                k256::ecdsa::SigningKey::from_slice(&[0x21; 32]).unwrap(),
            ),
            (context, root.path().to_path_buf(), descriptor),
        )
        .expect("founding validator spawns TeeLifecycleSupervisor without a socket");
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
