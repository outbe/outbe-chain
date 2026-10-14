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

pub(super) struct JoinRelayContext<'a> {
    pub(super) private_key_hex: &'a str,
    pub(super) evm_signer: &'a crate::tx::TxSigner,
    pub(super) rpc_chain_id: u64,
    pub(super) node_data_dir: Option<&'a std::path::Path>,
    pub(super) enclave_socket: &'a str,
    pub(super) call: Vec<u8>,
    pub(super) gas_limit: u64,
    pub(super) resumes_finalized_target: bool,
    pub(super) offer_key_state: super::JoinOfferKeyState,
    pub(super) expected_offer_pub: [u8; 32],
    pub(super) finalized: &'a FinalizedRegistryChainViewV1,
    pub(super) node_id_hash: B256,
    pub(super) enclave_id: B256,
    pub(super) intent_hash: B256,
    pub(super) is_committed_durable: bool,
}

pub(super) async fn submit_or_resume_join_tx(
    client: &(impl Rpc + Sync),
    enclave: &mut Option<super::JoinEnclave>,
    ctx: JoinRelayContext<'_>,
) -> Result<Option<String>> {
    let calldata_hash = keccak256(&ctx.call);
    if enclave
        .as_ref()
        .is_some_and(super::JoinEnclave::is_candidate)
    {
        let tx_hash = submit_candidate_join_tx(client, &ctx, calldata_hash).await?;
        return Ok(Some(tx_hash));
    }
    if ctx.is_committed_durable {
        return submit_committed_join_tx(client, enclave, &ctx, calldata_hash).await;
    }
    let tx_hash = ctx
        .evm_signer
        .send_tx_with_gas(
            client,
            crate::abi::TEE_REGISTRY_ADDR,
            ctx.call,
            alloy_primitives::U256::ZERO,
            ctx.gas_limit,
        )
        .await
        .wrap_err("V1 registerEnclave submission failed")?;
    Ok(Some(tx_hash))
}

async fn sign_join_relay_tx(
    client: &(impl Rpc + Sync),
    ctx: &JoinRelayContext<'_>,
) -> Result<outbe_operator::tx::RawRelayTransactionV1> {
    use alloy_primitives::U256;
    use outbe_operator::tx::{buffered_gas_price, RelaySignerV1, UnsignedRelayTransactionV1};

    let relay_signer = RelaySignerV1::new(ctx.private_key_hex)?;
    if relay_signer.address() != ctx.evm_signer.address() {
        eyre::bail!("global EVM signer address is inconsistent");
    }
    let account_nonce = client
        .eth_get_transaction_count(ctx.evm_signer.address())
        .await?;
    let gas_price = buffered_gas_price(client.eth_gas_price().await?);
    let required_balance = gas_price.saturating_mul(U256::from(ctx.gas_limit));
    let balance = client.eth_get_balance(ctx.evm_signer.address()).await?;
    if balance < required_balance {
        eyre::bail!(
            "TEE join EVM signer {} has {balance} but needs at least {required_balance}",
            ctx.evm_signer.address()
        );
    }
    relay_signer.sign_renewal(UnsignedRelayTransactionV1 {
        chain_id: ctx.rpc_chain_id,
        account_nonce,
        gas_price,
        gas_limit: ctx.gas_limit,
        to: crate::abi::TEE_REGISTRY_ADDR,
        calldata: &ctx.call,
    })
}

