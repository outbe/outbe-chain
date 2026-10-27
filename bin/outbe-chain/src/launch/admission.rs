use crate::errors::LaunchError;
use crate::*;
use outbe_node::tee_remote_session::{
    inspect_local_finalized_successor_status_v1, RegistryChainIdentity,
};

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
pub(crate) enum TeeLeaseGuardGateV1 {
    AwaitingAnchor,
    Pending(LocalTeeAdmissionAnchorV1),
    Armed,
}

impl TeeLeaseGuardGateV1 {
    pub(crate) const fn new(pending_anchor: Option<LocalTeeAdmissionAnchorV1>) -> Self {
        match pending_anchor {
            Some(anchor) => Self::Pending(anchor),
            None => Self::AwaitingAnchor,
        }
    }

    pub(crate) const fn is_armed(self) -> bool {
        matches!(self, Self::Armed)
    }

    pub(crate) const fn anchor_to_validate(
        self,
        local_finalized_height: u64,
    ) -> Option<LocalTeeAdmissionAnchorV1> {
        match self {
            Self::Pending(anchor) if local_finalized_height >= anchor.finalized_height => {
                Some(anchor)
            }
            _ => None,
        }
    }

    pub(crate) fn validate_and_arm(
        &mut self,
        observed_hash: alloy_primitives::B256,
        admission: outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
    ) -> eyre::Result<()> {
        let Self::Pending(anchor) = *self else {
            return Ok(());
        };
        if observed_hash != anchor.finalized_hash {
            return Err(LaunchError::TeeAdmissionAnchorHashMismatch {
                height: anchor.finalized_height,
                expected: anchor.finalized_hash,
                local: observed_hash,
            }
            .into());
        }
        match admission {
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. } => {
                *self = Self::Armed;
                Ok(())
            }
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::BootstrapPending => {
                Err(LaunchError::TeeAdmissionAnchorBootstrapPending.into())
            }
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Rejected(reason) => {
                Err(LaunchError::TeeAdmissionAnchorRejected {
                    reason: local_tee_rejection_message(reason),
                }
                .into())
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

    let TeeLeaseGuardGateV1::Pending(anchor) = gate else {
        return Err(
            outbe_engine::errors::StackEpochError::ValidatorBootstrapRequiresUpstream.into(),
        );
    };
    Err(LaunchError::ValidatorRecoveryCatchUpRequired {
        height: anchor.finalized_height,
        hash: anchor.finalized_hash,
        datadir: node_data_dir.display().to_string(),
    }
    .into())
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
    .map_err(|error| LaunchError::DeriveLocalNodeHostIdentity {
        details: crate::errors::display_error(error),
    })?;
    if durable.chain_id != alloy_primitives::U256::from(chain_id).to_be_bytes() {
        return Err(LaunchError::JoinAdmissionAnchorChainIdMismatch.into());
    }
    if durable.genesis_hash != genesis_hash {
        return Err(LaunchError::JoinAdmissionAnchorGenesisMismatch.into());
    }
    if durable.node_id_hash != node_id_hash {
        return Err(LaunchError::JoinAdmissionAnchorNodeHostMismatch.into());
    }
    if identity.expected_enclave_id != Some(durable.enclave_id) {
        return Err(LaunchError::JoinAdmissionAnchorEnclaveMismatch.into());
    }
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
        LocalTeeRuntimeRejectionV1::RetiredEnclave => {
            "enclave upgrade deadline reached; complete tee upgrade before restarting TEE work"
                .to_owned()
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

#[cfg(test)]
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
        .ok_or(LaunchError::TeeAdmissionHeaderUnavailable)?;
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

fn read_current_finalized_tee_admission<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    context: &'static str,
) -> eyre::Result<
    Option<(
        alloy_eips::BlockNumHash,
        alloy_primitives::B256,
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
    )>,
>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let Some(finalized) = provider
        .finalized_block_num_hash()
        .wrap_err(context)?
        .filter(|finalized| finalized.number != 0)
    else {
        return Ok(None);
    };
    let (header_hash, admission) = read_local_tee_admission_at_height(
        provider,
        chain_id,
        genesis_hash,
        identity,
        finalized.number,
    )?;
    Ok(Some((finalized, header_hash, admission)))
}

pub(crate) fn read_finalized_local_tee_admission<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<Option<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let Some((finalized, header_hash, admission)) = read_current_finalized_tee_admission(
        provider,
        chain_id,
        genesis_hash,
        identity,
        "read finalized head for TEE lease admission",
    )?
    else {
        return Ok(None);
    };
    if header_hash == finalized.hash {
        return Ok(Some(admission));
    }
    // Reread once to rule out a transient multi-call provider race before
    // declaring a canonical finalized-header conflict.
    let Some((refreshed, retry_hash, retry_admission)) = read_current_finalized_tee_admission(
        provider,
        chain_id,
        genesis_hash,
        identity,
        "reread finalized head for TEE lease admission",
    )?
    else {
        return Ok(None);
    };
    (retry_hash == refreshed.hash)
        .then_some(Some(retry_admission))
        .ok_or_else(|| {
            LaunchError::FinalizedTeeLeaseHeaderHashMismatch {
                marker: refreshed.hash,
                header: retry_hash,
            }
            .into()
        })
}

fn authenticate_pending_gate_anchor<P>(
    provider: &P,
    chain: RegistryChainIdentity,
    fullnode_identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    gate: TeeLeaseGuardGateV1,
    finalized_number: u64,
) -> eyre::Result<bool>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let Some(anchor) = gate.anchor_to_validate(finalized_number) else {
        return Ok(!matches!(gate, TeeLeaseGuardGateV1::Pending(_)));
    };
    let (mut hash, mut admission) = read_local_tee_admission_at_height(
        provider,
        chain.chain_id,
        chain.genesis_hash,
        fullnode_identity,
        anchor.finalized_height,
    )?;
    if hash != anchor.finalized_hash {
        (hash, admission) = read_local_tee_admission_at_height(
            provider,
            chain.chain_id,
            chain.genesis_hash,
            fullnode_identity,
            anchor.finalized_height,
        )?;
    }
    // Authenticate the checkpoint without granting validator authority yet.
    let mut authenticated = TeeLeaseGuardGateV1::Pending(anchor);
    authenticated.validate_and_arm(hash, admission)?;
    Ok(true)
}

