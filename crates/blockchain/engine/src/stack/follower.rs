mod finalization;
pub(in crate::stack) mod local_anchor;
mod startup;
#[cfg(test)]
pub(in crate::stack) use startup::wait_for_bootstrap_tee_lease;

use finalization::finalization_bytes_for_height;

use super::*;

/// Keyless replay has no enclave socket. Skip the resident probe only when the
/// process policy is explicitly `ReplayOnly`. A configured socket keeps the
/// resident-key check.
pub(in crate::stack) fn follower_skips_resident_offer_probe(args: &ConsensusArgs) -> bool {
    if args.tee_enclave_socket.is_some() {
        return false;
    }
    outbe_tee::witness::witness_policy() == Some(outbe_tee::witness::WitnessPolicy::ReplayOnly)
}

/// Local chain and process facts that decide `ValidatorBootstrapReady`.
#[derive(Clone, Copy, Debug)]
pub(in crate::stack) struct BootstrapFacts {
    pub(in crate::stack) finalized_height: u64,
    pub(in crate::stack) local_key_in_validator_set: bool,
    pub(in crate::stack) finalized_boundary_present: bool,
    pub(in crate::stack) on_chain_offer: B256,
    pub(in crate::stack) resident_offer: Option<B256>,
    pub(in crate::stack) tee_lease_armed: bool,
}

/// Pure `ValidatorBootstrapReady` decision.
pub(in crate::stack) fn bootstrap_ready_from_facts(facts: BootstrapFacts) -> bool {
    if !facts.local_key_in_validator_set {
        return false;
    }
    if facts.finalized_height == 0 {
        return true;
    }
    facts.finalized_boundary_present
        && !facts.on_chain_offer.is_zero()
        && facts.tee_lease_armed
        && facts.resident_offer == Some(facts.on_chain_offer)
}

/// Local authority required before a validator leaves certified sync.
pub(in crate::stack) struct ValidatorBootstrap {
    pub(in crate::stack) local_consensus_key: bls12381::PublicKey,
    pub(in crate::stack) local_key_in_genesis_validator_set: bool,
    pub(in crate::stack) dkg_rotation_params: DkgRotationParams,
    pub(in crate::stack) tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
}

impl ValidatorBootstrap {
    pub(in crate::stack) fn is_validator_bootstrap_ready(
        &self,
        node: &OutbeFullNode,
        height: u64,
    ) -> Result<bool> {
        if height == 0 {
            return Ok(bootstrap_ready_from_facts(BootstrapFacts {
                finalized_height: 0,
                local_key_in_validator_set: self.local_key_in_genesis_validator_set,
                finalized_boundary_present: false,
                on_chain_offer: B256::ZERO,
                resident_offer: None,
                tee_lease_armed: false,
            }));
        }
        let tee_lease_armed = *self.tee_lease_armed_rx.borrow();
        if !tee_lease_armed {
            return Ok(false);
        }
        let finalized_boundary_present =
            recover_latest_boundary_artifact(&node.provider, height, self.dkg_rotation_params)?
                .is_some();
        if !finalized_boundary_present {
            return Ok(false);
        }
        let hash = node
            .provider
            .block_hash(height)?
            .ok_or(crate::errors::StackEpochError::StartupP2pAdmissionBlockMissing)?;
        let state = node.provider.state_by_block_hash(hash)?;
        let on_chain_offer = validators::read_tee_offer_public_from_state(&state)?;
        let resident_offer = outbe_tee::resident_offer_public_key_state_v1()?;
        let local_key_in_validator_set =
            validators::read_consensus_validators_at_block(&node.provider, hash)?
                .public_keys
                .contains(&self.local_consensus_key)
                || validators::read_admitted_non_consensus_at_block(&node.provider, hash)?
                    .public_keys
                    .contains(&self.local_consensus_key);
        Ok(bootstrap_ready_from_facts(BootstrapFacts {
            finalized_height: height,
            local_key_in_validator_set,
            finalized_boundary_present,
            on_chain_offer,
            resident_offer,
            tee_lease_armed,
        }))
    }
}

