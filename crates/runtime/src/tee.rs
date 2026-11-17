use super::*;
mod in_process;
mod legacy_admission;
use crate::errors::{display_error, LaunchError};
use legacy_admission::*;

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
    // Chain id and genesis are process context. A keyless follower still
    // records them. It does not open a socket to do so.
    outbe_tee::call_context::set_snapshot(outbe_tee::call_context::EnclaveCallContextV1 {
        chain_id: config.chain.chain().id(),
        genesis_hash: config.chain.genesis_hash(),
        ..Default::default()
    })?;
    if keyless_follower_launch(args) {
        install_keyless_replay_policy()?;
        let (_node_host_signing, reth_p2p_public) =
            load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
        info!("keyless follower: enclave socket not configured; admission and offer-key checks skipped");
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
    if args.tee_enclave_socket.is_none() {
        install_enclave_backed_policy()?;
        let identity = in_process::initialize(config, &node_data_dir, initial_tee_policy)?;
        let (anchor, supervisor) = in_process::prepare_supervisor(
            (config, args),
            (&node_data_dir, initial_tee_policy),
            evm_signer.as_ref(),
            cancellation,
        )
        .await?;
        return Ok((
            evm_signer,
            outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
                validator: validator_evm_address,
                ..identity
            },
            anchor,
            supervisor,
        ));
    }
    // Every network declares exactly one attestation policy in genesis. The
    // local session protocol is an independent, explicit operator choice:
    // NoAttestationDev may use either the development transport or a real
    // SGX, production NodeHost session. There is no connection fallback.
    // A validator with no socket still fails here. A socket, validator or
    // TEE fullnode, keeps the checks below.
    let socket = args
        .tee_enclave_socket
        .clone()
        .ok_or(LaunchError::MissingTeeEnclaveSocket {
            mode: initial_tee_policy.attestation_mode,
        })?;
    install_enclave_backed_policy()?;
    let endpoint = socket
        .to_str()
        .ok_or(LaunchError::TeeEnclaveEndpointNotUtf8)?;
    let tee_session = args
        .tee_session_mode
        .resolve(initial_tee_policy.attestation_mode)?;
    let (node_host_signing, reth_p2p_public) =
        load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
    let expected_enclave_id = match tee_session {
        outbe_consensus::args::ResolvedTeeSession::ProductionNodeHost => {
            use k256::ecdsa::signature::hazmat::PrehashSigner as _;

            let client = outbe_tee::connect_or_initialize_node_host_enclave(
                endpoint,
                &node_data_dir,
                outbe_tee::NodeHostIdentityV1 {
                    network_binding: initial_tee_policy.network_binding(),
                    reth_p2p_public,
                },
                |hash| -> Result<[u8; 65], k256::ecdsa::Error> {
                    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
                        node_host_signing.sign_prehash(hash.as_slice())?;
                    let mut bytes = [0_u8; 65];
                    bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
                    bytes[64] = recovery.to_byte();
                    Ok(bytes)
                },
            )
            .wrap_err("NodeHost enclave initialization failed")?;
            // Session material for reconnect-with-identity-revalidation:
            // loaded once here (takes the NodeHost file lock), never in the
            // request hot path.
            let (manifest, node_host) =
                outbe_tee::node_host::committed_node_host_session_material(&node_data_dir)
                    .wrap_err("committed NodeHost session material load failed")?;
            let enclave_id = manifest.enclave_id().map_err(|error| {
                LaunchError::DeriveCommittedEnclaveIdentity {
                    details: display_error(error),
                }
            })?;
            outbe_tee::install_authorized_enclave_client(
                client,
                endpoint.to_owned(),
                node_data_dir.clone(),
                manifest,
                node_host,
            )
            .wrap_err("enclave session install failed")?;
            Some(enclave_id)
        }
        outbe_consensus::args::ResolvedTeeSession::Development => {
            let client = outbe_tee::EnclaveClient::connect_endpoint(endpoint)
                .wrap_err("development enclave connection failed")?;
            let enclave_id = in_process::sidecar_enclave_id(client.quote())?;
            outbe_tee::install_enclave_client(client, endpoint.to_owned())
                .wrap_err("enclave session install failed")?;
            Some(enclave_id)
        }
    };
    let local_tee_identity = outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
        reth_p2p_public,
        expected_enclave_id,
        validator: validator_evm_address,
    };
    info!(
        socket = %socket.display(),
        node_host_identity = "reth-p2p-secp256k1",
        attestation_mode = ?initial_tee_policy.attestation_mode,
        session_mode = ?tee_session,
        "mandatory TEE enclave sidecar connected before execution launch",
    );

    let tee_admission_anchor = if args.is_validator {
        if matches!(
            tee_session,
            outbe_consensus::args::ResolvedTeeSession::ProductionNodeHost
        ) {
            outbe_tee::load_finalized_join_admission_anchor(&node_data_dir)
                .wrap_err("load durable validator join admission anchor")?
                .map(|durable| {
                    validator_admission_anchor_from_durable_v1(
                        durable,
                        config.chain.chain().id(),
                        config.chain.genesis_hash(),
                        local_tee_identity,
                    )
                })
                .transpose()?
        } else {
            None
        }
    } else {
        // A follower re-executes every protected transaction and therefore must
        // already hold the exact permanent offer key committed by the running
        // chain. Prove that invariant before Reth opens networking, RPC, sync or
        // execution. Losing the key is terminal for this node identity: startup
        // never invokes recovery, replacement or another bootstrap path.
        let upstream = args
            .upstream
            .as_deref()
            .ok_or(LaunchError::FullNodeRequiresUpstreamForOfferKey)?;
        let admission_anchor =
            require_upstream_fullnode_tee_admission(upstream, local_tee_identity).await?;
        require_upstream_offer_key_v1(upstream).await?;
        Some(admission_anchor)
    };

    Ok((evm_signer, local_tee_identity, tee_admission_anchor, None))
}

