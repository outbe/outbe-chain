use super::*;
use crate::errors::{display_error, LaunchError};

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
            .ok_or(LaunchError::MissingValidatorEvmSignerKey)?;
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
            outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
                reth_p2p_public,
                expected_enclave_id: None,
                validator: validator_evm_address,
            },
            None,
        ));
    }
    // Every network declares exactly one attestation policy in genesis. The
    // local session protocol is an independent, explicit operator choice:
    // GramineDirectDev may use either the development transport or a real
    // SGX, production NodeHost session. There is no connection fallback.
    // A validator with no socket still fails here. A socket, validator or
    // TEE fullnode, keeps the checks below.
    let socket = args
        .tee_enclave_socket
        .clone()
        .ok_or(LaunchError::MissingTeeEnclaveSocket {
            mode: initial_tee_policy.attestation_mode,
        })?;
    let endpoint = socket
        .to_str()
        .ok_or(LaunchError::TeeEnclaveEndpointNotUtf8)?;
    let tee_session = args
        .tee_session_mode
        .resolve(initial_tee_policy.attestation_mode)?;
    let (node_host_signing, reth_p2p_public) =
        load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
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
        let upstream = args
            .upstream
            .as_deref()
            .ok_or(LaunchError::FullNodeRequiresUpstreamForOfferKey)?;
        let admission_anchor =
            require_upstream_fullnode_tee_admission(upstream, local_tee_identity).await?;
        require_upstream_offer_key_v1(upstream).await?;
        Some(admission_anchor)
    };

    Ok((evm_signer, local_tee_identity, tee_admission_anchor))
}

/// Require that the local enclave holds the exact offer key of the selected
/// upstream. Losing the key is terminal for this node identity.
async fn require_upstream_offer_key_v1(upstream: &str) -> eyre::Result<()> {
    let expected_offer = outbe_engine::read_upstream_tribute_offer_public_key(upstream)
        .await
        .wrap_err("failed to read mandatory offer key from the selected upstream")?;
    if expected_offer.is_zero() {
        return Err(LaunchError::UpstreamMissingOst3OfferKey.into());
    }
    let resident_offer = outbe_tee::resident_offer_public_key_v1()
        .wrap_err("failed to read the local enclave resident offer key")?;
    if resident_offer != expected_offer {
        return Err(LaunchError::LocalEnclaveOfferKeyMismatch.into());
    }
    info!(
        offer_public_key = %resident_offer,
        %upstream,
        "resident offer key matched upstream before execution launch"
    );
    Ok(())
}

/// Canonical block hash at `height` on the selected upstream.
async fn upstream_block_hash_v1(
    upstream: &str,
    height: u64,
) -> eyre::Result<Option<alloy_primitives::B256>> {
    use outbe_operator::rpc::FinalityRpc as _;

    let block = outbe_operator::rpc::HttpRenewalRpc::new(upstream)?
        .block_by_number(height)
        .await
        .wrap_err("read upstream block at the durable TEE join anchor")?;
    block
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .map(str::parse::<alloy_primitives::B256>)
        .transpose()
        .wrap_err("decode upstream block hash at the durable TEE join anchor")
}

/// `Ready` local admission at the latest finalized block, with a resident
/// offer key equal to the on-chain key. Returns the admission for the
/// rejection report.
fn local_tee_admission_ready_v1(
    node: &OutbeFullNode,
    (chain_id, genesis_hash): (u64, alloy_primitives::B256),
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<(
    bool,
    Option<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>,
)> {
    let admission = crate::launch::admission::read_finalized_local_tee_admission(
        &node.provider,
        chain_id,
        genesis_hash,
        identity,
    )?;
    if !matches!(
        admission,
        Some(outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. })
    ) {
        return Ok((false, admission));
    }
    let on_chain_offer = outbe_engine::validators::read_tee_offer_public_at_latest(&node.provider)
        .wrap_err("read the canonical offer key for the TEE lease guard")?;
    let resident_offer = outbe_tee::resident_offer_public_key_state_v1()
        .wrap_err("read the resident offer key for the TEE lease guard")?;
    Ok((
        !on_chain_offer.is_zero() && resident_offer == Some(on_chain_offer),
        admission,
    ))
}

