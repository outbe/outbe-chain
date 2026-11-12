//! S3-compatible anonymous read of the signed checkpoints, path-style:
//! `GET /<bucket>/<domain>/<key>`. The relayer reads it with its S3 client.
//!
//! Object keys and JSON forms follow hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/hyperlane-base/src/types/s3_storage.rs` (Apache-2.0, see
//! `LICENSE-APACHE`). Changes: read only, objects come from the store on each
//! request, no `metadata_latest.json` (the relayer does not read it).
//!
//! Only the keys of the allow-list exist. A request path never maps to a file.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use eyre::Result;
use tracing::{info, warn};

use crate::health::ValidatorHealth;

#[derive(Clone)]
pub struct Bucket {
    /// Lowercase `0x` address of the signer key.
    pub name: String,
    pub health: Arc<ValidatorHealth>,
}

pub fn router(bucket: Bucket) -> Router {
    Router::new()
        .route("/{bucket}/{domain}/{key}", get(object))
        .with_state(bucket)
}

pub async fn start(bind_address: &str, bucket: Bucket) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind_address).await?;
    info!(
        addr = bind_address,
        bucket = bucket.name,
        "checkpoint server listening"
    );
    let app = router(bucket);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            warn!(%error, "checkpoint server stopped");
        }
    });
    Ok(())
}

/// Parses a `u32` only in its canonical decimal form: "07" or "+7" is
/// another S3 key, so it does not exist.
fn canonical_u32(s: &str) -> Option<u32> {
    s.parse().ok().filter(|n: &u32| n.to_string() == s)
}

