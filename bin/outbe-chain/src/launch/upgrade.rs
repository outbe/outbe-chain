use crate::*;
use outbe_node::tee_remote_session::RegistryChainIdentity;

mod monitor;
mod ports;
mod promotion;
use ports::{NodeUpgradeIo, UpgradePromotionIo};

#[cfg(test)]
mod tests;

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
    let io = NodeUpgradeIo {
        provider,
        chain: RegistryChainIdentity {
            chain_id: config.chain_id,
            genesis_hash: config.genesis_hash,
        },
        node_data_dir: config.node_data_dir.clone(),
    };
    run_worker(&io, config).await;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CycleOutcome {
    Wait,
    Stop,
}

async fn run_worker(io: &impl UpgradePromotionIo, config: UpgradePromotionWorkerConfigV1) {
    while poll_once(io, &config) == CycleOutcome::Wait {
        tokio::time::sleep(std::time::Duration::from_secs(config.poll_secs)).await;
    }
}

fn poll_once(
    io: &impl UpgradePromotionIo,
    config: &UpgradePromotionWorkerConfigV1,
) -> CycleOutcome {
    match io.inspect_journal() {
        Ok(Some(snapshot)) => promotion::resume_checkpoint(io, snapshot, config),
        Ok(None) => CycleOutcome::Wait,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "read enclave-upgrade journal failed");
            CycleOutcome::Stop
        }
    }
}
