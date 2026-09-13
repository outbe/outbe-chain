use super::bound_materialization_attempts;
use super::classify_async_outcome_projection;
use super::AsyncOutcomeProjectionV1;
use super::EmbeddedOcompExExV1;
use super::MaterializationAttemptKeyV1;
use super::OcompExExStateReaderV1;

use alloy_primitives::Address;
use alloy_primitives::B256;

use eyre::Context as _;

use outbe_node::finalized_frame::FinalizedFrame;

use outbe_ocomp::embedded_runtime::EmbeddedMaterializationOutcomeV1;
use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;

use outbe_ocomp_protocol::result::LysisResultV1;

use outbe_primitives::storage::readonly::ReadOnlyStorageProvider;

use outbe_primitives::storage::StorageHandle;
use outbe_primitives::OutbeReceipt;

use reth_primitives_traits::Block as _;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;

use tracing::info;
use tracing::warn;

pub(super) fn authenticated_finalized_proposer<'a, T>(
    mut transactions: impl Iterator<Item = &'a T>,
) -> eyre::Result<Address>
where
    T: reth_primitives_traits::SignedTransaction + 'a,
{
    let transaction = transactions
        .next()
        .ok_or_else(|| eyre::eyre!("finalized block has no proposer-signed system transaction"))?;

    transaction
        .try_recover()
        .wrap_err("recover finalized block proposer from system transaction")
}

pub(super) fn finalized_materialization_proposer<'a, T>(
    height: u64,
    transactions: impl Iterator<Item = &'a T>,
) -> eyre::Result<Option<Address>>
where
    T: reth_primitives_traits::SignedTransaction + 'a,
{
    if height == 0 {
        return Ok(None);
    }
    authenticated_finalized_proposer(transactions).map(Some)
}

pub(super) fn should_wake_nod_materializer(
    policy: EmbeddedNodePolicyV1,
    represented_validator: Option<Address>,
    block_proposer: Address,
    finalized_height: u64,
    head: Option<&outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1>,
    progress_observed: bool,
    retry_interval_blocks: u64,
) -> bool {
    if policy != EmbeddedNodePolicyV1::Validator
        || represented_validator != Some(block_proposer)
        || retry_interval_blocks == 0
    {
        return false;
    }
    let Some(head) = head.filter(|head| head.next_nod_ordinal < head.nod_count) else {
        return false;
    };
    progress_observed
        || finalized_height
            >= head
                .last_progress_height
                .saturating_add(retry_interval_blocks)
}

