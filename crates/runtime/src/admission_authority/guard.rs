use super::live::LiveTeeRuntimeV1;
use super::{admitted_authority_state, BootstrapAdmissionPolicy};
use crate::admission::{self, TeeLeaseGuardConfigV1, TeeLeaseGuardGateV1};
use crate::*;
use outbe_consensus::{
    protocol_state::EngineProtocolState, validators::LocalTeeRuntimeAdmissionV1,
};
use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};

/// The explicit publisher keeps unit tests and integration owners off the global port.
pub(crate) async fn run_lease_guard<P>(
    provider: P,
    protocol: Arc<dyn EngineProtocolState>,
    config: TeeLeaseGuardConfigV1,
    owner: Arc<LiveTeeRuntimeV1>,
    shutdown: tokio_util::sync::CancellationToken,
) -> eyre::Result<Option<String>>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    let _lifetime = AuthorityPublisherLifetime(owner.authority.clone());
    owner.bind_sync_refresh(provider.clone(), Arc::clone(&protocol), config.chain);
    let mut failed_state = None;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(None),
            _ = owner.authority.guard_notified() => {},
            _ = config.finalization_notify.notified() => {},
            _ = interval.tick() => {},
        }
        let open = match owner.refresh(&provider, protocol.as_ref(), &config.chain) {
            Ok(state) => {
                failed_state = None;
                state.is_authority_open()
            }
            Err(error) => {
                let next = owner.authority.snapshot();
                if failed_state != Some(next) {
                    tracing::warn!(%error, ?next, "TEE authority suspended; lease guard remains alive");
                }
                failed_state = Some(next);
                false
            }
        };
        if *config.armed.borrow() != open {
            config.armed.send_replace(open);
        }
    }
}

pub(crate) fn tick<P>(
    (provider, protocol): (&P, &dyn EngineProtocolState),
    (chain, identity): (
        &outbe_execution::tee_remote_session::RegistryChainIdentity,
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    ),
    (gate, allow_bootstrap): (&mut TeeLeaseGuardGateV1, &mut bool),
    (authority, generation): (&TeeAuthorityChannel, u64),
    resident: impl FnOnce() -> eyre::Result<Option<alloy_primitives::B256>>,
) -> eyre::Result<TeeAuthorityState>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let resident = resident()?;
    let ((admitted, canonical, bootstrap), finalized_height) = read_authority_admission(
        (provider, protocol),
        (chain, identity),
        (gate, allow_bootstrap),
        resident,
    )?;
    let state = match admitted {
        Some(admitted) => admitted_authority_state(
            admitted,
            identity
                .expected_enclave_id
                .ok_or(super::live::LiveTeeError::LocalIdentityUnavailable)?,
            resident,
            canonical,
            bootstrap,
        )?,
        None if resident.is_some() => TeeAuthorityState::SuspendedPolicyMismatch,
        None => TeeAuthorityState::UnkeyedAwaitingAdmission,
    };
    let state =
        if !gate.is_armed() && matches!(admitted, Some(LocalTeeRuntimeAdmissionV1::Ready { .. })) {
            if resident.is_some() {
                TeeAuthorityState::SuspendedPolicyMismatch
            } else {
                TeeAuthorityState::UnkeyedAwaitingAdmission
            }
        } else {
            state
        };
    // The last applied verdict is still canonical until this provider catches up.
    if finalized_height.unwrap_or_default() >= authority.observed_finalized_height() {
        if !authority.publish_if_generation(generation, state) {
            return Ok(authority.snapshot());
        }
        if let Some(height) = finalized_height {
            authority.note_applied_finalized_height(height);
        }
    }
    if state == TeeAuthorityState::SuspendedPolicyMismatch
        && matches!(
            admitted,
            Some(LocalTeeRuntimeAdmissionV1::Rejected(_))
                | Some(LocalTeeRuntimeAdmissionV1::BootstrapPending)
        )
    {
        return Err(super::live::LiveTeeError::PolicyMismatch.into());
    }
    Ok(state)
}

type AdmissionFacts = (
    Option<LocalTeeRuntimeAdmissionV1>,
    Option<alloy_primitives::B256>,
    BootstrapAdmissionPolicy,
);

