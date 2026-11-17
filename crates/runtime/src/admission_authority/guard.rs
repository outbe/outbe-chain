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
        + Send
        + Sync
        + 'static,
{
    let _lifetime = AuthorityPublisherLifetime(owner.authority.clone());
    let mut failed_state = None;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(None),
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
    authority: &TeeAuthorityChannel,
    resident: impl FnOnce() -> eyre::Result<Option<alloy_primitives::B256>>,
) -> eyre::Result<TeeAuthorityState>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let resident = resident()?;
    let (admitted, canonical, bootstrap) = read_authority_admission(
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
    super::publish_state(authority, state);
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
) -> eyre::Result<AdmissionFacts>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let checkpoint =
        admission::read_consistent_finalized_head(provider, "read authority checkpoint")?;
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
            return Ok((None, None, BootstrapAdmissionPolicy::RejectBootstrap));
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

    Ok(facts)
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
        super::publish_failure(&self.0, false);
    }
}
