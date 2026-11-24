use super::*;
use outbe_primitives::{
    addresses::VALIDATOR_SET_ADDRESS, enclave_upgrade::EnclaveUpgradeV1,
    tee_attestation_v1::NodeIdV1, tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::registry_view::{self, ChainRpc, FinalizedStateRpc, RegistryRpc};
use outbe_validatorset::precompile::IValidatorSet;

// Feed existing registry decoders a frozen block, not a fresh finalized snapshot.
struct Pinned<'a> {
    port: &'a RpcLifecyclePort,
    at: LifecycleCheckpointV1,
}
impl ChainRpc for Pinned<'_> {
    type Error = RpcLifecycleError;
    async fn chain_id(&self) -> Result<u64, Self::Error> {
        Ok(self.at.chain_id)
    }
}
impl FinalizedStateRpc for Pinned<'_> {
    async fn finalized_block(&self) -> Result<Value, Self::Error> {
        Ok(
            json!({"number":format!("0x{:x}",self.at.height),"hash":self.at.hash,"stateRoot":self.at.state_root,"timestamp":format!("0x{:x}",self.at.timestamp)}),
        )
    }
    async fn call_at(&self, to: Address, data: &[u8], tag: &str) -> Result<Vec<u8>, Self::Error> {
        if tag != format!("0x{:x}", self.at.height) {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(self.port.call(self.at, to, data).await?.to_vec())
    }
}
impl RegistryRpc for Pinned<'_> {
    async fn tee_renewal_schedule_v1(
        &self,
        finalized_hash: B256,
    ) -> Result<TeeRenewalScheduleV1, Self::Error> {
        if finalized_hash != self.at.hash {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let schedule: TeeRenewalScheduleV1 = self
            .port
            .client
            .request("outbe_teeRenewalScheduleV1", rpc_params![self.at.hash])
            .await?;
        if schedule.finalized_height != self.at.height
            || schedule.finalized_hash != self.at.hash
            || schedule.finalized_timestamp != self.at.timestamp
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(schedule)
    }
}
impl RpcLifecyclePort {
    pub(super) async fn pending(
        &self,
        at: LifecycleCheckpointV1,
        selector: &NodeBindingSelectorV1,
    ) -> Result<ITeeRegistryV1::pendingEnclaveUpgradeReturn, RpcLifecycleError> {
        let node = match selector {
            NodeBindingSelectorV1::NodeHost(public) => NodeIdV1 {
                reth_p2p_public: *public,
            }
            .node_id_hash()?,
            NodeBindingSelectorV1::Validator(_) => {
                let encoded = self
                    .call(at, TEE_REGISTRY_ADDRESS, &selector.binding_call())
                    .await?;
                let binding = selector.decode_binding(&encoded)?;
                // A missing validator binding has no pending successor counter.
                if !binding.exists {
                    return Ok(ITeeRegistryV1::pendingEnclaveUpgradeReturn {
                        contextHash: B256::ZERO,
                        validUntil: 0,
                        sourceBindingId: B256::ZERO,
                        targetHash: B256::ZERO,
                        nonce: 0,
                    });
                }
                binding.nodeIdHash
            }
        };
        let encoded = self
            .call(
                at,
                TEE_REGISTRY_ADDRESS,
                &ITeeRegistryV1::pendingEnclaveUpgradeCall { nodeIdHash: node }.abi_encode(),
            )
            .await?;
        Ok(ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_decode_returns(&encoded)?)
    }
    pub(super) async fn registry(
        &self,
        at: LifecycleCheckpointV1,
        selector: &NodeBindingSelectorV1,
    ) -> Result<LifecycleRegistryStateV1, RpcLifecycleError> {
        self.check(at).await?;
        let pinned = Pinned { port: self, at };
        let view = registry_view::read_finalized_registry_view_v1(&pinned, selector)
            .await
            .map_err(|e| RpcLifecycleError::Registry(Box::new(e)))?;
        if view.policy.genesis_hash != at.genesis_hash {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let successor_policy = registry_view::read_finalized_upgrade_policy_v1(&pinned)
            .await
            .map_err(|e| RpcLifecycleError::Registry(Box::new(e)))?;
        if successor_policy
            .as_ref()
            .is_some_and(|p| p.policy.genesis_hash != at.genesis_hash)
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let encoded = self
            .call(
                at,
                TEE_REGISTRY_ADDRESS,
                &ITeeRegistryV1::enclaveUpgradeV1Call {}.abi_encode(),
            )
            .await?;
        let upgrade = ITeeRegistryV1::enclaveUpgradeV1Call::abi_decode_returns(&encoded)?;
        let active_upgrade = (!upgrade.proposalId.is_zero()).then_some(EnclaveUpgradeV1 {
            proposal_id: upgrade.proposalId,
            activation_height: upgrade.activationHeight,
            mrenclave: upgrade.mrenclave,
            successor_policy_hash: upgrade.successorPolicyHash,
            predecessor_policy_hash: upgrade.predecessorPolicyHash,
        });
        let pending = self.pending(at, selector).await?;
        let candidate = (!pending.contextHash.is_zero()).then_some(LifecycleUpgradeCandidateV1 {
            context_hash: pending.contextHash,
            valid_until: pending.validUntil,
            source_binding_id: pending.sourceBindingId,
            target_hash: pending.targetHash,
            nonce: pending.nonce,
        });
        let key_epoch = self.scalar(at, ITeeRegistryV1::keyEpochCall {}).await?;
        let network_encryption_epoch = self
            .scalar(at, ITeeRegistryV1::tributeOfferEpochCall {})
            .await?;
        self.check(at).await?;
        Ok(LifecycleRegistryStateV1 {
            checkpoint: at,
            binding: view.binding,
            policy: view.policy,
            schedule: view.schedule,
            active_upgrade,
            successor_policy,
            candidate,
            network_encryption_public_key: view.network_encryption_public_key,
            key_epoch,
            network_encryption_epoch,
        })
    }
    pub(super) async fn validator(
        &self,
        at: LifecycleCheckpointV1,
        validator: Address,
    ) -> Result<Option<ValidatorRecord>, RpcLifecycleError> {
        self.check(at).await?;
        let exists = self
            .call(
                at,
                VALIDATOR_SET_ADDRESS,
                &IValidatorSet::isValidatorCall { addr: validator }.abi_encode(),
            )
            .await?;
        if !IValidatorSet::isValidatorCall::abi_decode_returns(&exists)? {
            self.check(at).await?;
            return Ok(None);
        }
        let encoded = self
            .call(
                at,
                VALIDATOR_SET_ADDRESS,
                &IValidatorSet::validatorByAddressCall { addr: validator }.abi_encode(),
            )
            .await?;
        let v = IValidatorSet::validatorByAddressCall::abi_decode_returns(&encoded)?;
        if v.validatorAddress != validator {
            return Err(RpcLifecycleError::InvalidProof(
                ProofFailure::InvalidValidator,
            ));
        }
        let consensus_pubkey = v
            .consensusPubkey
            .as_ref()
            .try_into()
            .map_err(|_| RpcLifecycleError::InvalidProof(ProofFailure::InvalidValidator))?;
        self.check(at).await?;
        Ok(Some(ValidatorRecord {
            validator_address: v.validatorAddress,
            consensus_pubkey,
            stake: v.stake,
            status: v.status,
            slash_count: v.slashCount,
            missed_blocks: v.missedBlocks,
            missed_votes: v.missedVotes,
            blocks_proposed: v.blocksProposed,
            joined_at_height: v.joinedAtHeight,
            deactivated_at_height: v.deactivatedAtHeight,
            unbonding_end: v.unbondingEnd,
            has_bls_share: v.hasBLSShare,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::RpcModule;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn pinned_rejects_mismatched_schedule_hash_locally_before_rpc() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mut module = RpcModule::new(());
        module
            .register_method("outbe_teeRenewalScheduleV1", move |_, _, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok::<_, jsonrpsee::types::ErrorObjectOwned>(serde_json::Value::Null)
            })
            .unwrap();
        let at = LifecycleCheckpointV1 {
            height: 2,
            hash: B256::repeat_byte(0x22),
            state_root: B256::repeat_byte(0x99),
            timestamp: 20,
            base_fee_per_gas: U256::from(3),
            chain_id: 7,
            genesis_hash: B256::repeat_byte(0x11),
        };
        let port = RpcLifecyclePort::in_process(module.into(), at.chain_id, at.genesis_hash);
        let pinned = Pinned { port: &port, at };
        let err = pinned
            .tee_renewal_schedule_v1(B256::repeat_byte(0x33))
            .await
            .unwrap_err();
        assert!(matches!(err, RpcLifecycleError::CheckpointMismatch));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
