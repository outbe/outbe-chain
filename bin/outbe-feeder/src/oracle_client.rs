//! Oracle client for interacting with the chain via alloy provider + signer.
//!
//! Uses alloy's `ProviderBuilder` with a local ECDSA wallet for
//! production-like transaction signing and submission.

use alloy_eips::{eip1559::MIN_PROTOCOL_BASE_FEE, BlockId, BlockNumberOrTag};
use alloy_network::{EthereumWallet, TransactionBuilder};
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use eyre::{Context, Result};

use crate::abi::{IOracle, IValidatorSet};
use crate::config::AccountConfig;

/// Oracle precompile address (0xEE05).
const ORACLE_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE05");
/// Validator set precompile address (0xEE00).
const VALIDATOR_SET_ADDRESS: Address =
    alloy_primitives::address!("0x000000000000000000000000000000000000EE00");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OracleParams {
    vote_period: u64,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AggregateVoteState {
    exists: bool,
}

/// Creates an EthereumWallet from config. Call once at startup, reuse for all submissions.
pub fn create_wallet(account: &AccountConfig) -> Result<EthereumWallet> {
    let signer: PrivateKeySigner = account
        .private_key
        .parse()
        .with_context(|| "invalid private key")?;
    Ok(EthereumWallet::from(signer))
}

/// One canonical state anchor for both scheduling and every preflight read.
pub struct VoteHead {
    pub height: u64,
    pub block: BlockId,
}

pub async fn get_vote_head(rpc_endpoint: &str) -> Result<VoteHead> {
    let provider = ProviderBuilder::new()
        .connect_http(rpc_endpoint.parse().with_context(|| "invalid RPC URL")?);
    let head = provider
        .get_block_by_number(BlockNumberOrTag::Latest)
        .await
        .with_context(|| "latest block read failed")?
        .ok_or_else(|| eyre::eyre!("latest block missing"))?;
    Ok(VoteHead {
        height: head.header.number,
        block: BlockId::hash_canonical(head.header.hash),
    })
}

/// Submits an oracle vote as a signed EIP-1559 transaction.
///
/// Uses a pre-created wallet (from `create_wallet`) to avoid re-parsing the
/// private key on every submission.
pub async fn submit_vote(
    rpc_endpoint: &str,
    wallet: &EthereumWallet,
    chain_id: u64,
    calldata: &[u8],
    gasless_oracle_vote: bool,
) -> Result<B256> {
    // Build provider with signer
    let provider = ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect_http(rpc_endpoint.parse().with_context(|| "invalid RPC URL")?);

    // Build transaction with explicit chain_id
    let tx = TransactionRequest::default()
        .to(ORACLE_ADDRESS)
        .input(Bytes::copy_from_slice(calldata).into())
        .gas_limit(1_000_000);
    let mut tx = tx;
    tx.set_chain_id(chain_id);

    if gasless_oracle_vote {
        let fee_cap = zero_fee_max_fee_cap(provider.get_gas_price().await.ok());
        tx = tx.max_fee_per_gas(fee_cap).max_priority_fee_per_gas(0);
        tracing::debug!(
            max_fee_per_gas = fee_cap,
            "submitting oracle vote through ZeroFee txpool policy"
        );
    }

    // Send - alloy handles nonce, gas estimation, signing, and broadcasting.
    // Gasless votes remain normal signed EVM transactions. The fee cap is still
    // set high enough for Reth's public txpool protocol checks, while the node's
    // ZeroFee policy waives native fee debit after revalidating signer and state.
    let pending = provider
        .send_transaction(tx)
        .await
        .map_err(|e| eyre::eyre!("oracle vote tx failed: {e:#}"))?;

    // Retain this identity in the caller until inclusion or pool removal;
    // a receipt timeout must not turn an accepted vote into another send.
    Ok(*pending.tx_hash())
}

#[derive(Debug, PartialEq, Eq)]
pub enum VoteStatus {
    Pending,
    Included { height: u64, success: bool },
    Missing,
}

pub async fn vote_status(rpc_endpoint: &str, hash: B256) -> Result<VoteStatus> {
    let provider = ProviderBuilder::new().connect_http(rpc_endpoint.parse()?);
    if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
        if let (Some(height), Some(hash)) = (receipt.block_number, receipt.block_hash) {
            if let Some(block) = provider.get_block_by_number(height.into()).await? {
                if block.header.hash == hash {
                    return Ok(VoteStatus::Included {
                        height,
                        success: receipt.status(),
                    });
                }
            }
        }
    }
    // Null receipt alone is not a dropped transaction. Check the pool too.
    Ok(if provider.get_transaction_by_hash(hash).await?.is_some() {
        VoteStatus::Pending
    } else {
        VoteStatus::Missing
    })
}

fn zero_fee_max_fee_cap(observed_gas_price: Option<u128>) -> u128 {
    observed_gas_price
        .unwrap_or(MIN_PROTOCOL_BASE_FEE as u128)
        .max(MIN_PROTOCOL_BASE_FEE as u128)
}

