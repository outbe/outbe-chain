#[allow(
    dead_code,
    reason = "Shared helpers also serve the TEE lifecycle integration target."
)]
#[path = "../common/tee_cluster/mod.rs"]
mod tee_cluster;

use serde_json::json;
use std::time::{Duration, Instant};
pub use tee_cluster::Cluster;

impl Cluster {
    pub fn start() -> Self {
        Self::start_with_genesis(
            json!({"chain_id":424242,"epoch_length_blocks":1200}),
            |genesis, addresses| {
                for address in addresses {
                    genesis["alloc"][format!("{address:x}")] = json!({"balance":"0x0"});
                }
                // Both downtime lanes must remain out of reach throughout this short test.
                genesis["alloc"]["000000000000000000000000000000000000ee01"]["storage"]
                    [format!("0x{:064x}", 1)] = json!(format!("0x{:064x}", 300));
                genesis["alloc"]["000000000000000000000000000000000000ee01"]["storage"]
                    [format!("0x{:064x}", 12)] = json!(format!("0x{:064x}", 300));
            },
        )
    }

    pub async fn wait_finalized(&mut self, height: u64) -> u64 {
        let start = Instant::now();
        loop {
            self.assert_running(3);
            if let Some(current) = self.rpcs[0].finalized().await {
                if current >= height {
                    return current;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(180),
                "finalization stalled before block {height}; logs: {}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn zero_balances(&self) {
        for (rpc, address) in self.rpcs.iter().zip(&self.addresses).take(3) {
            assert_eq!(
                rpc.account("eth_getBalance", *address).await,
                alloy_primitives::U256::ZERO,
                "honest validator liquid balance"
            );
        }
    }
}
