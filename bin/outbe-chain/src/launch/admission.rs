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

/// Local TEE lease guard state. `is_armed()` is true only after an admission
/// anchor validated against the local canonical chain, or for a genesis
/// founder at height zero. A non-founding validator can never satisfy
/// `ValidatorBootstrapReady` before that validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TeeLeaseGuardGateV1 {
    /// Node is waiting for an admission anchor to be established/loaded.
    AwaitingAnchor,
    /// Node has a target admission anchor and must validate it at `local_finalized_height >= anchor.finalized_height`.
    Pending(LocalTeeAdmissionAnchorV1),
    /// Anchor has been validated against the local canonical Reth DB (or node is a genesis founder at H == 0).
    Armed,
}

impl TeeLeaseGuardGateV1 {
    pub(crate) const fn is_armed(self) -> bool {
        matches!(self, Self::Armed)
    }

    pub(crate) const fn pending_anchor(self) -> Option<LocalTeeAdmissionAnchorV1> {
        match self {
            Self::Pending(anchor) => Some(anchor),
            Self::AwaitingAnchor | Self::Armed => None,
        }
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

    pub(crate) fn validate_anchor(
        anchor: LocalTeeAdmissionAnchorV1,
        observed_hash: alloy_primitives::B256,
        admission: outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
    ) -> eyre::Result<()> {
        if observed_hash != anchor.finalized_hash {
            return Err(LaunchError::TeeAdmissionAnchorHashMismatch {
                height: anchor.finalized_height,
                expected: anchor.finalized_hash,
                local: observed_hash,
            }
            .into());
        }
        match admission {
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { .. } => Ok(()),
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

    #[cfg(test)]
    pub(crate) fn validate_and_arm(
        &mut self,
        observed_hash: alloy_primitives::B256,
        admission: outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
    ) -> eyre::Result<()> {
        let Self::Pending(anchor) = *self else {
            return Ok(());
        };
        Self::validate_anchor(anchor, observed_hash, admission)?;
        *self = Self::Armed;
        Ok(())
    }
}

/// A validator without `--upstream` must not start while its gate is not
/// armed. With `--upstream`, the engine runs `UpstreamBootstrap` until the
/// gate arms.
pub(crate) fn require_validator_tee_recovery_complete_v1(
    is_validator: bool,
    has_upstream: bool,
    gate: TeeLeaseGuardGateV1,
    node_data_dir: &Path,
) -> eyre::Result<()> {
    if !is_validator || has_upstream || gate.is_armed() {
        return Ok(());
    }
    let (height, hash) = gate
        .pending_anchor()
        .map_or((0, alloy_primitives::B256::ZERO), |anchor| {
            (anchor.finalized_height, anchor.finalized_hash)
        });
    Err(LaunchError::ValidatorRecoveryCatchUpRequired {
        height,
        hash,
        datadir: node_data_dir.display().to_string(),
    }
    .into())
}

/// TEE admission anchors verified before the lease guard starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TeeLeaseGuardAnchorsV1 {
    pub(crate) gate: TeeLeaseGuardGateV1,
    /// Durable join anchor that the guard also checks against the local
    /// canonical chain when it arms a different (upstream) target anchor.
    pub(crate) durable: Option<LocalTeeAdmissionAnchorV1>,
}

/// Fail-closed dual anchor decision for `--validator --upstream`.
///
/// The durable join anchor must not be above the upstream anchor, and the
/// upstream canonical hash at the durable height must equal the durable hash.
/// The target anchor is the higher of the two.
pub(crate) fn select_validator_upstream_anchor_v1(
    durable: Option<LocalTeeAdmissionAnchorV1>,
    upstream: LocalTeeAdmissionAnchorV1,
    upstream_hash_at_durable: Option<alloy_primitives::B256>,
) -> eyre::Result<TeeLeaseGuardAnchorsV1> {
    let Some(durable) = durable else {
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate: TeeLeaseGuardGateV1::Pending(upstream),
            durable: None,
        });
    };
    if durable.finalized_height > upstream.finalized_height {
        return Err(LaunchError::TeeJoinAnchorAboveUpstreamAnchor {
            durable: durable.finalized_height,
            upstream: upstream.finalized_height,
        }
        .into());
    }
    let observed = upstream_hash_at_durable.ok_or(LaunchError::TeeAdmissionAnchorHashMismatch {
        height: durable.finalized_height,
        expected: durable.finalized_hash,
        local: alloy_primitives::B256::ZERO,
    })?;
    if observed != durable.finalized_hash {
        return Err(LaunchError::TeeAdmissionAnchorHashMismatch {
            height: durable.finalized_height,
            expected: durable.finalized_hash,
            local: observed,
        }
        .into());
    }
    let target = if durable.finalized_height == upstream.finalized_height {
        durable
    } else {
        upstream
    };
    Ok(TeeLeaseGuardAnchorsV1 {
        gate: TeeLeaseGuardGateV1::Pending(target),
        durable: (target != durable).then_some(durable),
    })
}

