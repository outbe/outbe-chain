//! Finalized TEE upgrade admission and renewal scheduling behind the RPC facade.
use super::*;
use outbe_primitives::storage::readonly::ReadOnlyBlockContext;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_teeregistry::TeeRegistry;

pub(super) struct TeeRpc<'a, P> {
    provider: &'a P,
    chain_identity: Option<(u64, B256)>,
}
impl<'a, P> TeeRpc<'a, P> {
    pub(super) fn new(provider: &'a P, chain_identity: Option<(u64, B256)>) -> Self {
        Self {
            provider,
            chain_identity,
        }
    }
}
impl<P> TeeRpc<'_, P>
where
    P: StateProviderFactory
        + HeaderProvider<Header = OutbeHeader>
        + BlockIdReader
        + Send
        + Sync
        + 'static,
{
    pub(super) async fn upgrade_key(
        &self,
        context: Bytes,
        proof: outbe_tee::upgrade_transfer::UpgradeKeyProofV1,
        legacy_direct_dev_source: bool,
    ) -> RpcResult<Bytes> {
        // Bound concurrent expensive verification on this process. Never queue
        // it on the consensus enclave connection.
        static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let permit = GATE
            .try_acquire()
            .map_err(|_| internal_err(crate::errors::RpcInternalError::UpgradeKeySourceBusy))?;
        proof.validate().map_err(internal_err)?;
        let context = outbe_tee::dcap_protocol::DcapOnboardingContextV1::decode_canonical(&context)
            .map_err(|_| internal_err(crate::errors::RpcInternalError::InvalidUpgradeContext))?;
        self.check_upgrade_candidate(&context, legacy_direct_dev_source)?;
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            outbe_tee::upgrade_transfer::export_from_network_source(
                context,
                &proof,
                legacy_direct_dev_source,
            )
        })
        .await
        .map_err(|e| {
            internal_err(crate::errors::RpcInternalError::upgrade_export_task_failed(
                e,
            ))
        })?
        .map_err(internal_err)?;
        Ok(result.into())
    }
    fn check_upgrade_candidate(
        &self,
        context: &DcapOnboardingContextV1,
        legacy_direct_dev_source: bool,
    ) -> RpcResult<()> {
        let finalized = self
            .provider
            .finalized_block_num_hash()
            .map_err(internal_err)?
            .ok_or_else(|| {
                internal_err(crate::errors::RpcInternalError::FinalizedStateUnavailable)
            })?;
        let header = self
            .provider
            .sealed_header(finalized.number)
            .map_err(internal_err)?
            .ok_or_else(|| {
                internal_err(crate::errors::RpcInternalError::FinalizedHeaderUnavailable)
            })?;
        if header.hash() != finalized.hash {
            return Err(internal_err(
                crate::errors::RpcInternalError::FinalizedHeaderMismatch,
            ));
        }
        {
            let state = self
                .provider
                .state_by_block_hash(finalized.hash)
                .map_err(internal_err)?;
            let reader = RethStateReader { state: &state };
            let (chain_id, genesis_hash) = self
                .chain_identity
                .filter(|(id, hash)| *id != 0 && !hash.is_zero())
                .ok_or_else(|| {
                    internal_err(
                        crate::errors::RpcInternalError::ImmutableChainIdentityNotConfigured,
                    )
                })?;
            let mut provider = ReadOnlyStorageProvider::new_with_block_context(
                reader,
                ReadOnlyBlockContext {
                    chain_id,
                    genesis_hash,
                    block_number: finalized.number,
                    timestamp: header.timestamp(),
                },
            );
            let registry = outbe_teeregistry::TeeRegistry::new(StorageHandle::new(&mut provider));
            let check = live_upgrade_candidate(
                &registry,
                context,
                header.timestamp(),
                legacy_direct_dev_source,
            )
            .map_err(internal_err)?;
            if !check {
                return Err(internal_err(
                    crate::errors::RpcInternalError::RecipientNotLiveFinalizedUpgradeCandidate,
                ));
            }
        }
        Ok(())
    }
    pub(super) fn renewal_schedule(
        &self,
        config: Option<TeeRenewalScheduleConfigV1>,
    ) -> RpcResult<TeeRenewalScheduleV1> {
        let config = config.ok_or_else(|| {
            internal_err(crate::errors::RpcInternalError::TeeRenewalScheduleNotConfigured)
        })?;
        if config.minimum_block_time_millis == 0 {
            return Err(internal_err(
                crate::errors::RpcInternalError::TeeRenewalScheduleZeroMinBlockTime,
            ));
        }
        self.read_renewal_epoch()?.schedule(config)
    }
    fn read_renewal_epoch(&self) -> RpcResult<RenewalEpoch> {
        let finalized = self
            .provider
            .finalized_block_num_hash()
            .map_err(|error| {
                internal_err(crate::errors::RpcInternalError::finalized_block_read_failed(error))
            })?
            .ok_or_else(|| {
                internal_err(crate::errors::RpcInternalError::FinalizedBlockIsUnavailable)
            })?;
        let header = self
            .provider
            .sealed_header(finalized.number)
            .map_err(|error| {
                internal_err(
                    crate::errors::RpcInternalError::finalized_header_read_failed(
                        finalized.number,
                        error,
                    ),
                )
            })?
            .ok_or_else(|| {
                internal_err(crate::errors::RpcInternalError::FinalizedHeaderIsUnavailable)
            })?;
        if header.hash() != finalized.hash {
            return Err(internal_err(
                crate::errors::RpcInternalError::FinalizedBlockMarkerHeaderDisagree,
            ));
        }
        let state = self
            .provider
            .state_by_block_hash(finalized.hash)
            .map_err(|error| {
                internal_err(crate::errors::RpcInternalError::finalized_state_read_failed(error))
            })?;
        let reader = RethStateReader { state: &state };
        let mut provider = ReadOnlyStorageProvider::new(reader);
        let storage = StorageHandle::new(&mut provider);
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        let epoch = validators.epoch_snapshot().map_err(internal_err)?;
        Ok(RenewalEpoch {
            finalized_height: finalized.number,
            finalized_hash: finalized.hash,
            finalized_timestamp: header.timestamp(),
            epoch,
        })
    }
}
struct RenewalEpoch {
    finalized_height: u64,
    finalized_hash: B256,
    finalized_timestamp: u64,
    epoch: outbe_validatorset::EpochSnapshot,
}
impl RenewalEpoch {
    fn schedule(self, config: TeeRenewalScheduleConfigV1) -> RpcResult<TeeRenewalScheduleV1> {
        let Self {
            finalized_height,
            finalized_hash,
            finalized_timestamp,
            epoch,
        } = self;
        let epoch_number = epoch.number;
        if epoch_number > U256::from(u64::MAX) {
            return Err(internal_err(
                crate::errors::RpcInternalError::FinalizedEpochNumberExceedsU64,
            ));
        }
        let epoch_length_blocks = epoch.length_blocks;
        if epoch_length_blocks == 0 {
            return Err(internal_err(
                crate::errors::RpcInternalError::FinalizedEpochLengthZero,
            ));
        }
        let epoch_start_height = epoch.start_block;
        let planned_activation_height = epoch_start_height
            .checked_add(u64::from(epoch_length_blocks))
            .ok_or_else(|| {
                internal_err(crate::errors::RpcInternalError::PlannedActivationHeightOverflow)
            })?;
        let prepare = config
            .dkg_prepare_window_blocks
            .min(u64::from(epoch_length_blocks));
        TeeRenewalScheduleV1 {
            finalized_height,
            finalized_hash,
            finalized_timestamp,
            epoch_number: epoch_number.to::<u64>(),
            epoch_start_height,
            epoch_length_blocks,
            next_freeze_height: planned_activation_height.saturating_sub(prepare),
            planned_activation_height,
            dkg_prepare_window_blocks: prepare,
            minimum_block_time_millis: config.minimum_block_time_millis,
        }
        .validate()
        .map_err(|error| internal_err(error.to_owned()))
    }
}