/// Preflight check result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightResult {
    /// Safe to submit vote.
    Ok,
    /// Do not send on this observation; retry after the normal poll interval.
    Skip(String),
}

/// Runs preflight checks before vote submission.
pub async fn preflight_check(
    rpc_endpoint: &str,
    expected_vote_period: u64,
    validator_address: &str,
    block: BlockId,
) -> PreflightResult {
    let url = match rpc_endpoint.parse() {
        Ok(u) => u,
        Err(_) => return PreflightResult::Skip("invalid RPC URL".into()),
    };
    let validator = match validator_address.parse::<Address>() {
        Ok(addr) => addr,
        Err(_) => return PreflightResult::Skip("invalid validator address".into()),
    };
    let provider = ProviderBuilder::new().connect_http(url);

    let params = match read_oracle_params(&provider, block).await {
        Ok(params) => params,
        Err(e) => return PreflightResult::Skip(format!("preflight getParams failed: {e}")),
    };
    match check_vote_penalty_counter(&provider, validator, block).await {
        Ok(()) => {}
        Err(e) => {
            return PreflightResult::Skip(format!("preflight getVotePenaltyCounter failed: {e}"))
        }
    };
    match check_validator_status(&provider, validator, block).await {
        Ok(()) => {}
        Err(e) => {
            return PreflightResult::Skip(format!("preflight validatorByAddress failed: {e}"))
        }
    };
    let aggregate_vote = match read_aggregate_vote(&provider, validator, block).await {
        Ok(vote) => vote,
        Err(e) => return PreflightResult::Skip(format!("preflight getAggregateVote failed: {e}")),
    };

    evaluate_preflight(expected_vote_period, params, aggregate_vote)
}

async fn read_oracle_params<P: Provider>(provider: &P, block: BlockId) -> Result<OracleParams> {
    let output = eth_call(
        provider,
        ORACLE_ADDRESS,
        IOracle::getParamsCall {}.abi_encode(),
        block,
    )
    .await
    .with_context(|| "oracle getParams eth_call failed")?;
    let result = IOracle::getParamsCall::abi_decode_returns(&output)
        .with_context(|| "oracle getParams decode failed")?;

    Ok(OracleParams {
        vote_period: result.votePeriod,
        enabled: result.enabled,
    })
}

async fn check_vote_penalty_counter<P: Provider>(
    provider: &P,
    validator: Address,
    block: BlockId,
) -> Result<()> {
    let output = eth_call(
        provider,
        ORACLE_ADDRESS,
        IOracle::getVotePenaltyCounterCall { validator }.abi_encode(),
        block,
    )
    .await
    .with_context(|| "oracle getVotePenaltyCounter eth_call failed")?;
    IOracle::getVotePenaltyCounterCall::abi_decode_returns(&output)
        .with_context(|| "oracle getVotePenaltyCounter decode failed")?;
    Ok(())
}

async fn read_aggregate_vote<P: Provider>(
    provider: &P,
    validator: Address,
    block: BlockId,
) -> Result<AggregateVoteState> {
    let output = eth_call(
        provider,
        ORACLE_ADDRESS,
        IOracle::getAggregateVoteCall { validator }.abi_encode(),
        block,
    )
    .await
    .with_context(|| "oracle getAggregateVote eth_call failed")?;
    let result = IOracle::getAggregateVoteCall::abi_decode_returns(&output)
        .with_context(|| "oracle getAggregateVote decode failed")?;

    Ok(AggregateVoteState {
        exists: result.exists,
    })
}

async fn check_validator_status<P: Provider>(
    provider: &P,
    validator: Address,
    block: BlockId,
) -> Result<()> {
    let output = eth_call(
        provider,
        VALIDATOR_SET_ADDRESS,
        IValidatorSet::validatorByAddressCall { addr: validator }.abi_encode(),
        block,
    )
    .await
    .with_context(|| "validatorByAddress eth_call failed")?;
    IValidatorSet::validatorByAddressCall::abi_decode_returns(&output)
        .with_context(|| "validatorByAddress decode failed")?;

    Ok(())
}

async fn eth_call<P: Provider>(
    provider: &P,
    to: Address,
    calldata: Vec<u8>,
    block: BlockId,
) -> Result<Bytes> {
    let tx = TransactionRequest::default()
        .to(to)
        .input(Bytes::copy_from_slice(&calldata).into());
    provider
        .call(tx)
        .block(block)
        .await
        .with_context(|| "eth_call failed")
}