pub(crate) struct TeeRuntimeWorkers {
    pub(crate) finalization_notify: Arc<tokio::sync::Notify>,
    pub(crate) tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
    /// Keeps the keyless armed-watch open for the launcher lifetime. The
    /// enclave path moves its sender into the lease guard, so this is `None`.
    pub(crate) tee_lease_armed_tx: Option<tokio::sync::watch::Sender<bool>>,
    pub(crate) tee_lease_guard_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) tee_canary_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) upgrade_promotion: Arc<tokio::sync::Notify>,
    pub(crate) upgrade_handle: Option<tokio::task::JoinHandle<()>>,
}

/// A replay follower has neither an explicit EVM identity nor a sidecar.
pub(crate) fn keyless_follower_launch(args: &ConsensusArgs) -> bool {
    !args.is_validator
        && args.tee_enclave_socket.is_none()
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
        upgrade_promotion: Arc::new(tokio::sync::Notify::new()),
        upgrade_handle: None,
    }
}

pub(crate) async fn start_runtime_workers(
    (protocol, session, supervisor): (
        Arc<crate::OutbeRuntime>,
        Arc<dyn outbe_execution::tee_remote_session::TeeSessionStateReader>,
        Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
    ),
    (node, args): (&OutbeFullNode, &ConsensusArgs),
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
        info!("keyless follower: skipping TEE lease guard, canary, and upgrade watcher");
        return Ok(keyless_runtime_workers());
    }
    let chain = outbe_execution::tee_remote_session::RegistryChainIdentity {
        chain_id: proof_chain_id,
        genesis_hash,
    };
    let context = outbe_tee::in_process_tee_context();
    let anchors = if context.is_some()
        && (args.upstream.is_some()
            || !args.is_validator
            || node
                .provider
                .finalized_block_num_hash()?
                .is_some_and(|head| head.number > 0))
    {
        crate::admission::TeeLeaseGuardAnchorsV1 {
            gate: tee_admission_anchor.map_or(
                TeeLeaseGuardGateV1::AwaitingAnchor,
                TeeLeaseGuardGateV1::Pending,
            ),
            durable: None,
        }
    } else {
        let mut anchors = initial_tee_lease_gate_v1(
            (protocol.as_ref(), node, args),
            (proof_chain_id, genesis_hash),
            local_tee_identity,
            tee_admission_anchor,
        )
        .await?;
        if context.is_none() {
            verify_initial_local_tee_admission_v1(
                (protocol.as_ref(), node, args),
                chain,
                local_tee_identity,
                &mut anchors,
                node_data_dir,
            )?;
        }
        anchors
    };
    let finalization_notify = Arc::new(tokio::sync::Notify::new());
    let (tee_lease_armed_tx, tee_lease_armed_rx) = tokio::sync::watch::channel(false);
    let owner = context
        .as_ref()
        .map(|context| {
            crate::admission_authority::live::LiveTeeRuntimeV1::new(
                crate::admission_authority::live::LiveTeeSnapshotV1 {
                    context: Arc::clone(context),
                    identity: local_tee_identity,
                    anchors,
                },
                authority.clone(),
            )
            .map(Arc::new)
        })
        .transpose()?;
    let guard_config = crate::admission::TeeLeaseGuardConfigV1 {
        chain,
        identity: local_tee_identity,
        anchors,
        finalization_notify: finalization_notify.clone(),
        armed: tee_lease_armed_tx,
    };
    in_process::bind_activation_refresh(
        &protocol,
        node.provider.clone(),
        guard_config.clone(),
        (authority.clone(), owner.clone()),
    )?;
    outbe_consensus::protocol_state::EngineProtocolState::refresh_tee_authority(protocol.as_ref())?;
    let lease_check = crate::admission_authority::guard::run_lease_guard(
        node.provider.clone(),
        (
            Arc::clone(&protocol) as Arc<dyn outbe_consensus::protocol_state::EngineProtocolState>,
            Arc::clone(&session),
        ),
        guard_config,
        (authority, owner.clone()),
        shutdown_token.clone(),
    );
    let supervisor_owner = owner.clone();
    let supervisor_provider = node.provider.clone();
    let supervisor_protocol = protocol.clone();
    let supervisor_shutdown = shutdown_token.clone();
    let supervisor_finalized = finalization_notify.clone();
    let tee_lease_guard_handle = tokio::spawn(shutdown.track_task(
        "TEE lease guard and lifecycle",
        async move {
            let lifecycle = async move {
                if let (Some(supervisor), Some(owner)) = (supervisor, supervisor_owner) {
                    supervisor
                        .run(
                            owner,
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

    // Periodic enclave canary (signal only): known-plaintext decrypt +
    // Health telemetry through the process-global session. `0` disables.
    let canary_config = outbe_execution::tee_canary::TeeCanaryConfig {
        interval: std::time::Duration::from_secs(args.tee_canary_interval_secs),
        failure_threshold: args.tee_canary_failure_threshold,
    };
    let tee_canary_handle = (args.tee_canary_interval_secs > 0).then(|| {
        tokio::spawn(shutdown.track_task("TEE canary worker", async move {
            let config = canary_config;
            if let Some(context) = context {
                crate::tee_canary_probe::TributeCanaryProbe::run_in_process_worker(
                    context,
                    config,
                    tee_canary_status,
                    shutdown_token,
                )
                .await;
            } else {
                outbe_execution::tee_canary::run_tee_canary_worker(
                    outbe_execution::tee_canary::TeeCanaryTarget::new(
                        outbe_execution::tee_canary::GlobalEnclaveRequester,
                        crate::tee_canary_probe::TributeCanaryProbe,
                    ),
                    config,
                    tee_canary_status,
                    shutdown_token,
                )
                .await;
            }
        }))
    });

    let upgrade_promotion = Arc::new(tokio::sync::Notify::new());
    let upgrade_handle = if owner.is_none()
        && initial_tee_policy.attestation_mode
            == outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired
    {
        let provider = node.provider.clone();
        let promoted = upgrade_promotion.clone();
        Some(tokio::spawn(shutdown.track_task(
            "enclave-upgrade watcher",
            run_upgrade_promotion_worker_v1(
                provider,
                Arc::clone(&session),
                UpgradePromotionWorkerConfigV1 {
                    chain_id: proof_chain_id,
                    genesis_hash,
                    node_data_dir: node_data_dir.to_path_buf(),
                    poll_secs: TEE_UPGRADE_POLL_SECS,
                    warning_blocks: TEE_UPGRADE_WARNING_BLOCKS,
                    critical_blocks: TEE_UPGRADE_CRITICAL_BLOCKS,
                    promoted,
                },
                owner.map(|owner| {
                    (
                        owner,
                        Arc::clone(&protocol)
                            as Arc<dyn outbe_consensus::protocol_state::EngineProtocolState>,
                    )
                }),
            ),
        )))
    } else {
        None
    };

    Ok(TeeRuntimeWorkers {
        finalization_notify,
        tee_lease_armed_rx,
        tee_lease_armed_tx: None,
        tee_lease_guard_handle: Some(tee_lease_guard_handle),
        tee_canary_handle,
        upgrade_promotion,
        upgrade_handle,
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
    fn keyless_launch_requires_no_validator_key_or_socket() {
        assert!(keyless_follower_launch(&parsed_args(&[])));
        assert!(!keyless_follower_launch(&parsed_args(&[
            "--validator.evm-key",
            "/tmp/key.json"
        ])));
        assert!(!keyless_follower_launch(&parsed_args(&[
            "--tee-enclave-socket",
            "/tmp/tee.sock",
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
    fn keyless_workers_have_no_lease_guard() {
        let workers = keyless_runtime_workers();
        assert!(workers.tee_lease_guard_handle.is_none());
        assert!(workers.tee_canary_handle.is_none());
        assert!(workers.upgrade_handle.is_none());
        assert!(!*workers.tee_lease_armed_rx.borrow());
        assert!(workers.tee_lease_armed_tx.is_some());
    }
}