fn live_upgrade_candidate(
    registry: &TeeRegistry<'_>,
    context: &DcapOnboardingContextV1,
    timestamp: u64,
    legacy_direct_dev_source: bool,
) -> outbe_primitives::error::Result<bool> {
    // This function reads the policy even when the first candidate condition rejects.
    let policy = registry.active_policy_v1()?;
    if !candidate_binding_matches(registry, context, timestamp)? {
        return Ok(false);
    }
    if !candidate_offer_matches(registry, context)? {
        return Ok(false);
    }
    let chain_matches =
        context.chain_id == policy.chain_id && context.genesis_hash == policy.genesis_hash;
    let mode_allowed = !legacy_direct_dev_source
        || policy.attestation_mode
            == outbe_primitives::tee_attestation_v1::AttestationMode::GramineDirectDev;
    Ok(chain_matches && mode_allowed)
}
fn candidate_binding_matches(
    registry: &TeeRegistry<'_>,
    context: &DcapOnboardingContextV1,
    timestamp: u64,
) -> outbe_primitives::error::Result<bool> {
    let node = context.node_id_hash;
    let live_context = registry.upgrade_candidate_context.read(&node)? == context.context_hash()
        && registry.upgrade_candidate_expiry.read(&node)? > timestamp;
    Ok(live_context
        && registry.upgrade_candidate_source.read(&node)?
            == registry.v1_node_binding_id.read(&node)?
        && !registry.v1_node_binding_id.read(&node)?.is_zero())
}
fn candidate_offer_matches(
    registry: &TeeRegistry<'_>,
    context: &DcapOnboardingContextV1,
) -> outbe_primitives::error::Result<bool> {
    Ok(
        registry.strict_upgrade_successor.read()? == context.policy_hash
            && registry.offer_public_key()?.0 == context.tribute_offer_public
            && registry.key_epoch()? == context.key_epoch
            && registry.tribute_offer_epoch()? == context.tribute_offer_epoch,
    )
}

#[cfg(test)]
mod tests;
