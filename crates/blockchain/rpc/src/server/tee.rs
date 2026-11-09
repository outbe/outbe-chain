//! Finalized TEE upgrade admission and renewal scheduling behind the RPC facade.
use super::*;
use outbe_primitives::storage::readonly::ReadOnlyBlockContext;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;

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
        require_enclave_session()?;
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
            let genesis_timestamp = self
                .provider
                .header_by_number(0)
                .map_err(|error| {
                    internal_err(crate::errors::RpcInternalError::BlockHeaderReadFailed {
                        target: 0,
                        reason: error.to_string(),
                    })
                })?
                .map(|genesis| genesis.inner.timestamp)
                .filter(|timestamp| *timestamp != 0)
                .ok_or_else(|| {
                    internal_err(crate::errors::RpcInternalError::FinalizedHeaderUnavailable)
                })?;
            let read_only_ctx = ReadOnlyBlockContext::try_new(
                chain_id,
                genesis_hash,
                genesis_timestamp,
                finalized.number,
                header.timestamp(),
            )
            .map_err(|_| {
                internal_err(crate::errors::RpcInternalError::FinalizedHeaderUnavailable)
            })?;
            let mut provider =
                ReadOnlyStorageProvider::new_with_block_context(reader, read_only_ctx);
            let check = crate::state_reader::active_rpc_state_reader()
                .live_upgrade_candidate(
                    StorageHandle::new(&mut provider),
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
        let epoch = crate::state_reader::active_rpc_state_reader()
            .epoch_snapshot(storage)
            .map_err(internal_err)?;
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
    epoch: outbe_primitives::validator_dto::EpochSnapshot,
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

/// No installed enclave session. Keyless RPC cannot derive keys or export an
/// upgrade artifact. Display text is the stable error name.
#[derive(Debug, thiserror::Error)]
#[error("PrivateExecutionUnavailable")]
pub(super) struct PrivateExecutionUnavailable;

pub(super) fn require_enclave_session() -> RpcResult<()> {
    if outbe_tee::try_with_enclave(|_| ()).is_none() {
        Err(super::internal_err(PrivateExecutionUnavailable))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod private_execution_tests {
    use super::*;

    #[test]
    fn missing_session_is_private_execution_unavailable() {
        if outbe_tee::try_with_enclave(|_| ()).is_some() {
            return;
        }
        let error = require_enclave_session().expect_err("no session");
        assert_eq!(error.message(), "PrivateExecutionUnavailable");
    }
}

#[cfg(test)]
mod tests;
