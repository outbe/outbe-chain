use super::*;

/// Install a self-authorization EIP-7702 delegation and return its receipt as
/// JSON so scenario assertions can inspect the public RPC representation.
pub(crate) fn install_delegation(
    url: &str,
    key: &str,
    target: Address,
) -> Result<serde_json::Value> {
    install_delegation_with_overrides(url, key, target, None, None)
}

/// Submit an EIP-7702 authorization with optional chain-id and authorization-
/// nonce overrides. Negative live tests use this to prove that invalid or stale
/// authorizations cannot mutate an account's delegation.
pub(crate) fn install_delegation_with_overrides(
    url: &str,
    key: &str,
    target: Address,
    authorization_chain_id: Option<U256>,
    authorization_nonce: Option<u64>,
) -> Result<serde_json::Value> {
    let max_fee = canonical_next_block_fee_cap(url, 0)?;
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let authority = signer.address();
    let chain_id = raw_json(url, "eth_chainId")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        })
        .ok_or_else(|| eyre!("read chain id"))?;
    let tx_nonce = nonce(url, authority).ok_or_else(|| eyre!("read authority nonce"))?;
    let authorization = Authorization {
        chain_id: authorization_chain_id.unwrap_or_else(|| U256::from(chain_id)),
        address: target,
        nonce: authorization_nonce.unwrap_or(tx_nonce + 1),
    };
    let signature = signer.sign_hash_sync(&authorization.signature_hash())?;
    let signed = authorization.into_signed(signature);
    submit_delegation(
        DelegationSubmission {
            url,
            wallet: EthereumWallet::from(signer),
            sender: authority,
            nonce: tx_nonce,
            max_fee,
            signed,
        },
        20,
        "EIP-7702",
    )
}

/// Install an EIP-7702 delegation for `authority_key` while a distinct
/// funded `payer_key` signs the outer transaction and pays its gas.
///
/// A distinct authority signs its current account nonce. The `+1` rule in
/// [`install_delegation_with_overrides`] only applies when both are true:
///
/// - the authority is also the transaction sender
/// - its transaction nonce is incremented before the authorization tuple is
///   processed.
pub(crate) fn install_delegation_for_authority(
    url: &str,
    payer_key: &str,
    authority_key: &str,
    target: Address,
) -> Result<serde_json::Value> {
    let max_fee = canonical_next_block_fee_cap(url, 0)?;
    let payer: PrivateKeySigner = payer_key
        .parse()
        .map_err(|e| eyre!("invalid payer private key: {e}"))?;
    let authority: PrivateKeySigner = authority_key
        .parse()
        .map_err(|e| eyre!("invalid authority private key: {e}"))?;
    let payer_address = payer.address();
    let authority_address = authority.address();
    if payer_address == authority_address {
        return Err(eyre!(
            "sponsor-paid delegation requires distinct payer and authority"
        ));
    }
    let chain_id = raw_json(url, "eth_chainId")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        })
        .ok_or_else(|| eyre!("read chain id"))?;
    let payer_nonce = nonce(url, payer_address).ok_or_else(|| eyre!("read payer nonce"))?;
    let authority_nonce =
        nonce(url, authority_address).ok_or_else(|| eyre!("read authority nonce"))?;
    let authorization = Authorization {
        chain_id: U256::from(chain_id),
        address: target,
        nonce: authority_nonce,
    };
    let signature = authority.sign_hash_sync(&authorization.signature_hash())?;
    let signed = authorization.into_signed(signature);
    submit_delegation(
        DelegationSubmission {
            url,
            wallet: EthereumWallet::from(payer),
            sender: payer_address,
            nonce: payer_nonce,
            max_fee,
            signed,
        },
        60,
        "sponsor-paid EIP-7702",
    )
}

struct DelegationSubmission<'a> {
    url: &'a str,
    wallet: EthereumWallet,
    sender: Address,
    nonce: u64,
    max_fee: u128,
    signed: alloy_eips::eip7702::SignedAuthorization,
}