/// Initial validator gate from local facts only (no upstream).
///
/// - A durable join anchor at or below the local finalized height is pending
///   and validates at once.
/// - Without a durable anchor at height `H >= 1`, a `Ready` local admission at
///   the latest finalized block and a resident offer key equal to the
///   on-chain key start the gate armed. This covers founding validators and
///   nodes without an expected enclave identity.
/// - A genesis founder at height zero starts armed. The engine still requires
///   genesis `ValidatorSet` membership at that height.
///
/// `None` means that local facts cannot arm the gate. The caller then needs
/// the upstream path.
pub(crate) fn local_validator_tee_gate_v1(
    durable: Option<LocalTeeAdmissionAnchorV1>,
    local_finalized_height: u64,
    local_ready_with_offer: bool,
    local_is_genesis_founder: bool,
    has_upstream: bool,
) -> Option<TeeLeaseGuardGateV1> {
    match durable {
        Some(anchor) if local_finalized_height >= anchor.finalized_height => {
            Some(TeeLeaseGuardGateV1::Pending(anchor))
        }
        Some(anchor) if !has_upstream => Some(TeeLeaseGuardGateV1::Pending(anchor)),
        Some(_) => None,
        None if local_finalized_height >= 1 && local_ready_with_offer => {
            Some(TeeLeaseGuardGateV1::Armed)
        }
        None if local_finalized_height == 0 && local_is_genesis_founder => {
            Some(TeeLeaseGuardGateV1::Armed)
        }
        None if has_upstream => None,
        None => Some(TeeLeaseGuardGateV1::AwaitingAnchor),
    }
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
    chain: RegistryChainIdentity,
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
    read_local_tee_admission_at_block(
        provider,
        chain,
        identity,
        (
            alloy_eips::BlockNumHash::new(block_number, block_hash),
            header.header().inner.timestamp,
        ),
    )
    .map(|admission| (block_hash, admission))
}

fn validate_local_anchor_at_height<P>(
    provider: &P,
    chain: RegistryChainIdentity,
    fullnode_identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    anchor: LocalTeeAdmissionAnchorV1,
) -> eyre::Result<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let (mut hash, mut admission) = read_local_tee_admission_at_height(
        provider,
        chain,
        fullnode_identity,
        anchor.finalized_height,
    )?;
    if hash != anchor.finalized_hash {
        (hash, admission) = read_local_tee_admission_at_height(
            provider,
            chain,
            fullnode_identity,
            anchor.finalized_height,
        )?;
    }
    TeeLeaseGuardGateV1::validate_anchor(anchor, hash, admission)?;
    Ok(admission)
}

fn read_consistent_finalized_head<P>(
    provider: &P,
    context: &'static str,
) -> eyre::Result<Option<(alloy_eips::BlockNumHash, u64)>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader>,
{
    let mut last_error = LaunchError::FinalizedTeeLeaseHeaderHashMismatch {
        marker: alloy_primitives::B256::ZERO,
        header: alloy_primitives::B256::ZERO,
    };
    for _ in 0..2 {
        let Some(finalized) = provider
            .finalized_block_num_hash()
            .wrap_err(context)?
            .filter(|finalized| finalized.number != 0)
        else {
            return Ok(None);
        };
        let header = provider
            .sealed_header(finalized.number)
            .wrap_err("read exact header for TEE lease admission")?
            .ok_or(LaunchError::TeeAdmissionHeaderUnavailable)?;
        let header_hash = header.hash();
        if header_hash == finalized.hash {
            return Ok(Some((finalized, header.header().inner.timestamp)));
        }
        last_error = LaunchError::FinalizedTeeLeaseHeaderHashMismatch {
            marker: finalized.hash,
            header: header_hash,
        };
    }
    Err(last_error.into())
}