fn resident_offer_matches_finalized_state<P>(
    provider: &P,
    finalized_hash: alloy_primitives::B256,
    has_validator_identity: bool,
) -> eyre::Result<bool>
where
    P: StateProviderFactory,
{
    if !has_validator_identity {
        return Ok(true);
    }
    let state = provider.state_by_block_hash(finalized_hash)?;
    let on_chain_offer = outbe_engine::validators::read_tee_offer_public_from_state(&state)?;
    if on_chain_offer.is_zero() {
        return Ok(false);
    }
    if outbe_tee::resident_offer_public_key_state_v1()? != Some(on_chain_offer) {
        return Err(LaunchError::LocalEnclaveOfferKeyMismatch.into());
    }
    Ok(true)
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
        .filter(|finalized| finalized.number != 0)
    else {
        return Ok(None);
    };
    if gate.is_armed() {
        return read_finalized_local_tee_admission(provider, chain_id, genesis_hash, identity);
    }
    // Replay checks the NodeHost lease before validator stake/binding exists.
    let fullnode_identity = outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
        validator: None,
        ..identity
    };
    if !authenticate_pending_gate_anchor(
        provider,
        RegistryChainIdentity {
            chain_id,
            genesis_hash,
        },
        fullnode_identity,
        *gate,
        finalized.number,
    )? {
        return Ok(None);
    }
    let fullnode =
        read_finalized_local_tee_admission(provider, chain_id, genesis_hash, fullnode_identity)?;
    let validator = read_finalized_local_tee_admission(provider, chain_id, genesis_hash, identity)?;
    if matches!(
        (&fullnode, &validator),
        (
            Some(outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. }),
            Some(outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. }),
        )
    ) && resident_offer_matches_finalized_state(
        provider,
        finalized.hash,
        identity.validator.is_some(),
    )? {
        *gate = TeeLeaseGuardGateV1::Armed;
        info!(
            local_finalized_height = finalized.number,
            "local TEE lease guard armed at authenticated catch-up anchor"
        );
        return Ok(validator);
    }
    Ok(fullnode)
}

