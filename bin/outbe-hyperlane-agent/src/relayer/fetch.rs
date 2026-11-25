//! Reads the signed checkpoints of the validators from their announced
//! storage locations.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0` (Apache-2.0, see
//! `LICENSE-APACHE`):
//! - `rust/main/hyperlane-base/src/types/multisig.rs` (`fetch_checkpoint`):
//!   the batches, the checks of each checkpoint and the quorum rule.
//! - `rust/main/agents/relayer/src/msg/metadata/base_builder.rs`: the choice
//!   of the storage location and the reorg flag.
//! - `rust/main/hyperlane-base/src/types/s3_storage.rs`: the object keys, the
//!   10 second time limit and the 50 KiB size limit.
//!
//! Changes:
//! - Only the `s3+http(s)` location, read with a plain HTTP `GET`.
//! - No checkpoint cache and no syncer pool.
//! - A checkpoint with a signature that does not recover is skipped. Upstream
//!   stops the whole read.
//! - Each checkpoint must also have the wanted domain, hook and message id.
//!   Upstream checks the message id only after the quorum.
//! - The reorg flag read has a 3 second time limit and an error is "no flag".
//!
//! A validator chooses its location, so the location is not trusted: only
//! `http` and `https`, no redirect, a time limit and a size limit.

use std::collections::HashMap;
use std::time::Duration;

use alloy_primitives::{Address, B256};
use eyre::Result;
use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::{redirect, StatusCode, Url};
use tracing::{debug, warn};

use crate::checkpoint::{CheckpointWithMessageId, Signed};
use crate::errors::RelayError;

/// Upstream `S3_REQUEST_TIMEOUT`.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const REORG_FLAG_TIMEOUT: Duration = Duration::from_secs(3);
/// Upstream `MAX_CHECKPOINT_OBJECT_SIZE`.
const MAX_BODY: usize = 50 * 1024;
/// The largest number of validators that the relayer reads at one time.
const MAX_BATCH: usize = 10;

/// The checkpoint that the relayer needs for one message.
#[derive(Clone, Copy, Debug)]
pub struct Want {
    pub domain: u32,
    pub hook: Address,
    pub index: u32,
    pub message_id: B256,
}

/// The result of one read of the validator signatures.
#[derive(Debug, PartialEq, Eq)]
pub enum Quorum {
    /// A root that the threshold of validators signed, and their signatures
    /// in the order of the validator set: the multisig ISM needs this order.
    Found(B256, Vec<[u8; 65]>),
    /// Fewer validators than the threshold serve a signed checkpoint.
    NotYet,
    /// A validator reports a reorg of the origin chain in its storage.
    Reorg(Address),
}

/// Validators that the relayer reads at one time: `threshold * 1.5`, from 1
/// to 10. A quorum comes faster than with one read of all validators.
fn batch_size(threshold: usize) -> usize {
    ((threshold as f64 * 1.5) as usize).clamp(1, MAX_BATCH)
}

/// The base URL of an announced `s3+http(s)://host[:port]/path` location.
fn base_url(location: &str) -> Result<Url, RelayError> {
    let bad = || RelayError::BadLocation(location.to_owned());
    let base = location.strip_prefix("s3+").ok_or_else(bad)?;
    let url: Url = format!("{}/", base.trim_end_matches('/'))
        .parse()
        .map_err(|_| bad())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(bad());
    }
    Ok(url)
}

/// The storage of a validator: its newest announced location that the
/// relayer can read. As upstream, an older location is not a fallback when
/// the newest one has no checkpoint.
fn storage(validator: Address, locations: &[String]) -> Option<Url> {
    let url = locations.iter().rev().find_map(|l| base_url(l).ok());
    if url.is_none() {
        debug!(%validator, ?locations, "validator has no readable storage location");
    }
    url
}

/// True when `signed` is the wanted checkpoint and `validator` signed it.
fn is_wanted(signed: &Signed<CheckpointWithMessageId>, want: &Want, validator: Address) -> bool {
    let c = &signed.value.checkpoint;
    c.index == want.index
        && c.mailbox_domain == want.domain
        && c.merkle_tree_hook_address == want.hook.into_word()
        && signed.value.message_id == want.message_id
        && signed.recover().ok() == Some(validator)
}

/// Reads checkpoints over HTTP.
pub struct Fetcher {
    client: reqwest::Client,
}