fn evaluate_preflight(
    expected_vote_period: u64,
    params: OracleParams,
    aggregate_vote: AggregateVoteState,
) -> PreflightResult {
    if !params.enabled {
        return PreflightResult::Skip("oracle is disabled".into());
    }
    if params.vote_period != expected_vote_period {
        return PreflightResult::Skip(format!(
            "on-chain vote_period={} != config vote_period={}",
            params.vote_period, expected_vote_period
        ));
    }
    if aggregate_vote.exists {
        return PreflightResult::Skip("validator already voted this period".into());
    }

    PreflightResult::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise the RPC boundary, including EIP-1898 parameters, rather than
    // just testing the pure preflight predicate.
    #[tokio::test]
    async fn preflight_pins_boundary_reads_and_recovers_after_rpc_failure() {
        use alloy_primitives::U256;
        use alloy_sol_types::SolValue;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let old = BlockId::hash_canonical(B256::repeat_byte(87));
        let boundary = BlockId::hash_canonical(B256::repeat_byte(88));
        let next = BlockId::hash_canonical(B256::repeat_byte(89));
        let params = (
            8_u64,
            U256::ZERO,
            0_u64,
            U256::ZERO,
            U256::ZERO,
            0_u64,
            true,
        )
            .abi_encode_params();
        let penalty = (0_u64, 0_u64, 0_u64).abi_encode_params();
        let validator = IValidatorSet::validatorByAddressCall::abi_encode_returns(
            &IValidatorSet::validatorByAddressReturn {
                validatorAddress: Address::ZERO,
                consensusPubkey: Bytes::new(),
                stake: U256::ZERO,
                status: 1,
                slashCount: 0,
                missedBlocks: 0,
                missedVotes: 0,
                blocksProposed: 0,
                joinedAtHeight: 0,
                deactivatedAtHeight: 0,
                unbondingEnd: 0,
                hasBLSShare: false,
            },
        );
        let aggregate = |exists| {
            (
                exists,
                Vec::<Address>::new(),
                Vec::<Address>::new(),
                Vec::<U256>::new(),
                Vec::<U256>::new(),
            )
                .abi_encode_params()
        };
        let mut replies = Vec::new();
        for (block, exists) in [(old, true), (next, false)] {
            for bytes in [
                params.clone(),
                penalty.clone(),
                validator.clone(),
                aggregate(exists),
            ] {
                replies.push((block, Some(bytes)));
            }
        }
        replies.insert(4, (boundary, None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for (block, reply) in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let body = loop {
                    let mut chunk = [0_u8; 4096];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&chunk[..read]);
                    assert!(request.len() < 65_536);
                    if let Some(split) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&request[..split]).unwrap();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if request.len() >= split + 4 + length {
                            break serde_json::from_slice::<serde_json::Value>(
                                &request[split + 4..split + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                assert_eq!(body["method"], "eth_call");
                assert_eq!(body["params"][1], serde_json::to_value(block).unwrap());
                let response = match reply {
                    Some(bytes) => serde_json::json!({"jsonrpc":"2.0", "id":body["id"], "result":Bytes::from(bytes)}),
                    None => serde_json::json!({"jsonrpc":"2.0", "id":body["id"], "error":{"code":-32000,"message":"temporary state unavailable"}}),
                }.to_string();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response);
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let validator = format!("{:#x}", Address::ZERO);
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            assert!(matches!(preflight_check(&url, 8, &validator, old).await,
                PreflightResult::Skip(reason) if reason.contains("already voted")));
            assert!(
                matches!(preflight_check(&url, 8, &validator, boundary).await,
                PreflightResult::Skip(reason) if reason.contains("getParams failed"))
            );
            assert_eq!(
                preflight_check(&url, 8, &validator, next).await,
                PreflightResult::Ok
            );
            server.await.unwrap();
        })
        .await;
        result.expect("pinned preflight sequence must finish");
    }

    fn ok_params() -> OracleParams {
        OracleParams {
            vote_period: 2,
            enabled: true,
        }
    }

    fn no_vote() -> AggregateVoteState {
        AggregateVoteState { exists: false }
    }

    #[test]
    fn preflight_allows_normal_case() {
        let result = evaluate_preflight(2, ok_params(), no_vote());

        assert_eq!(result, PreflightResult::Ok);
    }

    #[test]
    fn zero_fee_fee_cap_never_goes_below_reth_protocol_minimum() {
        assert_eq!(zero_fee_max_fee_cap(None), MIN_PROTOCOL_BASE_FEE as u128);
        assert_eq!(zero_fee_max_fee_cap(Some(0)), MIN_PROTOCOL_BASE_FEE as u128);
        assert_eq!(zero_fee_max_fee_cap(Some(1_000_000_000)), 1_000_000_000);
    }

    #[test]
    fn preflight_skips_disabled_oracle() {
        let result = evaluate_preflight(
            2,
            OracleParams {
                enabled: false,
                ..ok_params()
            },
            no_vote(),
        );

        assert!(matches!(result, PreflightResult::Skip(reason) if reason == "oracle is disabled"));
    }

    #[test]
    fn preflight_skips_vote_period_mismatch() {
        let result = evaluate_preflight(3, ok_params(), no_vote());

        assert!(matches!(
            result,
            PreflightResult::Skip(reason)
                if reason == "on-chain vote_period=2 != config vote_period=3"
        ));
    }

    #[test]
    fn preflight_skips_existing_vote() {
        let result = evaluate_preflight(2, ok_params(), AggregateVoteState { exists: true });

        assert!(matches!(
            result,
            PreflightResult::Skip(reason) if reason == "validator already voted this period"
        ));
    }
}