pub(crate) async fn require_upstream_fullnode_tee_admission(
    upstream: &str,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<LocalTeeAdmissionAnchorV1> {
    let rpc = outbe_operator::rpc::HttpRenewalRpc::new(upstream)?;
    let view = read_finalized_registry_view_v1(
        &rpc,
        &NodeBindingSelectorV1::NodeHost(identity.reth_p2p_public),
    )
    .await
    .wrap_err("read upstream finalized FullNode TEE admission")?;
    let binding = view.binding.ok_or(LaunchError::FullNodeMissingBinding)?;
    if identity
        .expected_enclave_id
        .is_some_and(|expected| expected != binding.enclave_id)
    {
        return Err(LaunchError::FullNodeBindingEnclaveMismatch.into());
    }
    if binding.valid_until <= view.schedule.finalized_timestamp {
        return Err(LaunchError::FullNodeTeeLeaseExpired {
            valid_until: binding.valid_until,
        }
        .into());
    }
    Ok(LocalTeeAdmissionAnchorV1 {
        finalized_height: view.view.block_number,
        finalized_hash: view.view.block_hash,
    })
}

#[derive(Debug, thiserror::Error)]
enum UpstreamJoinAnchorError {
    #[error("durable join anchor at {durable} is ahead of upstream finalized height {upstream}")]
    Ahead { durable: u64, upstream: u64 },
    #[error("upstream canonical block at durable join height has no hash")]
    MissingHash,
}

pub(crate) async fn validate_upstream_join_anchor(
    upstream: &str,
    durable: LocalTeeAdmissionAnchorV1,
    anchor: LocalTeeAdmissionAnchorV1,
) -> eyre::Result<()> {
    use outbe_operator::rpc::FinalityRpc as _;
    if durable.finalized_height > anchor.finalized_height {
        return Err(UpstreamJoinAnchorError::Ahead {
            durable: durable.finalized_height,
            upstream: anchor.finalized_height,
        }
        .into());
    }
    let rpc = outbe_operator::rpc::HttpRenewalRpc::new(upstream)?;
    let block = rpc.block_by_number(durable.finalized_height).await?;
    let hash: alloy_primitives::B256 = serde_json::from_value(
        block
            .get("hash")
            .cloned()
            .ok_or(UpstreamJoinAnchorError::MissingHash)?,
    )?;
    if hash != durable.finalized_hash {
        return Err(LaunchError::TeeAdmissionAnchorHashMismatch {
            height: durable.finalized_height,
            expected: durable.finalized_hash,
            local: hash,
        }
        .into());
    }
    Ok(())
}

pub(crate) struct TeeLeaseGuardConfigV1 {
    pub(crate) chain: RegistryChainIdentity,
    pub(crate) identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    pub(crate) gate: TeeLeaseGuardGateV1,
}

fn maybe_retire_remote_sessions<P>(
    provider: &P,
    chain: RegistryChainIdentity,
    gate: TeeLeaseGuardGateV1,
    retired_at: &mut u64,
) -> eyre::Result<()>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    if !gate.is_armed()
        || provider
            .finalized_block_num_hash()?
            .is_none_or(|head| head.number == 0)
    {
        return Ok(());
    }
    let status = inspect_local_finalized_successor_status_v1(provider, chain)?;
    if status.retirement_height <= *retired_at {
        return Ok(());
    }
    let activation_height = status.retirement_height;
    let response = outbe_tee::try_with_enclave(|session| {
        session.request(
            &outbe_tee::protocol::EnclaveRequest::RetireRemoteSessionsV1 { activation_height },
        )
    })
    .ok_or(LaunchError::TeeClientUnavailableDuringRetirement)??;
    if !matches!(response, outbe_tee::protocol::EnclaveResponse::RemoteSessionsRetiredV1 { activation_height: echoed } if echoed == activation_height)
    {
        return Err(LaunchError::EnclaveDidNotAckSessionRetirement.into());
    }
    *retired_at = activation_height;
    Ok(())
}

pub(crate) async fn run_tee_lease_guard_v1<P>(
    provider: P,
    config: TeeLeaseGuardConfigV1,
    shutdown: tokio_util::sync::CancellationToken,
    finalization_notify: Arc<tokio::sync::Notify>,
    tee_lease_armed_tx: tokio::sync::watch::Sender<bool>,
) -> eyre::Result<Option<String>>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    let TeeLeaseGuardConfigV1 {
        chain,
        identity,
        mut gate,
    } = config;
    let RegistryChainIdentity {
        chain_id,
        genesis_hash,
    } = chain;
    let mut retired_at = 0;
    let mut interval = tokio::time::interval(Duration::from_secs(TEE_LEASE_GUARD_POLL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(None),
            _ = interval.tick() => {},
            _ = finalization_notify.notified() => {}
        }
        maybe_retire_remote_sessions(&provider, chain, gate, &mut retired_at)?;
        let admission = read_gated_finalized_local_tee_admission(
            &provider,
            chain_id,
            genesis_hash,
            identity,
            &mut gate,
        )
        .wrap_err("finalized TEE lease admission failed closed")?;
        if let Some(admission) = admission {
            if gate.is_armed() {
                tee_lease_armed_tx.send_replace(true);
            }
            if let Some(reason) = tee_lease_admission_rejection(admission) {
                return Ok(Some(reason));
            }
        }
    }
}

/// A canonical lease rejection is an expected stop. Failure to read that state
/// is still an operational error, even though both stop the local node.
pub(crate) fn tee_lease_exit_reason(
    verdict: Option<eyre::Result<String>>,
    shutdown: &outbe_node::shutdown::NodeShutdown,
) -> String {
    match verdict.unwrap_or_else(|| Err(LaunchError::TeeLeaseGuardStoppedWithoutVerdict.into())) {
        Ok(reason) => reason,
        Err(error) => {
            let reason = format!("{error:#}");
            shutdown.record_failure(error);
            reason
        }
    }
}
