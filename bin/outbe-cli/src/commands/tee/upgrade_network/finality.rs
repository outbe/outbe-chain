//! Exact saved transaction replay and canonical prepare finality.
use super::*;

pub(super) async fn wait_for_finalized_prepare(
    rpc: &(impl Rpc + Sync),
    relay: &RelaySignerV1,
    durable: &NetworkUpgradeSubmissionV1,
    timeout: Duration,
) -> Result<(DcapOnboardingContextV1, u64)> {
    let context = decode_saved_context(relay, durable)?;
    let expected = context.context_hash();
    let started = Instant::now();
    let mut sent = false;
    let finalized_height = loop {
        let block = rpc.eth_get_finalized_block().await?;
        let height = json_hex_u64_field(&block, "number")?;
        let now = json_hex_u64_field(&block, "timestamp")?;
        let pending = pending_at(rpc, context.node_id_hash, &format!("0x{height:x}")).await?;
        if pending.contextHash == expected && pending.validUntil > now {
            break height;
        }
        if started.elapsed() >= timeout {
            eyre::bail!(
                "prepare not finalized; saved transaction retained, rerun upgrade-provision"
            );
        }
        if !sent {
            relay_saved_prepare(rpc, durable).await?;
            sent = true;
        }
        require_successful_prepare_receipt(rpc, durable.transaction.transaction_hash).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    Ok((context, finalized_height))
}

async fn relay_saved_prepare(
    rpc: &(impl Rpc + Sync),
    durable: &NetworkUpgradeSubmissionV1,
) -> Result<()> {
    match rpc
        .eth_send_raw_transaction(&durable.transaction.raw_transaction)
        .await
    {
        Ok(hash) if hash.parse::<B256>()? == durable.transaction.transaction_hash => {}
        Ok(_) => eyre::bail!("RPC returned a different prepare transaction hash"),
        Err(e) if e.to_string().to_ascii_lowercase().contains("already known") => {}
        Err(e) => return Err(e).wrap_err("send saved prepare transaction"),
    }
    Ok(())
}

async fn require_successful_prepare_receipt(
    rpc: &(impl Rpc + Sync),
    transaction_hash: B256,
) -> Result<()> {
    if let Some(receipt) = rpc
        .eth_get_transaction_receipt(&format!("{:#x}", transaction_hash))
        .await?
    {
        if json_hex_u64_field(&receipt, "status")? == 0 {
            eyre::bail!("prepare transaction reverted; saved evidence retained for diagnosis");
        }
    }
    Ok(())
}

fn decode_saved_context(
    relay: &RelaySignerV1,
    durable: &NetworkUpgradeSubmissionV1,
) -> Result<DcapOnboardingContextV1> {
    if durable.transaction.relay != relay.address()
        || keccak256(&durable.transaction.raw_transaction) != durable.transaction.transaction_hash
    {
        eyre::bail!("saved prepare transaction does not match this relay");
    }
    let context = DcapOnboardingContextV1::decode_canonical(&durable.context)
        .map_err(|e| eyre::eyre!("saved context: {e:?}"))?;
    Ok(context)
}
