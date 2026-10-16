use super::super::{
    index::{remove_ready_key, ReadyIndexKey},
    state::{JobFsmCommand, JobFsmState, OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS},
};
use crate::{
    aggregate::WwdStatus,
    commit::commit_outer_transition,
    errors::MetadosisStorageCorruptionError,
    reducer::{OuterWwdTransition, OuterWwdTransitionKind},
    schema::{MetadosisContract, WorldwideDayEntryExt},
};
use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    intent::{intent_storage_key, JobIntentV1},
    receipts::RequestLimitSplitReceiptV1,
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_primitives::{error::Result, time::WorldwideDay};
struct PreparedRequest {
    wwd: WorldwideDay,
    state: JobFsmState,
    ready_key: ReadyIndexKey,
    existing_receipt: Option<RequestLimitSplitReceiptV1>,
    intent_id: B256,
}
impl MetadosisContract<'_> {
    /// Commits one canonical live job and all Metadosis-owned indexes.
    ///
    /// The caller has already applied or replay-validated the owner limit
    /// effect. This method nevertheless requires and stores the exact receipt,
    /// closing the persisted receipt/state equivalence.
    pub(crate) fn commit_ocomp_request(
        &mut self,
        outer_transition: &OuterWwdTransition,
        intent: &JobIntentV1,
        receipt: &RequestLimitSplitReceiptV1,
        schema_limits: &SchemaLimits,
    ) -> Result<()> {
        (|| {
            let PreparedRequest {
                wwd,
                state,
                ready_key,
                existing_receipt,
                intent_id,
            } = self.prepare_ocomp_request(outer_transition, intent, receipt, schema_limits)?;
            if existing_receipt.is_none() {
                self.ocomp_request_limit_receipts.get_bytes(&wwd).write(
                    &receipt.encode_canonical(schema_limits).map_err(|error| {
                        MetadosisStorageCorruptionError::EncodeRequestReceipt { error }
                    })?,
                )?;
            }
            let record = OcompJobRecordV1 {
                intent: intent.clone(),
                intent_height: intent.logical_evaluation_height,
                status: OcompJobStatus::AwaitingFinality,
                finalized: None,
                terminal: None,
            };
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            commit_outer_transition(
                self,
                wwd,
                outer_transition,
                intent.logical_evaluation_height,
            )?;
            let mut ready_index = self.read_ready_index()?;
            remove_ready_key(&mut ready_index, ready_key)?;
            self.write_ready_index(&ready_index)?;
            self.write_ocomp_state(&state)?;
            self.write_live_scheduler(&state)
        })()
    }

    fn prepare_ocomp_request(
        &self,
        outer_transition: &OuterWwdTransition,
        intent: &JobIntentV1,
        receipt: &RequestLimitSplitReceiptV1,
        schema_limits: &SchemaLimits,
    ) -> Result<PreparedRequest> {
        self.validate_request_authorization(&RequestAuthorization {
            outer_transition,
            intent,
            receipt,
        })?;
        let wwd = WorldwideDay::new(intent.wwd);
        let receipt_hash = validate_request_binding(intent, receipt, schema_limits)?;
        let mut state = self.ocomp_fsm_state(wwd, schema_limits)?;
        let ready_key = ReadyIndexKey::from_projection(state.projection())?;
        let existing_receipt = self.request_limit_receipt(wwd, schema_limits)?;
        if matches!(existing_receipt, Some(ref existing) if existing != receipt) {
            return Err(
                MetadosisStorageCorruptionError::ImmutableOcompRequestReceiptChanged.into(),
            );
        }
        let intent_id = intent
            .intent_id(schema_limits)
            .map_err(|error| MetadosisStorageCorruptionError::HashOcompIntent { error })?;
        let awaiting_finality_deadline = intent
            .logical_evaluation_height
            .checked_add(OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS)
            .ok_or(MetadosisStorageCorruptionError::OcompAwaitingFinalityDeadlineOverflow)?;
        let storage_key = intent_storage_key(intent_id).map_err(|error| {
            MetadosisStorageCorruptionError::DeriveOcompIntentStorageKey { error }
        })?;
        if !self.ocomp_job_records.get_bytes(&storage_key).is_empty()? {
            return Err(MetadosisStorageCorruptionError::OcompIntentIdAlreadyHasJobRecord.into());
        }
        state
            .apply(JobFsmCommand::Request {
                at_height: intent.logical_evaluation_height,
                deadline_height: awaiting_finality_deadline,
                intent_id,
                lysis_limit_minor: intent.frozen_metadosis_values.lysis_limit_minor,
                request_limit_receipt_hash: receipt_hash,
            })
            .map_err(MetadosisStorageCorruptionError::from)?;

        Ok(PreparedRequest {
            wwd,
            state,
            ready_key,
            existing_receipt,
            intent_id,
        })
    }
    fn validate_request_authorization(&self, request: &RequestAuthorization<'_>) -> Result<()> {
        let RequestAuthorization {
            outer_transition,
            intent,
            receipt,
        } = *request;
        super::super::authority::require_current_ocomp_attempt_snapshot(
            self.storage.clone(),
            intent,
        )?;
        if !matches!(
            outer_transition.kind(),
            OuterWwdTransitionKind::OcompRequestCommitted
        ) {
            return Err(
                MetadosisStorageCorruptionError::OcompRequestRequiresTypedOuterRequestTransition
                    .into(),
            );
        }
        intent
            .validate_semantics()
            .map_err(|error| MetadosisStorageCorruptionError::InvalidOcompIntent { error })?;
        receipt.validate_semantics().map_err(|error| {
            MetadosisStorageCorruptionError::InvalidOcompRequestReceipt { error }
        })?;
        let wwd = WorldwideDay::new(intent.wwd);
        if WwdStatus::try_from(self.worldwide_days.entry(wwd).status().read()?)? != WwdStatus::Ready
        {
            return Err(
                MetadosisStorageCorruptionError::OcompRequestRequiresReadyWorldwideDay.into(),
            );
        }

        Ok(())
    }
}

