use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

async fn serve_response(
    method: &str,
    params: Value,
    response: Value,
) -> Result<(HttpRenewalRpc, JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let rpc = HttpRenewalRpc::new(format!("http://{}", listener.local_addr()?));
    let expected = json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params});
    let server = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            let body = loop {
                let count = socket.read(&mut chunk).await?;
                eyre::ensure!(count != 0, "RPC request ended before its body");
                request.extend_from_slice(&chunk[..count]);
                let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                    continue;
                };
                let headers = std::str::from_utf8(&request[..end])?;
                let length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then_some(value.trim())
                }).ok_or_else(|| eyre::eyre!("request has no content length"))?.parse::<usize>()?;
                if request.len() >= end + 4 + length {
                    break serde_json::from_slice::<Value>(&request[end + 4..end + 4 + length])?;
                }
            };
            assert_eq!(body, expected);
            let body = response.to_string();
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await?;
            Ok(())
        }).await?
    });
    Ok((rpc, server))
}

#[tokio::test]
async fn finalized_reads_preserve_chain_identity_and_exact_block_tag() -> Result<()> {
    let (rpc, server) =
        serve_response("eth_chainId", json!([]), json!({"id":1,"result":"0x2a4"})).await?;
    assert_eq!(ChainRpc::chain_id(&rpc).await?, 676);
    server.await??;
    let block = json!({"number":"0x64","timestamp":"0x123"});
    let (rpc, server) = serve_response(
        "eth_getBlockByNumber",
        json!(["finalized", false]),
        json!({"id":1,"result":block}),
    )
    .await?;
    assert_eq!(FinalizedStateRpc::finalized_block(&rpc).await?, block);
    server.await??;
    let address = Address::repeat_byte(0x11);
    let (rpc, server) = serve_response(
        "eth_call",
        json!([{"to":format!("{address:#x}"),"data":"0x0102"},"0x64"]),
        json!({"id":1,"result":"0x0304"}),
    )
    .await?;
    assert_eq!(
        FinalizedStateRpc::call_at(&rpc, address, &[1, 2], "0x64").await?,
        [3, 4]
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn relay_preparation_and_submission_preserve_wire_contracts() -> Result<()> {
    let address = Address::repeat_byte(0x22);
    let (rpc, server) = serve_response(
        "eth_getTransactionCount",
        json!([format!("{address:#x}"), "pending"]),
        json!({"id":1,"result":"0x7"}),
    )
    .await?;
    assert_eq!(
        RelayPreparationRpc::transaction_count(&rpc, address).await?,
        7
    );
    server.await??;
    let (rpc, server) = serve_response(
        "eth_getBalance",
        json!([format!("{address:#x}"), "latest"]),
        json!({"id":1,"result":"0x10000000000000000"}),
    )
    .await?;
    assert_eq!(
        RelayPreparationRpc::balance(&rpc, address).await?,
        U256::from(u64::MAX) + U256::from(1)
    );
    server.await??;
    let (rpc, server) =
        serve_response("eth_gasPrice", json!([]), json!({"id":1,"result":"0x9"})).await?;
    assert_eq!(RelayPreparationRpc::gas_price(&rpc).await?, U256::from(9));
    server.await??;
    let (rpc, server) = serve_response(
        "eth_sendRawTransaction",
        json!(["0xaabb"]),
        json!({"id":1,"result":"0xhash"}),
    )
    .await?;
    assert_eq!(
        RelayRpc::send_raw_transaction(&rpc, &[0xaa, 0xbb]).await?,
        "0xhash"
    );
    server.await??;
    let (rpc, server) = serve_response(
        "eth_getTransactionReceipt",
        json!(["0xhash"]),
        json!({"id":1,"result":null}),
    )
    .await?;
    assert_eq!(
        TransactionReceiptRpc::transaction_receipt(&rpc, "0xhash").await?,
        None
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn adapter_rejects_bad_response_identity_errors_and_invalid_results() -> Result<()> {
    for (response, expected) in [
        (
            json!({"id":2,"result":"0x1"}),
            "eth_chainId RPC response id mismatch",
        ),
        (
            json!({"id":1,"error":{"code":-32000,"message":"unavailable"}}),
            "eth_chainId RPC error:",
        ),
        (json!({"id":1}), "eth_chainId RPC response has no result"),
        (
            json!({"id":1,"result":null}),
            "eth_chainId returned a non-string",
        ),
        (
            json!({"id":1,"result":"0x10000000000000000"}),
            "parse eth_chainId result",
        ),
    ] {
        let (rpc, server) = serve_response("eth_chainId", json!([]), response).await?;
        assert!(ChainRpc::chain_id(&rpc)
            .await
            .unwrap_err()
            .to_string()
            .starts_with(expected));
        server.await??;
    }
    let (rpc, server) = serve_response("outbe_teeRenewalScheduleV1",json!([]),json!({"id":1,"result":{
        "finalizedHeight":120,"finalizedHash":format!("0x{}","01".repeat(32)),"finalizedTimestamp":1000,
        "epochNumber":2,"epochStartHeight":100,"epochLengthBlocks":100,"nextFreezeHeight":181,
        "plannedActivationHeight":200,"dkgPrepareWindowBlocks":20,"minimumBlockTimeMillis":2000
    }})).await?;
    assert_eq!(
        RegistryRpc::tee_renewal_schedule_v1(&rpc)
            .await
            .unwrap_err()
            .to_string(),
        "renewal schedule freeze height is inconsistent"
    );
    server.await??;
    Ok(())
}
