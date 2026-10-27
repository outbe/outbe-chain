//! `ValidatorBootstrapReady`: the gate between `UpstreamBootstrap` and the
//! validator stack.
//!
//! A validator starts the validator stack only when its local canonical chain
//! holds the facts that the stack needs. At finalized height zero, the local
//! key must be a founding genesis validator. Above zero, the chain must hold a
//! finalized `BoundaryOutcome` and a non-zero offer key, the local TEE lease
//! guard must be armed, the enclave must hold that offer key, and the local
//! key must be in the `ValidatorSet`.
use super::super::*;
use reth_provider::BlockIdReader as _;

/// Local chain and process facts that decide `ValidatorBootstrapReady`.
#[derive(Clone, Copy, Debug)]
pub(in crate::stack) struct BootstrapFacts {
    pub(in crate::stack) finalized_height: u64,
    /// Genesis `ValidatorSet` at height zero. Consensus or admitted
    /// non-consensus set above zero.
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

/// Read `ValidatorBootstrapReady` at the local finalized height.
///
/// `tee_lease_armed` is the current value of the TEE lease guard watch. It is
/// true only after the guard validated the admission anchor against the local
/// canonical chain.
pub(in crate::stack) fn is_validator_bootstrap_ready(
    node: &OutbeFullNode,
    local_consensus_key: &bls12381::PublicKey,
    tee_lease_armed: bool,
) -> Result<bool> {
    let finalized = node
        .provider
        .finalized_block_num_hash()
        .wrap_err("failed to read the local finalized height for bootstrap readiness")?;
    let (finalized_height, finalized_hash) = match finalized {
        Some(finalized) if finalized.number > 0 => (finalized.number, finalized.hash),
        _ => (0, genesis_hash(node)?),
    };
    let consensus = validators::read_consensus_validators_at_block(&node.provider, finalized_hash)
        .wrap_err("failed to read the consensus validator set for bootstrap readiness")?;
    let in_consensus = consensus.public_keys.contains(local_consensus_key);
    if finalized_height == 0 {
        return Ok(bootstrap_ready_from_facts(BootstrapFacts {
            finalized_height,
            local_key_in_validator_set: in_consensus,
            finalized_boundary_present: false,
            on_chain_offer: B256::ZERO,
            resident_offer: None,
            tee_lease_armed,
        }));
    }
    // The cheap process-local facts decide most negative answers. Read chain
    // history only when they already hold.
    if !tee_lease_armed {
        return Ok(false);
    }
    let local_key_in_validator_set = in_consensus
        || validators::read_admitted_non_consensus_at_block(&node.provider, finalized_hash)
            .wrap_err("failed to read admitted validators for bootstrap readiness")?
            .public_keys
            .contains(local_consensus_key);
    if !local_key_in_validator_set {
        return Ok(false);
    }
    let on_chain_offer = validators::read_tee_offer_public_at_latest(&node.provider)
        .wrap_err("failed to read the canonical offer key for bootstrap readiness")?;
    let resident_offer = outbe_tee::resident_offer_public_key_state_v1()
        .wrap_err("failed to read the enclave offer key for bootstrap readiness")?;
    let epoch_length_blocks = epoch_length_blocks_from_genesis(node)?;
    let finalized_boundary_present = recover_latest_boundary_artifact(
        &node.provider,
        finalized_height,
        DkgRotationParams::from_genesis(node, epoch_length_blocks),
    )
    .wrap_err("failed to read the latest boundary for bootstrap readiness")?
    .is_some();
    Ok(bootstrap_ready_from_facts(BootstrapFacts {
        finalized_height,
        local_key_in_validator_set,
        finalized_boundary_present,
        on_chain_offer,
        resident_offer,
        tee_lease_armed,
    }))
}

/// Promotion check that the follower runs on each finalization while the
/// validator is in `UpstreamBootstrap`.
#[derive(Clone)]
pub(in crate::stack) struct PromotionProbe {
    pub(in crate::stack) node: OutbeFullNode,
    pub(in crate::stack) local_consensus_key: bls12381::PublicKey,
    pub(in crate::stack) tee_lease_armed: tokio::sync::watch::Receiver<bool>,
    /// Upstream finalized tip when the bootstrap phase started. Promotion
    /// waits until the local chain reaches it.
    pub(in crate::stack) initial_upstream_tip: u64,
}

impl PromotionProbe {
    /// Returns the local finalized height when the validator is ready.
    pub(in crate::stack) fn ready_height(&self) -> Result<Option<u64>> {
        let finalized_height = self
            .node
            .provider
            .finalized_block_num_hash()
            .wrap_err("failed to read the local finalized height for promotion")?
            .map_or(0, |finalized| finalized.number);
        if finalized_height < self.initial_upstream_tip {
            return Ok(None);
        }
        let armed = *self.tee_lease_armed.borrow();
        Ok(
            is_validator_bootstrap_ready(&self.node, &self.local_consensus_key, armed)?
                .then_some(finalized_height),
        )
    }
}

/// Run a pure certified follower (`--upstream` without `--validator`) until
/// shutdown.
pub(in crate::stack) async fn run_plain_follower<E>(
    ctx: E,
    args: ConsensusArgs,
    (node, bridge): (OutbeFullNode, ConsensusExecutionBridge),
    (finalized_execution_barrier, follower_shutdown): (
        Arc<dyn FinalizedExecutionBarrier>,
        Option<crate::follower_shutdown::FollowerDrain>,
    ),
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
    let upstream = args
        .upstream
        .clone()
        .ok_or(crate::errors::StackEpochError::FollowerRequiresUpstream)?;
    let follower_shutdown = follower_shutdown
        .ok_or(crate::errors::StackEpochError::FollowerPreStopHandshakeNotInstalled)?;
    bridge.set_operating_mode(ConsensusOperatingMode::Follower);
    let exit = run_follow_stack(
        ctx,
        args,
        services::FollowerConnection {
            node,
            bridge,
            upstream,
        },
        services::FollowerStackServices {
            finalized_execution_barrier,
            follower_shutdown,
            promotion: None,
        },
    )
    .await?;
    match exit {
        crate::follower_shutdown::FollowStackExit::Shutdown => Ok(()),
        crate::follower_shutdown::FollowStackExit::PromoteToValidator { .. } => {
            Err(crate::errors::StackEpochError::UnexpectedFollowerPromotion.into())
        }
    }
}

/// Phase 1 of the validator lifecycle.
pub(in crate::stack) struct UpstreamBootstrap<'a> {
    pub(in crate::stack) args: &'a ConsensusArgs,
    pub(in crate::stack) node: &'a OutbeFullNode,
    pub(in crate::stack) bridge: &'a ConsensusExecutionBridge,
    pub(in crate::stack) local_consensus_key: &'a bls12381::PublicKey,
    pub(in crate::stack) tee_lease_armed: &'a tokio::sync::watch::Receiver<bool>,
    pub(in crate::stack) finalized_execution_barrier: &'a Arc<dyn FinalizedExecutionBarrier>,
    pub(in crate::stack) follower_shutdown: &'a mut Option<crate::follower_shutdown::FollowerDrain>,
}