async fn object(
    State(bucket): State<Bucket>,
    Path((name, domain, key)): Path<(String, String, String)>,
) -> Response {
    if name != bucket.name {
        return error(
            "NoSuchBucket",
            "The specified bucket does not exist.",
            "BucketName",
            &name,
        );
    }
    match read(&bucket, &domain, &key) {
        Ok(Some(body)) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Ok(None) => error(
            "NoSuchKey",
            "The specified key does not exist.",
            "Key",
            &format!("{domain}/{key}"),
        ),
        Err(err) => {
            warn!(domain, key, error = %err, "checkpoint read failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The object body, or `None` when the key does not exist.
fn read(bucket: &Bucket, domain: &str, key: &str) -> Result<Option<Vec<u8>>> {
    let Some(signer) = canonical_u32(domain).and_then(|d| bucket.health.signer(d)) else {
        return Ok(None);
    };
    if key == "checkpoint_latest_index.json" {
        return Ok(signer
            .latest_index()?
            .map(|index| serde_json::to_vec(&index))
            .transpose()?);
    }
    if key == "announcement.json" {
        return signer.announcement();
    }
    let index = key
        .strip_prefix("checkpoint_")
        .and_then(|rest| rest.strip_suffix("_with_id.json"))
        .and_then(canonical_u32);
    match index {
        Some(index) => Ok(signer
            .signed_checkpoint(index)?
            .map(|signed| serde_json::to_vec(&signed))
            .transpose()?),
        None => Ok(None),
    }
}

/// An S3 error: 404 with the XML body of the S3 API.
fn error(code: &str, message: &str, field: &str, value: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{message}</Message><{field}>{}</{field}></Error>",
        xml_escape(value)
    );
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/xml")],
        body,
    )
        .into_response()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle::IncrementalMerkle;
    use crate::signer::DomainSigner;
    use crate::store::{Leaf, Meta, Store};
    use crate::sync::Origin;
    use alloy_primitives::{Address, B256};
    use alloy_signer_local::PrivateKeySigner;

    const BUCKET: &str = "0x14dc79964da2c08b23698b3d3cc7ca32193d9955";

    /// A server with domain 5: three leaves, none signed.
    async fn serve() -> (String, Arc<Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        let origin = Origin {
            domain: 5,
            hook: Address::repeat_byte(0x11),
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 1,
        };
        let key = PrivateKeySigner::from_bytes(&B256::repeat_byte(7)).unwrap();
        let signer = Arc::new(DomainSigner::new(origin, store.clone(), key).unwrap());
        let leaves: Vec<(u32, Leaf)> = (0..3u8)
            .map(|i| {
                let leaf = Leaf {
                    message_id: B256::repeat_byte(i + 1),
                    block: 10,
                };
                (u32::from(i), leaf)
            })
            .collect();
        store.commit_batch(5, &leaves, &[], 10).unwrap();
        health.set_running(signer);
        store
            .set_meta(5, Meta::Announcement, b"{\n  \"value\": 1\n}")
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = router(Bucket {
            name: BUCKET.into(),
            health,
        });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, store, dir)
    }

    async fn get(url: &str) -> (u16, String, Option<String>, String) {
        let r = reqwest::get(url).await.unwrap();
        let status = r.status().as_u16();
        let header =
            |h: header::HeaderName| r.headers().get(h).map(|v| v.to_str().unwrap().to_owned());
        let ct = header(header::CONTENT_TYPE).unwrap_or_default();
        let len = header(header::CONTENT_LENGTH);
        (status, ct, len, r.text().await.unwrap())
    }

    #[tokio::test]
    async fn serves_s3_objects_and_errors() {
        let (url, store, _dir) = serve().await;
        let base = format!("{url}/{BUCKET}/5");

        // Nothing signed yet: S3 NoSuchKey.
        let (status, ct, _, body) = get(&format!("{base}/checkpoint_latest_index.json")).await;
        assert_eq!((status, ct.as_str()), (404, "application/xml"));
        assert_eq!(
            body,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>5/checkpoint_latest_index.json</Key></Error>"
        );

        // The backfill of the real signer fills the store.
        let key = PrivateKeySigner::from_bytes(&B256::repeat_byte(7)).unwrap();
        let mut tree = IncrementalMerkle::default();
        let mut signed = Vec::new();
        for i in 0..3u8 {
            tree.ingest(B256::repeat_byte(i + 1));
            let value = crate::checkpoint::CheckpointWithMessageId {
                checkpoint: crate::checkpoint::Checkpoint {
                    merkle_tree_hook_address: Address::repeat_byte(0x11).into_word(),
                    mailbox_domain: 5,
                    root: tree.root(),
                    index: u32::from(i),
                },
                message_id: B256::repeat_byte(i + 1),
            };
            let s = crate::checkpoint::Signed::sign(value, &key).unwrap();
            signed.push((u32::from(i), s));
        }
        let rows: Vec<_> = signed
            .iter()
            .map(|(i, s)| {
                let root = crate::store::SignedRoot {
                    root: s.value.checkpoint.root,
                    signature: s.signature_bytes(),
                };
                (*i, root)
            })
            .collect();
        store.put_checkpoints(5, &rows).unwrap();

        let (status, ct, len, body) = get(&format!("{base}/checkpoint_latest_index.json")).await;
        assert_eq!(
            (status, ct.as_str(), body.as_str()),
            (200, "application/json", "2")
        );
        assert_eq!(len.as_deref(), Some("1"));

        let (status, ct, len, body) = get(&format!("{base}/checkpoint_1_with_id.json")).await;
        let expected = serde_json::to_string(&signed[1].1).unwrap();
        assert_eq!((status, ct.as_str()), (200, "application/json"));
        assert_eq!(body, expected, "byte-identical to serde_json::to_vec");
        assert_eq!(len, Some(expected.len().to_string()));

        let (status, _, _, body) = get(&format!("{base}/announcement.json")).await;
        assert_eq!((status, body.as_str()), (200, "{\n  \"value\": 1\n}"));

        // HEAD: the same headers, no body.
        let head = reqwest::Client::new()
            .head(format!("{base}/checkpoint_1_with_id.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), 200);
        assert_eq!(
            head.headers()[header::CONTENT_LENGTH],
            expected.len().to_string().as_str()
        );
        assert_eq!(head.text().await.unwrap(), "");

        // Keys outside the allow-list or in a non-canonical form do not exist.
        for key in [
            "checkpoint_01_with_id.json",
            "checkpoint_+1_with_id.json",
            "checkpoint_9_with_id.json",
            "checkpoint_4294967296_with_id.json",
            "metadata_latest.json",
            "..%2F..%2Fetc%2Fpasswd",
        ] {
            let (status, _, _, body) = get(&format!("{base}/{key}")).await;
            assert_eq!(status, 404, "{key}");
            assert!(body.contains("<Code>NoSuchKey</Code>"), "{key}: {body}");
        }
        let (status, _, _, body) = get(&format!("{url}/{BUCKET}/05/announcement.json")).await;
        assert_eq!(status, 404);
        assert!(body.contains("<Key>05/announcement.json</Key>"), "{body}");
        // The key is escaped in the XML body.
        let (_, _, _, body) = get(&format!("{base}/%3Cx%3E")).await;
        assert!(body.contains("<Key>5/&lt;x&gt;</Key>"), "{body}");

        // Another bucket.
        let (status, ct, _, body) = get(&format!("{url}/0xother/5/announcement.json")).await;
        assert_eq!((status, ct.as_str()), (404, "application/xml"));
        assert_eq!(
            body,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist.</Message><BucketName>0xother</BucketName></Error>"
        );

        // Other paths and methods.
        assert_eq!(get(&format!("{url}/{BUCKET}")).await.0, 404);
        assert_eq!(get(&format!("{base}/a/b")).await.0, 404);
        let put = reqwest::Client::new()
            .put(format!("{base}/announcement.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), 405);
    }
}
