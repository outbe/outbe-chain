use super::*;

pub(super) async fn prepare(
    config: &LaunchConfig,
    args: &ConsensusArgs,
    node_data_dir: PathBuf,
    initial_tee_policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> eyre::Result<(
    Option<Arc<OutbeEvmSigner>>,
    outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    Option<crate::launch::admission::LocalTeeAdmissionAnchorV1>,
)> {
    let evm_signer = if args.is_validator {
        let evm_key_path = args
            .effective_validator_evm_key()?
            .ok_or_else(|| eyre::eyre!("validator mode requires an EVM signer key"))?;
        let signer = Arc::new(
            outbe_primitives::signer::load::from_file(&evm_key_path).wrap_err_with(|| {
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
    // Every network declares exactly one attestation policy in genesis. The
    // local session protocol is an independent, explicit operator choice:
    // GramineDirectDev may use either the development transport or a real
    // SGX, production NodeHost session. There is no connection fallback.
    let socket = args.tee_enclave_socket.clone().ok_or_else(|| {
        eyre::eyre!(
            "mandatory {:?} ChainSpec requires --tee-enclave-socket before node startup",
            initial_tee_policy.attestation_mode
        )
    })?;
    let endpoint = socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let tee_session = args
        .tee_session_mode
        .resolve(initial_tee_policy.attestation_mode)
        .map_err(eyre::Report::msg)?;
    let (node_host_signing, reth_p2p_public) =
        load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
    outbe_tee::call_context::set_snapshot(outbe_tee::call_context::EnclaveCallContextV1 {
        chain_id: config.chain.chain().id(),
        genesis_hash: config.chain.genesis_hash(),
        ..Default::default()
    })
    .map_err(eyre::Report::msg)?;
    let expected_enclave_id = match tee_session {
        outbe_engine::args::ResolvedTeeSession::ProductionNodeHost => {
            use k256::ecdsa::signature::hazmat::PrehashSigner as _;

            let client = outbe_tee::connect_or_initialize_node_host_enclave(
                endpoint,
                &node_data_dir,
                outbe_tee::NodeHostIdentityV1 {
                    network_binding: initial_tee_policy.network_binding(),
                    reth_p2p_public,
                },
                |hash| {
                    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
                        node_host_signing
                            .sign_prehash(hash.as_slice())
                            .map_err(|error| error.to_string())?;
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
            let enclave_id = manifest
                .enclave_id()
                .map_err(|error| eyre::eyre!("derive committed enclave identity: {error}"))?;
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
        outbe_engine::args::ResolvedTeeSession::Development => {
            let client = outbe_tee::EnclaveClient::connect_endpoint(endpoint)
                .wrap_err("development enclave connection failed")?;
            outbe_tee::install_enclave_client(client, endpoint.to_owned())
                .wrap_err("enclave session install failed")?;
            None
        }
    };
    let local_tee_identity = outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
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
        if expected_enclave_id.is_some() {
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
        let upstream = args.upstream.as_deref().ok_or_else(|| {
            eyre::eyre!("full-node startup requires --upstream to authenticate the chain offer key")
        })?;
        let admission_anchor =
            require_upstream_fullnode_tee_admission(upstream, local_tee_identity).await?;
        let expected_offer = outbe_engine::read_upstream_tribute_offer_public_key(upstream)
            .await
            .wrap_err("failed to read mandatory offer key from the selected upstream")?;
        if expected_offer.is_zero() {
            return Err(eyre::eyre!(
                "selected upstream has no mandatory OST3 offer key; refusing full-node startup"
            ));
        }
        let resident_offer = outbe_tee::resident_offer_public_key_v1()
            .wrap_err("failed to read the local enclave resident offer key")?;
        if resident_offer != expected_offer {
            return Err(eyre::eyre!(
                    "local enclave does not hold the selected chain's exact offer key; refusing execution startup (no recovery or fallback)"
                ));
        }
        info!(
            offer_public_key = %resident_offer,
            %upstream,
            "full-node resident offer key matched upstream before execution launch"
        );
        Some(admission_anchor)
    };

    Ok((evm_signer, local_tee_identity, tee_admission_anchor))
}

pub(super) struct TeeRuntimeWorkers {
    pub(super) tee_lease_exit_rx: tokio::sync::mpsc::UnboundedReceiver<eyre::Result<String>>,
    pub(super) tee_lease_guard_handle: tokio::task::JoinHandle<()>,
    pub(super) tee_canary_handle: Option<tokio::task::JoinHandle<()>>,
    pub(super) upgrade_promotion: Arc<tokio::sync::Notify>,
    pub(super) upgrade_handle: Option<tokio::task::JoinHandle<()>>,
}

pub(super) fn start_runtime_workers(
    (node, args): (&OutbeFullNode, &ConsensusArgs),
    (proof_chain_id, genesis_hash, node_data_dir): (u64, alloy_primitives::B256, &Path),
    (local_tee_identity, tee_admission_anchor, initial_tee_policy): (
        outbe_engine::validators::LocalTeeRuntimeIdentityV1,
        Option<crate::launch::admission::LocalTeeAdmissionAnchorV1>,
        &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    ),
    (tee_canary_status, shutdown_token, shutdown): (
        outbe_tee::TeeEnclaveHealthChannel,
        tokio_util::sync::CancellationToken,
        outbe_node::shutdown::NodeShutdown,
    ),
) -> eyre::Result<TeeRuntimeWorkers> {
    let validator_has_recovery_anchor = args.is_validator && tee_admission_anchor.is_some();
    let mut tee_lease_guard_gate = TeeLeaseGuardGateV1::new(tee_admission_anchor);
    if let Some(admission) = read_gated_finalized_local_tee_admission(
        &node.provider,
        proof_chain_id,
        genesis_hash,
        local_tee_identity,
        &mut tee_lease_guard_gate,
    )? {
        let rejection = if validator_has_recovery_anchor {
            validator_recovery_startup_admission_rejection(admission)
        } else {
            tee_lease_admission_rejection(admission)
        };
        if let Some(reason) = rejection {
            eyre::bail!("local node rejected by finalized TEE lease state: {reason}");
        }
    }
    require_validator_tee_recovery_complete_v1(
        args.is_validator,
        tee_lease_guard_gate,
        node_data_dir,
    )?;
    let (tee_lease_exit_tx, tee_lease_exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let lease_check = run_tee_lease_guard_v1(
        node.provider.clone(),
        crate::launch::admission::TeeLeaseGuardConfigV1 {
            chain: outbe_node::tee_remote_session::RegistryChainIdentity {
                chain_id: proof_chain_id,
                genesis_hash,
            },
            identity: local_tee_identity,
            gate: tee_lease_guard_gate,
        },
        shutdown_token.clone(),
    );
    let lease_outcome = shutdown.clone();
    let tee_lease_guard_handle =
        tokio::spawn(shutdown.track_task("TEE lease guard", async move {
            let verdict = match lease_check.await {
                Ok(None) => return,
                Ok(Some(reason)) => Ok(reason),
                Err(error) => Err(error),
            };
            // Publish an actual failure before notifying a launcher that may
            // concurrently choose a different exit branch or be cancelled.
            let reason = tee_lease_exit_reason(Some(verdict), &lease_outcome);
            let _ = tee_lease_exit_tx.send(Ok(reason));
        }));

    // Periodic enclave canary (signal only): known-plaintext decrypt +
    // Health telemetry through the process-global session. `0` disables.
    let tee_canary_handle = (args.tee_canary_interval_secs > 0).then(|| {
        tokio::spawn(shutdown.track_task(
            "TEE canary worker",
            outbe_node::tee_canary::run_tee_canary_worker(
                outbe_node::tee_canary::GlobalEnclaveRequester,
                outbe_node::tee_canary::TeeCanaryConfig {
                    interval: std::time::Duration::from_secs(args.tee_canary_interval_secs),
                    failure_threshold: args.tee_canary_failure_threshold,
                },
                tee_canary_status,
                shutdown_token,
            ),
        ))
    });

    let upgrade_promotion = Arc::new(tokio::sync::Notify::new());
    let upgrade_handle = if initial_tee_policy.attestation_mode
        == outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired
    {
        let provider = node.provider.clone();
        let promoted = upgrade_promotion.clone();
        Some(tokio::spawn(shutdown.track_task(
            "enclave-upgrade watcher",
            run_upgrade_promotion_worker_v1(
                provider,
                UpgradePromotionWorkerConfigV1 {
                    chain_id: proof_chain_id,
                    genesis_hash,
                    node_data_dir: node_data_dir.to_path_buf(),
                    poll_secs: TEE_UPGRADE_POLL_SECS,
                    warning_blocks: TEE_UPGRADE_WARNING_BLOCKS,
                    critical_blocks: TEE_UPGRADE_CRITICAL_BLOCKS,
                    promoted,
                },
            ),
        )))
    } else {
        None
    };

    Ok(TeeRuntimeWorkers {
        tee_lease_exit_rx,
        tee_lease_guard_handle,
        tee_canary_handle,
        upgrade_promotion,
        upgrade_handle,
    })
}

