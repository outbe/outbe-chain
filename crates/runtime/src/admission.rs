use crate::errors::LaunchError;
use crate::*;
use outbe_consensus::protocol_state::EngineProtocolState;
use outbe_execution::tee_remote_session::RegistryChainIdentity;

/// Exact finalized checkpoint at which a node may arm its local lease guard
/// after replaying stale or pre-registration history. This is process-local
/// startup authority, never consensus or wire state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalTeeAdmissionAnchorV1 {
    pub(crate) finalized_height: u64,
    pub(crate) finalized_hash: alloy_primitives::B256,
}

/// Local TEE lease guard state. `is_armed()` is true only after an admission
/// anchor validated against the local canonical chain, or for a genesis founder
/// without a durable join/promotion floor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TeeLeaseGuardGateV1 {
    /// Node is waiting for an admission anchor to be established/loaded.
    AwaitingAnchor,
    /// Node has a target admission anchor (and optional durable floor below target)
    /// and must validate them at `local_finalized_height >= target.finalized_height`.
    Pending {
        target: LocalTeeAdmissionAnchorV1,
        floor: Option<LocalTeeAdmissionAnchorV1>,
    },
    /// Local admission may be read for a validated anchor or genesis founder.
    Armed,
}

impl TeeLeaseGuardGateV1 {
    pub(crate) const fn is_armed(self) -> bool {
        matches!(self, Self::Armed)
    }

    pub(crate) const fn pending_anchor(self) -> Option<LocalTeeAdmissionAnchorV1> {
        match self {
            Self::Pending { target, .. } => Some(target),
            Self::AwaitingAnchor | Self::Armed => None,
        }
    }

    pub(crate) const fn anchor_to_validate(
        self,
        local_finalized_height: u64,
    ) -> Option<LocalTeeAdmissionAnchorV1> {
        match self {
            Self::Pending { target, .. } if local_finalized_height >= target.finalized_height => {
                Some(target)
            }
            _ => None,
        }
    }

    pub(crate) const fn floor_to_validate(
        self,
        local_finalized_height: u64,
    ) -> Option<LocalTeeAdmissionAnchorV1> {
        match self {
            Self::Pending {
                target,
                floor: Some(floor),
            } if local_finalized_height >= target.finalized_height => Some(floor),
            _ => None,
        }
    }

