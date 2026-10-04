//! Establish the mandatory permanent offer key before consensus actors start.
use super::super::*;
use super::transport::Subchannel;

pub(super) struct TeeStartup<'a, E: Clock> {
    pub(super) args: &'a ConsensusArgs,
    pub(super) node: &'a OutbeFullNode,
    pub(super) bridge: &'a ConsensusExecutionBridge,
    pub(super) participants: &'a commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(super) validator_set: &'a validators::ValidatorSet,
    pub(super) local_consensus_key: &'a bls12381::PublicKey,
    pub(super) proposer_evm_address: Option<EthAddress>,
    pub(super) coordinate_genesis_bootstrap: bool,
    pub(super) shareless_verifier: bool,
    pub(super) genesis_dkg_boundary_artifact: &'a Option<DkgBoundaryArtifact>,
    pub(super) tee_dkg_round0: Option<Subchannel<E>>,
    pub(super) tee_bootstrap_round0: Option<Subchannel<E>>,
}

pub(super) async fn prepare_tee<E>(ctx: &E, startup: TeeStartup<'_, E>) -> Result<()>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    let TeeStartup {
        args,
        node,
        bridge,
        participants,
        validator_set,
        local_consensus_key,
        proposer_evm_address,
        coordinate_genesis_bootstrap,
        shareless_verifier,
        genesis_dkg_boundary_artifact,
        mut tee_dkg_round0,
        mut tee_bootstrap_round0,
    } = startup;
    // -- 7b. One-time TEE DKG + bootstrap coordination (startup, like the DKG) --
    // On a fresh chain (no executed blocks yet), this validator must run the TEE
    // enclave sidecar selected by the mandatory block-1 genesis policy:
    //   1. run the TEE DKG ceremony so the committee's enclaves collaboratively
    //      derive the shared tribute offer key (Seam F: a group threshold
    //      signature over a fixed message -> HKDF -> X25519; byte-identical on every
    //      honest node, secret resident in each enclave); then
    //   2. coordinate the committee's enclave registrations + EVM signatures into
    //      the block-1 `TeeBootstrap` payload - registering the DKG-derived offer
    //      key - and stash it in the bridge for the proposer to inject (slice 5.1).
    // `committee_snapshot_block` is the fixed block 1. The whole ceremony MUST
    // complete before block 1: it is wrapped in `--tee-bootstrap-timeout-secs` and
    // FAILS FAST (node halts via startup error) on timeout or error, rather than
    // proceeding into a permanently un-bootstrapped chain (no offer key on-chain =>
    // offers impossible). Local liveness only - not a consensus rule on imported
    // blocks. Missing local enclave or NodeHost identity is a startup error,
    // never a production fallback.
    let socket = args.tee_enclave_socket.clone().ok_or_else(|| {
        eyre::eyre!("mandatory TEE chain requires --tee-enclave-socket before consensus startup")
    })?;
    let tee_attestation =
        outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
            node.chain_spec().as_ref(),
        );
    let tee_activation = tee_attestation
        .activation()
        .map_err(|error| eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}"))?;
    let tee_policy = tee_activation
        .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
        .map_err(eyre::Report::msg)?
        .clone();
    let tee_session = args
        .tee_session_mode
        .resolve(tee_policy.attestation_mode)
        .map_err(eyre::Report::msg)?;
    {
        if coordinate_genesis_bootstrap {
            let my_validator = proposer_evm_address.ok_or_else(|| {
                eyre::eyre!("founding validator TEE bootstrap requires its EVM identity")
            })?;
            let n = participants.len();
            let tee_remote_peers: std::collections::BTreeSet<bls12381::PublicKey> = participants
                .iter()
                .filter(|peer| *peer != local_consensus_key)
                .cloned()
                .collect();
            let dkg_remote_peers = tee_remote_peers.clone();
            let deadline = std::time::Duration::from_secs(args.tee_bootstrap_timeout_secs);

            let (dkg_sender, dkg_receiver) = tee_dkg_round0
                .take()
                .ok_or_else(|| eyre::eyre!("TEE DKG P2P channel not registered"))?;
            let (tee_sender, tee_receiver) = tee_bootstrap_round0
                .take()
                .ok_or_else(|| eyre::eyre!("TEE bootstrap P2P channel not registered"))?;
            let (evm_signer, participant_committee) =
                tee_bootstrap_setup(args, participants, validator_set)?;
            let genesis_boundary = genesis_dkg_boundary_artifact.as_ref().ok_or_else(|| {
                eyre::eyre!("fresh OST3 bootstrap is missing its canonical genesis DKG boundary")
            })?;
            let committee_snapshot_hash = genesis_boundary.committee_set_hash;
            let committee: std::collections::BTreeSet<alloy_primitives::Address> = genesis_boundary
                .reshare
                .new_active_set
                .iter()
                .copied()
                .collect();
            ensure!(
                committee == participant_committee,
                "OST3 epoch-0 DKG boundary differs from the live genesis participants"
            );
            ensure!(
                committee.contains(&my_validator),
                "local validator is absent from the exact OST3 epoch-0 committee snapshot"
            );
            let requested_valid_until = node
                .chain_spec()
                .genesis
                .timestamp
                .checked_add(tee_policy.maximum_lease)
                .ok_or_else(|| eyre::eyre!("OST3 deterministic block-1 lease overflows u64"))?;
            let node_data_dir = node
                .config
                .datadir
                .clone()
                .resolve_datadir(node.chain_spec().chain())
                .data_dir()
                .to_path_buf();
            let endpoint = socket
                .to_str()
                .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?
                .to_owned();
            let reth_p2p_secret = node
                .config
                .network
                .secret_key(node.data_dir.p2p_secret())
                .wrap_err("failed to load persistent Reth P2P identity for OST3")?;
            let node_host_signing =
                k256::ecdsa::SigningKey::from_slice(reth_p2p_secret.secret_bytes().as_slice())
                    .map_err(|error| eyre::eyre!("invalid Reth P2P signing key: {error}"))?;
            let reth_p2p_public = node_host_signing
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))?;
            let node_id = outbe_primitives::tee_attestation_v1::NodeIdV1 { reth_p2p_public };

            // Step 1 (TEE DKG -> shared offer key) + Step 2 (bootstrap coordination ->
            // block-1 payload), under one deadline. Any error or timeout halts.
            // The deadline is measured on the consensus runtime `Clock` (the same
            // time source the deterministic test runtime can mock and advance), not
            // wall-clock - keeping startup-timeout behavior reproducible and free of a
            // direct async-runtime timer dependency in the consensus stack.
            // `Clock::timeout` requires a `Send + 'static` future; the `async move`
            // owns every capture, so the bound holds.
            // Owned `Clock` clone moved into the `'static` startup future so the TEE
            // DKG identity-exchange cadence runs on the consensus runtime clock, not
            // tokio's wall-clock (mockable under the deterministic test runtime).
            let dkg_clock = ctx.child("tee_dkg_clock");
            let bootstrap_clock = ctx.child("tee_bootstrap_clock");
            let payload = ctx
                .timeout(deadline, async move {
                    let (mut enclave, production_manifest) = match tee_session {
                        crate::args::ResolvedTeeSession::ProductionNodeHost => {
                            let production = outbe_tee::connect_committed_node_host_enclave(
                                &endpoint,
                                &node_data_dir,
                            )
                            .map_err(|error| {
                                eyre::eyre!("production NodeHost reconnect failed: {error}")
                            })?;
                            let manifest =
                                outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir)
                                    .map_err(|error| {
                                        eyre::eyre!(
                                            "committed NodeHost manifest load failed: {error}"
                                        )
                                    })?;
                            (
                                outbe_tee::RuntimeEnclaveClient::Production(production),
                                Some(manifest),
                            )
                        }
                        crate::args::ResolvedTeeSession::Development => {
                            let development = outbe_tee::EnclaveClient::connect_endpoint(&endpoint)
                                .map_err(|error| {
                                    eyre::eyre!(
                                        "GramineDirectDev enclave reconnect failed: {error}"
                                    )
                                })?;
                            (
                                outbe_tee::RuntimeEnclaveClient::Development(Box::new(development)),
                                None,
                            )
                        }
                    };
                    let (tribute_offer_public, tribute_offer_group_public_key) =
                        crate::tee_bootstrap::run_tee_dkg_at_startup(
                            &mut enclave,
                            crate::tee_bootstrap::TeeDkgStartup {
                                participant_count: n,
                                network_binding: tee_policy.network_binding(),
                                tribute_offer_epoch: 0,
                            },
                            crate::tee_bootstrap::StartupGossipTransport {
                                sender: dkg_sender,
                                receiver: dkg_receiver,
                                clock: dkg_clock,
                                allowed_remote_peers: dkg_remote_peers,
                            },
                        )
                        .await
                        .map_err(|e| eyre::eyre!("TEE DKG ceremony failed: {e}"))?;
                    info!(
                        tribute_offer_public = %B256::from(tribute_offer_public),
                        "TEE DKG complete - shared tribute offer key derived"
                    );
                    let local_submission =
                        crate::tee_bootstrap::build_local_tee_bootstrap_submission_v2(
                            &mut enclave,
                            crate::tee_bootstrap::LocalRegistrationRequest {
                                production_manifest: production_manifest.as_ref(),
                                node_id,
                                policy: &tee_policy,
                                requested_valid_until,
                            },
                            &evm_signer,
                            |hash| {
                                use k256::ecdsa::signature::hazmat::PrehashSigner as _;
                                let (signature, recovery): (
                                    k256::ecdsa::Signature,
                                    k256::ecdsa::RecoveryId,
                                ) = node_host_signing
                                    .sign_prehash(hash.as_slice())
                                    .map_err(|error| error.to_string())?;
                                let mut bytes = [0_u8; 65];
                                bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
                                bytes[64] = recovery.to_byte();
                                Ok(bytes)
                            },
                        )?;
                    let authority = outbe_primitives::tee_bootstrap_v2::TeeBootstrapAuthorityV2 {
                        policy: tee_policy,
                        committee_snapshot_hash,
                        committee_snapshot_block: 1,
                        key_epoch: 0,
                        tribute_offer_epoch: 0,
                        dkg_transcript_hash: B256::ZERO,
                        tribute_offer_public_key: B256::from(tribute_offer_public),
                        tribute_offer_group_public_key: Bytes::from(tribute_offer_group_public_key),
                    };
                    let payload = crate::tee_bootstrap::run_tee_bootstrap_v2_at_startup(
                        crate::tee_bootstrap::TeeBootstrapStartup {
                            local_submission,
                            authority,
                            committee,
                            evm_signer: &evm_signer,
                        },
                        crate::tee_bootstrap::StartupGossipTransport {
                            sender: tee_sender,
                            receiver: tee_receiver,
                            clock: bootstrap_clock,
                            allowed_remote_peers: tee_remote_peers,
                        },
                    )
                    .await
                    .map_err(|e| eyre::eyre!("TEE bootstrap coordination failed: {e}"))?;
                    Ok::<_, eyre::Report>(payload)
                })
                .await
                .map_err(|_| {
                    eyre::eyre!(
                        "TEE DKG + bootstrap did not complete within {}s \
                     (--tee-bootstrap-timeout-secs); halting before block 1",
                        args.tee_bootstrap_timeout_secs
                    )
                })??;

            info!(
                validators = payload.participants.len(),
                attestation_mode = ?payload.policy.attestation_mode,
                "mandatory OST3 bootstrap coordinated - payload ready for block 1"
            );
            bridge.set_pending_tee_bootstrap(payload);
        } else if shareless_verifier {
            // The permissionless V1 onboarding transaction must have installed
            // the permanent key before this process was launched. A shareless
            // validator certifies and replays the existing chain without threshold
            // authority and never reproduces genesis OST3. Its canonical EVM/BLS
            // identity may already be retained for the later DKG activation.
            let resident_offer = outbe_tee::resident_offer_public_key_v1().wrap_err(
                "verifier-join requires the permanent resident offer key before certified sync (no recovery or fallback)",
            )?;
            ensure!(
                !resident_offer.is_zero(),
                "verifier-join enclave has no permanent resident offer key; refusing certified sync (no recovery or fallback)"
            );
            info!(
                offer_public_key = %resident_offer,
                "verifier-join resident offer key present before certified sync"
            );
        } else {
            // A restarted active validator must already hold the exact resident
            // offer key committed on-chain. Startup never recovers or replaces a
            // lost key and never falls back to a pre-V1 delivery protocol.
            let _my_validator = proposer_evm_address.ok_or_else(|| {
                eyre::eyre!("active validator consensus startup requires its EVM identity")
            })?;
            let on_chain_offer = validators::read_tee_offer_public_at_latest(&node.provider)
                .wrap_err("failed to read the mandatory on-chain TEE offer key")?;
            ensure!(
                !on_chain_offer.is_zero(),
                "mandatory OST3 chain has no on-chain offer key after block 1"
            );
            let node_data_dir = node
                .config
                .datadir
                .clone()
                .resolve_datadir(node.chain_spec().chain())
                .data_dir()
                .to_path_buf();
            let endpoint = socket
                .to_str()
                .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
            let mut enclave = match tee_session {
                crate::args::ResolvedTeeSession::ProductionNodeHost => {
                    outbe_tee::RuntimeEnclaveClient::Production(
                        outbe_tee::connect_committed_node_host_enclave(endpoint, &node_data_dir)
                            .map_err(|error| {
                                eyre::eyre!("production NodeHost reconnect failed: {error}")
                            })?,
                    )
                }
                crate::args::ResolvedTeeSession::Development => {
                    outbe_tee::RuntimeEnclaveClient::Development(Box::new(
                        outbe_tee::EnclaveClient::connect_endpoint(endpoint).map_err(|error| {
                            eyre::eyre!("GramineDirectDev enclave reconnect failed: {error}")
                        })?,
                    ))
                }
            };
            let enclave_offer = crate::tee_bootstrap::query_enclave_offer_public(&mut enclave)?;
            ensure!(
                enclave_offer == on_chain_offer,
                "local enclave does not hold the chain offer key; refusing consensus startup (no recovery or fallback)"
            );
        }
    }

    Ok(())
}