impl UpstreamBootstrap<'_> {
    /// Returns `true` when the validator stack may start in this process.
    ///
    /// A bootstrap-ready validator skips this phase, even when `--upstream`
    /// is set. Otherwise the validator needs `--upstream`. It runs the
    /// certified follower stack until `ValidatorBootstrapReady` holds at or
    /// above the upstream tip of its start, and the follower phase drained.
    /// Returns `false` when the host stopped the node during the phase.
    pub(in crate::stack) async fn run_until_ready<E>(self, ctx: &E) -> Result<bool>
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
        let armed = *self.tee_lease_armed.borrow();
        if is_validator_bootstrap_ready(self.node, self.local_consensus_key, armed)? {
            info!("validator is bootstrap-ready; starting the validator stack without UpstreamBootstrap");
            return Ok(true);
        }
        let upstream = self
            .args
            .upstream
            .clone()
            .ok_or(crate::errors::StackEpochError::ValidatorBootstrapRequiresUpstream)?;
        let follower_shutdown = self
            .follower_shutdown
            .take()
            .ok_or(crate::errors::StackEpochError::FollowerPreStopHandshakeNotInstalled)?;
        self.bridge
            .set_operating_mode(ConsensusOperatingMode::UpstreamBootstrap);
        let initial_upstream_tip = {
            use outbe_consensus::follow::TipSource as _;
            crate::follow_transport::UpstreamRpcClient::new(&upstream)?
                .finalized_tip()
                .await
                .ok_or(crate::errors::StackEpochError::UpstreamBootstrapTipUnavailable)?
                .get()
        };
        info!(
            initial_upstream_tip,
            "validator is not bootstrap-ready; starting UpstreamBootstrap"
        );
        let exit = run_follow_stack(
            ctx.child("upstream_bootstrap"),
            self.args.clone(),
            services::FollowerConnection {
                node: self.node.clone(),
                bridge: self.bridge.clone(),
                upstream,
            },
            services::FollowerStackServices {
                finalized_execution_barrier: Arc::clone(self.finalized_execution_barrier),
                follower_shutdown,
                promotion: Some(PromotionProbe {
                    node: self.node.clone(),
                    local_consensus_key: self.local_consensus_key.clone(),
                    tee_lease_armed: self.tee_lease_armed.clone(),
                    initial_upstream_tip,
                }),
            },
        )
        .await?;
        let crate::follower_shutdown::FollowStackExit::PromoteToValidator { finalized_height } =
            exit
        else {
            return Ok(false);
        };
        info!(
            finalized_height,
            "UpstreamBootstrap complete; starting the validator stack in the same process"
        );
        Ok(true)
    }
}