impl<P> EmbeddedOcompExExV1<P>
where
    P: BlockIdReader
        + BlockHashReader
        + BlockReader
        + ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    pub(super) fn async_outcome_projection(
        &mut self,
        job_id: B256,
    ) -> eyre::Result<Option<AsyncOutcomeProjectionV1>> {
        let classified = classify_async_outcome_projection(
            self.jobs.get(&job_id).map(|job| job.generation),
            self.state.generation(job_id),
        );
        match classified {
            Ok(projection) => Ok(Some(projection)),
            Err(error) => {
                self.latch_fatal(job_id, format!("{error:#}"))?;
                Ok(None)
            }
        }
    }

    pub(super) fn reconcile_materialization(&mut self, frame: &FinalizedFrame) -> eyre::Result<()> {
        let identity = frame.identity();
        let height = identity.number;
        let hash = identity.hash;
        let Some(proposer) =
            finalized_materialization_proposer(height, frame.block().body().transactions())?
        else {
            return Ok(());
        };
        let state = self
            .provider
            .state_by_block_hash(hash)
            .wrap_err("open NOD materialization finalized state")?;
        let reader = OcompExExStateReaderV1 {
            state: state.as_ref(),
        };
        let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
            reader,
            self.chain_id,
            self.genesis_hash,
        );
        let storage = StorageHandle::new(&mut readonly);
        let head = outbe_nod::NodContract::new(storage.clone())
            .ocomp_materialization_head()
            .wrap_err("read finalized NOD materialization head")?;
        let current_attempt = head.as_ref().map(|head| MaterializationAttemptKeyV1 {
            queue_sequence: head.queue_sequence,
            first_nod_ordinal: head.next_nod_ordinal,
        });
        bound_materialization_attempts(&mut self.materialization_attempt_heights, current_attempt);
        let profile = outbe_chain_constants::NodMaterializationProfileV1 {
            batch_subtree_height:
                outbe_chain_constants::get_nod_materialization_batch_subtree_height(),
            retry_interval_blocks:
                outbe_chain_constants::get_nod_materialization_retry_interval_blocks(),
            max_attempts_per_block:
                outbe_chain_constants::get_nod_materialization_max_attempts_per_block(),
        };
        let represented_validator = match self.domain.validator_sender_address() {
            Some(sender) => outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                .resolve_validator_for_role(
                    sender,
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                )
                .wrap_err("resolve finalized OCOMP materializer")?,
            None => None,
        };
        let progress_observed = head
            .as_ref()
            .is_some_and(|head| head.last_progress_height == height);
        if !should_wake_nod_materializer(
            self.policy,
            represented_validator,
            proposer,
            height,
            head.as_ref(),
            progress_observed,
            profile.retry_interval_blocks,
        ) {
            return Ok(());
        }
        let head = head.ok_or_else(|| eyre::eyre!("materialization wake lost FIFO head"))?;
        let key = MaterializationAttemptKeyV1 {
            queue_sequence: head.queue_sequence,
            first_nod_ordinal: head.next_nod_ordinal,
        };
        if self.materialization_active.is_some()
            || self.materialization_attempt_heights.get(&key) == Some(&height)
        {
            return Ok(());
        }
        // The bundle the generation was certified under is chain state, not runtime
        // state: a node that has forgotten every terminal job still materializes.
        let protocol_bundle_hash = outbe_nod::NodContract::new(storage)
            .ocomp_certified_generation(outbe_primitives::time::WorldwideDay::from(
                head.worldwide_day,
            ))?
            .ok_or_else(|| eyre::eyre!("materialization head has no certified generation"))?
            .protocol_bundle_hash;
        self.domain.spawn_validator_materialization(
            protocol_bundle_hash,
            head,
            profile.batch_subtree_height,
            self.materialization_tx.clone(),
        )?;
        self.materialization_active = Some(key);
        self.materialization_attempt_heights.insert(key, height);
        info!(
            queue_sequence = key.queue_sequence,
            first_nod_ordinal = key.first_nod_ordinal,
            "finalized proposer woke NOD materialization"
        );
        Ok(())
    }

    pub(super) fn handle_materialization(&mut self, outcome: EmbeddedMaterializationOutcomeV1) {
        let (key, success, detail) = match outcome {
            EmbeddedMaterializationOutcomeV1::Finalized {
                job_id,
                queue_sequence,
                first_nod_ordinal,
                success,
            } => {
                info!(%job_id, queue_sequence, first_nod_ordinal, success, "NOD materialization transaction finalized");
                (
                    MaterializationAttemptKeyV1 {
                        queue_sequence,
                        first_nod_ordinal,
                    },
                    Some(success),
                    None,
                )
            }
            EmbeddedMaterializationOutcomeV1::Unavailable {
                job_id,
                queue_sequence,
                first_nod_ordinal,
                detail,
            } => {
                warn!(%job_id, queue_sequence, first_nod_ordinal, %detail, "NOD materialization attempt unavailable");
                (
                    MaterializationAttemptKeyV1 {
                        queue_sequence,
                        first_nod_ordinal,
                    },
                    None,
                    Some(detail),
                )
            }
        };
        if self.materialization_active == Some(key) {
            self.materialization_active = None;
        }
        let _ = (success, detail);
    }

    pub(super) fn verify_full_node_exact(
        &self,
        job_id: B256,
        canonical: &LysisResultV1,
    ) -> eyre::Result<()> {
        if self.policy == EmbeddedNodePolicyV1::FullNode {
            self.domain
                .verify_exact_canonical_result(job_id, canonical)?;
        }
        Ok(())
    }
}
