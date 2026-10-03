mod finalization;
mod startup;

use finalization::finalization_bytes_for_height;

use super::*;

/// Spawn the drainer that answers `outbe_getFinalization` RPC requests from the
/// marshal. The `outbe-rpc` handler cannot see the marshal or `ConsensusBlock`,
/// so it requests bytes through [`ConsensusExecutionBridge::request_finalization`];
/// this task is the consensus-side responder. Wired on BOTH the validator path
/// (`run_consensus_stack`) and the certified-follower path (a follower can serve
/// upstream too), right after `marshal_mailbox` exists.
///
/// For each `(height, reply)` it reads the finalization certificate and the
/// finalized block from the marshal, encodes both with `commonware_codec`, and
/// answers `Some` only when both are present locally (otherwise `None`, which
/// the RPC maps to a "not available" error).
pub(in crate::stack) fn spawn_finalization_drainer<E>(
    ctx: &E,
    marshal_mailbox: outbe_consensus::marshal_types::MarshalMailbox,
    bridge: ConsensusExecutionBridge,
    parent_store: outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
) where
    E: Spawner + Metrics,
{
    let rx = bridge.set_finalization_fetcher();
    ctx.child("finalization_drainer")
        .spawn(move |_| async move {
            let mut rx = rx;
            while let Some((height, reply)) = rx.recv().await {
                let answer = finalization_bytes_for_height(
                    &marshal_mailbox,
                    &parent_store,
                    Height::new(height),
                )
                .await;
                // The receiver may have gone away (RPC client disconnected); ignore.
                let _ = reply.send(answer);
            }
        });
}

/// Run the consensus stack.
///
/// Wires together:
/// 1. Validator configuration (static JSON or dynamic from EVM state)
/// 2. HybridScheme signing (BLS individual + BLS12-381 threshold VRF)
/// 3. P2P network channels (lookup::Network) with Muxers for epoch-scoped sub-channels
/// 4. Application handler (propose/verify via beacon engine)
/// 5. Executor actor (FCU updates, finalization)
/// 6. Simplex consensus engine (restarted on reshare)
/// 7. Block propagation - proposer broadcasts full blocks via P2P channel
/// 8. Automatic reshare detection and DKG execution
///
/// Follower stack: cold-sync finalized blocks from an upstream node, verify them
/// against the trusted network identity (committee-chaining - see the `follow`
/// module), and drive the EL via the existing executor, WITHOUT running the
/// consensus engine. Selected by `--upstream`.
pub(in crate::stack) async fn run_follow_stack<E>(
    ctx: E,
    args: ConsensusArgs,
    connection: services::FollowerConnection,
    services: services::FollowerStackServices,
) -> Result<()>
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
        return Err(eyre::eyre!(
            "--upstream.nocertify (uncertified dev sync) is not yet implemented"
        ));
    }

    // Trust anchor: the genesis validator committee (the MinPk consensus key
    // set), read from the follower's OWN genesis state. Consensus finality is a
    // multisig over these keys, so this set - not the VRF group key - is the
    // trust root, and it is already in genesis (the operator provides nothing).
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
                eyre::eyre!("genesis validator set is not a valid participant set: {e:?}")
            })?;

    // Defence in depth for callers that construct the engine stack outside the
    // node binary. The binary already proves this equality before Reth launch;
    // repeat it here before certified sync so no alternate embedding can process
    // a protected block with a missing or divergent permanent key.
    let tee_probe = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    let tee_offer_public = tee_probe
        .tribute_offer_public_key()
        .await
        .wrap_err("failed to probe the upstream for TEE-chain status (follower prerequisites)")?;
    ensure!(
        !tee_offer_public.is_zero(),
        "selected upstream has no mandatory OST3 offer key"
    );
    let resident_offer = outbe_tee::resident_offer_public_key_v1()
        .wrap_err("failed to read the mandatory local enclave offer key")?;
    ensure!(
        resident_offer == tee_offer_public,
        "local enclave does not hold the selected chain's exact offer key; refusing certified sync (no recovery or fallback)"
    );

    info!(
        %upstream,
        anchor_validators = anchor_participants.len(),
        epoch_length,
        "follower mode (--upstream) selected; anchored on the genesis validator set"
    );

    let ocomp_storage_root = args
        .storage_dir
        .clone()
        .ok_or_else(|| eyre::eyre!("consensus storage_dir must be set before follower startup"))?;

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
            storage_root: ocomp_storage_root,
        },
        services,
    )
    .await
}