/// Initial `TeeLeaseGuardGateV1` for this process.
///
/// A FullNode uses the upstream anchor that `prepare` verified. A validator
/// first tries local facts. When they cannot arm the gate and `--upstream` is
/// set, it verifies the upstream NodeHost admission and offer key, checks the
/// durable join anchor against the upstream chain, and waits at the higher
/// anchor. Every check fails closed.
async fn initial_tee_lease_gate_v1(
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    chain: (u64, alloy_primitives::B256),
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    prepared_anchor: Option<crate::launch::admission::LocalTeeAdmissionAnchorV1>,
) -> eyre::Result<crate::launch::admission::TeeLeaseGuardAnchorsV1> {
    use crate::launch::admission::{
        local_validator_tee_gate_v1, select_validator_upstream_anchor_v1, TeeLeaseGuardAnchorsV1,
    };

    if !args.is_validator {
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate: prepared_anchor.map_or(TeeLeaseGuardGateV1::Armed, TeeLeaseGuardGateV1::Pending),
            durable: None,
        });
    }
    let durable = prepared_anchor;
    let local_finalized_height = node
        .provider
        .finalized_block_num_hash()
        .wrap_err("read local finalized height for the TEE lease guard")?
        .map_or(0, |finalized| finalized.number);
    let (local_ready_with_offer, local_admission) =
        if durable.is_none() && local_finalized_height >= 1 {
            local_tee_admission_ready_v1(node, chain, identity)?
        } else {
            (false, None)
        };
    let local_is_genesis_founder = if durable.is_none() && local_finalized_height == 0 {
        let signing_key_path = args
            .signing_key
            .as_ref()
            .ok_or(outbe_engine::errors::StackEpochError::SigningKeyRequired)?;
        let key_backend = args.key_backend().wrap_err("invalid BLS key backend")?;
        let local_consensus_key =
            outbe_engine::validators::load_signing_public_key(signing_key_path, &key_backend)
                .wrap_err("failed to load consensus signing public key for TEE lease guard")?;
        let genesis_validators =
            outbe_engine::validators::read_consensus_validators_at_block(&node.provider, chain.1)
                .wrap_err("failed to read genesis consensus validator set for TEE lease guard")?;
        genesis_validators
            .public_keys
            .contains(&local_consensus_key)
    } else {
        false
    };
    let upstream = args.upstream.as_deref();
    if let Some(gate) = local_validator_tee_gate_v1(
        durable,
        local_finalized_height,
        local_ready_with_offer,
        local_is_genesis_founder,
        upstream.is_some(),
    ) {
        if gate == TeeLeaseGuardGateV1::AwaitingAnchor {
            if let Some(reason) = local_admission.and_then(tee_lease_admission_rejection) {
                return Err(LaunchError::LocalNodeRejectedByTeeLease { reason }.into());
            }
        }
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate,
            durable: None,
        });
    }
    let Some(upstream) = upstream else {
        // Without an upstream, the startup check reports the catch-up error.
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate: durable.map_or(
                TeeLeaseGuardGateV1::AwaitingAnchor,
                TeeLeaseGuardGateV1::Pending,
            ),
            durable: None,
        });
    };
    let upstream_anchor = require_upstream_fullnode_tee_admission(upstream, identity).await?;
    require_upstream_offer_key_v1(upstream).await?;
    let upstream_hash_at_durable = match durable {
        Some(anchor) => upstream_block_hash_v1(upstream, anchor.finalized_height).await?,
        None => None,
    };
    let anchors =
        select_validator_upstream_anchor_v1(durable, upstream_anchor, upstream_hash_at_durable)?;
    info!(
        ?anchors,
        local_finalized_height,
        "validator TEE lease guard waits for UpstreamBootstrap to reach the admission anchor"
    );
    Ok(anchors)
}