impl Fetcher {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }

    /// The object `key` of a storage. `None`: the object does not exist.
    async fn object(&self, storage: &Url, key: &str, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let url = storage.join(key)?;
        let mut response = self.client.get(url).timeout(timeout).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        // Only 200 is the object. A redirect is not followed, and a proxy
        // page with another status is not an object of the storage.
        if response.status() != StatusCode::OK {
            eyre::bail!("status {} from {storage}", response.status());
        }
        let mut response_body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if response_body.len() + chunk.len() >= MAX_BODY {
                return Err(RelayError::BadLocation(storage.to_string()).into());
            }
            response_body.extend_from_slice(&chunk);
        }
        Ok(Some(response_body))
    }

    /// The checkpoint of `want` that `validator` signed. `None`: the storage
    /// does not have it, does not answer, or has another checkpoint there.
    async fn checkpoint(
        &self,
        validator: Address,
        storage: &Url,
        want: &Want,
    ) -> Option<Signed<CheckpointWithMessageId>> {
        let key = format!("checkpoint_{}_with_id.json", want.index);
        let signed = match self.object(storage, &key, FETCH_TIMEOUT).await {
            Ok(Some(body)) => serde_json::from_slice(&body).ok()?,
            Ok(None) => return None,
            Err(error) => {
                debug!(%validator, %storage, index = want.index, %error, "checkpoint read failed");
                return None;
            }
        };
        if !is_wanted(&signed, want, validator) {
            debug!(%validator, index = want.index, "checkpoint has other content or another signer");
            return None;
        }
        Some(signed)
    }

    /// True when the storage has a reorg flag: a JSON object at
    /// `reorg_flag.json`. A storage that does not answer has no flag: the
    /// checkpoint read then finds no signature. Another body is not a flag: a
    /// proxy that answers each path with a page must not stop the deliveries.
    async fn reorg_flag(&self, storage: &Url) -> bool {
        let object = self
            .object(storage, "reorg_flag.json", REORG_FLAG_TIMEOUT)
            .await;
        match object {
            Ok(Some(body)) => serde_json::from_slice::<serde_json::Value>(&body)
                .is_ok_and(|flag| flag.is_object()),
            _ => false,
        }
    }

    /// True when the storage of `validator` serves the checkpoint of `want`
    /// with the root `root` and the signature of `validator`.
    pub async fn serves(
        &self,
        validator: Address,
        locations: &[String],
        want: &Want,
        root: B256,
    ) -> bool {
        let Some(url) = storage(validator, locations) else {
            return false;
        };
        self.checkpoint(validator, &url, want)
            .await
            .is_some_and(|signed| signed.value.checkpoint.root == root)
    }

    /// Reads the checkpoint of `want` from the validators until one root has
    /// `threshold` signatures. `locations[i]` are the announced locations of
    /// `validators[i]`.
    pub async fn quorum(
        &self,
        validators: &[Address],
        locations: &[Vec<String>],
        threshold: usize,
        want: &Want,
    ) -> Quorum {
        let storages: Vec<Option<Url>> = validators
            .iter()
            .zip(locations)
            .map(|(validator, locations)| storage(*validator, locations))
            .collect();
        // A validator that saw a reorg must stop every delivery from that
        // origin, also when the other validators have a quorum.
        let flags = storages.iter().flatten().map(|s| self.reorg_flag(s));
        let flags = futures::future::join_all(flags).await;
        let with_storage = validators
            .iter()
            .zip(&storages)
            .filter(|(_, s)| s.is_some());
        for ((validator, _), flag) in with_storage.zip(flags) {
            if flag {
                warn!(%validator, domain = want.domain, "validator reports a reorg in its storage");
                return Quorum::Reorg(*validator);
            }
        }

        // In practice all validators sign the same root at one index. The
        // roots stay apart, so signatures of different roots never mix.
        let mut by_root: HashMap<B256, Vec<(usize, [u8; 65])>> = HashMap::new();
        let indexed: Vec<(usize, &Address, &Option<Url>)> = validators
            .iter()
            .zip(&storages)
            .enumerate()
            .map(|(position, (validator, storage))| (position, validator, storage))
            .collect();
        for batch in indexed.chunks(batch_size(threshold)) {
            let mut reads: FuturesUnordered<_> =
                batch
                    .iter()
                    .filter_map(|(position, validator, storage)| {
                        let storage = storage.as_ref()?;
                        Some(async move {
                            (*position, self.checkpoint(**validator, storage, want).await)
                        })
                    })
                    .collect();
            while let Some((position, signed)) = reads.next().await {
                let Some(signed) = signed else { continue };
                let signatures = by_root.entry(signed.value.checkpoint.root).or_default();
                signatures.push((position, signed.signature_bytes()));
                if threshold > 0 && signatures.len() >= threshold {
                    signatures.sort_unstable_by_key(|(position, _)| *position);
                    let signatures = signatures.iter().map(|(_, s)| *s).collect();
                    return Quorum::Found(signed.value.checkpoint.root, signatures);
                }
            }
        }
        Quorum::NotYet
    }
}

