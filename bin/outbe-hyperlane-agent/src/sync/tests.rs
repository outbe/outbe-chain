use super::*;
use crate::store::Leaf;

fn b(n: u8) -> B256 {
    B256::repeat_byte(n)
}

fn pending(from: u32, count: u32) -> (IncrementalMerkle, Vec<SyncedLeaf>) {
    let mut tip = IncrementalMerkle::default();
    let mut leaves = Vec::new();
    let raw = (0..from + count).map(|i| (i, b(i as u8 + 1), 100 + u64::from(i)));
    append(1, &mut tip, &mut leaves, raw.collect()).unwrap();
    (tip, leaves.split_off(from as usize))
}

fn obs(index: u32, root: B256) -> Option<Observed> {
    Some(Observed { index, root })
}

#[test]
fn dedup_keeps_order_and_one_vote_per_url() {
    let urls = ["a", "b", "a", "c", "b"].map(String::from);
    assert_eq!(dedup_urls(&urls), ["a", "b", "c"].map(String::from));
}

#[test]
fn consensus_thresholds_match_upstream() {
    for (n, majority, quorum) in [(1, 1, 1), (2, 2, 1), (3, 2, 2), (4, 3, 2), (6, 4, 3)] {
        assert_eq!(RpcConsensus::Majority.required(n), majority, "majority {n}");
        assert_eq!(RpcConsensus::Quorum.required(n), quorum, "quorum {n}");
        assert_eq!(RpcConsensus::Single.required(n), 1);
    }
}

#[test]
fn log_ranges_and_range_errors() {
    assert_eq!(log_range(1, 250_000, MAX_LOG_RANGE), (1, 100_000));
    assert_eq!(
        log_range(100_001, 150_000, MAX_LOG_RANGE),
        (100_001, 150_000)
    );
    assert_eq!(log_range(5, 5, 1), (5, 5));
    assert!(is_range_error("query exceeds max block range 100000"));
    assert!(is_range_error("Log response size exceeded"));
    assert!(is_range_error(
        "query returned more than 10000 results; limit"
    ));
    assert!(!is_range_error("connection refused"));
}

#[test]
fn append_orders_skips_replays_and_rejects_gaps() {
    let mut tip = IncrementalMerkle::default();
    let mut leaves = Vec::new();
    append(1, &mut tip, &mut leaves, vec![(1, b(2), 10), (0, b(1), 10)]).unwrap();
    assert_eq!(leaves.iter().map(|l| l.index).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(leaves[1].root, tip.root());

    // A replay of known leaves changes nothing.
    append(1, &mut tip, &mut leaves, vec![(0, b(1), 10), (1, b(2), 10)]).unwrap();
    assert_eq!(leaves.len(), 2);

    // A gap leaves the state as it was.
    let before = tip.clone();
    assert!(append(1, &mut tip, &mut leaves, vec![(2, b(3), 11), (4, b(5), 12)]).is_err());
    assert_eq!(tip, before);
    assert_eq!(leaves.len(), 2);
}

#[test]
fn vote_verifies_the_required_highest_matching_index() {
    let (_, leaves) = pending(0, 5);
    let at = |i: usize| obs(leaves[i].index, leaves[i].root);
    // 2 of 3 match: index 4 and 2. Required 2 → boundary 2.
    let vote = evaluate(&[at(4), at(2), None], 2, None, &leaves);
    assert_eq!(vote, Vote::Verified(2));
    // Single: one match is sufficient.
    assert_eq!(
        evaluate(&[None, at(3)], 1, None, &leaves),
        Vote::Verified(3)
    );
}

#[test]
fn vote_waits_for_leaves_or_for_rpc() {
    let (_, leaves) = pending(0, 2);
    let ahead = obs(9, b(9));
    let at1 = obs(1, leaves[1].root);
    // One match and one endpoint ahead of the local leaves: read more logs.
    assert_eq!(
        evaluate(&[at1, ahead, None], 2, None, &leaves),
        Vote::WaitingForLeaves
    );
    // Two failed endpoints of three: do not shrink the pool.
    assert_eq!(
        evaluate(&[at1, None, None], 2, None, &leaves),
        Vote::WaitingForRpc
    );
}

#[test]
fn vote_detects_a_reorg_of_committed_leaves() {
    let (tip, _) = pending(0, 3);
    let committed = Some((2, tip.root()));
    let other = obs(2, b(0xee));
    assert_eq!(
        evaluate(&[other, other, None], 2, committed, &[]),
        Vote::Reorg {
            index: 2,
            local: tip.root(),
            observed: b(0xee)
        }
    );
    // One endpoint is not enough to call a reorg.
    assert_eq!(
        evaluate(&[other, None, None], 2, committed, &[]),
        Vote::WaitingForRpc
    );
    // A stale endpoint below the committed tree is ignored.
    let stale = obs(1, b(0xee));
    let at2 = obs(2, tip.root());
    assert_eq!(
        evaluate(&[stale, at2, at2], 2, committed, &[]),
        Vote::Verified(2)
    );
}

#[test]
fn vote_rewinds_wrong_pending_leaves() {
    let (_, leaves) = pending(3, 2);
    let committed = Some((2, leaves[0].root));
    let other = obs(4, b(0xee));
    assert_eq!(
        evaluate(&[other, other, None], 2, committed, &leaves),
        Vote::Rewind
    );
}

#[test]
fn domain_sync_starts_from_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let leaves: Vec<(u32, Leaf)> = (0..3)
        .map(|i| {
            (
                i,
                Leaf {
                    message_id: b(i as u8 + 1),
                    block: 50,
                },
            )
        })
        .collect();
    store.commit_batch(7, &leaves, &[], 60).unwrap();
    let origin = Origin {
        domain: 7,
        hook: Address::ZERO,
        validator_announce: Address::ZERO,
        mailbox: Address::ZERO,
        start_block: 40,
    };
    let config: OriginConfig = toml::from_str(
        "domain = 7\nrpc_urls = [\"http://127.0.0.1:1\", \"http://127.0.0.1:1\"]\nreorg_period = 0",
    )
    .unwrap();
    let sync = DomainSync::new(origin.clone(), &config, store.clone()).unwrap();
    let (tip, _) = pending(0, 3);
    assert_eq!(sync.committed.root(), tip.root());
    assert_eq!(sync.cursor, 60);
    assert_eq!(sync.endpoints.len(), 1);

    let empty = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(empty.path()).unwrap());
    let sync = DomainSync::new(origin, &config, store).unwrap();
    assert_eq!(sync.cursor, 39);
    assert_eq!(sync.committed.count(), 0);
}