/// Spawn the drainer that answers `outbe_getFinalization` RPC requests from the
/// marshal. The `outbe-rpc` handler cannot see the marshal or `ConsensusBlock`,
/// so it requests bytes through [`ConsensusExecutionBridge::request_finalization`].
/// This task is the consensus-side responder. The stack wires it on BOTH the
/// validator path (`run_consensus_stack`) and the certified-follower path (a
/// follower can serve upstream too), right after `marshal_mailbox` exists.
///
/// For each `(height, reply)`, it reads the finalized block from the marshal.
/// It looks for the finalization certificate in this order:
/// 1. the marshal.
/// 2. a retained finalization for the same block in the parent-cert store.
/// 3. the parent finalization that the child block carries.
///
/// It encodes both with `commonware_codec`. It answers `None` only when the
/// block is missing locally. The RPC maps `None` to a "not available" error.
/// When it finds no certificate, it answers `Some` with an empty
/// `finalization` field.
pub(in crate::stack) fn spawn_finalization_drainer<E>(
    ctx: &E,
    marshal_mailbox: outbe_consensus::marshal_types::MarshalMailbox,
    bridge: ConsensusExecutionBridge,
    parent_store: outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    mut phase_cancel: tokio::sync::watch::Receiver<bool>,
) -> commonware_runtime::Handle<()>
where
    E: Spawner + Metrics,
{
    let rx = bridge.set_finalization_fetcher();
    ctx.child("finalization_drainer")
        .spawn(move |_| async move {
            use tracing::Instrument as _;

            let mut rx = rx;
            let serve = async {
                while let Some((height, reply)) = rx.recv().await {
                    if reply.is_closed() {
                        continue;
                    }
                    let answer = finalization_bytes_for_height(
                        &marshal_mailbox,
                        &parent_store,
                        Height::new(height),
                    )
                    .instrument(tracing::debug_span!("finalization_fetch", height))
                    .await;
                    let _ = reply.send(answer);
                }
            };
            tokio::select! {
                _ = serve => {},
                _ = phase_cancel.wait_for(|cancelled| *cancelled) => {},
            }
        })
}

/// Run the follower stack. It does these steps WITHOUT running the consensus engine:
/// 1. Cold-sync finalized blocks from an upstream node.
/// 2. Verify them against the trusted network identity (committee-chaining - see
///    the `follow` module).
/// 3. Drive the EL via the existing executor.
///
/// The `--upstream` flag selects this stack.
pub(in crate::stack) async fn run_follow_stack<E>(
    ctx: E,
    args: ConsensusArgs,
    connection: services::FollowerConnection,
    services: services::FollowerStackServices,
) -> Result<crate::follower_shutdown::FollowStackExit>
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
    let services::FollowerConnection {
        node,
        bridge,
        upstream,
    } = connection;
    let epoch_length = epoch_length_blocks_from_genesis(&node)?;

    if args.upstream_nocertify {
        return Err(crate::errors::FollowerStackError::UpstreamNocertifyNotImplemented.into());
    }

    // Trust anchor: the genesis validator committee (the MinPk consensus key
    // set), read from the follower's OWN genesis state. Consensus finality is a
    // multisig over these keys. So this set, not the VRF group key, is the trust
    // root. It is already in genesis, and the operator provides nothing.
    let follower_genesis_hash = genesis_hash(&node)?;
    let genesis_validators =
        validators::read_consensus_validators_at_block(&node.provider, follower_genesis_hash)
            .wrap_err("failed to read genesis validator set for the follower trust anchor")?;
    let anchor_participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        genesis_validators
            .public_keys
            .iter()
            .cloned()
            .try_collect()
            .map_err(|e| {
                crate::errors::FollowerStackError::InvalidGenesisParticipantSet(format!("{e:?}"))
            })?;

    // Defence in depth for callers that construct the engine stack outside the
    // node binary. A TEE follower already proved resident-key equality before
    // Reth launch. Repeat that local check here. A keyless follower has no
    // socket and no session: keep the upstream non-zero check and skip the
    // local probe. Do not skip it when a socket was configured.
    let tee_probe = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    let tee_offer_public = tee_probe
        .tribute_offer_public_key()
        .await
        .wrap_err("failed to probe the upstream for TEE-chain status (follower prerequisites)")?;
    if tee_offer_public.is_zero() {
        return Err(crate::errors::FollowerStackError::UpstreamMissingOst3OfferKey.into());
    }
    if !follower_skips_resident_offer_probe(&args) {
        let resident_offer = outbe_tee::resident_offer_public_key_v1()
            .wrap_err("failed to read the mandatory local enclave offer key")?;
        if resident_offer != tee_offer_public {
            return Err(crate::errors::FollowerStackError::LocalEnclaveOfferKeyMismatch.into());
        }
    }

    info!(
        %upstream,
        anchor_validators = anchor_participants.len(),
        epoch_length,
        "follower mode (--upstream) selected; anchored on the genesis validator set"
    );

    startup::run_certified_follow_stack(
        ctx,
        services::FollowerConnection {
            node,
            bridge,
            upstream,
        },
        startup::FollowerTrustAnchor {
            participants: anchor_participants,
            epoch_length_blocks: epoch_length,
        },
        services,
    )
    .await
}