#[cfg(test)]
mod tests {
    use alloy_signer_local::PrivateKeySigner;
    use axum::http::StatusCode as Status;
    use axum::routing::get;

    use super::*;
    use crate::checkpoint::Checkpoint;

    const WANT: Want = Want {
        domain: 5,
        hook: Address::repeat_byte(0x11),
        index: 3,
        message_id: B256::repeat_byte(0x33),
    };

    fn sign(key: &PrivateKeySigner, want: &Want, root: u8) -> String {
        let value = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: want.hook.into_word(),
                mailbox_domain: want.domain,
                root: B256::repeat_byte(root),
                index: want.index,
            },
            message_id: want.message_id,
        };
        serde_json::to_string(&Signed::sign(value, key).unwrap()).unwrap()
    }

    /// Serves `body` as checkpoint 3 of a bucket, after `delay`, and a reorg
    /// flag when `reorg` is true. Returns the location.
    async fn bucket_with(body: String, delay: Duration, reorg: bool) -> String {
        let app = axum::Router::new()
            .route(
                "/b/5/checkpoint_3_with_id.json",
                get(move || {
                    let body = body.clone();
                    async move {
                        tokio::time::sleep(delay).await;
                        body
                    }
                }),
            )
            .route(
                "/b/5/reorg_flag.json",
                get(move || async move {
                    match reorg {
                        true => (Status::OK, "{\"reason\":\"test\"}"),
                        // A catch-all page of a proxy is not a flag.
                        false => (Status::OK, "<html>welcome</html>"),
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!("s3+http://{}/b/5", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        location
    }

    async fn bucket(body: String) -> String {
        bucket_with(body, Duration::ZERO, false).await
    }

    #[test]
    fn batch_sizes_follow_upstream() {
        let sizes = [0, 1, 2, 3, 4, 5, 6, 7, 30].map(batch_size);
        assert_eq!(sizes, [1, 1, 3, 4, 6, 7, 9, 10, 10]);
    }

    #[test]
    fn locations_are_checked_and_the_newest_readable_one_wins() {
        assert_eq!(
            base_url("s3+https://v.example.org:9000/0xab/5")
                .unwrap()
                .join("checkpoint_7_with_id.json")
                .unwrap()
                .as_str(),
            "https://v.example.org:9000/0xab/5/checkpoint_7_with_id.json"
        );
        for bad in [
            "s3://bucket/us-east-1",
            "file:///etc",
            "s3+ftp://h/b",
            "s3+http://u:p@h/b",
            "s3+http://h/b?x=1",
        ] {
            assert!(base_url(bad).is_err(), "{bad}");
        }
        let locations = ["s3+http://old/b", "s3+http://new/b", "s3://aws/region"].map(String::from);
        let chosen = storage(Address::ZERO, &locations).unwrap();
        assert_eq!(chosen.as_str(), "http://new/b/");
        assert!(storage(Address::ZERO, &[]).is_none());
    }

    #[tokio::test]
    async fn a_validator_serves_only_its_own_checkpoint_with_the_wanted_root() {
        let key = PrivateKeySigner::random();
        let location = vec![bucket(sign(&key, &WANT, 7)).await];
        let fetcher = Fetcher::new().unwrap();
        let root = B256::repeat_byte(7);
        assert!(fetcher.serves(key.address(), &location, &WANT, root).await);
        // Another root: the validator serves a checkpoint of another tree.
        let other = B256::repeat_byte(8);
        assert!(!fetcher.serves(key.address(), &location, &WANT, other).await);
        // Another signer, no location, and a storage that does not answer.
        let stranger = Address::repeat_byte(0x99);
        assert!(!fetcher.serves(stranger, &location, &WANT, root).await);
        assert!(!fetcher.serves(key.address(), &[], &WANT, root).await);
        let closed = vec!["s3+http://127.0.0.1:1/b/5".to_owned()];
        assert!(!fetcher.serves(key.address(), &closed, &WANT, root).await);
    }

    #[tokio::test]
    async fn quorum_needs_one_root_and_keeps_the_validator_order() {
        let keys: Vec<_> = (0..4).map(|_| PrivateKeySigner::random()).collect();
        let validators: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let other = Want { index: 4, ..WANT };
        let locations = vec![
            // An old location is not read: the newest one is the storage.
            vec![
                "s3+http://127.0.0.1:1/b/5".to_owned(),
                bucket(sign(&keys[0], &WANT, 7)).await,
            ],
            // A checkpoint that another key signed.
            vec![bucket(sign(&keys[0], &WANT, 7)).await],
            // A checkpoint of another index.
            vec![bucket(sign(&keys[2], &other, 7)).await],
            vec![bucket(sign(&keys[3], &WANT, 7)).await],
        ];
        let fetcher = Fetcher::new().unwrap();

        let Quorum::Found(root, signatures) =
            fetcher.quorum(&validators, &locations, 2, &WANT).await
        else {
            panic!("no quorum")
        };
        assert_eq!(root, B256::repeat_byte(7));
        let signer = |signature: &[u8; 65]| {
            alloy_primitives::Signature::try_from(&signature[..])
                .unwrap()
                .recover_address_from_prehash(&alloy_primitives::eip191_hash_message(
                    crate::checkpoint::Signable::signing_hash(&CheckpointWithMessageId {
                        checkpoint: Checkpoint {
                            merkle_tree_hook_address: WANT.hook.into_word(),
                            mailbox_domain: 5,
                            root,
                            index: 3,
                        },
                        message_id: WANT.message_id,
                    }),
                ))
                .unwrap()
        };
        assert_eq!(
            signatures.iter().map(signer).collect::<Vec<_>>(),
            [validators[0], validators[3]]
        );
        // Only two validators serve a valid checkpoint.
        assert_eq!(
            fetcher.quorum(&validators, &locations, 3, &WANT).await,
            Quorum::NotYet
        );
    }

    #[tokio::test]
    async fn two_roots_do_not_mix() {
        let keys: Vec<_> = (0..3).map(|_| PrivateKeySigner::random()).collect();
        let validators: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let locations = vec![
            vec![bucket(sign(&keys[0], &WANT, 7)).await],
            vec![bucket(sign(&keys[1], &WANT, 8)).await],
            vec![bucket(sign(&keys[2], &WANT, 8)).await],
        ];
        let fetcher = Fetcher::new().unwrap();
        let Quorum::Found(root, signatures) =
            fetcher.quorum(&validators, &locations, 2, &WANT).await
        else {
            panic!("no quorum")
        };
        assert_eq!((root, signatures.len()), (B256::repeat_byte(8), 2));
    }

    #[tokio::test]
    async fn the_read_ends_at_the_threshold() {
        let keys: Vec<_> = (0..3).map(|_| PrivateKeySigner::random()).collect();
        let validators: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let slow = Duration::from_secs(5);
        let locations = vec![
            vec![bucket(sign(&keys[0], &WANT, 7)).await],
            // The same batch as the two fast validators (threshold 2: 3 at one time).
            vec![bucket_with(sign(&keys[1], &WANT, 7), slow, false).await],
            vec![bucket(sign(&keys[2], &WANT, 7)).await],
        ];
        let fetcher = Fetcher::new().unwrap();
        let start = std::time::Instant::now();
        let quorum = fetcher.quorum(&validators, &locations, 2, &WANT).await;
        assert!(matches!(quorum, Quorum::Found(_, ref s) if s.len() == 2));
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "waited for the slow validator"
        );
    }

    #[tokio::test]
    async fn a_reorg_flag_of_one_validator_refuses_the_read() {
        let keys: Vec<_> = (0..3).map(|_| PrivateKeySigner::random()).collect();
        let validators: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let locations = vec![
            vec![bucket(sign(&keys[0], &WANT, 7)).await],
            vec![bucket(sign(&keys[1], &WANT, 7)).await],
            vec![bucket_with(sign(&keys[2], &WANT, 7), Duration::ZERO, true).await],
        ];
        let fetcher = Fetcher::new().unwrap();
        assert_eq!(
            fetcher.quorum(&validators, &locations, 2, &WANT).await,
            Quorum::Reorg(validators[2])
        );
    }
}