/// The chain state behind [`mock_rpc`].
pub(crate) struct MockChain {
    /// Message id and block of each leaf, in index order.
    pub leaves: Vec<(B256, u64)>,
    /// `latestCheckpoint()` returns this root when set.
    pub root: Option<B256>,
    pub head: u64,
    /// The next `eth_getLogs` requests that fail with a rate limit.
    pub rate_limited: u32,
}

impl MockChain {
    pub(crate) fn new(leaves: Vec<(B256, u64)>) -> Arc<std::sync::Mutex<Self>> {
        Arc::new(std::sync::Mutex::new(Self {
            leaves,
            root: None,
            head: 100,
            rate_limited: 0,
        }))
    }
}

/// A JSON-RPC server for one hook. `eth_getLogs` rejects ranges above 50
/// blocks. `latestCheckpoint()` returns the tree at the requested block, or
/// `root` when it is set.
pub(crate) async fn mock_rpc(chain: Arc<std::sync::Mutex<MockChain>>) -> String {
    use serde_json::{json, Value};

    let respond = move |req: &Value| -> Value {
        let id = req["id"].clone();
        let hex = |v: &Value| {
            u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
        };
        let mut chain = chain.lock().unwrap();
        let result = match req["method"].as_str().unwrap() {
            "eth_blockNumber" => json!(format!("{:#x}", chain.head)),
            "eth_getLogs" => {
                if chain.rate_limited > 0 {
                    chain.rate_limited -= 1;
                    return json!({"jsonrpc": "2.0", "id": id, "error": {"code": 429, "message": "Too Many Requests: rate limit exceeded"}});
                }
                let (from, to) = (
                    hex(&req["params"][0]["fromBlock"]),
                    hex(&req["params"][0]["toBlock"]),
                );
                if to - from + 1 > 50 {
                    return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "query exceeds max block range 50"}});
                }
                let logs: Vec<Value> = chain
                    .leaves
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, block))| (from..=to).contains(block))
                    .map(|(i, (id, block))| {
                        let data = IMerkleTreeHook::InsertedIntoTree {
                            messageId: *id,
                            index: i as u32,
                        }
                        .encode_data();
                        json!({
                            "address": Address::ZERO,
                            "topics": [IMerkleTreeHook::InsertedIntoTree::SIGNATURE_HASH],
                            "data": alloy_primitives::Bytes::from(data),
                            "blockNumber": format!("{block:#x}"),
                            "blockHash": B256::repeat_byte(1),
                            "transactionHash": B256::repeat_byte(2),
                            "transactionIndex": "0x0",
                            "logIndex": format!("{i:#x}"),
                            "removed": false,
                        })
                    })
                    .collect();
                json!(logs)
            }
            "eth_call" => {
                let at = hex(&req["params"][1]);
                let mut tree = IncrementalMerkle::default();
                chain
                    .leaves
                    .iter()
                    .filter(|(_, block)| *block <= at)
                    .for_each(|(id, _)| tree.ingest(*id));
                let ret = IMerkleTreeHook::latestCheckpointReturn {
                    root: chain.root.unwrap_or(tree.root()),
                    index: tree.index().unwrap(),
                };
                json!(alloy_primitives::Bytes::from(
                    IMerkleTreeHook::latestCheckpointCall::abi_encode_returns(&ret)
                ))
            }
            other => panic!("unexpected method {other}"),
        };
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    };
    serve_json_rpc(respond).await
}

