//! Outbe-owned pool maintenance: two-snapshot pending staleness eviction.
//!
//! Incident background (2026-08-22, `outbe-plan/txpool_stuck_tx_livelock_22-08-2026.md`):
//! pending transactions have no lifetime bound in the upstream pool. Only the
//! parked subpools age out. A transaction can keep landing in proposals that
//! fail to finalize. The reorg path then re-injects it, and every payload build
//! selects it again, indefinitely. This task bounds pending residency node-locally.
//!
//! Mechanism: on the canonical-state stream, take a snapshot of the pending-tx
//! hash set at most once per configured interval. The interval is measured in
//! canonical tip block TIMESTAMPS, not wall clock. A hash present in two
//! CONSECUTIVE snapshots was pending for at least one full interval and was not
//! mined. Evict it (with descendants). Mined or dropped transactions simply
//! vanish from the next snapshot. A re-added transaction starts a fresh cycle.
//! Effective pending TTL is therefore one to two intervals.
//!
//! Eviction is node-local pool policy and never consensus-visible. Payload
//! content is proposer-discretionary, and block validation audits only parent
//! binding, beneficiary and system-transaction layout.
//!
//! Metrics use `outbe_txpool_*`: gauges count transactions with `_txs`, and
//! counters end in `_total`. See `crates/runtime/src/metrics_recorder.rs`.

use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub mod inclusion_tracker;
pub use inclusion_tracker::{InclusionAlarmKind, InclusionStallTracker};

use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;
use futures::StreamExt as _;
use metrics::{counter, gauge};
use outbe_primitives::OutbePrimitives;
use reth_chain_state::{CanonStateNotification, CanonStateSubscriptions};
use reth_transaction_pool::{PoolSize, TransactionPool};

/// Configuration for [`maintain_outbe_pool`].
#[derive(Clone, Copy, Debug)]
pub struct OutbePoolMaintainConfig {
    /// Seconds (of canonical block time) between pending-set snapshots. The
    /// tracker evicts a pending transaction that survives two consecutive
    /// snapshots, so the effective pending TTL is one to two intervals. Must be
    /// non-zero (CLI validation enforces this).
    pub staleness_interval_secs: u64,
    /// Minimum block interval, used for inclusion-stall telemetry.
    pub min_block_time: Duration,
    /// Queued transaction lifetime, used for inclusion-stall telemetry.
    pub queued_lifetime: Duration,
}

impl Default for OutbePoolMaintainConfig {
    fn default() -> Self {
        Self {
            staleness_interval_secs: 600,
            min_block_time: Duration::from_secs(1),
            queued_lifetime: Duration::from_secs(120),
        }
    }
}

/// Two-snapshot pending staleness tracker (pure state machine).
#[derive(Debug, Default)]
pub struct PendingStalenessTracker {
    previous_pending: HashSet<B256>,
    last_snapshot_time: Option<u64>,
    interval_secs: u64,
}

impl PendingStalenessTracker {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            previous_pending: HashSet::new(),
            last_snapshot_time: None,
            interval_secs,
        }
    }

    /// Whether canonical time has reached the next snapshot boundary.
    pub fn should_check(&self, now_secs: u64) -> bool {
        self.last_snapshot_time
            .is_none_or(|last| now_secs.saturating_sub(last) >= self.interval_secs)
    }

    /// Feed the current pending set at canonical tip time `tip_timestamp`
    /// (unix seconds from the block header - deterministic input, never wall
    /// clock). Returns the hashes to evict: transactions present both in the
    /// previous snapshot and in this one.
    ///
    /// Outside a snapshot boundary, this returns empty and keeps state
    /// unchanged. Outside a boundary means less than one interval since the
    /// last snapshot, including out-of-order timestamps after a head switch.
    pub fn check_and_update(
        &mut self,
        current_pending: HashSet<B256>,
        tip_timestamp: u64,
    ) -> Vec<B256> {
        match self.last_snapshot_time {
            None => {
                // First observation: seed the snapshot, never evict.
                self.previous_pending = current_pending;
                self.last_snapshot_time = Some(tip_timestamp);
                Vec::new()
            }
            Some(last) => {
                // saturating_sub: a lower timestamp (pre-finalization head
                // rollback) must never evict early or panic.
                if tip_timestamp.saturating_sub(last) < self.interval_secs {
                    return Vec::new();
                }
                let previous = std::mem::take(&mut self.previous_pending);
                let mut stale = Vec::new();
                let mut fresh = HashSet::with_capacity(current_pending.len());
                for hash in current_pending {
                    if previous.contains(&hash) {
                        stale.push(hash);
                    } else {
                        fresh.insert(hash);
                    }
                }
                self.previous_pending = fresh;
                self.last_snapshot_time = Some(tip_timestamp);
                stale
            }
        }
    }
}