pub(super) struct TeeRuntimeWorkers {
    pub(super) finalization_notify: Arc<tokio::sync::Notify>,
    pub(super) tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
    /// Keeps the keyless armed-watch open for the launcher lifetime. The
    /// enclave path moves its sender into the lease guard, so this is `None`.
    pub(super) tee_lease_armed_tx: Option<tokio::sync::watch::Sender<bool>>,
    pub(super) tee_lease_exit_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<eyre::Result<String>>>,
    pub(super) tee_lease_guard_handle: Option<tokio::task::JoinHandle<()>>,
    pub(super) tee_canary_handle: Option<tokio::task::JoinHandle<()>>,
    pub(super) upgrade_promotion: Arc<tokio::sync::Notify>,
    pub(super) upgrade_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Non-validator with no enclave socket. A validator without a socket is not
/// keyless and still fails in `prepare` with `MissingTeeEnclaveSocket`.
pub(super) fn keyless_follower_launch(args: &ConsensusArgs) -> bool {
    !args.is_validator && args.tee_enclave_socket.is_none()
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

fn keyless_runtime_workers() -> TeeRuntimeWorkers {
    let (tee_lease_armed_tx, tee_lease_armed_rx) = tokio::sync::watch::channel(false);
    TeeRuntimeWorkers {
        finalization_notify: Arc::new(tokio::sync::Notify::new()),
        tee_lease_armed_rx,
        tee_lease_armed_tx: Some(tee_lease_armed_tx),
        tee_lease_exit_rx: None,
        tee_lease_guard_handle: None,
        tee_canary_handle: None,
        upgrade_promotion: Arc::new(tokio::sync::Notify::new()),
        upgrade_handle: None,
    }
}

fn verify_initial_local_tee_admission_v1(
    (node, args): (&OutbeFullNode, &ConsensusArgs),
    chain: outbe_node::tee_remote_session::RegistryChainIdentity,
    local_tee_identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    anchors: &mut crate::launch::admission::TeeLeaseGuardAnchorsV1,
    node_data_dir: &Path,
) -> eyre::Result<()> {
    let validator_has_recovery_anchor =
        args.is_validator && anchors.gate.pending_anchor().is_some();
    if let Some(admission) = read_gated_finalized_local_tee_admission(
        &node.provider,
        chain,
        local_tee_identity,
        anchors,
    )? {
        let rejection = if validator_has_recovery_anchor {
            validator_recovery_startup_admission_rejection(admission)
        } else {
            tee_lease_admission_rejection(admission)
        };
        if let Some(reason) = rejection {
            return Err(LaunchError::LocalNodeRejectedByTeeLease { reason }.into());
        }
        if args.is_validator && args.upstream.is_none() && !anchors.gate.is_armed() {
            if let Some(reason) = crate::launch::admission::read_finalized_local_tee_admission(
                &node.provider,
                chain.chain_id,
                chain.genesis_hash,
                local_tee_identity,
            )?
            .and_then(validator_recovery_startup_admission_rejection)
            {
                return Err(LaunchError::LocalNodeRejectedByTeeLease { reason }.into());
            }
        }
    }
    require_validator_tee_recovery_complete_v1(
        args.is_validator,
        args.upstream.is_some(),
        anchors.gate,
        node_data_dir,
    )
}

pub(super) async fn start_runtime_workers(
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
    if keyless_follower_launch(args) {
        info!("keyless follower: skipping TEE lease guard, canary, and upgrade watcher");
        return Ok(keyless_runtime_workers());
    }
    let chain = outbe_node::tee_remote_session::RegistryChainIdentity {
        chain_id: proof_chain_id,
        genesis_hash,
    };
    let mut anchors = initial_tee_lease_gate_v1(
        node,
        args,
        (proof_chain_id, genesis_hash),
        local_tee_identity,
        tee_admission_anchor,
    )
    .await?;
    verify_initial_local_tee_admission_v1(
        (node, args),
        chain,
        local_tee_identity,
        &mut anchors,
        node_data_dir,
    )?;
    let finalization_notify = Arc::new(tokio::sync::Notify::new());
    let (tee_lease_armed_tx, tee_lease_armed_rx) =
        tokio::sync::watch::channel(anchors.gate.is_armed());
    let (tee_lease_exit_tx, tee_lease_exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let lease_check = run_tee_lease_guard_v1(
        node.provider.clone(),
        crate::launch::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity: local_tee_identity,
            anchors,
            finalization_notify: finalization_notify.clone(),
            armed: tee_lease_armed_tx,
        },
        shutdown_token.clone(),
    );
    let lease_outcome = shutdown.clone();
    let tee_lease_guard_handle = tokio::spawn(shutdown.track_task("TEE lease guard", async move {
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
        finalization_notify,
        tee_lease_armed_rx,
        tee_lease_armed_tx: None,
        tee_lease_exit_rx: Some(tee_lease_exit_rx),
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
    fn keyless_launch_is_only_a_non_validator_without_a_socket() {
        assert!(keyless_follower_launch(&parsed_args(&[])));
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
    fn keyless_workers_have_no_lease_guard() {
        let workers = keyless_runtime_workers();
        assert!(workers.tee_lease_exit_rx.is_none());
        assert!(workers.tee_lease_guard_handle.is_none());
        assert!(workers.tee_canary_handle.is_none());
        assert!(workers.upgrade_handle.is_none());
        assert!(!*workers.tee_lease_armed_rx.borrow());
        assert!(workers.tee_lease_armed_tx.is_some());
    }
}
