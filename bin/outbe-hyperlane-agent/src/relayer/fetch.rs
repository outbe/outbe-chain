//! Reads the signed checkpoints of the validators from their announced
//! storage locations.
//!
//! The flow follows hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/relayer/src/msg/metadata/multisig/message_id_multisig.rs`
//! and `hyperlane-base/src/types/multisig.rs` (Apache-2.0, see
//! `LICENSE-APACHE`). Changes: only the `s3+http(s)` location, a plain HTTP
//! `GET`, no checkpoint cache.
//!
//! A validator chooses its location, so the location is not trusted: only
//! `http` and `https`, no redirect, a time limit and a size limit.

use std::collections::BTreeMap;
use std::time::Duration;

use alloy_primitives::{Address, B256};
use eyre::Result;
use reqwest::{redirect, Url};
use tracing::debug;

use crate::checkpoint::{CheckpointWithMessageId, Signed};
use crate::errors::RelayError;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// A checkpoint file has about 700 bytes.
const MAX_BODY: usize = 16 * 1024;

/// The checkpoint that the relayer needs for one message.
#[derive(Clone, Copy, Debug)]
pub struct Want {
    pub domain: u32,
    pub hook: Address,
    pub index: u32,
    pub message_id: B256,
}

/// The URL of a checkpoint in an announced `s3+http(s)://host[:port]/path`
/// location.
fn checkpoint_url(location: &str, index: u32) -> Result<Url, RelayError> {
    let bad = || RelayError::BadLocation(location.to_owned());
    let base = location.strip_prefix("s3+").ok_or_else(bad)?;
    let url: Url = format!(
        "{}/checkpoint_{index}_with_id.json",
        base.trim_end_matches('/')
    )
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

/// Checks that `signed` is the wanted checkpoint and that `validator` signed
/// it.
fn check(
    signed: &Signed<CheckpointWithMessageId>,
    want: &Want,
    validator: Address,
) -> Result<(), RelayError> {
    let c = &signed.value.checkpoint;
    let same = c.mailbox_domain == want.domain
        && c.merkle_tree_hook_address == want.hook.into_word()
        && c.index == want.index
        && signed.value.message_id == want.message_id;
    if !same || signed.recover().ok() != Some(validator) {
        return Err(RelayError::WrongCheckpoint {
            validator,
            index: want.index,
        });
    }
    Ok(())
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

    /// The checkpoint of `want` that `validator` signed, from one location.
    async fn checkpoint(
        &self,
        location: &str,
        want: &Want,
        validator: Address,
    ) -> Result<Signed<CheckpointWithMessageId>> {
        let url = checkpoint_url(location, want.index)?;
        let mut response = self.client.get(url).send().await?.error_for_status()?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(RelayError::BadLocation(location.to_owned()).into());
            }
            body.extend_from_slice(&chunk);
        }
        let signed = serde_json::from_slice(&body)?;
        check(&signed, want, validator)?;
        Ok(signed)
    }

    /// The checkpoint of one validator. The validator can have more than one
    /// announced location: the newest location is first.
    async fn of_validator(
        &self,
        validator: Address,
        locations: &[String],
        want: &Want,
    ) -> Option<Signed<CheckpointWithMessageId>> {
        for location in locations.iter().rev() {
            match self.checkpoint(location, want, validator).await {
                Ok(signed) => return Some(signed),
                Err(error) => {
                    debug!(%validator, location, index = want.index, %error, "no checkpoint")
                }
            }
        }
        None
    }

    /// Reads the checkpoint of `want` from every validator in parallel.
    /// Returns a root that `threshold` validators signed and their signatures
    /// in the order of `validators`: the multisig ISM needs this order.
    /// `locations[i]` are the announced locations of `validators[i]`.
    pub async fn quorum(
        &self,
        validators: &[Address],
        locations: &[Vec<String>],
        threshold: usize,
        want: &Want,
    ) -> Option<(B256, Vec<[u8; 65]>)> {
        let reads = validators
            .iter()
            .zip(locations)
            .map(|(validator, locations)| self.of_validator(*validator, locations, want));
        let mut by_root: BTreeMap<B256, Vec<[u8; 65]>> = BTreeMap::new();
        for signed in futures::future::join_all(reads).await.into_iter().flatten() {
            by_root
                .entry(signed.value.checkpoint.root)
                .or_default()
                .push(signed.signature_bytes());
        }
        by_root
            .into_iter()
            .find(|(_, signatures)| threshold > 0 && signatures.len() >= threshold)
            .map(|(root, mut signatures)| {
                signatures.truncate(threshold);
                (root, signatures)
            })
    }
}

#[cfg(test)]
mod tests {
    use alloy_signer_local::PrivateKeySigner;
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

    /// Serves `body` as checkpoint 3 of a bucket. Returns the location.
    async fn bucket(body: String) -> String {
        let app = axum::Router::new().route(
            "/b/5/checkpoint_3_with_id.json",
            get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!("s3+http://{}/b/5", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        location
    }

    #[test]
    fn locations_are_checked() {
        assert_eq!(
            checkpoint_url("s3+https://v.example.org:9000/0xab/5", 7)
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
            assert!(checkpoint_url(bad, 7).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn quorum_needs_one_root_and_keeps_the_validator_order() {
        let keys: Vec<_> = (0..4).map(|_| PrivateKeySigner::random()).collect();
        let validators: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let other = Want { index: 4, ..WANT };
        let locations = vec![
            // An old location that does not answer, then the current one.
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

        let (root, signatures) = fetcher
            .quorum(&validators, &locations, 2, &WANT)
            .await
            .unwrap();
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
        assert!(fetcher
            .quorum(&validators, &locations, 3, &WANT)
            .await
            .is_none());
    }
}