/// One maintenance tick against `pool` at canonical tip time `tip_timestamp`.
/// The tick does these steps:
/// - Snapshot the pending set.
/// - Evict what the tracker reports stale (with descendants).
/// - Log every removal.
/// - Update the metrics.
///
/// Returns the hashes actually removed from the pool.
///
/// This function is separate from the stream loop, so tests can drive it
/// directly without a provider subscription.
pub fn run_staleness_tick<Pool>(
    pool: &Pool,
    tracker: &mut PendingStalenessTracker,
    tip_timestamp: u64,
) -> Vec<B256>
where
    Pool: TransactionPool,
{
    publish_pool_size(pool.pool_size());
    if !tracker.should_check(tip_timestamp) {
        return Vec::new();
    }
    let pending: HashSet<B256> = pool
        .pending_transactions()
        .into_iter()
        .map(|tx| *tx.hash())
        .collect();
    let stale = tracker.check_and_update(pending, tip_timestamp);
    if stale.is_empty() {
        return Vec::new();
    }
    let stale_set: HashSet<B256> = stale.iter().copied().collect();
    let removed = pool.remove_transactions_and_descendants(stale);
    for tx in &removed {
        let reason = if stale_set.contains(tx.hash()) {
            "stale_pending"
        } else {
            "descendant_of_stale"
        };
        tracing::warn!(
            target: "outbe::txpool",
            tx_hash = %tx.hash(),
            sender = %tx.sender(),
            nonce = tx.nonce(),
            reason,
            staleness_interval_secs = tracker.interval_secs,
            "evicting stale pending transaction"
        );
    }
    counter!("outbe_txpool_stale_evicted_total").increment(removed.len() as u64);
    removed.iter().map(|tx| *tx.hash()).collect()
}