fn read_authority_admission<P>(
    (provider, protocol): (&P, &dyn EngineProtocolState),
    (chain, identity): (
        &outbe_execution::tee_remote_session::RegistryChainIdentity,
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    ),
    (gate, allow_bootstrap): (&mut TeeLeaseGuardGateV1, &mut bool),
    resident: Option<alloy_primitives::B256>,
) -> eyre::Result<(AdmissionFacts, Option<u64>)>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let checkpoint =
        admission::read_consistent_finalized_head(provider, "read authority checkpoint")?;
    let finalized_height = checkpoint.map(|(head, _)| head.number);
    let facts = if let Some((head, _)) = checkpoint {
        *allow_bootstrap = false;
        let admitted = admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (protocol, provider),
            *chain,
            identity,
            gate,
            || Ok(resident),
        )?;
        let key = outbe_consensus::validators::read_network_encryption_public_at_block(
            protocol, provider, head.hash,
        )?;
        if admission::read_consistent_finalized_head(provider, "confirm authority checkpoint")?
            != checkpoint
        {
            return Ok((
                (None, None, BootstrapAdmissionPolicy::RejectBootstrap),
                None,
            ));
        }
        (
            admitted,
            (!key.is_zero()).then_some(key),
            BootstrapAdmissionPolicy::RejectBootstrap,
        )
    } else if *allow_bootstrap && gate.is_armed() && identity.validator.is_some() {
        let header = provider
            .sealed_header(0)?
            .ok_or(crate::errors::LaunchError::TeeAdmissionHeaderUnavailable)?;
        // The ordinary finalized admission reader rejects block zero as an
        // incomplete context. Founders instead prove absence of a binding at
        // the activation state, after the live ceremony installed their key.
        let state = provider.state_by_block_hash(header.hash())?;
        let mut readonly =
            outbe_primitives::storage::readonly::ReadOnlyStorageProvider::new_with_chain_identity(
                GenesisStateReader(&state),
                chain.chain_id,
                chain.genesis_hash,
            );
        let binding = protocol.node_host_binding(
            outbe_primitives::storage::StorageHandle::new(&mut readonly),
            identity.reth_p2p_public,
        )?;
        if binding.is_some() {
            return Err(super::live::LiveTeeError::PolicyMismatch.into());
        }
        let admitted = LocalTeeRuntimeAdmissionV1::BootstrapPending;
        let key = outbe_consensus::validators::read_network_encryption_public_at_block(
            protocol,
            provider,
            header.hash(),
        )?;
        (
            Some(admitted),
            (!key.is_zero()).then_some(key),
            BootstrapAdmissionPolicy::AllowFoundingBootstrap,
        )
    } else {
        (None, None, BootstrapAdmissionPolicy::RejectBootstrap)
    };

    Ok((facts, finalized_height))
}

struct GenesisStateReader<'a>(&'a reth_provider::StateProviderBox);
impl outbe_primitives::storage::readonly::StorageReader for GenesisStateReader<'_> {
    fn read_storage(
        &self,
        address: alloy_primitives::Address,
        key: alloy_primitives::B256,
    ) -> outbe_primitives::error::Result<alloy_primitives::U256> {
        self.0
            .storage(address, key)
            .map(|value| value.unwrap_or_default())
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::FatalWithCause(Box::new(error))
            })
    }
}

/// The shared owner may outlive its guard. A panic, abort or clean stop must
/// therefore close authority even when other publisher clones remain alive.
struct AuthorityPublisherLifetime(TeeAuthorityChannel);
impl Drop for AuthorityPublisherLifetime {
    fn drop(&mut self) {
        self.0.retire_publisher(false);
    }
}

#[cfg(test)]
mod sync_refresh_tests {
    use super::super::live::{LiveTeeRuntimeV1, LiveTeeSnapshotV1};
    use super::super::test_support as support;
    use crate::admission::TeeLeaseGuardGateV1;
    use outbe_consensus::protocol_state::EngineProtocolState;
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    use std::sync::{Arc, Mutex};

    struct ArmedRuntime {
        owner: Arc<LiveTeeRuntimeV1>,
        provider: support::SharedProvider,
        channel: TeeAuthorityChannel,
        context: Arc<outbe_tee::in_process::InProcessTeeContext>,
        chain: outbe_execution::tee_remote_session::RegistryChainIdentity,
        protocol: Arc<dyn EngineProtocolState>,
    }

    fn fresh_armed() -> ArmedRuntime {
        let context = support::context(9);
        let initial = support::provider(&context, 7, 1_800_000_000, 1_800_001_000, false);
        let provider = support::SharedProvider(Arc::new(Mutex::new(initial.0)));
        let channel = TeeAuthorityChannel::default();
        let owner = Arc::new(
            LiveTeeRuntimeV1::new(
                LiveTeeSnapshotV1 {
                    identity: initial.2,
                    gate: TeeLeaseGuardGateV1::Armed,
                    context: Arc::clone(&context),
                },
                channel.clone(),
            )
            .unwrap(),
        );
        let protocol: Arc<dyn EngineProtocolState> = Arc::new(crate::OutbeRuntime::new());
        let chain = initial.1;
        let before_guard = owner.refresh(&provider, protocol.as_ref(), &chain).unwrap();
        assert!(before_guard.is_authority_open());
        owner.bind_sync_refresh(provider.clone(), Arc::clone(&protocol), chain);
        owner.refresh(&provider, protocol.as_ref(), &chain).unwrap();
        assert_eq!(channel.applied_finalized_height(), 7);
        assert!(channel.snapshot().is_authority_open());
        ArmedRuntime {
            owner,
            provider,
            channel,
            context,
            chain,
            protocol,
        }
    }

