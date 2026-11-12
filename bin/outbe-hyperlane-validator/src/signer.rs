//! Per-domain signing: verified leaves become signed checkpoints in the store.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/validator/src/submit.rs` (Apache-2.0, see
//! `LICENSE-APACHE`): the latest-first order of
//! `consensus_checkpoint_submitter` and `checkpoint_history_submitter`, the
//! reorg halt of `verify_checkpoint` and `persist_reorg_guard`, the shared halt
//! of `unless_reorg`, and the "never sign a second value" check of
//! `sign_and_submit_checkpoint`. Changes: the store replaces the checkpoint
//! syncer, a halt stops one domain and the process continues, the reorg
//! reporter is a log line in `sync.rs`.
//!
//! Order: the live loop commits all leaves of a verified batch, the cursor
//! and the signed checkpoint of the last leaf in one transaction. The backfill
//! worker then signs every earlier index that has a leaf but no checkpoint.
//!
//! Key rotation follows `sign_and_submit_checkpoint`: a stored checkpoint of
//! another key is signed again. Here only the signature may differ, the value
//! must be the same.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_primitives::{Address, Signature, B256};
use alloy_provider::Provider;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use eyre::Result;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::abi::IHyperlaneController;
use crate::checkpoint::{Checkpoint, CheckpointWithMessageId, Signed};
use crate::client::eth_call;
use crate::config::OriginConfig;
use crate::errors::ValidatorError;
use crate::liveness::HYPERLANE_CONTROLLER_ADDRESS;
use crate::merkle::IncrementalMerkle;
use crate::store::{Leaf, Meta, SignedRoot, Store};
use crate::sync::{wait_next, DomainSync, Origin, Tick, VerifiedBatch};

/// Leaves per backfill transaction.
const BACKFILL_PAGE: usize = 1000;
/// Pause before the next try after a store error.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Signing state of one origin domain. The live loop and the backfill worker
/// share it, and with it the halt.
pub struct DomainSigner {
    origin: Origin,
    store: Arc<Store>,
    key: PrivateKeySigner,
    halted: AtomicBool,
    /// Wakes the backfill worker after a commit or a halt.
    wake: Notify,
}

impl DomainSigner {
    /// A domain with a stored reorg flag starts halted.
    pub fn new(origin: Origin, store: Arc<Store>, key: PrivateKeySigner) -> Result<Self> {
        let halted = store.meta(origin.domain, Meta::ReorgFlag)?.is_some();
        Ok(Self {
            origin,
            store,
            key,
            halted: AtomicBool::new(halted),
            wake: Notify::new(),
        })
    }

    pub fn domain(&self) -> u32 {
        self.origin.domain
    }

    /// The signed announcement JSON of this domain.
    pub fn announcement(&self) -> Result<Option<Vec<u8>>> {
        self.store.meta(self.domain(), Meta::Announcement)
    }

    pub fn latest_index(&self) -> Result<Option<u32>> {
        self.store.latest_checkpoint_index(self.domain())
    }

    /// The stored signed checkpoint at `index`, in the upstream JSON type.
    pub fn signed_checkpoint(&self, index: u32) -> Result<Option<Signed<CheckpointWithMessageId>>> {
        let domain = self.domain();
        let (Some(leaf), Some(stored)) = (
            self.store.leaf(domain, index)?,
            self.store.checkpoint(domain, index)?,
        ) else {
            return Ok(None);
        };
        Ok(Some(Signed {
            value: self.value(index, stored.root, leaf.message_id),
            signature: Signature::try_from(&stored.signature[..])?,
        }))
    }

    /// True after a reorg. Only `db clear-reorg` and a restart clear it.
    pub fn halted(&self) -> bool {
        self.halted.load(Ordering::Acquire)
    }

