use crate::*;

const TEE_LEASE_GUARD_POLL_SECS: u64 = 1;

/// Exact finalized checkpoint at which a node may arm its local lease guard
/// after replaying stale or pre-registration history. This is process-local
/// startup authority, never consensus or wire state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalTeeAdmissionAnchorV1 {
    pub(crate) finalized_height: u64,
    pub(crate) finalized_hash: alloy_primitives::B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TeeLeaseGuardGateV1 {
    pending_anchor: Option<LocalTeeAdmissionAnchorV1>,
}

impl TeeLeaseGuardGateV1 {
    pub(crate) const fn new(pending_anchor: Option<LocalTeeAdmissionAnchorV1>) -> Self {
        Self { pending_anchor }
    }

    pub(crate) const fn is_armed(self) -> bool {
        self.pending_anchor.is_none()
    }

    pub(crate) const fn anchor_to_validate(
        self,
        local_finalized_height: u64,
    ) -> Option<LocalTeeAdmissionAnchorV1> {
        match self.pending_anchor {
            Some(anchor) if local_finalized_height >= anchor.finalized_height => Some(anchor),
            _ => None,
        }
    }

    pub(crate) fn validate_and_arm(
        &mut self,
        observed_hash: alloy_primitives::B256,
        admission: outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
    ) -> eyre::Result<()> {
        let Some(anchor) = self.pending_anchor else {
            return Ok(());
        };
        eyre::ensure!(
            observed_hash == anchor.finalized_hash,
            "TEE admission anchor hash mismatch at height {}: expected {}, local {}",
            anchor.finalized_height,
            anchor.finalized_hash,
            observed_hash
        );
        match admission {
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. } => {
                self.pending_anchor = None;
                Ok(())
            }
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::BootstrapPending => {
                eyre::bail!("TEE admission anchor unexpectedly has bootstrap-pending TEE state");
            }
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Rejected(reason) => {
                eyre::bail!(
                    "TEE admission anchor rejected the local identity: {}",
                    local_tee_rejection_message(reason)
                );
            }
        }
    }
}

pub(crate) fn require_validator_tee_recovery_complete_v1(
    is_validator: bool,
    gate: TeeLeaseGuardGateV1,
    node_data_dir: &Path,
) -> eyre::Result<()> {
    if !is_validator || gate.is_armed() {
        return Ok(());
    }

    let Some(anchor) = gate.pending_anchor else {
        return Ok(());
    };
    eyre::bail!(
        "validator recovery requires certified follower catch-up before authority startup: local finalized state has not reached the durable TEE join anchor at height {} ({}) in {}. Stop this process; start the same outbe-chain binary with this same datadir and the same network/TEE options, omit --validator and every validator signing/Radicle authority flag, and add --upstream <healthy-certified-rpc> (do not use --upstream.nocertify). Wait for `local TEE lease guard armed at authenticated catch-up anchor`, stop the follower, then restart the original validator command. Submit readiness only after the validator is caught up; signing authority returns only after a fresh DKG installs current private material",
        anchor.finalized_height,
        anchor.finalized_hash,
        node_data_dir.display(),
    );
}

pub(crate) fn validator_admission_anchor_from_durable_v1(
    durable: outbe_tee::FinalizedJoinAdmissionAnchorV1,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<LocalTeeAdmissionAnchorV1> {
    use outbe_primitives::tee_attestation_v1::NodeIdV1;

    let node_id_hash = NodeIdV1 {
        reth_p2p_public: identity.reth_p2p_public,
    }
    .node_id_hash()
    .map_err(|error| eyre::eyre!("derive local NodeHost identity: {error}"))?;
    eyre::ensure!(
        durable.chain_id == alloy_primitives::U256::from(chain_id).to_be_bytes(),
        "finalized join admission anchor chain id mismatch"
    );
    eyre::ensure!(
        durable.genesis_hash == genesis_hash,
        "finalized join admission anchor genesis mismatch"
    );
    eyre::ensure!(
        durable.node_id_hash == node_id_hash,
        "finalized join admission anchor NodeHost identity mismatch"
    );
    eyre::ensure!(
        identity.expected_enclave_id == Some(durable.enclave_id),
        "finalized join admission anchor enclave identity mismatch"
    );
    Ok(LocalTeeAdmissionAnchorV1 {
        finalized_height: durable.finalized_height,
        finalized_hash: durable.finalized_hash,
    })
}

fn local_tee_rejection_message(
    rejection: outbe_engine::validators::LocalTeeRuntimeRejectionV1,
) -> String {
    use outbe_engine::validators::LocalTeeRuntimeRejectionV1;

    match rejection {
        LocalTeeRuntimeRejectionV1::MissingBinding => {
            "finalized Registry has no binding for the local NodeHost; run tee join".to_owned()
        }
        LocalTeeRuntimeRejectionV1::EnclaveIdentityMismatch => {
            "finalized Registry binding does not match the committed local enclave; run tee join"
                .to_owned()
        }
        LocalTeeRuntimeRejectionV1::ValidatorBindingMismatch => {
            "finalized validator and local NodeHost bindings disagree; refusing consensus startup"
                .to_owned()
        }
        LocalTeeRuntimeRejectionV1::ValidatorJailed => {
            "validator is jailed; complete ordinary unjail and then run tee join".to_owned()
        }
        LocalTeeRuntimeRejectionV1::Expired { valid_until } => {
            format!("finalized TEE lease expired at {valid_until}; stop node and run tee join")
        }
    }
}

pub(crate) fn tee_lease_admission_rejection(
    admission: outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
) -> Option<String> {
    match admission {
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1::BootstrapPending
        | outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. } => None,
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Rejected(reason) => {
            Some(local_tee_rejection_message(reason))
        }
    }
}