fn submit_delegation(
    request: DelegationSubmission<'_>,
    attempts: usize,
    label: &str,
) -> Result<serde_json::Value> {
    let DelegationSubmission {
        url,
        wallet,
        sender,
        nonce,
        max_fee,
        signed,
    } = request;
    let url = url.to_owned();
    let label = label.to_owned();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let tx = TransactionRequest::default()
            .to(sender)
            .nonce(nonce)
            .gas_limit(100_000)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(0)
            .with_authorization_list(vec![signed]);
        let pending = provider.send_transaction(tx).await?;
        let hash = *pending.tx_hash();
        for _ in 0..attempts {
            if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
                return Ok(serde_json::to_value(receipt)?);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Err(eyre!("{label} transaction was not mined: {hash:#x}"))
    })
}

/// Only this explicit admission rejection proves that the call was not accepted.
/// Transport failures and other RPC errors must never trigger a fresh-nonce retry.
pub(super) fn delegated_sender_busy(code: i64, message: &str) -> bool {
    code == -32000 && message == "in-flight transaction limit reached for delegated accounts"
}

/// Submit a canonical sponsored protocol call from an already delegated signer.
/// The live Oracle feeder shares this signer, so retry explicit slot contention
/// with a fresh nonce, then wait for the accepted call's canonical finality.
pub(crate) fn send_sponsored_call<C: SolCall>(
    url: &str,
    key: &str,
    to: Address,
    gas_limit: u64,
    call: &C,
) -> Result<MinedCallOutcome> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let sender = signer.address();
    let wallet = EthereumWallet::from(signer);
    let data = call.abi_encode();
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let (hash, nonce) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut attempt = 0;
            loop {
                attempt += 1;
                let block = provider
                    .get_block_by_number(BlockNumberOrTag::Latest)
                    .await?
                    .ok_or_else(|| eyre!("sponsored admission: latest block missing"))?;
                let base_fee = block.header.base_fee_per_gas
                    .ok_or_else(|| eyre!("sponsored admission: base fee missing"))?;
                let max_fee = next_block_fee_cap(u128::from(base_fee), 0)?;
                let nonce = provider.get_transaction_count(sender).pending().await?;
                let tx = TransactionRequest::default()
                    .from(sender)
                    .nonce(nonce)
                    .to(to)
                    .input(Bytes::from(data.clone()).into())
                    .gas_limit(gas_limit)
                    .max_fee_per_gas(max_fee)
                    .max_priority_fee_per_gas(0);
                match provider.send_transaction(tx).await {
                    Ok(pending) => {
                        let hash = *pending.tx_hash();
                        eprintln!("sponsored admission accepted: sender={sender} to={to} nonce={nonce} attempt={attempt} tx={hash}");
                        break Ok::<_, eyre::Report>((hash, nonce));
                    }
                    Err(error) if error.as_error_resp().is_some_and(|response| {
                        delegated_sender_busy(response.code, &response.message)
                    }) => {
                        eprintln!("sponsored admission busy: sender={sender} to={to} nonce={nonce} attempt={attempt} head={} error={error}", block.header.number);
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }).await.map_err(|_| eyre!("sponsored admission exceeded 15 seconds: sender={sender} to={to}; see admission diagnostics"))??;

        // Once accepted, never create another transaction for this operation.
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
                    let height = receipt.block_number.ok_or_else(|| eyre!("sponsored receipt omitted block number: {hash}"))?;
                    let block_hash = receipt.block_hash.ok_or_else(|| eyre!("sponsored receipt omitted block hash: {hash}"))?;
                    if let Some(finalized) = provider.get_block_by_number(BlockNumberOrTag::Finalized).await? {
                        if finalized.header.number >= height {
                            let canonical = provider.get_block_by_number(BlockNumberOrTag::Number(height)).await?
                                .ok_or_else(|| eyre!("sponsored receipt canonical block missing: {hash} height={height}"))?;
                            ensure!(canonical.header.hash == block_hash,
                                "sponsored receipt is not canonical: tx={hash} height={height} receipt={block_hash} canonical={}", canonical.header.hash);
                            eprintln!("sponsored receipt finalized: sender={sender} nonce={nonce} tx={hash} height={height} block={block_hash} gas_used={} success={}", receipt.gas_used, receipt.status());
                            return Ok(MinedCallOutcome {
                                transaction_hash: format!("{:#x}", receipt.transaction_hash),
                                success: receipt.status(),
                                receipt: serde_json::to_value(receipt)?,
                            });
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }).await.map_err(|_| eyre!("sponsored transaction did not finalize within 60 seconds: sender={sender} nonce={nonce} tx={hash}"))?
    })
}
