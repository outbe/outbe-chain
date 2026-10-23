use super::*;
use alloy_primitives::address;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

async fn serve_rpc_once(
    status: &str,
    response_body: impl Into<String>,
) -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let status = status.to_string();
    let response_body = response_body.into();
    let (body_tx, body_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_http_request_body(&mut socket).await;
        let _ = body_tx.send(body);

        write_http_response(&mut socket, &status, &response_body).await;
    });

    (format!("http://{addr}"), body_rx)
}

async fn serve_rpc_sequence(
    response_bodies: impl IntoIterator<Item = impl Into<String>>,
) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let response_bodies: Vec<String> = response_bodies.into_iter().map(Into::into).collect();
    let (body_tx, body_rx) = mpsc::channel(response_bodies.len().max(1));

    tokio::spawn(async move {
        for response_body in response_bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = read_http_request_body(&mut socket).await;
            let _ = body_tx.send(body).await;

            write_http_response(&mut socket, "200 OK", &response_body).await;
        }
    });

    (format!("http://{addr}"), body_rx)
}

async fn read_http_request_body(socket: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];

    loop {
        let n = socket.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);

        if let Some(header_end) = find_header_end(&request) {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_len = content_length(&headers);
            if request.len() >= header_end + 4 + content_len {
                break;
            }
        }
    }

    let body_start = find_header_end(&request)
        .map(|pos| pos + 4)
        .unwrap_or(request.len());
    String::from_utf8_lossy(&request[body_start..]).to_string()
}

async fn write_http_response(socket: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).await.unwrap();
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

// --- transport-level RpcClient tests ---

