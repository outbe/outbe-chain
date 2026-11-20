//! One bounded lifecycle observation; scheduling never depends on consensus role.
use alloy_primitives::{keccak256, B256, U256};
use outbe_tee::{
    in_process::InProcessTeeContext,
    lifecycle::{enrollment::*, renewal::*, *},
    registry_view::NodeBindingSelectorV1,
};
use std::path::Path;

const _: () =
    assert!(VALIDATOR_STATUS_JAILED == outbe_consensus::protocol_state::VALIDATOR_STATUS_JAILED);

#[derive(Clone, Debug)]
pub(crate) enum Step {
    AwaitingNetwork,
    Enrollment(EnrollmentOutcome),
    Renewal(RenewalStep),
    SuccessorRequired,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SupervisorError<E: std::error::Error + 'static, S: std::error::Error + 'static> {
    #[error(transparent)]
    Port(#[from] LifecyclePortError<E>),
    #[error(transparent)]
    Enrollment(#[from] EnrollmentError<E, S>),
    #[error(transparent)]
    Renewal(#[from] RenewalError<E, S>),
    #[error(transparent)]
    Resident(#[from] outbe_tee::in_process::InProcessError),
    #[error(transparent)]
    Codec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("lifecycle counter or lease window overflow")]
    Overflow,
    #[error("registration fee does not fit EIP-1559")]
    FeeOverflow,
}

/// A distinct lifecycle domain: the same resident enclave must
/// acquire a distinct binding when an expired lease increments its counters.
pub(crate) fn binding_id(
    live: &InProcessTeeContext,
    view: &LifecycleRegistryStateV1,
) -> Result<B256, outbe_primitives::tee_attestation_v1::CodecError> {
    let manifest = live.manifest();
    let mut bytes = Vec::from(b"outbe/tee/lifecycle-binding/v1".as_slice());
    bytes.extend_from_slice(manifest.node_id.node_id_hash()?.as_slice());
    bytes.extend_from_slice(manifest.enclave_id()?.as_slice());
    bytes.extend_from_slice(view.policy.policy_hash()?.as_slice());
    let versions = match &view.binding {
        Some(previous) => (
            previous
                .binding_version
                .checked_add(1)
                .ok_or(outbe_primitives::tee_attestation_v1::CodecError::ArithmeticOverflow)?,
            previous
                .registration_version
                .checked_add(1)
                .ok_or(outbe_primitives::tee_attestation_v1::CodecError::ArithmeticOverflow)?,
        ),
        None => (1, 0),
    };
    bytes.extend_from_slice(&versions.0.to_be_bytes());
    bytes.extend_from_slice(&versions.1.to_be_bytes());
    Ok(keccak256(bytes))
}

pub(crate) struct ResidentSession<'a> {
    pub(crate) live: &'a InProcessTeeContext,
    pub(crate) journal: &'a mut ServiceTxJournalV1,
    pub(crate) node_data_dir: &'a Path,
    pub(crate) phase: RenewalUpgradePhaseV1,
}

pub(crate) async fn step<P, S>(
    port: &P,
    signer: &S,
    session: ResidentSession<'_>,
) -> Result<Step, SupervisorError<P::Error, <S as LifecycleSignerV1>::Error>>
where
    P: LifecycleChainPort,
    S: LifecycleSignerV1 + EnrollmentSignerV1<Error = <S as LifecycleSignerV1>::Error>,
{
    let ResidentSession {
        live,
        journal,
        node_data_dir,
        phase,
    } = session;
    reconcile_service_tx_v1(port, journal, live, signer.address())
        .await
        .map_err(RenewalError::Reconcile)?;
    let Some(view) = network_view(port, signer.address()).await? else {
        return Ok(Step::AwaitingNetwork);
    };
    let at = view.checkpoint;
    let jailed = port
        .validator_record(at, signer.address())
        .await?
        .is_some_and(|v| v.status == VALIDATOR_STATUS_JAILED);
    if needs_successor(&view)? {
        return Ok(Step::SuccessorRequired);
    }
    let enclave_id = live.manifest().enclave_id()?;
    let own_live = view
        .binding
        .as_ref()
        .is_some_and(|b| b.enclave_id == enclave_id && at.timestamp < b.valid_until);
    let ready = live.offer_key_ready()?;
    // Renewal changes intent_hash without replacing resident admission. Only a
    // finalized registration for this binding needs another onboarding import.
    let current_registration = journal
        .load()
        .map_err(EnrollmentError::Journal)?
        .is_some_and(|record| {
            record.kind == ServiceTxKindV1::Register
                && view.binding.as_ref().is_some_and(|binding| {
                    let target = &record.public_intent.target;
                    (
                        target.enclave_id,
                        target.binding_id,
                        target.binding_version,
                        target.registration_version,
                    ) == (
                        binding.enclave_id,
                        binding.binding_id,
                        binding.binding_version,
                        binding.registration_version,
                    )
                })
        });
    let imported_current =
        if ready && own_live && current_registration && live.admission_anchor()?.is_some() {
            let anchor = load_live_join_admission_anchor_v1(node_data_dir, live)
                .map_err(EnrollmentError::Anchor)?;
            view.binding
                .as_ref()
                .is_some_and(|b| anchor.is_some_and(|a| a.intent_hash == b.intent_hash))
        } else {
            true
        };
    if live.offer_key_ready()? && own_live && imported_current {
        if jailed {
            return Ok(Step::Enrollment(EnrollmentOutcome::WaitForUnjail));
        }
        return Ok(Step::Renewal(
            renew_once_v1(port, signer, live, journal, phase).await?,
        ));
    }
    if ready
        && view
            .binding
            .as_ref()
            .is_some_and(|b| at.timestamp >= b.valid_until)
        && journal
            .load()
            .map_err(EnrollmentError::Journal)?
            .is_some_and(|r| {
                r.kind == ServiceTxKindV1::Renew && r.state == ServiceTxStateV1::Pending
            })
    {
        let expired = renew_once_v1(port, signer, live, journal, phase).await?;
        if expired == RenewalStep::AwaitingFinality {
            return Ok(Step::Renewal(expired));
        }
    }
    let fee = at
        .base_fee_per_gas
        .max(U256::from(1))
        .checked_mul(U256::from(2))
        .ok_or(SupervisorError::FeeOverflow)?;
    let max_fee_per_gas = u128::try_from(fee).map_err(|_| SupervisorError::FeeOverflow)?;
    let valid_until = at
        .timestamp
        .checked_add(view.policy.maximum_lease)
        .ok_or(SupervisorError::Overflow)?;
    let outcome = enroll_step_v1(
        port,
        signer,
        journal,
        EnrollmentSessionV1 {
            live,
            config: EnrollmentConfigV1 {
                sender: signer.address(),
                binding_id: binding_id(live, &view)?,
                valid_until,
                max_fee_per_gas,
                max_priority_fee_per_gas: 0,
            },
            node_data_dir,
        },
    )
    .await?;
    Ok(match outcome {
        EnrollmentOutcome::RequiresStrictUpgradeLateRecovery => Step::SuccessorRequired,
        outcome => Step::Enrollment(outcome),
    })
}

pub(crate) fn needs_successor(
    view: &LifecycleRegistryStateV1,
) -> Result<bool, outbe_primitives::tee_attestation_v1::CodecError> {
    let Some(binding) = &view.binding else {
        return Ok(false);
    };
    let Some(successor) = &view.successor_policy else {
        return Ok(false);
    };
    Ok(binding.policy_hash != successor.policy.policy_hash()?)
}

/// Permit source renewal during preparation only after the shared journal is free.
pub(crate) async fn maintain_source<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    session: ResidentSession<'_>,
) -> Result<Option<RenewalStep>, RenewalError<P::Error, S::Error>> {
    let ResidentSession {
        live,
        journal,
        phase,
        ..
    } = session;
    if phase != RenewalUpgradePhaseV1::CandidatePrepared || !live.offer_key_ready()? {
        return Ok(None);
    }
    if journal
        .load()?
        .is_some_and(|r| r.state == ServiceTxStateV1::Pending && r.kind != ServiceTxKindV1::Renew)
    {
        return Ok(None);
    }
    let at = port.finalized_checkpoint().await?;
    let view = port
        .registry_state(at, &NodeBindingSelectorV1::Validator(signer.address()))
        .await?;
    let jailed = port
        .validator_record(at, signer.address())
        .await?
        .is_some_and(|v| v.status == VALIDATOR_STATUS_JAILED);
    let current_policy = view.policy.policy_hash()?;
    if jailed
        || view
            .binding
            .as_ref()
            .is_none_or(|b| b.policy_hash != current_policy)
    {
        return Ok(None);
    }
    Ok(
        match renew_once_v1(port, signer, live, journal, phase).await? {
            step @ (RenewalStep::Submitted { .. } | RenewalStep::AwaitingFinality) => Some(step),
            _ => None,
        },
    )
}

async fn network_view<P: LifecycleChainPort>(
    port: &P,
    sender: alloy_primitives::Address,
) -> Result<Option<LifecycleRegistryStateV1>, LifecyclePortError<P::Error>> {
    let at = port.finalized_checkpoint().await?;
    if at.height == 0 {
        return Ok(None);
    }
    let view = port
        .registry_state(at, &NodeBindingSelectorV1::Validator(sender))
        .await?;
    Ok((!view.network_encryption_public_key.is_zero()).then_some(view))
}

/// A completed source renewal may replace the prepare record in the shared journal.
/// It does not invalidate the still-resident candidate attempt or authorize a replay.
pub(crate) async fn source_renewal_settled<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    source: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
) -> Result<bool, ReconcileError<P::Error>> {
    let renewal = journal.load()?.is_some_and(|record| {
        record.kind == ServiceTxKindV1::Renew && record.sender == signer.address()
    });
    if !renewal {
        return Ok(false);
    }
    Ok(matches!(
        reconcile_service_tx_v1(port, journal, source, signer.address()).await?,
        ServiceTxReconciliationV1::Completed
    ))
}