    #[test]
    fn same_height_post_fcu_signal_rearms_the_lagging_guard() {
        use outbe_tee::authority::{
            install_tee_authority_watch, note_execution_finalized_authority_revision,
            note_finalized_authority_revision, tee_authority_test_guard,
        };
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        let _guard = tee_authority_test_guard();
        let armed = fresh_armed();
        install_tee_authority_watch(armed.channel.subscribe()).unwrap();
        let poll_wakeup = || {
            let mut wakeup = std::pin::pin!(armed.channel.guard_notified());
            wakeup
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
        };
        let refresh_on_signal = || {
            assert_eq!(poll_wakeup(), Poll::Ready(()));
            armed
                .owner
                .refresh(&armed.provider, armed.protocol.as_ref(), &armed.chain)
                .unwrap()
        };

        note_finalized_authority_revision(8);
        assert_eq!(
            refresh_on_signal(),
            TeeAuthorityState::SuspendedPolicyMismatch
        );
        assert_eq!(armed.channel.observed_finalized_height(), 8);
        assert_eq!(armed.channel.applied_finalized_height(), 7);
        assert!(armed.channel.snapshot().has_finalized_lease());

        *armed.provider.0.lock().expect("provider lock") =
            support::provider(&armed.context, 8, 1_800_000_000, 1_800_001_000, false).0;
        assert!(poll_wakeup().is_pending());
        note_execution_finalized_authority_revision(8);
        assert!(refresh_on_signal().has_finalized_lease());
        assert_eq!(armed.channel.observed_finalized_height(), 8);
        assert_eq!(armed.channel.applied_finalized_height(), 8);
        assert!(armed.channel.snapshot().has_finalized_lease());
    }

    #[test]
    fn sync_refresh_closes_from_fresh_armed_to_jailed() {
        let armed = fresh_armed();
        let resident = armed.context.network_encryption_public().unwrap();
        armed.channel.note_observed_finalized_height(8);
        let lagging = armed.channel.subscribe().ensure_current();
        assert!(!lagging.is_authority_open());
        assert!(lagging.has_verified_resident_key());
        assert!(armed.channel.snapshot().has_finalized_lease());
        *armed.provider.0.lock().expect("provider lock") =
            support::provider(&armed.context, 8, 1_800_000_000, 1_800_001_000, true).0;
        let jailed = armed.channel.subscribe().ensure_current();
        assert_eq!(jailed, TeeAuthorityState::SuspendedJailed);
        assert!(jailed.has_verified_resident_key());
        assert_eq!(armed.context.network_encryption_public().unwrap(), resident);
        assert!(Arc::ptr_eq(
            &armed.owner.lock().unwrap().context,
            &armed.context
        ));
    }

    #[test]
    fn sync_refresh_closes_from_fresh_armed_to_lease_expiry() {
        let armed = fresh_armed();
        let resident = armed.context.network_encryption_public().unwrap();
        *armed.provider.0.lock().expect("provider lock") =
            support::provider(&armed.context, 9, 1_800_000_000, 1_800_000_000, false).0;
        armed.channel.note_observed_finalized_height(9);
        let expired = armed.channel.subscribe().ensure_current();
        assert_eq!(expired, TeeAuthorityState::SuspendedLeaseExpired);
        assert!(expired.has_verified_resident_key());
        assert!(!expired.is_authority_open());
        assert_eq!(armed.context.network_encryption_public().unwrap(), resident);
    }

    #[test]
    fn refresh_after_guard_stop_cannot_republish_armed() {
        let armed = fresh_armed();
        let resident = armed.context.network_encryption_public().unwrap();
        let sampled = armed.channel.refresh_generation();
        armed.channel.retire_publisher(true);
        assert!(armed.channel.accept_sampled_refresh(sampled).is_none());
        let closed = armed
            .owner
            .refresh(&armed.provider, armed.protocol.as_ref(), &armed.chain)
            .unwrap();
        assert!(!closed.is_authority_open());
        assert!(closed.has_verified_resident_key());
        assert!(!armed.channel.snapshot().is_authority_open());
        assert_eq!(armed.context.network_encryption_public().unwrap(), resident);
    }
}
