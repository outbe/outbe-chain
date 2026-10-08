use super::*;

/// The one `E` event `emitter` logged in `receipt`.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn receipt_event<E: alloy_sol_types::SolEvent>(
    receipt: &serde_json::Value,
    emitter: Address,
) -> E {
    let mut found = receipt["logs"]
        .as_array()
        .expect("receipt logs")
        .iter()
        .filter_map(|log| {
            if log["address"].as_str()?.parse::<Address>().ok()? != emitter {
                return None;
            }
            let topics = log["topics"]
                .as_array()?
                .iter()
                .map(|t| t.as_str()?.parse::<B256>().ok())
                .collect::<Option<Vec<_>>>()?;
            if topics.first() != Some(&E::SIGNATURE_HASH) {
                return None;
            }
            let data = log["data"].as_str()?.parse::<Bytes>().ok()?;
            Some(E::decode_raw_log(topics, &data).expect("decode matching event"))
        });
    let value = found.next().expect("expected event missing");
    assert!(found.next().is_none(), "duplicate {} event", E::SIGNATURE);
    value
}

/// Receipt success flag for `tx`, or `None` if not yet mined / unreadable.
pub(crate) fn receipt_success(url: &str, tx: &str) -> Option<bool> {
    let url = url.to_string();
    let hash: TxHash = tx.parse().ok()?;
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let receipt = provider.get_transaction_receipt(hash).await.ok()??;
        Some(receipt.status())
    })
}

/// Public JSON-RPC representation of a mined receipt. Lifecycle accounting uses
/// this to prove the exact gas charge paid by a claimant.
pub(crate) fn receipt_json(url: &str, tx: &str) -> Option<serde_json::Value> {
    raw_json_with_params(url, "eth_getTransactionReceipt", serde_json::json!([tx]))
}