    /// Syncs, signs and backfills the domain until a reorg halts it. A domain
    /// with a stored reorg flag does not start. Dropping this future also
    /// stops the backfill.
    pub async fn run(self: Arc<Self>, config: OriginConfig, ws_wake: Option<Arc<Notify>>) {
        let domain = self.domain();
        if self.halted() {
            error!(
                domain,
                "reorg flag is set: the domain stays halted. Fix the cause, stop the daemon, then run `db clear-reorg`"
            );
            return;
        }
        let mut backfill = JoinSet::new();
        backfill.spawn(self.clone().backfill());
        let interval = Duration::from_secs(config.interval_secs);
        let mut sync = None;
        while !self.halted() {
            if sync.is_none() {
                sync = DomainSync::new(self.origin.clone(), &config, &self.store)
                    .inspect_err(|error| warn!(domain, %error, "sync start failed"))
                    .ok();
            }
            if let Some(s) = &mut sync {
                match s.tick().await {
                    Ok(Tick::Batch(batch)) => {
                        if let Err(error) = self.commit(&batch).await {
                            // The sync state may be ahead of the store.
                            warn!(domain, %error, "commit failed; the sync starts again from the store");
                            sync = None;
                        }
                    }
                    Ok(Tick::Idle) => {}
                    Ok(Tick::Reorg {
                        index,
                        local,
                        observed,
                    }) => {
                        self.halt(format!(
                            "index {index}: endpoints agree on root {observed}, the signed root is {local}"
                        ))
                        .await
                    }
                    Err(error) => warn!(domain, %error, "sync tick failed"),
                }
            }
            if !self.halted() {
                wait_next(interval, ws_wake.as_deref()).await;
            }
        }
        let _ = backfill.join_next().await;
        error!(domain, "domain halted: signing and backfill stopped");
    }