async fn submit_candidate_join_tx(
    client: &(impl Rpc + Sync),
    ctx: &JoinRelayContext<'_>,
    calldata_hash: B256,
) -> Result<String> {
    let node_data_dir = ctx
        .node_data_dir
        .expect("candidate join requires NodeHost state");
    let relay = if let Some(durable) = outbe_tee::load_replacement_candidate_relay(node_data_dir)
        .map_err(|error| eyre::eyre!("load durable candidate relay: {error}"))?
    {
        if durable.calldata_hash() != calldata_hash {
            eyre::bail!("durable candidate transaction targets different calldata");
        }
        durable
    } else {
        if ctx.resumes_finalized_target {
            eyre::bail!(
                "finalized candidate binding has no durable pre-relay transaction checkpoint"
            );
        }
        let raw = sign_join_relay_tx(client, ctx).await?;
        outbe_tee::persist_replacement_candidate_relay(
            node_data_dir,
            calldata_hash,
            &raw.raw_transaction,
        )
        .map_err(|error| eyre::eyre!("persist exact candidate transaction: {error}"))?
    };
    let exact = ExactJoinRelayV1 {
        transaction_hash: relay.transaction_hash(),
        raw_transaction: relay.raw_transaction().to_vec(),
    };
    relay_exact_join_transaction(client, &exact, ctx.resumes_finalized_target).await
}

async fn submit_committed_join_tx(
    client: &(impl Rpc + Sync),
    enclave: &mut Option<super::JoinEnclave>,
    ctx: &JoinRelayContext<'_>,
    calldata_hash: B256,
) -> Result<Option<String>> {
    let node_data_dir = ctx
        .node_data_dir
        .expect("committed join requires NodeHost state");
    let relay = if let Some(durable) = outbe_tee::load_committed_join_relay(node_data_dir)
        .map_err(|error| eyre::eyre!("load durable committed relay: {error}"))?
    {
        if durable.calldata_hash() != calldata_hash {
            eyre::bail!("durable committed transaction targets different calldata");
        }
        durable
    } else {
        match super::plan_missing_committed_relay(
            ctx.resumes_finalized_target,
            ctx.offer_key_state,
        )? {
            super::MissingCommittedRelayPlan::CleanupReadyExact => {
                let _ = enclave.take();
                cleanup_completed_committed_join(ctx, node_data_dir)?;
                return Ok(None);
            }
            super::MissingCommittedRelayPlan::ConstructAndPersist => {}
        }
        let raw = sign_join_relay_tx(client, ctx).await?;
        let from_block = client.eth_block_number().await?;
        outbe_tee::persist_committed_join_relay(
            node_data_dir,
            calldata_hash,
            from_block,
            &raw.raw_transaction,
        )
        .map_err(|error| eyre::eyre!("persist exact committed transaction: {error}"))?
    };
    let exact = ExactJoinRelayV1 {
        transaction_hash: relay.transaction_hash(),
        raw_transaction: relay.raw_transaction().to_vec(),
    };
    let tx_hash =
        relay_exact_join_transaction(client, &exact, ctx.resumes_finalized_target).await?;
    Ok(Some(tx_hash))
}

fn cleanup_completed_committed_join(
    ctx: &JoinRelayContext<'_>,
    node_data_dir: &std::path::Path,
) -> Result<()> {
    use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

    let mut reopened =
        outbe_tee::connect_committed_node_host_enclave(ctx.enclave_socket, node_data_dir)
            .map_err(|error| eyre::eyre!("reopen completed committed enclave: {error}"))?;
    match reopened.request(&EnclaveRequest::GetPublicKeys)? {
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub,
            ..
        } if recipient_x25519_pub == ctx.expected_offer_pub => {}
        other => {
            eyre::bail!("cleanup recovery did not reopen the exact permanent offer key: {other:?}");
        }
    }
    super::persist_authorized_join_admission_anchor_v1(
        node_data_dir,
        ctx.finalized,
        ctx.node_id_hash,
        ctx.enclave_id,
        ctx.intent_hash,
    )?;
    outbe_tee::clear_committed_join_checkpoint(node_data_dir, ctx.intent_hash)
        .map_err(|error| eyre::eyre!("clear recovered committed join checkpoint: {error}"))?;
    println!(
        "[ok] finalized tee join cleanup recovered without another transaction or onboarding ingest"
    );
    Ok(())
}