    pub(crate) fn validate_anchor(
        anchor: LocalTeeAdmissionAnchorV1,
        observed_hash: alloy_primitives::B256,
        admission: outbe_consensus::validators::LocalTeeRuntimeAdmissionV1,
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
            outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready { .. } => Ok(()),
            outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::BootstrapPending => {
                Err(LaunchError::TeeAdmissionAnchorBootstrapPending.into())
            }
            outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Rejected(reason) => {
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
        admission: outbe_consensus::validators::LocalTeeRuntimeAdmissionV1,
    ) -> eyre::Result<()> {
        let Self::Pending { target, .. } = *self else {
            return Ok(());
        };
        Self::validate_anchor(target, observed_hash, admission)?;
        *self = Self::Armed;
        Ok(())
    }
}

fn local_tee_rejection_message(
    rejection: outbe_consensus::validators::LocalTeeRuntimeRejectionV1,
) -> String {
    use outbe_consensus::validators::LocalTeeRuntimeRejectionV1;

    match rejection {
        LocalTeeRuntimeRejectionV1::MissingBinding => {
            "finalized Registry has no binding for the local NodeHost; check autonomous enrollment with outbe-cli tee status".to_owned()
        }
        LocalTeeRuntimeRejectionV1::EnclaveIdentityMismatch => {
            "finalized Registry binding does not match the resident enclave; check the lifecycle supervisor with outbe-cli tee status"
                .to_owned()
        }
        LocalTeeRuntimeRejectionV1::ValidatorBindingMismatch => {
            "finalized validator and local NodeHost bindings disagree; refusing consensus startup"
                .to_owned()
        }
        LocalTeeRuntimeRejectionV1::ValidatorJailed => {
            "validator is jailed; complete ordinary unjail and check admission with outbe-cli tee status".to_owned()
        }
        LocalTeeRuntimeRejectionV1::RetiredEnclave => {
            "enclave upgrade deadline reached; check the lifecycle supervisor with outbe-cli tee status"
                .to_owned()
        }
        LocalTeeRuntimeRejectionV1::Expired { valid_until } => {
            format!("finalized TEE lease expired at {valid_until}; check autonomous renewal with outbe-cli tee status")
        }
    }
}

fn read_local_tee_admission_at_height<P>(
    protocol: &dyn EngineProtocolState,
    provider: &P,
    chain: RegistryChainIdentity,
    identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    block_number: u64,
) -> eyre::Result<(
    alloy_primitives::B256,
    outbe_consensus::validators::LocalTeeRuntimeAdmissionV1,
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
        protocol,
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

pub(crate) fn verify_local_anchor_hash<P>(
    provider: &P,
    anchor: LocalTeeAdmissionAnchorV1,
) -> eyre::Result<()>
where
    P: HeaderProvider<Header = OutbeHeader>,
{
    let mut observed = provider
        .sealed_header(anchor.finalized_height)
        .wrap_err("read exact header for durable TEE admission floor")?
        .ok_or(LaunchError::TeeAdmissionHeaderUnavailable)?
        .hash();
    if observed != anchor.finalized_hash {
        observed = provider
            .sealed_header(anchor.finalized_height)
            .wrap_err("read exact header for durable TEE admission floor")?
            .ok_or(LaunchError::TeeAdmissionHeaderUnavailable)?
            .hash();
    }
    if observed != anchor.finalized_hash {
        return Err(LaunchError::TeeAdmissionAnchorHashMismatch {
            height: anchor.finalized_height,
            expected: anchor.finalized_hash,
            local: observed,
        }
        .into());
    }
    Ok(())
}

fn validate_local_anchor_at_height<P>(
    protocol: &dyn EngineProtocolState,
    provider: &P,
    chain: RegistryChainIdentity,
    fullnode_identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    anchor: LocalTeeAdmissionAnchorV1,
) -> eyre::Result<outbe_consensus::validators::LocalTeeRuntimeAdmissionV1>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let (mut hash, mut admission) = read_local_tee_admission_at_height(
        protocol,
        provider,
        chain,
        fullnode_identity,
        anchor.finalized_height,
    )?;
    if hash != anchor.finalized_hash {
        (hash, admission) = read_local_tee_admission_at_height(
            protocol,
            provider,
            chain,
            fullnode_identity,
            anchor.finalized_height,
        )?;
    }
    TeeLeaseGuardGateV1::validate_anchor(anchor, hash, admission)?;
    Ok(admission)
}

pub(super) fn read_consistent_finalized_head<P>(
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

pub(super) fn read_local_tee_admission_at_block<P>(
    protocol: &dyn EngineProtocolState,
    provider: &P,
    chain: RegistryChainIdentity,
    identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    (finalized, timestamp): (alloy_eips::BlockNumHash, u64),
) -> eyre::Result<outbe_consensus::validators::LocalTeeRuntimeAdmissionV1>
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
    outbe_consensus::validators::read_local_tee_runtime_admission_from_state(
        protocol,
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

pub(crate) fn read_gated_finalized_local_tee_admission_with_resident_offer<P, F>(
    (protocol, provider): (&dyn EngineProtocolState, &P),
    chain: RegistryChainIdentity,
    identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    gate: &mut TeeLeaseGuardGateV1,
    read_resident_offer: F,
) -> eyre::Result<Option<outbe_consensus::validators::LocalTeeRuntimeAdmissionV1>>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
    F: FnOnce() -> eyre::Result<Option<alloy_primitives::B256>>,
{
    use outbe_consensus::validators::LocalTeeRuntimeAdmissionV1;

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
            .then(|| read_local_tee_admission_at_block(protocol, provider, chain, identity, head))
            .transpose();
    };
    if let Some(floor) = gate.floor_to_validate(finalized.number) {
        verify_local_anchor_hash(provider, floor)?;
    }

    let fullnode_identity = outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
        validator: None,
        ..identity
    };
    let anchor_fullnode_admission =
        validate_local_anchor_at_height(protocol, provider, chain, fullnode_identity, anchor)?;

    let fullnode_now =
        if finalized.number == anchor.finalized_height && finalized.hash == anchor.finalized_hash {
            anchor_fullnode_admission
        } else {
            read_local_tee_admission_at_block(protocol, provider, chain, fullnode_identity, head)?
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

    let validator_now =
        read_local_tee_admission_at_block(protocol, provider, chain, identity, head)?;
    try_arm_validator_tee_gate(
        (protocol, provider),
        finalized,
        (fullnode_now, validator_now),
        (gate, anchor),
        read_resident_offer,
    )
    .map(Some)
}

fn try_arm_validator_tee_gate<P, F>(
    (protocol, provider): (&dyn EngineProtocolState, &P),
    finalized: alloy_eips::BlockNumHash,
    (fullnode_now, validator_now): (
        outbe_consensus::validators::LocalTeeRuntimeAdmissionV1,
        outbe_consensus::validators::LocalTeeRuntimeAdmissionV1,
    ),
    (gate, anchor): (&mut TeeLeaseGuardGateV1, LocalTeeAdmissionAnchorV1),
    read_resident_offer: F,
) -> eyre::Result<outbe_consensus::validators::LocalTeeRuntimeAdmissionV1>
where
    P: StateProviderFactory,
    F: FnOnce() -> eyre::Result<Option<alloy_primitives::B256>>,
{
    use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    match validator_now {
        LocalTeeRuntimeAdmissionV1::BootstrapPending
        | LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::MissingBinding) => {
            Ok(fullnode_now)
        }
        LocalTeeRuntimeAdmissionV1::Rejected(_) => Ok(validator_now),
        LocalTeeRuntimeAdmissionV1::Ready { .. } => {
            let on_chain_offer =
                outbe_consensus::validators::read_network_encryption_public_at_block(
                    protocol,
                    provider,
                    finalized.hash,
                )
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

#[derive(Clone)]
pub(crate) struct TeeLeaseGuardConfigV1 {
    pub(crate) chain: RegistryChainIdentity,
    /// Signalled by the FinalizationActor and the FinalityObserver on every
    /// local finalization.
    pub(crate) finalization_notify: Arc<tokio::sync::Notify>,
    /// Publishes `true` when the gate becomes `Armed`.
    pub(crate) armed: tokio::sync::watch::Sender<bool>,
}

/// Retains canonical rejection diagnostics and records typed guard failures.
/// Authority loss does not control the resident execution lifetime.
pub(crate) fn tee_lease_exit_reason(
    verdict: Option<eyre::Result<String>>,
    shutdown: &outbe_execution::shutdown::NodeShutdown,
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