struct RequestAuthorization<'request> {
    outer_transition: &'request OuterWwdTransition,
    intent: &'request JobIntentV1,
    receipt: &'request RequestLimitSplitReceiptV1,
}

fn validate_request_binding(
    intent: &JobIntentV1,
    receipt: &RequestLimitSplitReceiptV1,
    schema_limits: &SchemaLimits,
) -> Result<B256> {
    let receipt_hash = receipt
        .receipt_hash(schema_limits)
        .map_err(|error| MetadosisStorageCorruptionError::HashOcompRequestReceipt { error })?;
    let identity_matches =
        receipt.wwd == intent.wwd && receipt.pending_nonce <= intent.pending_nonce;
    let limits_match = receipt.protocol_bundle_hash == intent.protocol_bundle_hash
        && receipt.lysis_limit_minor == intent.frozen_metadosis_values.lysis_limit_minor;
    if receipt_hash
        != intent
            .frozen_metadosis_values
            .request_limit_split_receipt_hash
        || !identity_matches
        || !limits_match
    {
        return Err(
            MetadosisStorageCorruptionError::OcompIntentRequestReceiptBindingMismatch.into(),
        );
    }
    Ok(receipt_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::StorageHandle;

    #[test]
    fn request_binding_accepts_retained_receipt_nonce_and_rejects_future_nonce() {
        let mut fixture = crate::fixture_kernel::ActivationScenario::build(14, 1000, false);
        StorageHandle::enter(&mut fixture.provider, |storage| {
            let contract = MetadosisContract::new(storage);
            let mut intent = contract
                .ocomp_job_record(fixture.intent_id, &fixture.limits)
                .unwrap()
                .unwrap()
                .intent;
            let receipt = &fixture.request_receipt;
            assert!(validate_request_binding(&intent, receipt, &fixture.limits).is_ok());
            intent.pending_nonce = receipt.pending_nonce + 1;
            assert!(validate_request_binding(&intent, receipt, &fixture.limits).is_ok());
            let mut future = receipt.clone();
            future.pending_nonce = intent.pending_nonce + 1;
            intent
                .frozen_metadosis_values
                .request_limit_split_receipt_hash = future.receipt_hash(&fixture.limits).unwrap();
            assert!(validate_request_binding(&intent, &future, &fixture.limits).is_err());
        });
    }
}
