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
#[cfg(feature = "ocomp-integration")]
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