/// Rebuild the exact parent-proof record used by certified follower finality.
///
/// Marshal already verified the finalization certificate against `scheme`.
/// This seam also binds that verified certificate to the exact block that the
/// follower executed. It also binds the certificate to the historical committee
/// snapshot committed in the follower's own canonical state. A mismatch is
/// node-fatal. A substitute (the current committee or a same-height block)
/// would make the locally produced parent certificate unverifiable.
pub(in crate::stack) fn build_certified_follower_parent_record(
    finalization: &outbe_consensus::marshal_types::Finalization,
    block: &outbe_consensus::block::ConsensusBlock,
    historical_snapshot: &outbe_consensus::proof::CommitteeSnapshot,
    scheme: &HybridScheme<MinSig>,
) -> Result<outbe_consensus::finalization::parent_cert_store::CertifiedParentProofRecord> {
    let finalized_hash = finalization.proposal.payload.0;
    if finalized_hash != block.block_hash() {
        return Err(
            crate::errors::FollowerStackError::FinalizationPayloadDiffersFromExecuted {
                finalized_hash,
                block_hash: block.block_hash(),
                height: block.number(),
            }
            .into(),
        );
    }

    let finalized_epoch = finalization.proposal.round.epoch().get();
    let ordered_addresses: Vec<EthAddress> = historical_snapshot
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect();
    let record = outbe_consensus::finalization::resolver::build_recovered_finalization_record(
        outbe_consensus::finalization::resolver::RecoveredFinalizedBlock::from_proposal(
            block.number(),
            &finalization.proposal,
        ),
        outbe_consensus::finalization::resolver::RecoveredFinalizationMaterial {
            ordered_committee: &ordered_addresses,
            certificate: &finalization.certificate,
            encoded_certificate: finalization.encode().into(),
            scheme,
        },
    )?;
    let historical_hash = historical_snapshot.committee_set_hash_v2(finalized_epoch);
    if record.committee_set_hash != historical_hash {
        return Err(
            crate::errors::FollowerStackError::VerifierCommitteeDiffersFromHistorical {
                verifier_hash: record.committee_set_hash,
                historical_hash,
                finalized_epoch,
            }
            .into(),
        );
    }
    Ok(record)
}

