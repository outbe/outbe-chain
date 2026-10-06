//! Preparation under the existing upgrade journal lock.
use super::*;
use outbe_operator::tee::registry::{
    FinalizedRenewalChainViewV1, FinalizedStagedSuccessorPolicyV1,
};
use outbe_operator::tee::upgrade::PreparedTransitionEvidenceV1;
use outbe_operator::tx::RawRelayTransactionV1;

use outbe_operator::tx::UnsignedRelayTransactionV1;
pub(super) struct ProvisioningTarget {
    pub(super) binding_id: B256,
    pub(super) valid_until: u64,
    pub(super) manifest_hash: B256,
}

pub(super) struct NetworkPreparation<'a, R> {
    pub(super) rpc: &'a R,
    pub(super) candidate: &'a mut outbe_tee::ReplacementCandidateEnclaveV1,
    pub(super) signing: &'a k256::ecdsa::SigningKey,
    pub(super) relay: &'a RelaySignerV1,
    pub(super) target: ProvisioningTarget,
}

struct FreshPreparation {
    successor: FinalizedStagedSuccessorPolicyV1,
    prepared: PreparedTransitionEvidenceV1,
    node: B256,
}

struct NetworkPreparePayload {
    evidence: Vec<u8>,
    context: DcapOnboardingContextV1,
    calldata: Vec<u8>,
    gas: u64,
}

struct SigningAttempt<'a> {
    guard: &'a UpgradeJournalGuardV1,
    previous_transaction: Option<RawRelayTransactionV1>,
}