/// Rebuild the exact parent-proof record a certified follower needs before
/// OCOMP retention may consume a finalized block.
///
/// Marshal has already verified the finalization certificate against `scheme`.
/// This seam additionally binds that verified certificate to the exact block
/// executed by the follower and to the historical committee snapshot committed
/// in the follower's own canonical state. A mismatch is node-fatal: substituting
/// either the current committee or a same-height block would make the locally
/// produced OCOMP input proof unverifiable.
pub(in crate::stack) fn build_certified_follower_parent_record(
    finalization: &outbe_consensus::marshal_types::Finalization,
    block: &outbe_consensus::block::ConsensusBlock,
    historical_snapshot: &outbe_consensus::proof::CommitteeSnapshot,
    scheme: &HybridScheme<MinSig>,
) -> Result<outbe_consensus::finalization::parent_cert_store::CertifiedParentProofRecord> {
    let finalized_hash = finalization.proposal.payload.0;
    ensure!(
        finalized_hash == block.block_hash(),
        "certified follower finalization payload {finalized_hash} differs from executed block {} at height {}",
        block.block_hash(),
        block.number(),
    );

    let finalized_epoch = finalization.proposal.round.epoch().get();
    let ordered_addresses: Vec<EthAddress> = historical_snapshot
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect();
    let record = outbe_consensus::finalization::resolver::build_finalization_record_from_recovered(
        finalized_epoch,
        finalization.proposal.round.view().get(),
        finalization.proposal.parent.get(),
        block.number(),
        finalized_hash,
        &ordered_addresses,
        &finalization.certificate,
        finalization.encode().into(),
        scheme,
    )?;
    let historical_hash = historical_snapshot.committee_set_hash_v2(finalized_epoch);
    ensure!(
        record.committee_set_hash == historical_hash,
        "certified follower verifier committee {} differs from historical committee snapshot {} at epoch {finalized_epoch}",
        record.committee_set_hash,
        historical_hash,
    );
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
            .ok_or_else(|| {
                eyre::eyre!("marshal has no certified finalization at follower height {height}")
            })?;
        let block = marshal_mailbox
            .get_block(&finalization.proposal.payload)
            .await
            .ok_or_else(|| {
                eyre::eyre!(
                    "marshal has no finalized block {} at follower height {height}",
                    finalization.proposal.payload.0
                )
            })?;
        ensure!(
            block.number() == height,
            "marshal finalized block {} reports height {}, expected {height}",
            block.block_hash(),
            block.number(),
        );

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
            eyre::eyre!(
                "certified follower has no verifier scheme for finalized epoch {}",
                epoch.get()
            )
        })?;
        let snapshot = validators::read_committee_snapshot_at_latest(&node.provider, epoch.get())?
            .ok_or_else(|| {
                eyre::eyre!(
                    "certified follower has no historical committee snapshot for epoch {}",
                    epoch.get()
                )
            })?;
        let height = block.number();
        let record = build_certified_follower_parent_record(
            finalization,
            block,
            &snapshot,
            scheme.as_ref(),
        )?;
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
/// observes the already-canonical genesis block, so finality observers must
/// ignore that one notification instead of asking marshal for an impossible
/// certificate.
pub(in crate::stack) const fn follower_height_has_certified_finalization(height: u64) -> bool {
    height > 0
}