#[tokio::test]
async fn test_rpc_client_eth_call_sends_exact_body_and_decodes_bytes() {
    let (url, body_rx) =
        serve_rpc_once("200 OK", r#"{"jsonrpc":"2.0","result":"0xaabb","id":1}"#).await;
    let client = RpcClient::new(&url).expect("rpc client");
    let to = address!("0x000000000000000000000000000000000000EE02");

    let result = client.eth_call(to, &[0x12, 0x34]).await.unwrap();

    assert_eq!(result, vec![0xaa, 0xbb]);
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [
                {
                    "to": format!("{to:?}"),
                    "data": "0x1234",
                },
                "latest",
            ],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_estimate_gas_sends_exact_body() {
    let (url, body_rx) =
        serve_rpc_once("200 OK", r#"{"jsonrpc":"2.0","result":"0x5208","id":1}"#).await;
    let client = RpcClient::new(&url).expect("rpc client");
    let from = address!("0x1111111111111111111111111111111111111111");
    let to = address!("0x000000000000000000000000000000000000EE02");

    let value = U256::from(7u64);
    let gas = client
        .eth_estimate_gas(from, to, &[0xde, 0xad], value)
        .await
        .unwrap();

    assert_eq!(gas, 21_000);
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_estimateGas",
            "params": [{
                "from": format!("{from:?}"),
                "to": format!("{to:?}"),
                "data": "0xdead",
                "value": "0x7",
            }],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_send_raw_transaction_sends_hex_body() {
    let (url, body_rx) =
        serve_rpc_once("200 OK", r#"{"jsonrpc":"2.0","result":"0xfeed","id":1}"#).await;
    let client = RpcClient::new(&url).expect("rpc client");

    let tx_hash = client
        .eth_send_raw_transaction(&[0xde, 0xad, 0xbe, 0xef])
        .await
        .unwrap();

    assert_eq!(tx_hash, "0xfeed");
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_sendRawTransaction",
            "params": ["0xdeadbeef"],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_send_raw_transaction_non_string_errors() {
    let (url, _body_rx) =
        serve_rpc_once("200 OK", r#"{"jsonrpc":"2.0","result":123,"id":1}"#).await;
    let client = RpcClient::new(&url).expect("rpc client");

    let err = client
        .eth_send_raw_transaction(&[0xde, 0xad])
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("expected tx hash string"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_rpc_client_get_balance_sends_exact_body_and_decodes_u256() {
    let (url, body_rx) =
        serve_rpc_once("200 OK", r#"{"jsonrpc":"2.0","result":"0xf4240","id":1}"#).await;
    let client = RpcClient::new(&url).expect("rpc client");
    let address = address!("0x1111111111111111111111111111111111111111");

    let balance = client.eth_get_balance(address).await.unwrap();

    assert_eq!(balance, U256::from(1_000_000u64));
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_getBalance",
            "params": [format!("{address:?}"), "latest"],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_get_block_by_number_sends_hex_block_and_false() {
    let result = json!({"number": "0x2a", "hash": "0xabc"});
    let response = json!({"jsonrpc": "2.0", "result": result, "id": 1}).to_string();
    let (url, body_rx) = serve_rpc_once("200 OK", response).await;
    let client = RpcClient::new(&url).expect("rpc client");

    let block = client.eth_get_block_by_number(42).await.unwrap();

    assert_eq!(block, json!({"number": "0x2a", "hash": "0xabc"}));
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": ["0x2a", false],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_get_latest_block_sends_latest_and_false() {
    let result = json!({"number": "0x2b", "hash": "0xdef"});
    let response = json!({"jsonrpc": "2.0", "result": result, "id": 1}).to_string();
    let (url, body_rx) = serve_rpc_once("200 OK", response).await;
    let client = RpcClient::new(&url).expect("rpc client");

    let block = client.eth_get_latest_block().await.unwrap();

    assert_eq!(block, json!({"number": "0x2b", "hash": "0xdef"}));
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": ["latest", false],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_get_vrf_seed_sends_null_block_param() {
    let result = json!({"seed": "0x1234"});
    let response = json!({"jsonrpc": "2.0", "result": result, "id": 1}).to_string();
    let (url, body_rx) = serve_rpc_once("200 OK", response).await;
    let client = RpcClient::new(&url).expect("rpc client");

    let seed = client.outbe_get_vrf_seed().await.unwrap();

    assert_eq!(seed, json!({"seed": "0x1234"}));
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "outbe_getVrfSeed",
            "params": [null],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_get_vrf_seed_at_sends_numeric_block_param() {
    let result = json!({"seed": "0xabcd"});
    let response = json!({"jsonrpc": "2.0", "result": result, "id": 1}).to_string();
    let (url, body_rx) = serve_rpc_once("200 OK", response).await;
    let client = RpcClient::new(&url).expect("rpc client");

    let seed = client.outbe_get_vrf_seed_at(99).await.unwrap();

    assert_eq!(seed, json!({"seed": "0xabcd"}));
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "outbe_getVrfSeed",
            "params": [99],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_get_logs_sends_exact_filter_and_decodes_array() {
    let result = json!([
        {
            "address": "0x0000000000000000000000000000000000001006",
            "topics": ["0xaaa"],
            "data": "0x",
        }
    ]);
    let response = json!({"jsonrpc": "2.0", "result": result, "id": 1}).to_string();
    let (url, body_rx) = serve_rpc_once("200 OK", response).await;
    let client = RpcClient::new(&url).expect("rpc client");
    let address = address!("0x0000000000000000000000000000000000001006");
    let topics = [Some("0xaaa".to_string()), None, Some("0xccc".to_string())];

    let logs = client
        .eth_get_logs(address, &topics, "0x10", "latest")
        .await
        .unwrap();

    assert_eq!(
        logs,
        vec![json!({
            "address": "0x0000000000000000000000000000000000001006",
            "topics": ["0xaaa"],
            "data": "0x",
        })]
    );
    let body: Value = serde_json::from_str(&body_rx.await.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_getLogs",
            "params": [{
                "address": format!("{address:?}"),
                "topics": ["0xaaa", null, "0xccc"],
                "fromBlock": "0x10",
                "toBlock": "latest",
            }],
            "id": 1,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_get_logs_non_array_errors() {
    let (url, _body_rx) =
        serve_rpc_once("200 OK", r#"{"jsonrpc":"2.0","result":{"log":1},"id":1}"#).await;
    let client = RpcClient::new(&url).expect("rpc client");
    let address = address!("0x0000000000000000000000000000000000001006");

    let err = client
        .eth_get_logs(address, &[], "earliest", "latest")
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("expected array of logs"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_rpc_client_ids_increment_across_calls() {
    let (url, mut body_rx) = serve_rpc_sequence([
        r#"{"jsonrpc":"2.0","result":"0x1","id":1}"#,
        r#"{"jsonrpc":"2.0","result":"0x2","id":2}"#,
        r#"{"jsonrpc":"2.0","result":"0x3","id":3}"#,
    ])
    .await;
    let client = RpcClient::new(&url).expect("rpc client");

    assert_eq!(client.eth_block_number().await.unwrap(), 1);
    assert_eq!(client.net_peer_count().await.unwrap(), 2);
    assert_eq!(client.eth_chain_id().await.unwrap(), 3);

    let first: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();
    let second: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();
    let third: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();

    assert_eq!(
        first,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1,
        })
    );
    assert_eq!(
        second,
        json!({
            "jsonrpc": "2.0",
            "method": "net_peerCount",
            "params": [],
            "id": 2,
        })
    );
    assert_eq!(
        third,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_chainId",
            "params": [],
            "id": 3,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_quantity_wrappers_send_exact_bodies_and_decode() {
    let (url, mut body_rx) = serve_rpc_sequence([
        r#"{"jsonrpc":"2.0","result":"0x7","id":1}"#,
        r#"{"jsonrpc":"2.0","result":"0x7","id":2}"#,
    ])
    .await;
    let client = RpcClient::new(&url).expect("rpc client");
    let address = address!("0x1111111111111111111111111111111111111111");

    assert_eq!(
        client.eth_gas_price().await.unwrap(),
        U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE)
    );
    assert_eq!(client.eth_get_transaction_count(address).await.unwrap(), 7);

    let first: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();
    let second: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();

    assert_eq!(
        first,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_gasPrice",
            "params": [],
            "id": 1,
        })
    );
    assert_eq!(
        second,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_getTransactionCount",
            "params": [format!("{address:?}"), "pending"],
            "id": 2,
        })
    );
}

#[tokio::test]
async fn test_rpc_client_outbe_no_param_wrappers_send_exact_bodies() {
    let consensus = json!({"mode": "validator"});
    let epoch = json!({"epoch": 3});
    let emission = json!({"rate": "0x1"});
    let slash = json!({"slashAmountPercent": 5});
    let (url, mut body_rx) = serve_rpc_sequence([
        json!({"jsonrpc": "2.0", "result": consensus, "id": 1}).to_string(),
        json!({"jsonrpc": "2.0", "result": epoch, "id": 2}).to_string(),
        json!({"jsonrpc": "2.0", "result": emission, "id": 3}).to_string(),
        json!({"jsonrpc": "2.0", "result": slash, "id": 4}).to_string(),
    ])
    .await;
    let client = RpcClient::new(&url).expect("rpc client");

    assert_eq!(
        client.outbe_consensus_status().await.unwrap(),
        json!({"mode": "validator"})
    );
    assert_eq!(
        client.outbe_get_epoch_info().await.unwrap(),
        json!({"epoch": 3})
    );
    assert_eq!(
        client.outbe_get_emission_info().await.unwrap(),
        json!({"rate": "0x1"})
    );
    assert_eq!(
        client.outbe_get_slash_config().await.unwrap(),
        json!({"slashAmountPercent": 5})
    );

    let first: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();
    let second: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();
    let third: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();
    let fourth: Value = serde_json::from_str(&body_rx.recv().await.unwrap()).unwrap();

    assert_eq!(
        first,
        json!({
            "jsonrpc": "2.0",
            "method": "outbe_consensusStatus",
            "params": [],
            "id": 1,
        })
    );
    assert_eq!(
        second,
        json!({
            "jsonrpc": "2.0",
            "method": "outbe_getEpochInfo",
            "params": [],
            "id": 2,
        })
    );
    assert_eq!(
        third,
        json!({
            "jsonrpc": "2.0",
            "method": "outbe_getEmissionInfo",
            "params": [],
            "id": 3,
        })
    );
    assert_eq!(
        fourth,
        json!({
            "jsonrpc": "2.0",
            "method": "outbe_getSlashConfig",
            "params": [],
            "id": 4,
        })
    );
}

async fn assert_block_number_rpc_error(status: &str, response_body: &str, expected_msg: &str) {
    let (url, _body_rx) = serve_rpc_once(status, response_body).await;
    let client = RpcClient::new(&url).expect("rpc client");
    let err = client.eth_block_number().await.unwrap_err();
    assert!(
        err.to_string().contains(expected_msg),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_rpc_client_json_rpc_error_returns_error() {
    assert_block_number_rpc_error(
        "200 OK",
        r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"missing method"},"id":1}"#,
        "RPC error -32601: missing method",
    )
    .await;
}

#[tokio::test]
async fn test_rpc_client_missing_result_errors() {
    assert_block_number_rpc_error("200 OK", r#"{"jsonrpc":"2.0","id":1}"#, "empty RPC result")
        .await;
}

#[tokio::test]
async fn test_rpc_client_invalid_json_errors() {
    assert_block_number_rpc_error("200 OK", "not json", "failed to parse RPC response").await;
}

#[tokio::test]
async fn test_rpc_client_http_status_error() {
    assert_block_number_rpc_error(
        "500 Internal Server Error",
        r#"{"jsonrpc":"2.0","result":"0x1","id":1}"#,
        "RPC HTTP request failed",
    )
    .await;
}

/// Loopback peer that accepts one TCP connection and writes nothing.
/// `https` stalls inside the connect timeout (TLS handshake). `http` stalls
/// on the response-header wait.
async fn silent_loopback(scheme: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((socket, _)) = listener.accept().await {
            let _hold = socket;
            std::future::pending::<()>().await;
        }
    });
    format!("{scheme}://{addr}")
}

async fn assert_block_number_fails_between(
    url: &str,
    ceiling_secs: u64,
    floor_secs: u64,
    label: &str,
) {
    let client = RpcClient::new(url).expect("rpc client");
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(ceiling_secs),
        client.eth_block_number(),
    )
    .await;
    assert!(result.is_ok(), "{label} exceeded {ceiling_secs}s");
    assert!(result.unwrap().is_err());
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(floor_secs),
        "{elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(ceiling_secs),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn stalled_response_returns_inside_the_request_timeout() {
    let url = silent_loopback("http").await;
    assert_block_number_fails_between(&url, 45, 25, "stalled response headers").await;
}

#[tokio::test]
async fn stalled_connect_returns_inside_the_connect_timeout() {
    let url = silent_loopback("https").await;
    assert_block_number_fails_between(&url, 15, 8, "stalled TLS handshake").await;
}

#[tokio::test]
async fn stalled_partial_body_returns_inside_the_request_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let _ = socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 200\r\n\r\n{\"incomplete\":",
            )
            .await;
        let _ = socket.flush().await;
        std::future::pending::<()>().await;
    });
    assert_block_number_fails_between(
        &format!("http://{addr}"),
        45,
        25,
        "stalled partial response body",
    )
    .await;
}

#[tokio::test]
async fn test_rpc_client_response_id_mismatch_errors() {
    assert_block_number_rpc_error(
        "200 OK",
        r#"{"jsonrpc":"2.0","result":"0x1","id":99}"#,
        "RPC response id mismatch",
    )
    .await;
}

// --- parse_hex_u64 ---

#[test]
fn test_parse_hex_u64_with_prefix() {
    assert_eq!(RpcClient::parse_hex_u64(&json!("0x1a")).unwrap(), 26);
}

#[test]
fn test_parse_hex_u64_without_prefix() {
    assert_eq!(RpcClient::parse_hex_u64(&json!("1a")).unwrap(), 26);
}

#[test]
fn test_parse_hex_u64_zero() {
    assert_eq!(RpcClient::parse_hex_u64(&json!("0x0")).unwrap(), 0);
}

#[test]
fn test_parse_hex_u64_max() {
    assert_eq!(
        RpcClient::parse_hex_u64(&json!("0xffffffffffffffff")).unwrap(),
        u64::MAX
    );
}

#[test]
fn test_parse_hex_u64_not_string() {
    assert!(RpcClient::parse_hex_u64(&json!(42)).is_err());
}

#[test]
fn test_parse_hex_u64_invalid_hex() {
    assert!(RpcClient::parse_hex_u64(&json!("0xGG")).is_err());
}

#[test]
fn test_parse_hex_u64_overflow() {
    assert!(RpcClient::parse_hex_u64(&json!("0x10000000000000000")).is_err());
}

// --- parse_hex_bytes ---

#[test]
fn test_parse_hex_bytes_with_prefix() {
    assert_eq!(
        RpcClient::parse_hex_bytes(&json!("0xdeadbeef")).unwrap(),
        vec![0xde, 0xad, 0xbe, 0xef]
    );
}

#[test]
fn test_parse_hex_bytes_without_prefix() {
    assert_eq!(
        RpcClient::parse_hex_bytes(&json!("deadbeef")).unwrap(),
        vec![0xde, 0xad, 0xbe, 0xef]
    );
}

#[test]
fn test_parse_hex_bytes_empty() {
    assert_eq!(
        RpcClient::parse_hex_bytes(&json!("0x")).unwrap(),
        Vec::<u8>::new()
    );
}

#[test]
fn test_parse_hex_bytes_single_byte() {
    assert_eq!(
        RpcClient::parse_hex_bytes(&json!("0xff")).unwrap(),
        vec![0xff]
    );
}

#[test]
fn test_parse_hex_bytes_odd_length() {
    assert!(RpcClient::parse_hex_bytes(&json!("0xabc")).is_err());
}

#[test]
fn test_parse_hex_bytes_not_string() {
    assert!(RpcClient::parse_hex_bytes(&json!(123)).is_err());
}

#[test]
fn test_parse_hex_bytes_invalid_chars() {
    assert!(RpcClient::parse_hex_bytes(&json!("0xZZZZ")).is_err());
}

// --- parse_hex_u256 ---

#[test]
fn test_parse_hex_u256_with_prefix() {
    assert_eq!(
        RpcClient::parse_hex_u256(&json!("0xde0b6b3a7640000")).unwrap(),
        U256::from(1_000_000_000_000_000_000u128)
    );
}

#[test]
fn test_parse_hex_u256_zero() {
    assert_eq!(
        RpcClient::parse_hex_u256(&json!("0x0")).unwrap(),
        U256::ZERO
    );
}

#[test]
fn test_parse_hex_u256_without_prefix() {
    assert_eq!(
        RpcClient::parse_hex_u256(&json!("ff")).unwrap(),
        U256::from(255u64)
    );
}

#[test]
fn test_parse_hex_u256_max() {
    let hex = format!("0x{}", "f".repeat(64));
    assert_eq!(RpcClient::parse_hex_u256(&json!(hex)).unwrap(), U256::MAX);
}

#[test]
fn test_parse_hex_u256_not_string() {
    assert!(RpcClient::parse_hex_u256(&json!(42)).is_err());
}

#[test]
fn test_parse_hex_u256_invalid_hex() {
    assert!(RpcClient::parse_hex_u256(&json!("0xGHIJ")).is_err());
}
