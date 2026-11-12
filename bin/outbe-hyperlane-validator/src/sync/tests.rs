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
    let store = Store::open(dir.path()).unwrap();
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
    let sync = DomainSync::new(origin.clone(), &config, &store).unwrap();
    let (tip, _) = pending(0, 3);
    assert_eq!(sync.committed.root(), tip.root());
    assert_eq!(sync.cursor, 60);
    assert_eq!(sync.endpoints.len(), 1);

    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let sync = DomainSync::new(origin, &config, &store).unwrap();
    assert_eq!(sync.cursor, 39);
    assert_eq!(sync.committed.count(), 0);
}

/// A JSON-RPC server for one hook: leaves at fixed blocks, head at block
/// 100, `eth_getLogs` rejects ranges above 50 blocks, and
/// `latestCheckpoint()` returns `root` (or the real root when `None`).
pub(crate) async fn mock_rpc(
    leaves: Vec<(B256, u64)>,
    root: Arc<std::sync::Mutex<Option<B256>>>,
) -> String {
    use serde_json::{json, Value};

    let respond = move |req: &Value| -> Value {
        let id = req["id"].clone();
        let hex = |v: &Value| {
            u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
        };
        let result = match req["method"].as_str().unwrap() {
            "eth_blockNumber" => json!("0x64"),
            "eth_getLogs" => {
                let (from, to) = (
                    hex(&req["params"][0]["fromBlock"]),
                    hex(&req["params"][0]["toBlock"]),
                );
                if to - from + 1 > 50 {
                    return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "query exceeds max block range 50"}});
                }
                let logs: Vec<Value> = leaves
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
                let mut tree = IncrementalMerkle::default();
                leaves.iter().for_each(|(id, _)| tree.ingest(*id));
                let ret = IMerkleTreeHook::latestCheckpointReturn {
                    root: root.lock().unwrap().unwrap_or(tree.root()),
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
    let fake_root = Arc::new(std::sync::Mutex::new(None));
    let a = mock_rpc(leaves.clone(), fake_root.clone()).await;
    let c = mock_rpc(leaves.clone(), fake_root.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
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
    let mut sync = DomainSync::new(origin, &config, &store).unwrap();

    let Tick::Batch(batch) = sync.tick().await.unwrap() else {
        panic!("batch")
    };
    assert!(sync.chunk <= 50, "range errors shrink the chunk");
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

    *fake_root.lock().unwrap() = Some(b(0xee));
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
    let store = Store::open(dir.path()).unwrap();
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
    let mut sync = DomainSync::new(origin, &config, &store).unwrap();
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