pub(crate) fn validator_recovery_startup_admission_rejection(
    admission: outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
) -> Option<String> {
    match admission {
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1::BootstrapPending => Some(
            "finalized TEE admission is bootstrap-pending; refusing validator authority startup"
                .to_owned(),
        ),
        other => tee_lease_admission_rejection(other),
    }
}

fn read_local_tee_admission_at_height<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    block_number: u64,
) -> eyre::Result<(
    alloy_primitives::B256,
    outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
)>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let header = provider
        .sealed_header(block_number)
        .wrap_err("read exact header for TEE lease admission")?
        .ok_or_else(|| eyre::eyre!("exact TEE lease admission header is unavailable"))?;
    let block_hash = header.hash();
    let state = provider
        .state_by_block_hash(block_hash)
        .wrap_err("read exact state for TEE lease admission")?;
    let admission = outbe_engine::validators::read_local_tee_runtime_admission_from_state(
        &state,
        outbe_primitives::storage::readonly::ReadOnlyBlockContext {
            chain_id,
            genesis_hash,
            block_number,
            timestamp: header.header().inner.timestamp,
        },
        identity,
    )?;
    Ok((block_hash, admission))
}

fn read_finalized_local_tee_admission<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<Option<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let Some(finalized) = provider
        .finalized_block_num_hash()
        .wrap_err("read finalized head for TEE lease admission")?
    else {
        return Ok(None);
    };
    if finalized.number == 0 {
        return Ok(None);
    }
    let (header_hash, admission) = read_local_tee_admission_at_height(
        provider,
        chain_id,
        genesis_hash,
        identity,
        finalized.number,
    )?;
    eyre::ensure!(
        header_hash == finalized.hash,
        "finalized TEE lease header hash mismatch: marker {}, header {}",
        finalized.hash,
        header_hash
    );
    Ok(Some(admission))
}

pub(crate) fn read_gated_finalized_local_tee_admission<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    gate: &mut TeeLeaseGuardGateV1,
) -> eyre::Result<Option<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let Some(finalized) = provider
        .finalized_block_num_hash()
        .wrap_err("read finalized head for gated TEE lease admission")?
    else {
        return Ok(None);
    };
    if finalized.number == 0 {
        return Ok(None);
    }
    let Some(anchor) = gate.anchor_to_validate(finalized.number) else {
        return if gate.is_armed() {
            read_finalized_local_tee_admission(provider, chain_id, genesis_hash, identity)
        } else {
            Ok(None)
        };
    };

    let (anchor_hash, anchor_admission) = read_local_tee_admission_at_height(
        provider,
        chain_id,
        genesis_hash,
        identity,
        anchor.finalized_height,
    )?;
    gate.validate_and_arm(anchor_hash, anchor_admission)?;
    info!(
        anchor_height = anchor.finalized_height,
        anchor_hash = %anchor.finalized_hash,
        local_finalized_height = finalized.number,
        "local TEE lease guard armed at authenticated catch-up anchor"
    );
    if finalized.number == anchor.finalized_height {
        return Ok(Some(anchor_admission));
    }
    read_finalized_local_tee_admission(provider, chain_id, genesis_hash, identity)
}

pub(crate) async fn require_upstream_fullnode_tee_admission(
    upstream: &str,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<LocalTeeAdmissionAnchorV1> {
    let rpc = outbe_operator::rpc::HttpRenewalRpc::new(upstream);
    let view = read_finalized_registry_view_v1(
        &rpc,
        &NodeBindingSelectorV1::NodeHost(identity.reth_p2p_public),
    )
    .await
    .wrap_err("read upstream finalized FullNode TEE admission")?;
    let binding = view.binding.ok_or_else(|| {
        eyre::eyre!("finalized Registry has no binding for this FullNode; run tee join first")
    })?;
    if identity
        .expected_enclave_id
        .is_some_and(|expected| expected != binding.enclave_id)
    {
        eyre::bail!(
            "finalized FullNode binding does not match the committed local enclave; run tee join"
        );
    }
    if binding.valid_until <= view.schedule.finalized_timestamp {
        eyre::bail!(
            "finalized FullNode TEE lease expired at {}; run tee join before startup",
            binding.valid_until
        );
    }
    Ok(LocalTeeAdmissionAnchorV1 {
        finalized_height: view.view.block_number,
        finalized_hash: view.view.block_hash,
    })
}

pub(crate) async fn run_tee_lease_guard_v1<P>(
    provider: P,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    mut gate: TeeLeaseGuardGateV1,
    shutdown: tokio_util::sync::CancellationToken,
) -> eyre::Result<Option<String>>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    let mut interval = tokio::time::interval(Duration::from_secs(TEE_LEASE_GUARD_POLL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(None),
            _ = interval.tick() => {}
        }
        match read_gated_finalized_local_tee_admission(
            &provider,
            chain_id,
            genesis_hash,
            identity,
            &mut gate,
        ) {
            Ok(None) => {}
            Ok(Some(admission)) => {
                if let Some(reason) = tee_lease_admission_rejection(admission) {
                    return Ok(Some(reason));
                }
            }
            Err(error) => {
                return Err(error.wrap_err("finalized TEE lease admission failed closed"));
            }
        }
    }
}

/// A canonical lease rejection is an expected stop; failure to read that state
/// is still an operational error, even though both stop the local node.
pub(crate) fn tee_lease_exit_reason(
    verdict: Option<eyre::Result<String>>,
    shutdown: &outbe_node::shutdown::NodeShutdown,
) -> String {
    match verdict.unwrap_or_else(|| Err(eyre::eyre!("TEE lease guard stopped without a verdict"))) {
        Ok(reason) => reason,
        Err(error) => {
            let reason = format!("{error:#}");
            shutdown.record_failure(error);
            reason
        }
    }
}