fn read_local_tee_admission_at_block<P>(
    provider: &P,
    chain: RegistryChainIdentity,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    (finalized, timestamp): (alloy_eips::BlockNumHash, u64),
) -> eyre::Result<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let genesis_timestamp = provider
        .header_by_number(0)
        .wrap_err("read genesis header for TEE lease admission")?
        .map(|header| header.inner.timestamp)
        .filter(|timestamp| *timestamp != 0)
        .ok_or(LaunchError::TeeAdmissionHeaderUnavailable)?;
    let state = provider
        .state_by_block_hash(finalized.hash)
        .wrap_err("read exact state for TEE lease admission")?;
    outbe_engine::validators::read_local_tee_runtime_admission_from_state(
        &state,
        outbe_primitives::storage::readonly::ReadOnlyBlockContext::try_new(
            chain.chain_id,
            chain.genesis_hash,
            genesis_timestamp,
            finalized.number,
            timestamp,
        )?,
        identity,
    )
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
    let Some(head) =
        read_consistent_finalized_head(provider, "read finalized head for TEE lease admission")?
    else {
        return Ok(None);
    };
    read_local_tee_admission_at_block(
        provider,
        RegistryChainIdentity {
            chain_id,
            genesis_hash,
        },
        identity,
        head,
    )
    .map(Some)
}

pub(crate) fn read_gated_finalized_local_tee_admission<P>(
    provider: &P,
    chain: RegistryChainIdentity,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    anchors: &mut TeeLeaseGuardAnchorsV1,
) -> eyre::Result<Option<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    read_gated_finalized_local_tee_admission_with_resident_offer(
        provider,
        chain,
        identity,
        anchors,
        || {
            outbe_tee::resident_offer_public_key_state_v1()
                .wrap_err("read the resident offer key for gated TEE lease admission")
        },
    )
}

pub(crate) fn read_gated_finalized_local_tee_admission_with_resident_offer<P, F>(
    provider: &P,
    chain: RegistryChainIdentity,
    identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    anchors: &mut TeeLeaseGuardAnchorsV1,
    read_resident_offer: F,
) -> eyre::Result<Option<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
    F: FnOnce() -> eyre::Result<Option<alloy_primitives::B256>>,
{
    use outbe_engine::validators::LocalTeeRuntimeAdmissionV1;

    let durable = anchors.durable;
    let gate = &mut anchors.gate;
    let Some(head @ (finalized, _)) = read_consistent_finalized_head(
        provider,
        "read finalized head for gated TEE lease admission",
    )?
    else {
        return Ok(None);
    };
    let Some(anchor) = gate.anchor_to_validate(finalized.number) else {
        return gate
            .is_armed()
            .then(|| read_local_tee_admission_at_block(provider, chain, identity, head))
            .transpose();
    };

    let fullnode_identity = outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
        validator: None,
        ..identity
    };
    let anchor_fullnode_admission =
        validate_local_anchor_at_height(provider, chain, fullnode_identity, anchor)?;
    if let Some(durable) = durable.filter(|durable| *durable != anchor) {
        validate_local_anchor_at_height(provider, chain, fullnode_identity, durable)?;
    }

    let fullnode_now =
        if finalized.number == anchor.finalized_height && finalized.hash == anchor.finalized_hash {
            anchor_fullnode_admission
        } else {
            read_local_tee_admission_at_block(provider, chain, fullnode_identity, head)?
        };
    if !matches!(fullnode_now, LocalTeeRuntimeAdmissionV1::Ready { .. }) {
        return Ok(Some(fullnode_now));
    }
    if identity.validator.is_none() {
        *gate = TeeLeaseGuardGateV1::Armed;
        info!(
            anchor_height = anchor.finalized_height,
            anchor_hash = %anchor.finalized_hash,
            local_finalized_height = finalized.number,
            "local TEE lease guard armed at authenticated catch-up anchor"
        );
        return Ok(Some(fullnode_now));
    }

    let validator_now = read_local_tee_admission_at_block(provider, chain, identity, head)?;
    try_arm_validator_tee_gate(
        provider,
        finalized,
        (fullnode_now, validator_now),
        (gate, anchor),
        read_resident_offer,
    )
    .map(Some)
}

