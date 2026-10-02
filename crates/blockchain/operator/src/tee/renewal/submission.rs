//! Private renewal submission stage.
use super::*;

pub(super) struct RenewalSubmission {
    pub(super) attempt: PreparedRenewalV1,
    pub(super) finalized_height: u64,
    pub(super) replayed: bool,
}

pub(super) async fn submit_attempt(
    rpc: &(impl RenewalRpc + Sync),
    journal: &RenewalJournalGuard,
    submission: RenewalSubmission,
) -> Result<RenewalOutcomeV1> {
    let RenewalSubmission {
        attempt,
        finalized_height,
        replayed,
    } = submission;
    let raw = attempt
        .relay_variants
        .last()
        .ok_or_else(|| eyre::eyre!("renewal attempt has no relay transaction"))?;
    let already_observed = replayed && exact_transaction_receipt_exists(rpc, raw).await?;
    let returned_hash = if already_observed {
        raw.transaction_hash
    } else {
        send_exact_transaction(rpc, raw).await?
    };
    if returned_hash != raw.transaction_hash {
        eyre::bail!("RPC returned a transaction hash different from the signed renewal bytes");
    }
    let hashes = attempt
        .relay_variants
        .iter()
        .map(|variant| variant.transaction_hash)
        .collect();
    journal.store(RenewalJournalSnapshotV1::new(
        RenewalJournalStateV1::Submitted {
            attempt,
            submitted_at_finalized_height: finalized_height,
            transaction_hashes: hashes,
        },
    ))?;
    Ok(RenewalOutcomeV1::Submitted {
        transaction_hash: returned_hash,
        replayed,
    })
}

pub(super) async fn send_exact_transaction(
    rpc: &(impl RenewalRpc + Sync),
    raw: &crate::tx::RawRelayTransactionV1,
) -> Result<B256> {
    Ok(match rpc.send_raw_transaction(&raw.raw_transaction).await {
        Ok(returned) => returned
            .parse::<B256>()
            .wrap_err("parse eth_sendRawTransaction renewal hash")?,
        Err(error) if transaction_is_already_known(&error) => raw.transaction_hash,
        Err(error) if transaction_nonce_is_too_low(&error) => {
            if exact_transaction_receipt_exists(rpc, raw).await? {
                raw.transaction_hash
            } else {
                return Err(error)
                    .wrap_err("renewal nonce was consumed without the exact transaction receipt");
            }
        }
        Err(error) => return Err(error).wrap_err("submit exact renewal transaction"),
    })
}

pub(super) async fn exact_transaction_receipt_exists(
    rpc: &(impl RenewalRpc + Sync),
    raw: &crate::tx::RawRelayTransactionV1,
) -> Result<bool> {
    let expected_hash = format!("{:#x}", raw.transaction_hash);
    let Some(receipt) = rpc.transaction_receipt(&expected_hash).await? else {
        return Ok(false);
    };
    let observed_hash = receipt
        .get("transactionHash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("exact renewal transaction receipt has no transactionHash"))?;
    eyre::ensure!(
        observed_hash.eq_ignore_ascii_case(&expected_hash),
        "renewal RPC returned a receipt for a different transaction hash"
    );
    let status = receipt
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("exact renewal transaction receipt has no status"))?;
    eyre::ensure!(
        status == "0x1",
        "exact renewal transaction receipt has non-success status {status}"
    );
    Ok(true)
}

pub(super) fn transaction_is_already_known(error: &eyre::Report) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("already known") || message.contains("known transaction")
}

pub(super) fn transaction_nonce_is_too_low(error: &eyre::Report) -> bool {
    format!("{error:#}")
        .to_ascii_lowercase()
        .contains("nonce too low")
}