    fn value(&self, index: u32, root: B256, message_id: B256) -> CheckpointWithMessageId {
        CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: self.origin.hook.into_word(),
                mailbox_domain: self.domain(),
                root,
                index,
            },
            message_id,
        }
    }

    fn sign(&self, index: u32, root: B256, message_id: B256) -> Result<SignedRoot> {
        let value = self.value(index, root, message_id);
        Ok(SignedRoot {
            root,
            signature: Signed::sign(value, &self.key)?.signature_bytes(),
        })
    }

    /// True when the stored signature does not recover to the current key.
    fn other_signer(&self, index: u32, stored: &SignedRoot, message_id: B256) -> bool {
        let signer = Signature::try_from(&stored.signature[..])
            .ok()
            .and_then(|signature| {
                let value = self.value(index, stored.root, message_id);
                Signed { value, signature }.recover().ok()
            });
        signer != Some(self.key.address())
    }

    /// Commits the leaves and the cursor of `batch`, and the signed
    /// checkpoint of its last leaf, in one transaction. Then wakes the
    /// backfill for the other leaves.
    pub async fn commit(&self, batch: &VerifiedBatch) -> Result<()> {
        if self.halted() {
            return Err(ValidatorError::Halted(self.domain()).into());
        }
        let leaves: Vec<(u32, Leaf)> = batch
            .leaves
            .iter()
            .map(|l| {
                let leaf = Leaf {
                    message_id: l.message_id,
                    block: l.block,
                };
                (l.index, leaf)
            })
            .collect();
        let mut signed = Vec::new();
        if let Some(last) = batch.leaves.last() {
            signed.push((
                last.index,
                self.sign(last.index, last.root, last.message_id)?,
            ));
        }
        if let Err(error) = self
            .store
            .commit_batch(self.domain(), &leaves, &signed, batch.cursor)
        {
            self.halt_on_conflict(&error).await;
            return Err(error);
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Signs every stored leaf without a checkpoint, in index order, until
    /// the domain halts. Wakes after each commit.
    async fn backfill(self: Arc<Self>) {
        let mut tree = None;
        while !self.halted() {
            let me = self.clone();
            let (next, result) = tokio::task::spawn_blocking(move || {
                let result = me.backfill_page(&mut tree);
                (tree, result)
            })
            .await
            .expect("backfill page panicked");
            tree = next;
            match result {
                Ok(true) => {}
                Ok(false) => self.wake.notified().await,
                Err(error) => {
                    warn!(domain = self.domain(), %error, "backfill failed");
                    self.halt_on_conflict(&error).await;
                    if !self.halted() {
                        tokio::time::sleep(RETRY_INTERVAL).await;
                    }
                }
            }
        }
    }

    /// Signs and stores one page of unsigned leaves after `tree`. Returns true
    /// when more leaves may follow. `tree` holds every leaf before the page.
    /// It is built once, from the stored leaves before the first unsigned
    /// index, and moves only after a successful write.
    ///
    /// Until a key completes one pass, the pass starts at index 0 and signs
    /// again each stored checkpoint of another key (`Meta::Signer`).
    fn backfill_page(&self, tree: &mut Option<IncrementalMerkle>) -> Result<bool> {
        let domain = self.domain();
        let me = self.key.address();
        // ponytail: a pass after a key change recovers every stored signature
        // (about 50 µs each, minutes for millions); a rotation is rare.
        let resign = self.store.meta(domain, Meta::Signer)?.as_deref() != Some(me.as_slice());
        let mut next = match tree {
            Some(tree) => tree.clone(),
            None => {
                let first = match self.store.first_unsigned_index(domain)? {
                    _ if resign => 0,
                    Some(first) => first,
                    None => self.store.leaf_count(domain)?,
                };
                let mut rebuilt = IncrementalMerkle::default();
                self.store.for_each_leaf(domain, |index, leaf| {
                    if index < first {
                        rebuilt.ingest(leaf.message_id);
                    }
                })?;
                info!(domain, first_unsigned = first, "backfill starts");
                *tree = Some(rebuilt.clone());
                rebuilt
            }
        };
        let page = self
            .store
            .leaves_from(domain, next.count() as u32, BACKFILL_PAGE)?;
        let mut signed = Vec::new();
        let mut resigned = Vec::new();
        for (index, leaf, stored) in &page {
            next.ingest(leaf.message_id);
            let root = next.root();
            match stored {
                // Same value and another key: sign it again (key rotation).
                Some(stored)
                    if stored.root == root
                        && resign
                        && self.other_signer(*index, stored, leaf.message_id) =>
                {
                    resigned.push((*index, self.sign(*index, root, leaf.message_id)?))
                }
                // Already signed with this value (the live loop signs the
                // last leaf of each batch): keep it.
                Some(stored) if stored.root == root => {}
                // Never sign a second value for a signed index.
                Some(stored) => {
                    return Err(ValidatorError::RootConflict {
                        domain,
                        index: *index,
                        signed: stored.root,
                        derived: root,
                    }
                    .into())
                }
                None => signed.push((*index, self.sign(*index, root, leaf.message_id)?)),
            }
        }
        // A halt drops the pending writes.
        if self.halted() {
            return Ok(false);
        }
        self.store.put_checkpoints(domain, &signed)?;
        if !resigned.is_empty() {
            warn!(
                domain,
                count = resigned.len(),
                "signed again the checkpoints of another key"
            );
            self.store.resign_checkpoints(domain, &resigned)?;
        }
        *tree = Some(next);
        let more = page.len() == BACKFILL_PAGE;
        if !more && resign {
            self.store.set_meta(domain, Meta::Signer, me.as_slice())?;
        }
        Ok(more)
    }

    async fn halt_on_conflict(&self, error: &eyre::Report) {
        if let Some(
            conflict @ (ValidatorError::CheckpointConflict { .. }
            | ValidatorError::RootConflict { .. }),
        ) = error.downcast_ref::<ValidatorError>()
        {
            self.halt(conflict.to_string()).await;
        }
    }

    /// Stops signing and backfill of this domain, then writes the reorg flag.
    /// It retries until the flag is durable, so a restart cannot sign again.
    async fn halt(&self, reason: String) {
        self.halted.store(true, Ordering::Release);
        self.wake.notify_one();
        let domain = self.domain();
        error!(
            domain,
            reason,
            "conflicting signed checkpoint, most likely a reorg. Signing of this domain is halted. Do NOT clear the reorg flag until the cause is known"
        );
        while let Err(error) = self
            .store
            .set_meta(domain, Meta::ReorgFlag, reason.as_bytes())
        {
            error!(domain, %error, "reorg flag write failed; retrying. Do NOT restart: the flag is not durable yet");
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    /// Reads `submittedIndex(validator, domain)` from the controller and
    /// compares it with the local store. See [`history_lost`].
    pub async fn check_history(&self, outbe: &impl Provider, validator: Address) -> Result<bool> {
        let call = IHyperlaneController::submittedIndexCall {
            validator,
            domain: self.domain(),
        };
        let out = eth_call(
            outbe,
            HYPERLANE_CONTROLLER_ADDRESS,
            call.abi_encode(),
            BlockId::latest(),
        )
        .await?;
        let submitted = IHyperlaneController::submittedIndexCall::abi_decode_returns(&out)?;
        let local = self.store.latest_checkpoint_index(self.domain())?;
        Ok(history_lost(self.domain(), submitted, local))
    }
}

/// True when the controller has a higher submitted index than the latest
/// local checkpoint: the local history was lost (for example a new disk).
/// Signing continues. The roots come again from the chain and the endpoint
/// vote, and RFC 6979 signatures are deterministic.
pub fn history_lost(domain: u32, submitted: u32, local: Option<u32>) -> bool {
    let lost = submitted > local.unwrap_or(0);
    if lost {
        warn!(
            domain,
            submitted,
            ?local,
            "the controller has a higher submitted index than the local store: local signing history was lost"
        );
    }
    lost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{tests::mock_rpc, SyncedLeaf};

    fn key() -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&B256::repeat_byte(7)).unwrap()
    }

    fn origin(domain: u32, hook: Address) -> Origin {
        Origin {
            domain,
            hook,
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 1,
        }
    }

    /// The Outbe testnet vector: domain, hook and 43 leaves.
    fn testnet() -> (u32, Address, Vec<B256>, serde_json::Value) {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../testdata/outbe-testnet-checkpoints.json"))
                .unwrap();
        let domain = v["domain"].as_u64().unwrap() as u32;
        let hook = serde_json::from_value(v["merkle_tree_hook"].clone()).unwrap();
        let leaves = serde_json::from_value(v["leaves"].clone()).unwrap();
        (domain, hook, leaves, v)
    }

    /// A verified batch of `ids` after `before` stored leaves.
    fn batch(domain: u32, before: &[B256], ids: &[B256], cursor: u64) -> VerifiedBatch {
        let mut tree = IncrementalMerkle::default();
        before.iter().for_each(|id| tree.ingest(*id));
        let leaves = ids
            .iter()
            .map(|id| {
                tree.ingest(*id);
                SyncedLeaf {
                    index: tree.index().unwrap(),
                    message_id: *id,
                    block: cursor,
                    root: tree.root(),
                }
            })
            .collect();
        VerifiedBatch {
            domain,
            leaves,
            cursor,
        }
    }

    fn backfill_all(signer: &DomainSigner) {
        let mut tree = None;
        while signer.backfill_page(&mut tree).unwrap() {}
    }

    /// Every stored checkpoint recovers to the key and has the root of an
    /// independent tree.
    fn assert_all_signed(store: &Store, domain: u32, hook: Address, leaves: &[B256]) {
        let mut tree = IncrementalMerkle::default();
        for (i, id) in leaves.iter().enumerate() {
            tree.ingest(*id);
            let stored = store.checkpoint(domain, i as u32).unwrap().expect("signed");
            assert_eq!(stored.root, tree.root(), "index {i}");
            let signed = Signed {
                value: CheckpointWithMessageId {
                    checkpoint: Checkpoint {
                        merkle_tree_hook_address: hook.into_word(),
                        mailbox_domain: domain,
                        root: stored.root,
                        index: i as u32,
                    },
                    message_id: *id,
                },
                signature: alloy_primitives::Signature::try_from(&stored.signature[..]).unwrap(),
            };
            assert_eq!(signed.recover().unwrap(), key().address());
        }
        assert_eq!(store.first_unsigned_index(domain).unwrap(), None);
    }

    #[tokio::test]
    async fn batch_then_backfill_signs_every_index() {
        let (domain, hook, ids, v) = testnet();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let signer = DomainSigner::new(origin(domain, hook), store.clone(), key()).unwrap();

        signer
            .commit(&batch(domain, &[], &ids[..30], 100))
            .await
            .unwrap();
        assert_eq!(store.leaf_count(domain).unwrap(), 30);
        assert_eq!(store.cursor(domain).unwrap(), Some(100));
        assert_eq!(store.latest_checkpoint_index(domain).unwrap(), Some(29));
        assert_eq!(store.first_unsigned_index(domain).unwrap(), Some(0));

        backfill_all(&signer);
        signer
            .commit(&batch(domain, &ids[..30], &ids[30..], 200))
            .await
            .unwrap();
        backfill_all(&signer);
        assert_all_signed(&store, domain, hook, &ids);

        // The roots at the checkpoints of the testnet validators are equal.
        for c in v["checkpoints"].as_array().unwrap() {
            let index = c["index"].as_u64().unwrap() as u32;
            let root: B256 = serde_json::from_value(c["root"].clone()).unwrap();
            assert_eq!(store.checkpoint(domain, index).unwrap().unwrap().root, root);
        }
    }

    #[test]
    fn backfill_resumes_and_skips_same_values() {
        let (domain, hook, ids, _) = testnet();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let signer = DomainSigner::new(origin(domain, hook), store.clone(), key()).unwrap();
        let leaves: Vec<(u32, Leaf)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let leaf = Leaf {
                    message_id: *id,
                    block: 1,
                };
                (i as u32, leaf)
            })
            .collect();
        store.commit_batch(domain, &leaves, &[], 1).unwrap();

        // A partial run before a restart: 0..20 and 25 are signed.
        let mut tree = IncrementalMerkle::default();
        let mut partial = Vec::new();
        for (i, id) in ids.iter().enumerate().take(26) {
            tree.ingest(*id);
            if i < 20 || i == 25 {
                partial.push((i as u32, signer.sign(i as u32, tree.root(), *id).unwrap()));
            }
        }
        store.put_checkpoints(domain, &partial).unwrap();
        let kept = store.checkpoint(domain, 25).unwrap();

        // A new signer resumes at index 20 and keeps index 25.
        let signer = DomainSigner::new(origin(domain, hook), store.clone(), key()).unwrap();
        backfill_all(&signer);
        assert_all_signed(&store, domain, hook, &ids);
        assert_eq!(store.checkpoint(domain, 25).unwrap(), kept);

        // A replay over signed indexes with the same values writes nothing.
        let mut tree = Some(IncrementalMerkle::default());
        assert!(!signer.backfill_page(&mut tree).unwrap());
        assert_eq!(tree.unwrap().count(), ids.len());
        assert!(!signer.halted());
    }

    #[tokio::test]
    async fn a_new_key_signs_the_history_again() {
        let (domain, hook, ids, _) = testnet();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let old_key = PrivateKeySigner::from_bytes(&B256::repeat_byte(3)).unwrap();
        let old = DomainSigner::new(origin(domain, hook), store.clone(), old_key).unwrap();
        old.commit(&batch(domain, &[], &ids, 100)).await.unwrap();
        backfill_all(&old);
        assert_eq!(store.first_unsigned_index(domain).unwrap(), None);

        // Same values, new key: every index is signed again with the new key.
        let new = DomainSigner::new(origin(domain, hook), store.clone(), key()).unwrap();
        backfill_all(&new);
        assert_all_signed(&store, domain, hook, &ids);
        assert_eq!(
            store.meta(domain, Meta::Signer).unwrap().as_deref(),
            Some(key().address().as_slice())
        );
        let latest = new.signed_checkpoint(42).unwrap().unwrap();
        assert_eq!(latest.recover().unwrap(), key().address());
        assert_eq!(new.signed_checkpoint(43).unwrap(), None);
    }

    #[tokio::test]
    async fn different_root_at_a_signed_index_halts_the_domain() {
        let (domain, hook, ids, _) = testnet();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let leaves: Vec<(u32, Leaf)> = (0..5)
            .map(|i| {
                let leaf = Leaf {
                    message_id: ids[i],
                    block: 1,
                };
                (i as u32, leaf)
            })
            .collect();
        let wrong = SignedRoot {
            root: B256::repeat_byte(0xee),
            signature: [1; 65],
        };
        store
            .commit_batch(domain, &leaves, &[(2, wrong)], 1)
            .unwrap();
        let signer =
            Arc::new(DomainSigner::new(origin(domain, hook), store.clone(), key()).unwrap());

        // The backfill stops by itself and writes nothing of the page.
        tokio::time::timeout(Duration::from_secs(10), signer.clone().backfill())
            .await
            .expect("backfill stops");
        assert!(signer.halted());
        assert!(store.meta(domain, Meta::ReorgFlag).unwrap().is_some());
        assert_eq!(store.checkpoint(domain, 0).unwrap(), None);
        assert_eq!(store.checkpoint(domain, 2).unwrap(), Some(wrong));

        // The live loop refuses a new batch.
        let err = signer
            .commit(&batch(domain, &ids[..5], &ids[5..6], 2))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("halted"), "{err}");
        assert_eq!(store.leaf_count(domain).unwrap(), 5);

        // A restart keeps the domain halted.
        let signer = DomainSigner::new(origin(domain, hook), store.clone(), key()).unwrap();
        assert!(signer.halted());
    }

    #[tokio::test]
    async fn reorg_tick_halts_one_domain_only() {
        let ids = [
            B256::repeat_byte(1),
            B256::repeat_byte(2),
            B256::repeat_byte(3),
        ];
        let leaves = vec![(ids[0], 10), (ids[1], 60), (ids[2], 90)];
        let fake_root = Arc::new(std::sync::Mutex::new(None));
        let a = mock_rpc(leaves.clone(), fake_root.clone()).await;
        let c = mock_rpc(leaves, fake_root.clone()).await;
        let config: OriginConfig = toml::from_str(&format!(
            "domain = 5\nrpc_urls = [\"{a}\", \"{c}\"]\nreorg_period = 0\ninterval_secs = 1"
        ))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let signer =
            Arc::new(DomainSigner::new(origin(5, Address::ZERO), store.clone(), key()).unwrap());
        let run = tokio::spawn(signer.clone().run(config, None));

        // The first tick commits 3 leaves. The backfill signs 0 and 1.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while store.first_unsigned_index(5).unwrap().is_some() || store.leaf_count(5).unwrap() < 3 {
            assert!(tokio::time::Instant::now() < deadline, "not signed in time");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_all_signed(&store, 5, Address::ZERO, &ids);

        // Both endpoints now report another root at index 2.
        *fake_root.lock().unwrap() = Some(B256::repeat_byte(0xee));
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("the loop stops")
            .unwrap();
        assert!(signer.halted());
        let flag = store.meta(5, Meta::ReorgFlag).unwrap().unwrap();
        assert!(String::from_utf8(flag).unwrap().contains("index 2"));

        // Another domain on the same store keeps signing.
        let other = DomainSigner::new(origin(6, Address::ZERO), store.clone(), key()).unwrap();
        other.commit(&batch(6, &[], &ids, 7)).await.unwrap();
        backfill_all(&other);
        assert_all_signed(&store, 6, Address::ZERO, &ids);
        assert_eq!(store.meta(6, Meta::ReorgFlag).unwrap(), None);
    }

    #[tokio::test]
    async fn a_domain_with_the_flag_does_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        store
            .set_meta(5, Meta::ReorgFlag, b"earlier reorg")
            .unwrap();
        let signer =
            Arc::new(DomainSigner::new(origin(5, Address::ZERO), store.clone(), key()).unwrap());
        assert!(signer.halted());
        let config: OriginConfig =
            toml::from_str("domain = 5\nrpc_urls = [\"http://127.0.0.1:1\"]\nreorg_period = 0")
                .unwrap();
        tokio::time::timeout(Duration::from_secs(1), signer.run(config, None))
            .await
            .expect("returns at once");
        assert_eq!(store.leaf_count(5).unwrap(), 0);
        assert_eq!(store.domains().unwrap(), vec![5]);
    }

    #[test]
    fn history_lost_when_the_controller_is_ahead() {
        assert!(history_lost(1, 10, Some(4)));
        assert!(history_lost(1, 3, None));
        assert!(!history_lost(1, 4, Some(4)));
        assert!(!history_lost(1, 4, Some(9)));
        assert!(!history_lost(1, 0, None));
    }
}