fn try_arm_validator_tee_gate<P, F>(
    provider: &P,
    finalized: alloy_eips::BlockNumHash,
    (fullnode_now, validator_now): (
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1,
    ),
    (gate, anchor): (&mut TeeLeaseGuardGateV1, LocalTeeAdmissionAnchorV1),
    read_resident_offer: F,
) -> eyre::Result<outbe_engine::validators::LocalTeeRuntimeAdmissionV1>
where
    P: StateProviderFactory,
    F: FnOnce() -> eyre::Result<Option<alloy_primitives::B256>>,
{
    use outbe_engine::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    match validator_now {
        LocalTeeRuntimeAdmissionV1::BootstrapPending
        | LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::MissingBinding) => {
            Ok(fullnode_now)
        }
        LocalTeeRuntimeAdmissionV1::Rejected(_) => Ok(validator_now),
        LocalTeeRuntimeAdmissionV1::Ready { .. } => {
            let on_chain_offer =
                outbe_engine::validators::read_tee_offer_public_at_block(provider, finalized.hash)
                    .wrap_err("read the canonical offer key for gated TEE lease admission")?;
            let Some(resident_offer) = (!on_chain_offer.is_zero())
                .then(read_resident_offer)
                .transpose()?
                .flatten()
            else {
                return Ok(fullnode_now);
            };
            if resident_offer != on_chain_offer {
                return Err(LaunchError::LocalEnclaveOfferKeyMismatch.into());
            }
            *gate = TeeLeaseGuardGateV1::Armed;
            info!(
                anchor_height = anchor.finalized_height,
                anchor_hash = %anchor.finalized_hash,
                local_finalized_height = finalized.number,
                "local TEE lease guard armed at authenticated catch-up anchor"
            );
            Ok(validator_now)
        }
    }
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

pub(crate) struct TeeLeaseGuardConfigV1 {
    pub(crate) chain: RegistryChainIdentity,
    pub(crate) identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    /// Initial gate and the durable join anchor that must also validate when
    /// the gate arms.
    pub(crate) anchors: TeeLeaseGuardAnchorsV1,
    /// Signalled by the FinalizationActor and the FinalityObserver on every
    /// local finalization.
    pub(crate) finalization_notify: Arc<tokio::sync::Notify>,
    /// Publishes `true` when the gate becomes `Armed`.
    pub(crate) armed: tokio::sync::watch::Sender<bool>,
}

pub(crate) async fn run_tee_lease_guard_v1<P>(
    provider: P,
    config: TeeLeaseGuardConfigV1,
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
    let TeeLeaseGuardConfigV1 {
        chain,
        identity,
        mut anchors,
        finalization_notify,
        armed,
    } = config;
    let mut retired_at = 0;
    let mut interval = tokio::time::interval(Duration::from_secs(TEE_LEASE_GUARD_POLL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // Each local finalization wakes the guard at once. The interval
        // remains a fallback for missed wake-ups.
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(None),
            _ = finalization_notify.notified() => {}
            _ = interval.tick() => {}
        }
        if anchors.gate.is_armed() {
            retire_superseded_remote_sessions_v1(&provider, chain, &mut retired_at)?;
        }
        let admission =
            read_gated_finalized_local_tee_admission(&provider, chain, identity, &mut anchors)
                .map_err(|error| error.wrap_err("finalized TEE lease admission failed closed"))?;
        if let Some(reason) = admission.and_then(tee_lease_admission_rejection) {
            return Ok(Some(reason));
        }
        if anchors.gate.is_armed() && !*armed.borrow() {
            // Receivers may be gone (consensus stopped). The guard still runs.
            let _ = armed.send(true);
        }
    }
}

/// Ask the enclave to retire remote sessions once the local finalized chain
/// schedules a successor retirement above `retired_at`.
fn retire_superseded_remote_sessions_v1<P>(
    provider: &P,
    chain: RegistryChainIdentity,
    retired_at: &mut u64,
) -> eyre::Result<()>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    if provider
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
