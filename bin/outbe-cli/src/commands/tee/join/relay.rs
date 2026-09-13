use super::super::CliFinalityRpc;
use super::finalized_binding_matches_intent;

use super::ExactJoinRelayV1;

use crate::rpc::Rpc;

use alloy_primitives::keccak256;

use alloy_primitives::B256;

use eyre::Result;
use eyre::WrapErr;

use outbe_operator::tee::read_finalized_registry_view_v1;

use outbe_operator::tee::FinalizedRegistryChainViewV1;
use outbe_operator::tee::NodeBindingSelectorV1;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use std::time::Duration;

pub(in super::super) async fn relay_exact_join_transaction(
    client: &(impl Rpc + Sync),
    relay: &ExactJoinRelayV1,
    resumes_finalized_target: bool,
) -> Result<String> {
    if keccak256(&relay.raw_transaction) != relay.transaction_hash {
        eyre::bail!("durable join relay transaction hash mismatch");
    }
    let encoded_hash = format!("0x{}", hex::encode(relay.transaction_hash));
    if resumes_finalized_target {
        return Ok(encoded_hash);
    }
    let returned = match client
        .eth_send_raw_transaction(&relay.raw_transaction)
        .await
    {
        Ok(returned) => returned,
        Err(error) => {
            let message = format!("{error:#}").to_ascii_lowercase();
            if message.contains("already known") || message.contains("known transaction") {
                return Ok(encoded_hash);
            }
            if message.contains("nonce too low") {
                let exact_receipt_exists = client
                    .eth_get_transaction_receipt(&encoded_hash)
                    .await?
                    .and_then(|receipt| {
                        receipt
                            .get("transactionHash")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&encoded_hash));
                if exact_receipt_exists {
                    return Ok(encoded_hash);
                }
            }
            return Err(error).wrap_err("relay exact durable registerEnclave transaction");
        }
    };
    let returned_hash = returned
        .parse::<B256>()
        .wrap_err("parse durable registerEnclave transaction hash")?;
    if returned_hash != relay.transaction_hash {
        eyre::bail!("RPC returned a different durable join transaction hash");
    }
    Ok(encoded_hash)
}

pub(in super::super) async fn await_finalized_join_target(
    client: &(impl Rpc + Sync),
    selector: &NodeBindingSelectorV1,
    intent: &RegistrationIntentV1,
    timeout: Duration,
) -> Result<FinalizedRegistryChainViewV1> {
    let started = tokio::time::Instant::now();
    loop {
        let view = read_finalized_registry_view_v1(&CliFinalityRpc(client), selector).await?;
        match view.binding.as_ref() {
            Some(binding) if finalized_binding_matches_intent(binding, intent)? => return Ok(view),
            Some(binding) if view.schedule.finalized_timestamp < binding.valid_until => {
                eyre::bail!("finalized Registry contains a competing live join binding");
            }
            _ => {}
        }
        if started.elapsed() >= timeout {
            eyre::bail!(
                "timed out after {}s waiting for exact finalized tee join binding",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