/// A JSON-RPC server over HTTP. `respond` maps a request to a response.
pub(crate) async fn serve_json_rpc(
    respond: impl Fn(&serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let respond = Arc::new(respond);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let respond = respond.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse().unwrap())
                        .unwrap_or(0);
                    if buf.len() < end + 4 + len {
                        continue;
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[end + 4..end + 4 + len]).unwrap();
                    buf.drain(..end + 4 + len);
                    let reply = respond(&body).to_string();
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n", reply.len());
                    stream.write_all(head.as_bytes()).await.unwrap();
                    stream.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    url
}

#[tokio::test]
async fn tick_reads_votes_and_detects_a_reorg() {
    let leaves = vec![(b(1), 10), (b(2), 60), (b(3), 90)];
    let chain = MockChain::new(leaves.clone());
    let a = mock_rpc(chain.clone()).await;
    let c = mock_rpc(chain.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let origin = Origin {
        domain: 5,
        hook: Address::ZERO,
        validator_announce: Address::ZERO,
        mailbox: Address::ZERO,
        start_block: 1,
    };
    let config: OriginConfig = toml::from_str(&format!(
        "domain = 5\nrpc_urls = [\"{a}\", \"{c}\"]\nreorg_period = 0"
    ))
    .unwrap();
    let mut sync = DomainSync::new(origin, &config, store).unwrap();

    let Tick::Batch(batch) = sync.tick().await.unwrap() else {
        panic!("batch")
    };
    assert_eq!(batch.leaves.len(), 3);
    assert_eq!(batch.cursor, 100);
    let mut tree = IncrementalMerkle::default();
    for (leaf, (id, block)) in batch.leaves.iter().zip(&leaves) {
        tree.ingest(*id);
        assert_eq!(
            (leaf.message_id, leaf.block, leaf.root),
            (*id, *block, tree.root())
        );
    }
    assert_eq!(sync.tick().await.unwrap(), Tick::Idle);

    chain.lock().unwrap().root = Some(b(0xee));
    assert_eq!(
        sync.tick().await.unwrap(),
        Tick::Reorg {
            index: 2,
            local: tree.root(),
            observed: b(0xee)
        }
    );
}

#[test]
fn take_verified_stops_the_cursor_before_unverified_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let origin = Origin {
        domain: 1,
        hook: Address::ZERO,
        validator_announce: Address::ZERO,
        mailbox: Address::ZERO,
        start_block: 100,
    };
    let config: OriginConfig =
        toml::from_str("domain = 1\nrpc_urls = [\"http://127.0.0.1:1\"]\nreorg_period = 0")
            .unwrap();
    let mut sync = DomainSync::new(origin, &config, store).unwrap();
    let (tip, leaves) = pending(0, 4);
    sync.tip = tip;
    sync.pending = leaves;
    sync.scanned = 200;
    let Tick::Batch(batch) = sync.take_verified(1) else {
        panic!("batch")
    };
    assert_eq!(
        batch.leaves.iter().map(|l| l.index).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(batch.cursor, 101, "leaf 2 is in block 102");
    assert_eq!(sync.committed.root(), batch.leaves[1].root);
    let Tick::Batch(batch) = sync.take_verified(3) else {
        panic!("batch")
    };
    assert_eq!(batch.cursor, 200);
    assert_eq!(sync.take_verified(3), Tick::Idle);
}

/// A sync of domain 5 over `urls` on `store`.
fn domain_sync(urls: &[String], store: &Arc<Store>) -> DomainSync {
    let origin = Origin {
        domain: 5,
        hook: Address::ZERO,
        validator_announce: Address::ZERO,
        mailbox: Address::ZERO,
        start_block: 1,
    };
    let config: OriginConfig = toml::from_str(&format!(
        "domain = 5\nrpc_urls = {urls:?}\nreorg_period = 0"
    ))
    .unwrap();
    DomainSync::new(origin, &config, store.clone()).unwrap()
}

/// Leaves 0 and 1 are stored. Leaf 1 is in block 60, after the cursor 59, so
/// the next read gets it again.
fn store_with_two_leaves(cursor: u64) -> (Arc<Store>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let leaf = |n: u8, block| Leaf {
        message_id: b(n),
        block,
    };
    store
        .commit_batch(5, &[(0, leaf(1, 10)), (1, leaf(2, 60))], &[], cursor)
        .unwrap();
    (store, dir)
}

#[tokio::test]
async fn replayed_leaf_with_another_id_is_a_reorg() {
    let (store, _dir) = store_with_two_leaves(59);
    // The chain now has another message at the stored index 1.
    let chain = MockChain::new(vec![(b(1), 10), (b(0xaa), 60), (b(3), 90)]);
    let urls = [mock_rpc(chain.clone()).await, mock_rpc(chain).await];
    let mut sync = domain_sync(&urls, &store);
    // Two endpoints: the majority needs both. The first one alone is not enough.
    assert!(sync.tick().await.is_err());
    assert_eq!(
        sync.tick().await.unwrap(),
        Tick::Reorg {
            index: 1,
            local: b(2),
            observed: b(0xaa)
        }
    );
}

#[tokio::test]
async fn one_endpoint_with_another_leaf_does_not_halt() {
    let (store, _dir) = store_with_two_leaves(59);
    let good = vec![(b(1), 10), (b(2), 60), (b(3), 90)];
    let urls = [
        mock_rpc(MockChain::new(vec![(b(1), 10), (b(0xaa), 60), (b(3), 90)])).await,
        mock_rpc(MockChain::new(good.clone())).await,
        mock_rpc(MockChain::new(good)).await,
    ];
    let mut sync = domain_sync(&urls, &store);
    // The bad source gives an error and the source rotates.
    assert!(sync.tick().await.is_err());
    for _ in 0..3 {
        if let Ok(tick) = sync.tick().await {
            assert!(
                !matches!(tick, Tick::Reorg { .. }),
                "one endpoint must not halt"
            );
        }
    }
}

#[tokio::test]
async fn a_stale_log_source_rotates() {
    let leaves = vec![(b(1), 10), (b(2), 60), (b(3), 90)];
    let stale = MockChain::new(leaves.clone());
    stale.lock().unwrap().head = 20;
    let urls = [
        mock_rpc(stale).await,
        mock_rpc(MockChain::new(leaves)).await,
    ];
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let mut sync = domain_sync(&urls, &store);
    // The first source stopped at block 20. The other endpoint is ahead.
    for _ in 0..STALE_SOURCE_TICKS {
        assert_eq!(sync.tick().await.unwrap(), Tick::Idle);
    }
    assert_eq!(sync.source, 1, "the source rotates");
    assert!(matches!(sync.tick().await.unwrap(), Tick::Batch(_)));
}

#[tokio::test]
async fn a_rate_limit_keeps_the_range_and_the_chunk_grows_back() {
    assert!(!is_range_error(
        "HTTP error 429 with body: Too Many Requests"
    ));
    assert!(!is_range_error("rate limit exceeded"));
    let chain = MockChain::new(vec![(b(1), 10), (b(2), 60), (b(3), 90)]);
    chain.lock().unwrap().rate_limited = 1;
    let urls = [mock_rpc(chain).await];
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let mut sync = domain_sync(&urls, &store);

    // A rate limit is not a range error: the next tick reads the same range.
    assert!(sync.tick().await.is_err());
    assert_eq!((sync.chunk, sync.scanned), (MAX_LOG_RANGE, 0));
    let Tick::Batch(batch) = sync.tick().await.unwrap() else {
        panic!("batch")
    };
    assert_eq!(batch.leaves.len(), 3);
    // Range errors shrink the chunk below 50. Each read that succeeds
    // doubles it again.
    assert!(sync.chunk > 50, "chunk {}", sync.chunk);
}

#[tokio::test]
async fn repeated_rewinds_mark_the_sync_stalled() {
    let (store, _dir) = store_with_two_leaves(60);
    // Leaf 0 changed in a block below the cursor: no replay reads it.
    let chain = MockChain::new(vec![(b(0xbb), 10), (b(2), 60), (b(3), 90)]);
    let urls = [mock_rpc(chain.clone()).await, mock_rpc(chain).await];
    let mut sync = domain_sync(&urls, &store);
    for _ in 0..MAX_REWINDS {
        assert!(!sync.stalled());
        assert_eq!(sync.tick().await.unwrap(), Tick::Idle);
    }
    assert!(sync.stalled());
}