impl<R: Rpc + Sync> NetworkPreparation<'_, R> {
    pub(super) async fn load_or_prepare(
        &mut self,
        args: &UpgradeProvisionArgs,
        selector: &outbe_operator::tee::NodeBindingSelectorV1,
    ) -> Result<NetworkUpgradeSubmissionV1> {
        let rpc = self.rpc;
        let node_dir = args.candidate.node_data_dir.as_path();
        let manifest_hash = self.target.manifest_hash;
        // Also serializes with ordinary renewal preparation. No lock is held
        // while waiting for finality or downloading the committee history.
        let guard = UpgradeJournalGuardV1::acquire(node_dir)?;
        let journal = guard
            .load()?
            .ok_or_else(|| eyre::eyre!("run upgrade-prepare first"))?;
        if journal.lifecycle.context().candidate_manifest_hash != manifest_hash {
            eyre::bail!("connected candidate differs from prepared upgrade");
        }
        if !matches!(
            journal.lifecycle,
            UpgradeJournalStateV1::CandidatePrepared { .. }
                | UpgradeJournalStateV1::KeyProvisioned { .. }
        ) {
            eyre::bail!(
                "candidate has already advanced to {}; use upgrade-submit or upgrade-finalize",
                journal.lifecycle.label()
            );
        }
        let active = read_finalized_bound_renewal_view_v1(&CliFinalityRpc(rpc), selector).await?;
        let existing = guard.load_network_submission()?;
        let existing = existing.filter(|v| v.candidate_manifest_hash == manifest_hash);
        let previous_transaction = existing.as_ref().map(|v| v.transaction.clone());
        let durable = self
            .reuse_submission(existing, &active, args.new_attempt)
            .await?;
        if let Some(value) = durable {
            Ok(value)
        } else {
            let fresh = self
                .prepare_evidence(&active, journal.lifecycle.context())
                .await?;
            let payload = self.prepare_payload(&active, fresh).await?;
            self.sign_and_store(
                payload,
                SigningAttempt {
                    guard: &guard,
                    previous_transaction,
                },
            )
            .await
        }
    }

    async fn reuse_submission(
        &self,
        existing: Option<NetworkUpgradeSubmissionV1>,
        active: &FinalizedRenewalChainViewV1,
        new_attempt: bool,
    ) -> Result<Option<NetworkUpgradeSubmissionV1>> {
        let binding_id = self.target.binding_id;
        let Some(value) = existing else {
            return Ok(None);
        };
        let evidence = AttestationEvidenceV1::decode_canonical(&value.evidence)
            .map_err(|e| eyre::eyre!("saved prepare evidence: {e}"))?;
        if evidence.intent().binding_id != binding_id {
            eyre::bail!(
                "saved candidate uses another binding ID; retain the original --binding-id"
            );
        }
        if new_attempt {
            self.require_new_attempt(&evidence, active).await?;
        }
        Ok(
            if new_attempt
                || active.schedule.finalized_timestamp >= evidence.intent().requested_valid_until
            {
                None // Next nonce and fresh evidence. The expired authority cannot be reused.
            } else {
                Some(value)
            },
        )
    }

    async fn require_new_attempt(
        &self,
        evidence: &AttestationEvidenceV1,
        active: &FinalizedRenewalChainViewV1,
    ) -> Result<()> {
        let rpc = self.rpc;
        let pending = pending_at(
            rpc,
            evidence
                .intent()
                .node_id
                .node_id_hash()
                .map_err(|e| eyre::eyre!("node: {e}"))?,
            &format!("0x{:x}", active.schedule.finalized_height),
        )
        .await?;
        if !pending.contextHash.is_zero()
            && pending.validUntil > active.schedule.finalized_timestamp
        {
            eyre::bail!("cancel the live candidate before --new-attempt");
        }
        if pending.nonce < evidence.intent().transition_nonce
            && active.schedule.finalized_timestamp < evidence.intent().requested_valid_until
        {
            eyre::bail!(
                "previous prepare is not finalized or expired; replay it before starting another"
            );
        }
        Ok(())
    }

    async fn prepare_evidence(
        &mut self,
        active: &FinalizedRenewalChainViewV1,
        context: &outbe_operator::tee::UpgradeContextV1,
    ) -> Result<FreshPreparation> {
        let rpc = self.rpc;
        let candidate = &mut *self.candidate;
        let binding_id = self.target.binding_id;
        let valid_until = self.target.valid_until;
        let successor = read_finalized_upgrade_policy_v1(&CliFinalityRpc(rpc))
            .await?
            .ok_or_else(|| eyre::eyre!("no approved successor"))?;
        if successor.finalized_hash != active.schedule.finalized_hash {
            eyre::bail!("finalized head advanced during preparation; retry");
        }
        if successor
            .policy
            .policy_hash()
            .map_err(|e| eyre::eyre!("policy: {e}"))?
            != context.successor_policy_hash
        {
            eyre::bail!("approved successor changed");
        }
        let tag = format!("0x{:x}", active.schedule.finalized_height);
        let node = candidate
            .manifest()
            .node_id
            .node_id_hash()
            .map_err(|e| eyre::eyre!("node: {e}"))?;
        let pending = pending_at(rpc, node, &tag).await?;
        if !pending.contextHash.is_zero()
            && pending.validUntil > active.schedule.finalized_timestamp
        {
            eyre::bail!("a live candidate already exists; recover its saved submission or cancel it explicitly");
        }
        let mut intent = transition_intent_v1(
            candidate,
            active,
            &successor.policy,
            binding_id,
            valid_until,
        )?;
        intent.operation = AttestationOperationV1::PrepareEnclaveUpgrade;
        intent.transition_nonce = pending
            .nonce
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("prepare nonce exhausted"))?;
        let prepared = generate_transition_evidence_v1(candidate, intent, &successor.policy)?;
        validate_candidate_lease(
            &prepared,
            &successor.policy,
            LeaseWindow {
                valid_until,
                finalized_timestamp: active.schedule.finalized_timestamp,
            },
        )?;
        Ok(FreshPreparation {
            successor,
            prepared,
            node,
        })
    }

    async fn prepare_payload(
        &self,
        active: &FinalizedRenewalChainViewV1,
        fresh: FreshPreparation,
    ) -> Result<NetworkPreparePayload> {
        let rpc = self.rpc;
        let signing = self.signing;
        let binding_id = self.target.binding_id;
        let FreshPreparation {
            successor,
            prepared,
            node,
        } = fresh;
        let tag = format!("0x{:x}", active.schedule.finalized_height);
        let evidence = prepared
            .evidence
            .encode_canonical()
            .map_err(|e| eyre::eyre!("evidence: {e}"))?;
        let intent = &prepared.intent;
        let context = DcapOnboardingContextV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            intent_hash: intent
                .intent_hash()
                .map_err(|e| eyre::eyre!("intent: {e}"))?,
            node_id_hash: node,
            enclave_id: intent.enclave_id,
            binding_id,
            policy_hash: intent.policy_hash,
            recipient_x25519: intent.recipient_x25519,
            tribute_offer_public: active.tribute_offer_public.0,
            key_epoch: scalar_at(rpc, ITeeRegistryV1::keyEpochCall {}.abi_encode(), &tag).await?,
            tribute_offer_epoch: scalar_at(
                rpc,
                ITeeRegistryV1::tributeOfferEpochCall {}.abi_encode(),
                &tag,
            )
            .await?,
        };
        let signature = sign_node_hash(signing, context.intent_hash).map_err(|e| eyre::eyre!(e))?;
        let calldata = ITeeRegistryV1::prepareEnclaveUpgradeCall {
            evidence: evidence.clone().into(),
            nodeSignature: signature.to_vec().into(),
            enclaveSignature: prepared.enclave_signature.to_vec().into(),
        }
        .abi_encode();
        let gas = TeeRegistryGasScheduleV1::normative()
            .maximum_transaction_gas(
                RegistryMutatorV1::PrepareEnclaveUpgrade,
                calldata.len(),
                evidence.len(),
                successor.policy.measurement_rules.len(),
                successor.policy.attestation_mode,
            )
            .map_err(|e| eyre::eyre!("prepare gas: {e}"))?;
        Ok(NetworkPreparePayload {
            evidence,
            context,
            calldata,
            gas,
        })
    }

    async fn sign_and_store(
        &self,
        payload: NetworkPreparePayload,
        attempt: SigningAttempt<'_>,
    ) -> Result<NetworkUpgradeSubmissionV1> {
        let rpc = self.rpc;
        let relay = self.relay;
        let manifest_hash = self.target.manifest_hash;
        let SigningAttempt {
            guard,
            previous_transaction,
        } = attempt;
        let NetworkPreparePayload {
            evidence,
            context,
            calldata,
            gas,
        } = payload;
        let nonce = rpc.eth_get_transaction_count(relay.address()).await?;
        let mut price = buffered_gas_price(rpc.eth_gas_price().await?);
        if let Some(previous) = previous_transaction.filter(|t| t.account_nonce == nonce) {
            price = price.max(
                previous.gas_price.saturating_mul(U256::from(1125)) / U256::from(1000)
                    + U256::from(1),
            );
        }
        if rpc.eth_get_balance(relay.address()).await? < price.saturating_mul(U256::from(gas)) {
            eyre::bail!("insufficient balance for prepare transaction");
        }
        let transaction = relay.sign_renewal(UnsignedRelayTransactionV1 {
            chain_id: rpc.eth_chain_id().await?,
            account_nonce: nonce,
            gas_price: price,
            gas_limit: gas,
            to: TEE_REGISTRY_ADDRESS,
            calldata: &calldata,
        })?;
        let value = NetworkUpgradeSubmissionV1 {
            candidate_manifest_hash: manifest_hash,
            evidence,
            context: context.encode_canonical(),
            calldata,
            transaction,
        };
        guard.store_network_submission(&value)?;
        Ok(value)
    }
}

struct LeaseWindow {
    valid_until: u64,
    finalized_timestamp: u64,
}

fn validate_candidate_lease(
    prepared: &PreparedTransitionEvidenceV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    window: LeaseWindow,
) -> Result<()> {
    let ceiling = prepared
        .collateral_expiration
        .checked_sub(policy.collateral_margin)
        .ok_or_else(|| eyre::eyre!("collateral safety margin"))?;
    let lease = window
        .valid_until
        .checked_sub(window.finalized_timestamp)
        .ok_or_else(|| eyre::eyre!("candidate already expired"))?;
    let outside_policy = !(policy.minimum_lease..=policy.maximum_lease).contains(&lease);
    let outside_collateral = window.valid_until > ceiling
        || prepared.collateral_issue_floor > window.finalized_timestamp;
    if outside_policy || outside_collateral {
        eyre::bail!(
            "candidate lease is outside policy/collateral limits; choose a valid --valid-until"
        );
    }
    Ok(())
}