/// Long-running maintenance loop. It subscribes to the canonical-state stream
/// and applies pending staleness eviction. It runs alongside the upstream
/// maintenance task, which continues to own nonce/balance pruning and reorg
/// handling. It runs on every node mode. Full nodes are the RPC ingress and
/// need it most.
pub async fn maintain_outbe_pool<Provider, Pool>(
    provider: Provider,
    pool: Pool,
    config: OutbePoolMaintainConfig,
) where
    Provider: CanonStateSubscriptions<Primitives = OutbePrimitives>,
    Pool: TransactionPool,
{
    let mut staleness = PendingStalenessTracker::new(config.staleness_interval_secs);
    let mut inclusion = InclusionStallTracker::new(
        config.min_block_time,
        config.queued_lifetime,
        config.staleness_interval_secs,
    );
    let mut stream = provider.canonical_state_stream();
    let period = Duration::from_secs(5);
    let started_at = tokio::time::Instant::now();
    let unix_start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut ticker = tokio::time::interval_at(started_at + period, period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_tip_timestamp = None;
    tracing::info!(
        target: "outbe::txpool",
        staleness_interval_secs = config.staleness_interval_secs,
        "pending staleness eviction active"
    );
    loop {
        tokio::select! {
            notification = stream.next() => {
                let Some(notification) = notification else { break };
                observe_notification(&notification, &pool, &mut inclusion);
                if let Some(tip) = notification.tip_checked() {
                    let timestamp = tip.header().timestamp();
                    last_tip_timestamp = Some(timestamp);
                    let _ = run_staleness_tick(&pool, &mut staleness, timestamp);
                }
            }
            _ = ticker.tick() => {}
        }
        publish_pool_size(pool.pool_size());
        if let Some(timestamp) = last_tip_timestamp {
            // Anchor Unix time to Tokio's monotonic clock, including paused-time tests.
            let now_unix_secs = unix_start.saturating_add(started_at.elapsed().as_secs());
            gauge!("outbe_txpool_canonical_tip_age_seconds")
                .set(now_unix_secs.saturating_sub(timestamp) as f64);
        }
    }
    tracing::info!(
        target: "outbe::txpool",
        "canonical state stream closed; pending staleness eviction stopped"
    );
}

fn publish_pool_size(size: PoolSize) {
    gauge!("outbe_txpool_pending_snapshot_txs").set(size.pending as f64);
    gauge!("outbe_txpool_queued_txs").set(size.queued as f64);
}

fn observe_notification<Pool: TransactionPool>(
    notification: &CanonStateNotification<OutbePrimitives>,
    pool: &Pool,
    tracker: &mut InclusionStallTracker,
) {
    if matches!(notification, CanonStateNotification::Reorg { .. }) {
        tracker.reset();
    }
    for block in notification.committed().blocks_iter() {
        let user_txs = block
            .body()
            .transactions
            .iter()
            .filter(|tx| !outbe_primitives::system_tx::is_reserved_system_tx(*tx))
            .count();
        gauge!("outbe_txpool_block_user_txs").set(user_txs as f64);
        if let Some(kind) = tracker.observe_block(
            block.number(),
            block.timestamp(),
            user_txs,
            pool.pool_size(),
        ) {
            counter!("outbe_txpool_inclusion_alarms_total", "kind" => kind.as_label()).increment(1);
            tracing::warn!(target: "outbe::txpool", kind = kind.as_label(),
                block_number = block.number(), "pool transactions not included in canonical blocks");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex,
        },
    };

    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };

    #[test]
    fn maintenance_config_defaults() {
        let config = OutbePoolMaintainConfig::default();
        assert_eq!(config.staleness_interval_secs, 600);
        assert_eq!(config.min_block_time, Duration::from_secs(1));
        assert_eq!(config.queued_lifetime, Duration::from_secs(120));
    }

    #[derive(Default)]
    struct TestRecorder {
        values: Mutex<HashMap<String, Arc<AtomicU64>>>,
    }

    impl TestRecorder {
        fn register(&self, key: &Key) -> Arc<AtomicU64> {
            self.values
                .lock()
                .unwrap()
                .entry(key.name().to_owned())
                .or_default()
                .clone()
        }

        fn value(&self, name: &str) -> Option<u64> {
            self.values
                .lock()
                .unwrap()
                .get(name)
                .map(|value| value.load(Ordering::Relaxed))
        }
    }

    impl Recorder for TestRecorder {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            Counter::from_arc(self.register(key))
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::from_arc(self.register(key))
        }

        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }

    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn set(bytes: &[u8]) -> HashSet<B256> {
        bytes.iter().map(|b| hash(*b)).collect()
    }

    #[test]
    fn two_consecutive_snapshots_evict_only_survivors() {
        let mut tracker = PendingStalenessTracker::new(10);
        assert!(
            tracker.check_and_update(set(&[1, 2]), 100).is_empty(),
            "first snapshot only seeds"
        );
        // 1 survived, 2 was mined, 3 is fresh.
        let stale = tracker.check_and_update(set(&[1, 3]), 110);
        assert_eq!(stale, vec![hash(1)]);
        // 3 survives into the next boundary and is evicted then.
        let stale = tracker.check_and_update(set(&[3]), 120);
        assert_eq!(stale, vec![hash(3)]);
    }

    #[test]
    fn mined_transactions_vanish_without_eviction() {
        let mut tracker = PendingStalenessTracker::new(10);
        // Empty initial snapshots are no-ops.
        assert!(tracker.check_and_update(set(&[]), 90).is_empty());
        assert!(tracker.check_and_update(set(&[1]), 100).is_empty());
        // Mined before the boundary: absent from the next snapshot, and consecutive empty snapshot is a no-op.
        assert!(tracker.check_and_update(set(&[]), 110).is_empty());
        assert!(tracker.check_and_update(set(&[]), 120).is_empty());
        // Re-added later (reorg re-injection): starts a FRESH cycle...
        let stale = tracker.check_and_update(set(&[1]), 130);
        assert!(stale.is_empty(), "re-added tx is fresh again");
        // ...and is evicted one interval later.
        assert_eq!(tracker.check_and_update(set(&[1]), 140), vec![hash(1)]);
    }

    #[test]
    fn interval_gates_snapshots_and_tolerates_time_regression() {
        let mut tracker = PendingStalenessTracker::new(10);
        tracker.check_and_update(set(&[1]), 100);
        // Inside the interval: no snapshot, no eviction.
        assert!(tracker.check_and_update(set(&[1]), 105).is_empty());
        // Head rollback: tip timestamp goes BACKWARDS. Never evicts, never panics.
        assert!(tracker.check_and_update(set(&[1]), 95).is_empty());
        // Boundary reached: 1 was present in the seed snapshot and survived.
        assert_eq!(tracker.check_and_update(set(&[1]), 110), vec![hash(1)]);
    }

    /// End-to-end against a real pool. The tick removes a transaction that
    /// survives two snapshots, and also removes its higher-nonce descendant. A
    /// transaction admitted after the first snapshot survives this round.
    #[tokio::test]
    async fn tick_evicts_stale_pending_and_descendants_from_a_real_pool() {
        use reth_transaction_pool::{
            test_utils::{testing_pool, MockTransactionFactory},
            TransactionOrigin,
        };

        let pool = testing_pool();
        let mut factory = MockTransactionFactory::default();

        // One sender, two consecutive nonces: the stale head and its descendant.
        let head = factory.create_eip1559();
        let descendant = factory.validated(head.transaction.next());
        let head_hash = *head.transaction.get_hash();
        let descendant_hash = *descendant.transaction.get_hash();
        pool.add_transaction(TransactionOrigin::External, head.transaction.clone())
            .await
            .expect("add head");
        pool.add_transaction(TransactionOrigin::External, descendant.transaction.clone())
            .await
            .expect("add descendant");

        let mut tracker = PendingStalenessTracker::new(10);
        let recorder = TestRecorder::default();
        // First snapshot seeds. Nothing is evicted.
        assert!(metrics::with_local_recorder(&recorder, || {
            run_staleness_tick(&pool, &mut tracker, 100)
        })
        .is_empty());
        assert_eq!(
            recorder.value("outbe_txpool_pending_snapshot_txs"),
            Some(2.0_f64.to_bits())
        );
        assert_eq!(
            recorder.value("outbe_txpool_queued_txs"),
            Some(0.0_f64.to_bits())
        );
        assert_eq!(recorder.values.lock().unwrap().len(), 2);
        assert_eq!(recorder.value("outbe_txpool_stale_evicted_total"), None);

        // A newcomer admitted between snapshots must survive this round.
        let newcomer = factory.create_eip1559();
        let newcomer_hash = *newcomer.transaction.get_hash();
        pool.add_transaction(TransactionOrigin::External, newcomer.transaction.clone())
            .await
            .expect("add newcomer");

        let removed = metrics::with_local_recorder(&recorder, || {
            run_staleness_tick(&pool, &mut tracker, 110)
        });
        assert_eq!(
            recorder.value("outbe_txpool_pending_snapshot_txs"),
            Some(3.0_f64.to_bits())
        );
        assert_eq!(recorder.values.lock().unwrap().len(), 3);
        assert_eq!(recorder.value("outbe_txpool_stale_evicted_total"), Some(2));
        assert!(removed.contains(&head_hash), "stale head must be evicted");
        assert!(
            removed.contains(&descendant_hash),
            "descendant must be evicted with its stale ancestor"
        );
        assert!(
            !removed.contains(&newcomer_hash),
            "a transaction admitted after the seed snapshot must survive"
        );
        let remaining: HashSet<B256> = pool
            .pending_transactions()
            .into_iter()
            .map(|tx| *tx.hash())
            .collect();
        assert!(!remaining.contains(&head_hash));
        assert!(!remaining.contains(&descendant_hash));
        assert!(remaining.contains(&newcomer_hash));
    }

    /// Upstream queued-lifetime maintenance must remove expired transactions.
    /// Per-transaction eviction logs are not part of this contract.
    #[tokio::test]
    async fn queued_lifetime_eviction_removes_expired_transaction() {
        use alloy_consensus::{SignableTransaction as _, TxEip1559};
        use alloy_primitives::{Signature, TxKind, U256};
        use futures::stream;
        use reth_ethereum::{evm::EthEvmConfig, EthPrimitives, TransactionSigned};
        use reth_primitives_traits::SignedTransaction as _;
        use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
        use reth_tasks::Runtime;
        use reth_transaction_pool::{
            blobstore::InMemoryBlobStore, maintain::MaintainPoolConfig,
            validate::EthTransactionValidatorBuilder, EthPooledTransaction, Pool, PoolTransaction,
            TransactionOrigin,
        };

        const NONCE_GAP: u64 = 64;
        let signed: TransactionSigned = TxEip1559 {
            chain_id: 1,
            nonce: NONCE_GAP,
            gas_limit: 21_000,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 1,
            to: TxKind::Call(alloy_primitives::Address::repeat_byte(0x44)),
            value: U256::ZERO,
            input: Default::default(),
            access_list: Default::default(),
        }
        .into_signed(Signature::test_signature())
        .into();
        let encoded_length = alloy_eips::eip2718::Encodable2718::encode_2718_len(&signed);
        let recovered = signed
            .try_into_recovered()
            .expect("test transaction signer must recover");
        let transaction = EthPooledTransaction::new(recovered, encoded_length);
        let tx_hash = *transaction.hash();
        let sender = transaction.sender();

        let provider = MockEthProvider::<EthPrimitives>::new().with_genesis_block();
        provider.add_account(sender, ExtendedAccount::new(0, U256::MAX));
        let blob_store = InMemoryBlobStore::default();
        let inner = EthTransactionValidatorBuilder::new(provider.clone(), EthEvmConfig::mainnet())
            .set_block_gas_limit(30_000_000)
            .build(blob_store.clone());
        let validator =
            crate::OutbeTransactionValidator::new(inner, crate::policy::normal_policy());
        let pool = Pool::new(
            validator,
            crate::OutbeTransactionOrdering::new(crate::policy::normal_policy()),
            blob_store,
            Default::default(),
        );

        pool.add_transaction(TransactionOrigin::External, transaction)
            .await
            .expect("nonce-gap transaction must enter the real pool");
        let queued = pool.queued_transactions();
        assert_eq!(queued.len(), 1, "the nonce-gap transaction must be queued");
        assert_eq!(*queued[0].hash(), tx_hash);
        assert_eq!(queued[0].sender(), sender);
        assert_eq!(queued[0].nonce(), NONCE_GAP);

        let lifetime = Duration::from_millis(20);
        tokio::time::sleep(lifetime + Duration::from_millis(10)).await;

        let runtime = Runtime::test();
        let maintenance = reth_transaction_pool::maintain::maintain_transaction_pool_future::<
            EthPrimitives,
            _,
            _,
            _,
        >(
            provider,
            pool.clone(),
            stream::pending(),
            runtime,
            MaintainPoolConfig {
                max_tx_lifetime: lifetime,
                no_local_exemptions: true,
                ..Default::default()
            },
        );
        let maintenance = tokio::spawn(maintenance);

        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.get(&tx_hash).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued-lifetime maintenance must remove the exact transaction");
        maintenance.abort();
    }

    fn recorded_metrics(
        recorder: &metrics_util::debugging::DebuggingRecorder,
    ) -> HashMap<Key, metrics_util::debugging::DebugValue> {
        recorder
            .snapshotter()
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(key, _, _, value)| (key.key().clone(), value))
            .collect()
    }

    fn alarm_key(kind: InclusionAlarmKind) -> Key {
        Key::from_parts(
            "outbe_txpool_inclusion_alarms_total",
            vec![metrics::Label::new("kind", kind.as_label())],
        )
    }

    fn test_block(
        number: u64,
        timestamp: u64,
        with_user: bool,
    ) -> reth_primitives_traits::RecoveredBlock<outbe_primitives::OutbeBlock> {
        use alloy_consensus::{SignableTransaction as _, TxLegacy};
        use alloy_primitives::{Address, Signature, TxKind};
        use outbe_primitives::{OutbeBlock, OutbeBlockBody, OutbeHeader};
        let system_address = outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS;
        let destinations = if with_user {
            vec![system_address, system_address, Address::repeat_byte(0x44)]
        } else {
            vec![system_address, system_address]
        };
        let transactions = destinations
            .into_iter()
            .map(|to| {
                TxLegacy {
                    to: TxKind::Call(to),
                    ..Default::default()
                }
                .into_signed(Signature::test_signature())
                .into()
            })
            .collect::<Vec<_>>();
        let senders = vec![Address::ZERO; transactions.len()];
        reth_primitives_traits::RecoveredBlock::new_unhashed(
            OutbeBlock {
                header: OutbeHeader::new(alloy_consensus::Header {
                    number,
                    timestamp,
                    ..Default::default()
                }),
                body: OutbeBlockBody {
                    transactions,
                    ..Default::default()
                },
            },
            senders,
        )
    }

    fn notification(numbers: std::ops::Range<u64>) -> CanonStateNotification<OutbePrimitives> {
        CanonStateNotification::Commit {
            new: Arc::new(reth_provider::Chain::new(
                numbers.map(|number| test_block(number, number * 2, false)),
                Default::default(),
                Default::default(),
            )),
        }
    }

    #[tokio::test]
    async fn system_only_batch_counts_each_block_but_skips_genesis_and_bootstrap() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};
        use reth_transaction_pool::{
            test_utils::{testing_pool, MockTransactionFactory},
            TransactionOrigin,
        };
        let pool = testing_pool();
        let transaction = MockTransactionFactory::default().create_eip1559();
        pool.add_transaction(TransactionOrigin::External, transaction.transaction)
            .await
            .unwrap();
        let recorder = DebuggingRecorder::new();
        let mut tracker =
            InclusionStallTracker::new(Duration::from_secs(2), Duration::from_secs(120), 600);
        metrics::with_local_recorder(&recorder, || {
            observe_notification(&notification(0..76), &pool, &mut tracker);
        });
        let values = recorded_metrics(&recorder);
        assert_eq!(
            values.get(&Key::from_name("outbe_txpool_block_user_txs")),
            Some(&DebugValue::Gauge(0.0.into()))
        );
        assert!(!values.contains_key(&alarm_key(InclusionAlarmKind::PendingNotIncluded)));
        metrics::with_local_recorder(&recorder, || {
            observe_notification(&notification(76..77), &pool, &mut tracker);
        });
        assert_eq!(
            recorded_metrics(&recorder).get(&alarm_key(InclusionAlarmKind::PendingNotIncluded)),
            Some(&DebugValue::Counter(1))
        );
    }

    #[tokio::test]
    async fn queued_only_alarm_is_emitted_before_queued_expiry() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};
        use reth_transaction_pool::{
            test_utils::{testing_pool, MockTransactionFactory},
            TransactionOrigin,
        };
        let pool = testing_pool();
        let mut transaction = MockTransactionFactory::default()
            .create_eip1559()
            .transaction;
        transaction.set_nonce(64);
        pool.add_transaction(TransactionOrigin::External, transaction)
            .await
            .unwrap();
        assert_eq!(pool.pool_size().pending, 0);
        assert_eq!(pool.pool_size().queued, 1);
        let recorder = DebuggingRecorder::new();
        let mut tracker =
            InclusionStallTracker::new(Duration::from_secs(2), Duration::from_secs(120), 600);
        metrics::with_local_recorder(&recorder, || {
            observe_notification(&notification(2..17), &pool, &mut tracker);
            publish_pool_size(pool.pool_size());
        });
        let values = recorded_metrics(&recorder);
        assert_eq!(
            values.get(&alarm_key(InclusionAlarmKind::QueuedOnly)),
            Some(&DebugValue::Counter(1))
        );
        assert_eq!(
            values.get(&Key::from_name("outbe_txpool_queued_txs")),
            Some(&DebugValue::Gauge(1.0.into()))
        );
    }

    #[tokio::test]
    async fn reorg_revert_and_user_transactions_reset_inclusion_series() {
        use metrics_util::debugging::DebuggingRecorder;
        use reth_transaction_pool::{
            test_utils::{testing_pool, MockTransactionFactory},
            TransactionOrigin,
        };
        let pool = testing_pool();
        let transaction = MockTransactionFactory::default().create_eip1559();
        pool.add_transaction(TransactionOrigin::External, transaction.transaction)
            .await
            .unwrap();
        let recorder = DebuggingRecorder::new();
        let mut tracker =
            InclusionStallTracker::new(Duration::from_secs(2), Duration::from_secs(120), 600);
        metrics::with_local_recorder(&recorder, || {
            observe_notification(&notification(2..76), &pool, &mut tracker);
            let reorg = CanonStateNotification::Reorg {
                old: notification(75..76).committed(),
                new: notification(75..76).committed(),
            };
            observe_notification(&reorg, &pool, &mut tracker);
            observe_notification(&notification(76..149), &pool, &mut tracker);
            let user = CanonStateNotification::Commit {
                new: Arc::new(reth_provider::Chain::new(
                    [test_block(149, 298, true)],
                    Default::default(),
                    Default::default(),
                )),
            };
            observe_notification(&user, &pool, &mut tracker);
            observe_notification(&notification(150..224), &pool, &mut tracker);
            let revert = CanonStateNotification::Reorg {
                old: notification(223..224).committed(),
                new: Arc::default(),
            };
            observe_notification(&revert, &pool, &mut tracker);
            observe_notification(&notification(224..298), &pool, &mut tracker);
        });
        let values = recorded_metrics(&recorder);
        assert!(!values.contains_key(&alarm_key(InclusionAlarmKind::PendingNotIncluded)));
        assert_eq!(
            values.get(&Key::from_name("outbe_txpool_block_user_txs")),
            Some(&metrics_util::debugging::DebugValue::Gauge(0.0.into()))
        );
    }

    struct Subscription(Mutex<Option<reth_chain_state::CanonStateNotifications<OutbePrimitives>>>);

    impl CanonStateSubscriptions for Subscription {
        type Primitives = OutbePrimitives;

        fn subscribe_to_canonical_state(
            &self,
        ) -> reth_chain_state::CanonStateNotifications<OutbePrimitives> {
            self.0.lock().unwrap().take().unwrap()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_stream_updates_tip_age_every_five_seconds_and_stops_on_eof() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};
        use reth_transaction_pool::test_utils::testing_pool;
        let (sender, receiver) = tokio::sync::broadcast::channel(8);
        let pool = testing_pool();
        let recorder = DebuggingRecorder::new();
        let mut task = Box::pin(maintain_outbe_pool(
            Subscription(Mutex::new(Some(receiver))),
            pool,
            OutbePoolMaintainConfig::default(),
        ));
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        sender
            .send(CanonStateNotification::Commit {
                new: Arc::new(reth_provider::Chain::new(
                    [test_block(2, timestamp, false)],
                    Default::default(),
                    Default::default(),
                )),
            })
            .unwrap();
        assert!(
            metrics::with_local_recorder(&recorder, || futures::FutureExt::now_or_never(
                task.as_mut()
            ))
            .is_none()
        );
        let initial_age = match recorded_metrics(&recorder)
            .get(&Key::from_name("outbe_txpool_canonical_tip_age_seconds"))
        {
            Some(DebugValue::Gauge(age)) => age.into_inner(),
            value => panic!("expected initial canonical tip age gauge, got {value:?}"),
        };
        assert!(initial_age <= 1.0);
        for elapsed in [5, 10] {
            tokio::time::advance(Duration::from_secs(5)).await;
            assert!(
                metrics::with_local_recorder(&recorder, || futures::FutureExt::now_or_never(
                    task.as_mut()
                ))
                .is_none()
            );
            let values = recorded_metrics(&recorder);
            assert_eq!(
                values.get(&Key::from_name("outbe_txpool_canonical_tip_age_seconds")),
                Some(&DebugValue::Gauge(
                    (initial_age + f64::from(elapsed)).into()
                ))
            );
            assert_eq!(
                values.get(&Key::from_name("outbe_txpool_pending_snapshot_txs")),
                Some(&DebugValue::Gauge(0.0.into()))
            );
            assert_eq!(
                values.get(&Key::from_name("outbe_txpool_queued_txs")),
                Some(&DebugValue::Gauge(0.0.into()))
            );
        }
        drop(sender);
        assert!(
            metrics::with_local_recorder(&recorder, || futures::FutureExt::now_or_never(
                task.as_mut()
            ))
            .is_some()
        );
    }

    #[test]
    fn should_check_matches_snapshot_boundaries_and_time_regression() {
        let mut tracker = PendingStalenessTracker::new(10);
        assert!(tracker.should_check(100));
        tracker.check_and_update(set(&[1]), 100);
        assert!(!tracker.should_check(105));
        assert!(!tracker.should_check(95));
        assert!(tracker.should_check(110));
        assert_eq!(tracker.check_and_update(set(&[1]), 110), vec![hash(1)]);
    }
}