struct FollowerProofPersistence<'a> {
    node: &'a OutbeFullNode,
    certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    parent_cert_store:
        &'a outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
}
impl FollowerProofPersistence<'_> {
    async fn reconcile_height(
        &self,
        marshal_mailbox: &outbe_consensus::marshal_types::MarshalMailbox,
        height: u64,
    ) -> Result<outbe_consensus::block::ConsensusBlock> {
        let finalization = marshal_mailbox
            .get_finalization(Height::new(height))
            .await
            .ok_or(
                crate::errors::FollowerStackError::MarshalMissingCertifiedFinalization(height),
            )?;
        let block = marshal_mailbox
            .get_block(&finalization.proposal.payload)
            .await
            .ok_or(
                crate::errors::FollowerStackError::MarshalMissingFinalizedBlock {
                    payload: finalization.proposal.payload.0,
                    height,
                },
            )?;
        if block.number() != height {
            return Err(
                crate::errors::FollowerStackError::MarshalFinalizedBlockHeightMismatch {
                    block_hash: block.block_hash(),
                    actual: block.number(),
                    expected: height,
                }
                .into(),
            );
        }

        self.reconcile_record(&finalization, &block)?;
        Ok(block)
    }

    fn reconcile_record(
        &self,
        finalization: &outbe_consensus::marshal_types::Finalization,
        block: &outbe_consensus::block::ConsensusBlock,
    ) -> Result<()> {
        use outbe_consensus::finalization::parent_cert_store::CertifiedParentProofStore as _;
        let Self {
            node,
            certificate_scheme_provider,
            parent_cert_store,
        } = self;

        let epoch = finalization.proposal.round.epoch();
        let scheme = certificate_scheme_provider.scoped(epoch).ok_or_else(|| {
            crate::errors::FollowerStackError::MissingVerifierSchemeForFinalizedEpoch(epoch.get())
        })?;
        let snapshot = validators::read_committee_snapshot_at_latest(&node.provider, epoch.get())?
            .ok_or_else(|| {
                crate::errors::FollowerStackError::MissingHistoricalCommitteeSnapshot(epoch.get())
            })?;
        let height = block.number();
        let record = match build_certified_follower_parent_record(
            finalization,
            block,
            &snapshot,
            scheme.as_ref(),
        ) {
            Ok(record) => record,
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::errors::FollowerStackError>(),
                    Some(
                        crate::errors::FollowerStackError::VerifierCommitteeDiffersFromHistorical { .. }
                    )
                ) =>
            {
                let refreshed =
                    validators::read_committee_snapshot_at_latest(&node.provider, epoch.get())?
                        .ok_or_else(|| {
                            crate::errors::FollowerStackError::MissingHistoricalCommitteeSnapshot(
                                epoch.get(),
                            )
                        })?;
                build_certified_follower_parent_record(
                    finalization,
                    block,
                    &refreshed,
                    scheme.as_ref(),
                )?
            }
            Err(error) => return Err(error),
        };
        parent_cert_store
            .put_finalization(record)
            .wrap_err_with(|| {
                format!("failed to persist follower finalization at height {height}")
            })?;
        parent_cert_store
            .prune_below_height(
                height.saturating_sub(outbe_consensus::finalization::actor::PARENT_CERT_KEEP_DEPTH),
            )
            .wrap_err("failed to prune follower finalized parent certificates")?;

        Ok(())
    }
}

/// Genesis is the trusted follower anchor, not a block carrying a certified
/// marshal finalization. The executor still acknowledges height zero when it
/// observes the already-canonical genesis block. So finality observers must
/// ignore that one notification. They must not ask marshal for an impossible
/// certificate.
pub(in crate::stack) const fn follower_height_has_certified_finalization(height: u64) -> bool {
    height > 0
}

#[cfg(test)]
mod resident_probe_tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct ArgsCli {
        #[command(flatten)]
        consensus: ConsensusArgs,
    }

    #[test]
    fn socket_keeps_the_resident_probe() {
        let args = ArgsCli::try_parse_from(["test"])
            .expect("default args")
            .consensus;
        assert!(args.tee_enclave_socket.is_none());
        let replay_only = outbe_tee::witness::witness_policy()
            == Some(outbe_tee::witness::WitnessPolicy::ReplayOnly);
        assert_eq!(follower_skips_resident_offer_probe(&args), replay_only);
        let mut with_socket = args;
        with_socket.tee_enclave_socket = Some(std::path::PathBuf::from("/tmp/tee.sock"));
        assert!(!follower_skips_resident_offer_probe(&with_socket));
    }
}
