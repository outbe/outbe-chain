//! One lifecycle lock serializes finalized admission with resident promotion.
use super::{admitted_authority_state, BootstrapAdmissionPolicy};
use crate::admission::{LocalTeeAdmissionAnchorV1, TeeLeaseGuardGateV1};
use crate::errors::TeeAuthorityError;
use alloy_primitives::B256;
use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeIdentityV1};
use outbe_tee::{in_process::InProcessTeeContext, TeeAuthorityChannel, TeeAuthorityState};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, MutexGuard,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum LiveTeeError {
    #[error("live TEE lifecycle lock poisoned")]
    LockPoisoned,
    #[error("candidate does not preserve the active network, node or resident key")]
    CandidateMismatch,
    #[error("finalized admission does not permit the candidate promotion")]
    CandidateNotAdmitted,
    #[error("local enclave identity is unavailable for authority publication")]
    LocalIdentityUnavailable,
    #[error("finalized TEE policy no longer permits authority")]
    PolicyMismatch,
    #[error("TEE authority rejected")]
    Authority(#[from] TeeAuthorityError),
    #[error("in-process dispatcher replacement failed")]
    Dispatch(#[from] outbe_tee::InProcessDispatchError),
    #[error("resident TEE operation failed")]
    Resident(#[from] outbe_tee::in_process::InProcessError),
    #[error("candidate manifest is invalid")]
    Manifest(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("durable candidate promotion failed")]
    Durable(#[source] eyre::Report),
}

#[derive(Clone)]
pub(crate) struct LiveTeeSnapshotV1 {
    pub(crate) identity: LocalTeeRuntimeIdentityV1,
    pub(crate) gate: TeeLeaseGuardGateV1,
    pub(crate) context: Arc<InProcessTeeContext>,
}

/// All readers must take this lock, not retain a startup copy of the identity.
/// Global execution dispatch is connected to this owner by the startup integrator.
pub(crate) struct LiveTeeRuntimeV1 {
    active: Mutex<LiveTeeSnapshotV1>,
    staged_candidate: Mutex<Option<Arc<InProcessTeeContext>>>,
    armed: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    bootstrap_allowed: AtomicBool,
    pub(crate) authority: TeeAuthorityChannel,
}

impl LiveTeeRuntimeV1 {
    pub(crate) fn new(
        active: LiveTeeSnapshotV1,
        authority: TeeAuthorityChannel,
    ) -> Result<Self, LiveTeeError> {
        if active.identity.reth_p2p_public != active.context.identity().reth_p2p_public
            || active.identity.expected_enclave_id != Some(active.context.manifest().enclave_id()?)
        {
            return Err(LiveTeeError::CandidateMismatch);
        }
        let bootstrap_allowed = active.gate.is_armed()
            && active.identity.validator.is_some()
            && active.context.admission_anchor()?.is_none();
        Ok(Self {
            bootstrap_allowed: AtomicBool::new(bootstrap_allowed),
            active: Mutex::new(active),
            staged_candidate: Mutex::new(None),
            armed: Mutex::new(None),
            authority,
        })
    }

    pub(crate) fn bind_armed(
        &self,
        sender: tokio::sync::watch::Sender<bool>,
    ) -> Result<(), LiveTeeError> {
        *self.armed.lock().map_err(|_| LiveTeeError::LockPoisoned)? = Some(sender);
        Ok(())
    }

    pub(crate) fn stage_candidate(
        &self,
        candidate: Arc<InProcessTeeContext>,
    ) -> Result<(), LiveTeeError> {
        let active = self.lock()?;
        if candidate.identity() != active.context.identity() || !candidate.offer_key_ready()? {
            return Err(LiveTeeError::CandidateMismatch);
        }
        *self
            .staged_candidate
            .lock()
            .map_err(|_| LiveTeeError::LockPoisoned)? = Some(candidate);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn staged_candidate(
        &self,
    ) -> Result<Option<Arc<InProcessTeeContext>>, LiveTeeError> {
        let _active = self.lock()?;
        Ok(self
            .staged_candidate
            .lock()
            .map_err(|_| LiveTeeError::LockPoisoned)?
            .clone())
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, LiveTeeSnapshotV1>, LiveTeeError> {
        self.active.lock().map_err(|_| {
            super::publish_state(&self.authority, TeeAuthorityState::SuspendedPolicyMismatch);
            LiveTeeError::LockPoisoned
        })
    }

    /// Refresh synchronously after resident activation, before starting Signer;
    /// the same operation is used by the finalized-block guard afterward.
    pub(crate) fn refresh<P>(
        &self,
        provider: &P,
        protocol: &dyn outbe_consensus::protocol_state::EngineProtocolState,
        chain: &outbe_execution::tee_remote_session::RegistryChainIdentity,
    ) -> eyre::Result<outbe_tee::TeeAuthorityState>
    where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory,
    {
        let mut active = self.lock()?;
        let resident = active
            .context
            .offer_key_ready()
            .and_then(|ready| {
                ready
                    .then(|| active.context.network_encryption_public())
                    .transpose()
            })
            .map(|key| key.map(B256::from))
            .map_err(eyre::Report::new);
        let has_resident = resident.as_ref().is_ok_and(Option::is_some);
        let mut bootstrap = self.bootstrap_allowed.load(Ordering::Relaxed);
        let identity = active.identity;
        let result = super::guard::tick(
            (provider, protocol),
            (chain, identity),
            (&mut active.gate, &mut bootstrap),
            &self.authority,
            || resident,
        );
        self.bootstrap_allowed.store(bootstrap, Ordering::Relaxed);
        if result.is_err() {
            super::publish_failure(&self.authority, has_resident);
        }
        if let Some(armed) = self
            .armed
            .lock()
            .map_err(|_| LiveTeeError::LockPoisoned)?
            .as_ref()
        {
            let open = self.authority.snapshot().is_authority_open();
            if *armed.borrow() != open {
                armed.send_replace(open);
            }
        }
        result
    }

    /// Validate everything before the durable effect; publication is last under
    /// the same lock used by admission so an old guard tick cannot reopen A.
    pub(crate) fn promote(
        &self,
        candidate: Arc<InProcessTeeContext>,
        anchor: LocalTeeAdmissionAnchorV1,
        admission: LocalTeeRuntimeAdmissionV1,
        canonical_offer: B256,
        durable: impl FnOnce() -> eyre::Result<()>,
    ) -> Result<(), LiveTeeError> {
        let mut active = self.lock()?;
        let result = (|| {
            let old = &active.context;
            let previous = active.gate.pending_anchor();
            if previous.is_some_and(|old| {
                anchor.finalized_height < old.finalized_height
                    || (anchor.finalized_height == old.finalized_height
                        && anchor.finalized_hash != old.finalized_hash)
            }) {
                return Err(LiveTeeError::CandidateMismatch);
            }
            if candidate.identity() != old.identity()
                || candidate.manifest().network_binding() != old.manifest().network_binding()
                || (
                    candidate.network_encryption_public()?,
                    candidate.key_epoch()?,
                    candidate.network_encryption_epoch()?,
                ) != (
                    old.network_encryption_public()?,
                    old.key_epoch()?,
                    old.network_encryption_epoch()?,
                )
            {
                return Err(LiveTeeError::CandidateMismatch);
            }
            let enclave_id = candidate.manifest().enclave_id()?;
            let state = admitted_authority_state(
                admission,
                enclave_id,
                Some(B256::from(candidate.network_encryption_public()?)),
                Some(canonical_offer),
                BootstrapAdmissionPolicy::RejectBootstrap,
            )?;
            if !state.has_finalized_lease() {
                return Err(LiveTeeError::CandidateNotAdmitted);
            }
            let expected = Arc::clone(&active.context);
            let mut staged = self
                .staged_candidate
                .lock()
                .map_err(|_| LiveTeeError::LockPoisoned)?;
            if staged
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &candidate))
            {
                return Err(LiveTeeError::CandidateMismatch);
            }
            outbe_tee::replace_in_process_tee_context(&expected, candidate.clone(), || {
                durable().map_err(LiveTeeError::Durable)?;
                *active = LiveTeeSnapshotV1 {
                    identity: LocalTeeRuntimeIdentityV1 {
                        expected_enclave_id: Some(enclave_id),
                        ..active.identity
                    },
                    gate: TeeLeaseGuardGateV1::Pending(anchor),
                    context: candidate,
                };
                *staged = None;
                Ok::<_, LiveTeeError>(())
            })?;
            self.bootstrap_allowed.store(false, Ordering::Relaxed);
            super::publish_state(&self.authority, state);
            Ok(())
        })();
        if result.is_err() {
            super::publish_state(&self.authority, TeeAuthorityState::SuspendedPolicyMismatch);
        }
        result
    }
}
