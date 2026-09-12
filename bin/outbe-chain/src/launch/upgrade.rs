use crate::*;

pub(crate) const TEE_UPGRADE_POLL_SECS: u64 = 30;

pub(crate) const TEE_UPGRADE_WARNING_BLOCKS: u64 = 600;

pub(crate) const TEE_UPGRADE_CRITICAL_BLOCKS: u64 = 120;

pub(crate) struct UpgradePromotionWorkerConfigV1 {
    pub(crate) chain_id: u64,
    pub(crate) genesis_hash: alloy_primitives::B256,
    pub(crate) node_data_dir: PathBuf,
    pub(crate) poll_secs: u64,
    pub(crate) warning_blocks: u64,
    pub(crate) critical_blocks: u64,
    pub(crate) promoted: Arc<tokio::sync::Notify>,
}

pub(crate) async fn run_upgrade_promotion_worker_v1<P>(
    provider: P,
    config: UpgradePromotionWorkerConfigV1,
) where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory + Send + Sync + 'static,
{
    let UpgradePromotionWorkerConfigV1 {
        chain_id,
        genesis_hash,
        node_data_dir,
        poll_secs,
        warning_blocks,
        critical_blocks,
        promoted,
    } = config;
    loop {
        let snapshot = match inspect_upgrade_journal_v1(&node_data_dir) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                continue;
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "read enclave-upgrade journal failed");
                return;
            }
        };
        let context = snapshot.lifecycle.context().clone();
        match &snapshot.lifecycle {
            UpgradeJournalStateV1::Promoted { .. } => return,
            UpgradeJournalStateV1::TerminalMissedCutoff {
                finalized_height,
                activation_height,
                ..
            } => {
                tracing::error!(
                    finalized_height,
                    activation_height,
                    "enclave upgrade is terminal after missing successor activation cutoff"
                );
                return;
            }
            UpgradeJournalStateV1::Finalized {
                finalized_height,
                finalized_hash,
                ..
            } => {
                let committed = match outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir)
                {
                    Ok(committed) => committed,
                    Err(error) => {
                        tracing::error!(error = %error, "load committed manifest during upgrade recovery failed");
                        return;
                    }
                };
                let committed_hash = match committed.authorization_hash() {
                    Ok(hash) => hash,
                    Err(error) => {
                        tracing::error!(error = %error, "hash committed manifest during upgrade recovery failed");
                        return;
                    }
                };
                if committed_hash == context.candidate_manifest_hash {
                    if let Err(error) = record_upgrade_promoted_v1(&node_data_dir) {
                        tracing::error!(error = %format!("{error:#}"), "record recovered upgrade promotion failed");
                        return;
                    }
                    info!(finalized_height, %finalized_hash, "reconciled already-promoted enclave candidate");
                    return;
                }
                match outbe_node::tee_remote_session::construct_local_finalized_replacement_authorization_with_view_v1(
                    &provider,
                    chain_id,
                    genesis_hash,
                    &node_data_dir,
                    &committed.node_id,
                ) {
                    Ok(authorized) => {
                        if let Err(error) = outbe_tee::promote_replacement_candidate(
                            &node_data_dir,
                            &authorized.authorization,
                        ) {
                            tracing::error!(error = %error, "promote finalized enclave candidate failed");
                            return;
                        }
                        if let Err(error) = record_upgrade_promoted_v1(&node_data_dir) {
                            tracing::error!(error = %format!("{error:#}"), "record finalized enclave promotion failed");
                            return;
                        }
                        info!(finalized_height, %finalized_hash, "finalized enclave candidate promoted; execution restart required");
                        promoted.notify_one();
                        return;
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "reconstruct finalized candidate promotion authority failed");
                        return;
                    }
                }
            }
            _ => {}
        }

        let status =
            match outbe_node::tee_remote_session::inspect_local_finalized_successor_status_v1(
                &provider,
                chain_id,
                genesis_hash,
            ) {
                Ok(status) => status,
                Err(error) => {
                    tracing::error!(error = %error, "read node-local finalized successor status failed");
                    tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                    continue;
                }
            };
        if let Some(policy) = &status.staged_policy {
            let policy_hash = match policy.policy_hash() {
                Ok(hash) => hash,
                Err(error) => {
                    tracing::error!(error = %error, "hash node-local staged successor policy failed");
                    return;
                }
            };
            if policy_hash != context.successor_policy_hash
                || policy.activation_height != context.activation_height
            {
                tracing::error!(
                    "journaled enclave upgrade no longer matches the finalized staged successor"
                );
                return;
            }
        }
        if matches!(snapshot.lifecycle, UpgradeJournalStateV1::Submitted { .. }) {
            let committed = match outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir) {
                Ok(committed) => committed,
                Err(error) => {
                    tracing::error!(error = %error, "load active manifest for finalized upgrade check failed");
                    return;
                }
            };
            match outbe_node::tee_remote_session::construct_local_finalized_replacement_authorization_with_view_v1(
                &provider,
                chain_id,
                genesis_hash,
                &node_data_dir,
                &committed.node_id,
            ) {
                Ok(authorized) => {
                    // A matching finalized B proves that transition execution
                    // happened before the Registry cutoff, even when finality
                    // advanced across H in one step.
                    if let Err(error) = record_upgrade_finalized_v1(
                        &node_data_dir,
                        authorized.view.block_number,
                        authorized.view.block_hash,
                    ) {
                        tracing::error!(error = %format!("{error:#}"), "record finalized enclave transition failed");
                        return;
                    }
                    if let Err(error) = outbe_tee::promote_replacement_candidate(
                        &node_data_dir,
                        &authorized.authorization,
                    ) {
                        tracing::error!(error = %error, "promote finalized enclave candidate failed");
                        return;
                    }
                    if let Err(error) = record_upgrade_promoted_v1(&node_data_dir) {
                        tracing::error!(error = %format!("{error:#}"), "record enclave candidate promotion failed");
                        return;
                    }
                    info!(
                        finalized_height = authorized.view.block_number,
                        finalized_hash = %authorized.view.block_hash,
                        "finalized enclave candidate promoted; execution restart required"
                    );
                    promoted.notify_one();
                    return;
                }
                Err(outbe_node::tee_remote_session::LocalRegistryAdmissionError::ReplacementBindingMissing) => {}
                Err(error) => {
                    tracing::error!(error = %error, "finalized enclave transition authorization failed");
                    return;
                }
            }
        }
        if status.view.block_number >= context.activation_height {
            match record_upgrade_missed_cutoff_v1(
                &node_data_dir,
                status.view.block_number,
                context.activation_height,
            ) {
                Ok(_) => tracing::error!(
                    finalized_height = status.view.block_number,
                    activation_height = context.activation_height,
                    "enclave upgrade missed its finalized successor activation cutoff"
                ),
                Err(error) => {
                    tracing::error!(error = %format!("{error:#}"), "record missed enclave-upgrade cutoff failed")
                }
            }
            return;
        }
        let remaining = context
            .activation_height
            .saturating_sub(status.view.block_number);
        if remaining <= critical_blocks {
            tracing::error!(
                finalized_height = status.view.block_number,
                activation_height = context.activation_height,
                remaining_blocks = remaining,
                "enclave upgrade is inside its critical finalized activation margin"
            );
        } else if remaining <= warning_blocks {
            tracing::warn!(
                finalized_height = status.view.block_number,
                activation_height = context.activation_height,
                remaining_blocks = remaining,
                "enclave upgrade is inside its warning finalized activation margin"
            );
        }

        tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
    }
}
