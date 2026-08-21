//! Native JSON-RPC layer — the typed replacement for the harness's `cast`
//! shell-outs.
//!
//! Reads (`eth_call` to the protocol precompiles, block/receipt queries) and
//! local-signer sends go through an alloy [`Provider`] instead of spawning
//! `cast`. Mirrors the canonical provider client in `bin/outbe-feeder`
//! (`ProviderBuilder::new().connect_http(...)`, calldata `abi_encode` + `call` +
//! `abi_decode_returns`, `EthereumWallet` sends).
//!
//! The harness is synchronous (cucumber steps are plain fns), so each call
//! bridges to alloy's async API via [`block_on`], which runs the future on a
//! dedicated background runtime and blocks the caller on a channel. That works
//! regardless of whether the calling step runs on a tokio worker or a plain
//! thread — there is no runtime nesting to panic on.

use std::future::Future;
use std::sync::mpsc::sync_channel;
use std::sync::OnceLock;

use alloy_eips::{
    eip1559::MIN_PROTOCOL_BASE_FEE, eip7702::Authorization, BlockId, BlockNumberOrTag,
};
use alloy_network::{EthereumWallet, TransactionBuilder7702};
use alloy_primitives::{Address, Bytes, TxHash, B256, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall};
use eyre::{eyre, Result};
use tokio::runtime::Runtime;

/// Fee cap, in COEN units per gas. EIP-1559 charges the block's base fee and
/// treats this only as the ceiling the sender accepts, so headroom above the
/// protocol floor costs nothing — it just has to be held up front. Without it a
/// burst of deploy-sized blocks lifts the base fee and every later send is
/// rejected as underpriced.
const GAS_PRICE_UNITS: u128 = MIN_PROTOCOL_BASE_FEE as u128 * 8;

/// Explicit limit used by negative-path calls.
///
/// Supplying a limit prevents the provider from estimating gas first: an
/// intentional contract revert is then submitted and observed through its
/// mined receipt instead of being returned as a preflight RPC error.
const REVERT_FRIENDLY_GAS_LIMIT: u64 = 10_000_000;

// Precompile ABI surface the harness reads/writes, generated from the canonical
// Solidity sources so the harness exercises the same selectors the node
// dispatches.
sol!("../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../contracts/precompiles/src/IUpdate.sol");
sol!("../../contracts/precompiles/src/IGovernance.sol");
sol!("../../contracts/precompiles/src/IL2Registry.sol");
sol!("../../contracts/precompiles/src/ITribute.sol");
sol!("../../contracts/precompiles/src/INod.sol");
sol!("../../contracts/precompiles/src/INodFactory.sol");
sol!("../../contracts/precompiles/src/IGratis.sol");
sol!("../../contracts/precompiles/src/IMetadosis.sol");
sol!("../../contracts/precompiles/src/IPromisLimit.sol");
sol!("../../contracts/precompiles/src/IDesis.sol");
sol!("../../contracts/precompiles/src/IStaking.sol");
sol!("../../contracts/precompiles/src/IZeroFee.sol");
sol!("../../contracts/precompiles/src/IAgentReward.sol");
sol!("../../contracts/precompiles/src/ITeeRegistryV1.sol");
sol!("../../contracts/precompiles/src/ISlashIndicator.sol");
sol!("../../contracts/precompiles/src/IRadicleRegistry.sol");

// Negative-path tests submit this deliberately unsupported legacy selector and
// assert that the canonical ValidatorSet ABI rejects it without mutation.
sol! {
    interface IValidatorSetRaw {
        function activateResharedSet(address[] calldata newActiveSet, bytes32 groupPublicKey)
            external;
    }
}

sol!(
    #![sol(extra_derives(Debug, PartialEq))]
    "../../contracts/precompiles/src/IVote.sol"
);

sol!(
    #![sol(extra_derives(Debug, PartialEq))]
    "../../contracts/precompiles/src/IStablecoinFactory.sol"
);

sol!(
    #![sol(extra_derives(Debug, PartialEq))]
    "../../contracts/precompiles/src/IStablecoinPolicyRegistry.sol"
);

sol!(
    #![sol(extra_derives(Debug, PartialEq))]
    "../../contracts/precompiles/src/IStablecoin.sol"
);

/// A contract call that was submitted and mined, including reverted calls.
#[derive(Clone, Debug)]
pub(crate) struct MinedCallOutcome {
    pub transaction_hash: String,
    pub success: bool,
    pub receipt: serde_json::Value,
}

/// One already ABI-encoded call for an explicit-nonce transaction batch.
#[derive(Clone, Debug)]
pub(crate) struct PreparedCall {
    pub to: Address,
    pub data: Bytes,
    pub value: Option<U256>,
}

/// A dedicated multi-thread runtime that drives every RPC future, independent of
/// whatever thread/runtime the cucumber step is on.
fn eth_runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build eth runtime")
    })
}

/// Run an async future to completion from a synchronous step. The future runs on
/// [`eth_runtime`] and the caller blocks on a channel, so there is no runtime
/// nesting — this is safe whether the caller is a tokio worker or a plain thread.
pub(crate) fn block_on<F>(f: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (tx, rx) = sync_channel(1);
    eth_runtime().spawn(async move {
        let _ = tx.send(f.await);
    });
    rx.recv().expect("eth runtime dropped the task")
}

/// `eth_call` a view function and decode its return while preserving the failure.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn read_call_result<C: SolCall>(
    url: &str,
    to: Address,
    call: &C,
) -> std::result::Result<C::Return, String>
where
    C::Return: Send + 'static,
{
    let url = url.to_string();
    let data = call.abi_encode();
    block_on(async move {
        let endpoint = url
            .parse()
            .map_err(|error| format!("invalid RPC URL: {error}"))?;
        let provider = ProviderBuilder::new().connect_http(endpoint);
        let tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into());
        let out = provider
            .call(tx)
            .block(BlockId::latest())
            .await
            .map_err(|error| format!("eth_call failed: {error}"))?;
        C::abi_decode_returns(&out).map_err(|error| format!("ABI decode failed: {error}"))
    })
}

/// `eth_call` a view function and decode its return, or `None` on any transport /
/// decode error (the analogue of the old `cast … 2>/dev/null`).
pub(crate) fn read_call<C: SolCall>(url: &str, to: Address, call: &C) -> Option<C::Return>
where
    C::Return: Send + 'static,
{
    #[cfg(feature = "ocomp-integration")]
    {
        read_call_result(url, to, call).ok()
    }

    #[cfg(not(feature = "ocomp-integration"))]
    {
        let url = url.to_string();
        let data = call.abi_encode();
        block_on(async move {
            let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
            let tx = TransactionRequest::default()
                .to(to)
                .input(Bytes::from(data).into());
            // Pin state and block context to the canonical head. Omitting the block
            // tag lets some RPC implementations execute against `pending`, whose
            // timestamp can cross a UTC boundary before a block is canonical.
            let out = provider.call(tx).block(BlockId::latest()).await.ok()?;
            C::abi_decode_returns(&out).ok()
        })
    }
}

/// Execute a typed view against the exact canonical state at `height`.
pub(crate) fn read_call_at<C: SolCall>(
    url: &str,
    to: Address,
    call: &C,
    height: u64,
) -> Option<C::Return>
where
    C::Return: Send + 'static,
{
    let url = url.to_string();
    let data = call.abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into());
        let out = provider
            .call(tx)
            .block(BlockId::number(height))
            .await
            .ok()?;
        C::abi_decode_returns(&out).ok()
    })
}

/// Require a typed view call to fail specifically as an EVM revert at the
/// exact canonical block. Transport failures are not accepted as ABI evidence.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn read_call_reverts_at<C: SolCall>(
    url: &str,
    to: Address,
    call: &C,
    height: u64,
) -> Option<bool> {
    let url = url.to_string();
    let data = call.abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into());
        match provider.call(tx).block(BlockId::number(height)).await {
            Ok(_) => Some(false),
            Err(error) => {
                let message = error.to_string().to_ascii_lowercase();
                Some(message.contains("revert"))
            }
        }
    })
}

/// Simulate a state-changing call at the canonical head while preserving the
/// RPC server's revert error. This is diagnostic only: E2E mutation paths still
/// submit a real transaction and assert its mined receipt.
pub(crate) fn simulate_call<C: SolCall>(
    url: &str,
    to: Address,
    from: Address,
    call: &C,
) -> Result<()> {
    let url = url.to_string();
    let data = call.abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse()?);
        let tx = TransactionRequest::default()
            .from(from)
            .to(to)
            .input(Bytes::from(data).into())
            .gas_limit(REVERT_FRIENDLY_GAS_LIMIT);
        provider.call(tx).block(BlockId::latest()).await?;
        Ok(())
    })
}

/// Head block number (`eth_blockNumber`).
pub(crate) fn block_number(url: &str) -> Option<u64> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        provider.get_block_number().await.ok()
    })
}

/// Consensus timestamp of the canonical head block.
pub(crate) fn latest_block_timestamp(url: &str) -> Option<u64> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .ok()??;
        Some(block.header.timestamp)
    })
}

/// Number of the finalized block.
pub(crate) fn finalized_number(url: &str) -> Option<u64> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await
            .ok()??;
        Some(block.header.number)
    })
}

/// `stateRoot` of block `height`, `0x`-hex (parity-comparison friendly).
pub(crate) fn state_root(url: &str, height: u64) -> Option<String> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Number(height))
            .await
            .ok()??;
        Some(format!("{:#x}", block.header.state_root))
    })
}

/// Canonical hash, state root and protocol header artifacts for one block.
pub(crate) fn block_commitment(url: &str, height: u64) -> Option<(B256, B256, Bytes)> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Number(height))
            .await
            .ok()??;
        Some((
            block.header.hash,
            block.header.state_root,
            block.header.extra_data.clone(),
        ))
    })
}

/// Canonical block hash at `height`.
pub(crate) fn block_hash(url: &str, height: u64) -> Option<String> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Number(height))
            .await
            .ok()??;
        Some(format!("{:#x}", block.header.hash))
    })
}

/// A custom JSON-RPC method returning an arbitrary JSON value (e.g.
/// `outbe_consensusStatus`).
pub(crate) fn raw_json(url: &str, method: &'static str) -> Option<serde_json::Value> {
    raw_json_with_params(url, method, serde_json::json!([]))
}

/// A custom JSON-RPC method with explicit positional parameters.
pub(crate) fn raw_json_with_params(
    url: &str,
    method: &'static str,
    params: serde_json::Value,
) -> Option<serde_json::Value> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        provider
            .raw_request::<_, serde_json::Value>(method.into(), params)
            .await
            .ok()
    })
}

/// A raw JSON-RPC request that preserves the server error. Negative E2E paths
/// use this instead of [`raw_json_with_params`], whose `Option` intentionally
/// treats transport and server errors alike for polling reads.
pub(crate) fn raw_json_result(
    url: &str,
    method: &'static str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse()?);
        provider
            .raw_request::<_, serde_json::Value>(method.into(), params)
            .await
            .map_err(Into::into)
    })
}

/// Request the caller's Gratis modify key through the production enclave RPC
/// and open the sealed response with a fresh client X25519 key.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn derive_gratis_modify_key(url: &str, key: &str) -> Result<[u8; 32]> {
    use outbe_tee::protocol::{derive_account_keys_message, eip191_hash, Ledger};
    use outbe_tee_enclave::crypto::{decrypt_share, x25519_public, EncryptedShare};
    use rand_core::RngCore as _;

    let signer: PrivateKeySigner = key
        .parse()
        .map_err(|error| eyre!("invalid private key: {error}"))?;
    let account = signer.address();
    let mut ephemeral_secret = [0_u8; 32];
    rand_core::OsRng.fill_bytes(&mut ephemeral_secret);
    let ephemeral_public = x25519_public(&ephemeral_secret);
    let digest = eip191_hash(&derive_account_keys_message(
        Ledger::Gratis,
        account,
        B256::from(ephemeral_public),
    ));
    let signature = signer
        .sign_hash_sync(&digest)
        .map_err(|error| eyre!("sign Gratis key request: {error}"))?;
    let response = raw_json_result(
        url,
        "outbe_deriveKeys",
        serde_json::json!([
            "Gratis",
            format!("{account:#x}"),
            format!("{:#x}", B256::from(ephemeral_public)),
            format!("0x{}", hex::encode(signature.as_bytes())),
        ]),
    )?;

    let decode = |field: &'static str| -> Result<Vec<u8>> {
        let value = response
            .get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("outbe_deriveKeys omitted {field}"))?;
        hex::decode(value.trim_start_matches("0x"))
            .map_err(|error| eyre!("decode outbe_deriveKeys {field}: {error}"))
    };
    let ephemeral_pubkey: [u8; 32] = decode("enclaveEphemeralPubkey")?
        .try_into()
        .map_err(|value: Vec<u8>| eyre!("enclave ephemeral key is {} bytes", value.len()))?;
    let nonce: [u8; 12] = decode("nonce")?
        .try_into()
        .map_err(|value: Vec<u8>| eyre!("sealed-key nonce is {} bytes", value.len()))?;
    let plaintext = decrypt_share(
        &ephemeral_secret,
        &EncryptedShare {
            ephemeral_pub: ephemeral_pubkey,
            nonce,
            ciphertext: decode("sealed")?,
        },
    )
    .map_err(|error| eyre!("open sealed Gratis keys: {error}"))?;
    if plaintext.len() != 64 {
        return Err(eyre!(
            "outbe_deriveKeys plaintext is {} bytes instead of 64",
            plaintext.len()
        ));
    }
    plaintext[32..]
        .try_into()
        .map_err(|_| eyre!("Gratis modify key is not 32 bytes"))
}

/// Broadcast one already signed public transaction without reconstructing or
/// re-signing its envelope.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn send_raw_transaction(url: &str, raw_transaction: &[u8]) -> Result<String> {
    let encoded = format!("0x{}", hex::encode(raw_transaction));
    let value = raw_json_result(url, "eth_sendRawTransaction", serde_json::json!([encoded]))?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| eyre!("eth_sendRawTransaction returned a non-hash value"))
}

/// Public nonce from canonical chain state for one account.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn canonical_nonce(url: &str, address: Address) -> Option<u64> {
    let value = raw_json_with_params(
        url,
        "eth_getTransactionCount",
        serde_json::json!([format!("{address:#x}"), "latest"]),
    )?;
    u64::from_str_radix(value.as_str()?.trim_start_matches("0x"), 16).ok()
}

/// Public gas price used only as the restricted node signing seam input.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn gas_price(url: &str) -> Option<u128> {
    let value = raw_json(url, "eth_gasPrice")?;
    u128::from_str_radix(value.as_str()?.trim_start_matches("0x"), 16).ok()
}

/// Fetch a bounded inclusive block range with full public transactions through
/// one shared HTTP provider. OCOMP evidence must not spend one connection
/// setup/poll interval per block and accidentally consume the result window it
/// is trying to observe.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn blocks_with_transactions(
    url: &str,
    from_height: u64,
    to_height: u64,
    max_blocks: usize,
) -> Option<Vec<serde_json::Value>> {
    let count = to_height
        .checked_sub(from_height)?
        .checked_add(1)
        .and_then(|value| usize::try_from(value).ok())?;
    if count > max_blocks {
        return None;
    }
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let requests = (from_height..=to_height).map(|height| {
            provider.raw_request::<_, serde_json::Value>(
                "eth_getBlockByNumber".into(),
                serde_json::json!([format!("0x{height:x}"), true]),
            )
        });
        futures::future::try_join_all(requests).await.ok()
    })
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

/// Sign and send a contract call from `key`, waiting for its receipt; returns the
/// tx hash. `value` funds a payable call (e.g. `stake`).
pub(crate) fn send_call<C: SolCall>(
    url: &str,
    to: Address,
    key: &str,
    call: &C,
    value: Option<U256>,
) -> Result<String> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    let data = call.abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let mut tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into())
            .max_fee_per_gas(GAS_PRICE_UNITS)
            .max_priority_fee_per_gas(0);
        if let Some(v) = value {
            tx = tx.value(v);
        }
        let pending = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.send_transaction(tx),
        )
        .await
        .map_err(|_| eyre!("timed out submitting public call transaction"))??;
        let receipt =
            tokio::time::timeout(std::time::Duration::from_secs(30), pending.get_receipt())
                .await
                .map_err(|_| eyre!("timed out waiting for public call receipt"))??;
        Ok(format!("{:#x}", receipt.transaction_hash))
    })
}

/// Sign and send exact calldata through the ordinary public transaction path,
/// waiting for the mined receipt. This is used by adversarial protocol tests
/// that must preserve a production ABI envelope while changing its payload.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn send_calldata(
    url: &str,
    to: Address,
    key: &str,
    calldata: Vec<u8>,
    gas_limit: u64,
) -> Result<String> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(calldata).into())
            .gas_limit(gas_limit)
            .max_fee_per_gas(GAS_PRICE_UNITS)
            .max_priority_fee_per_gas(0);
        let pending = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.send_transaction(tx),
        )
        .await
        .map_err(|_| eyre!("timed out submitting public calldata transaction"))??;
        let receipt =
            tokio::time::timeout(std::time::Duration::from_secs(30), pending.get_receipt())
                .await
                .map_err(|_| eyre!("timed out waiting for public calldata receipt"))??;
        Ok(format!("{:#x}", receipt.transaction_hash))
    })
}

/// Sign and submit a contract call without revert-sensitive gas estimation,
/// then return the mined receipt regardless of its success bit.
///
/// Transport, signing, and inclusion failures remain [`Err`]. A contract-level
/// revert is an [`Ok`] outcome with `success == false`, which lets E2E scenarios
/// assert both the rejection and the absence of partial state changes.
pub(crate) fn send_call_outcome<C: SolCall>(
    url: &str,
    to: Address,
    key: &str,
    call: &C,
    value: Option<U256>,
) -> Result<MinedCallOutcome> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    let data = call.abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let mut tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into())
            .gas_limit(REVERT_FRIENDLY_GAS_LIMIT)
            .max_fee_per_gas(GAS_PRICE_UNITS)
            .max_priority_fee_per_gas(0);
        if let Some(v) = value {
            tx = tx.value(v);
        }
        let pending = provider.send_transaction(tx).await?;
        let receipt = pending.get_receipt().await?;
        Ok(MinedCallOutcome {
            transaction_hash: format!("{:#x}", receipt.transaction_hash),
            success: receipt.status(),
            receipt: serde_json::to_value(receipt)?,
        })
    })
}

/// Submit sequential-nonce calls before waiting for any receipt.
///
/// This is the narrow batching primitive used by lifecycle tests that must put
/// dependent transactions into the same candidate block without enabling
/// non-production automine or storage controls. The caller can compare the
/// returned receipt block numbers and fail explicitly if the node split them.
pub(crate) fn send_prepared_calls_outcomes(
    url: &str,
    key: &str,
    calls: Vec<PreparedCall>,
) -> Result<Vec<MinedCallOutcome>> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let sender = signer.address();
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let first_nonce = provider.get_transaction_count(sender).pending().await?;
        let mut hashes = Vec::with_capacity(calls.len());
        for (offset, call) in calls.into_iter().enumerate() {
            let offset =
                u64::try_from(offset).map_err(|_| eyre!("transaction batch is too large"))?;
            let nonce = first_nonce
                .checked_add(offset)
                .ok_or_else(|| eyre!("transaction batch nonce overflow"))?;
            let mut tx = TransactionRequest::default()
                .to(call.to)
                .input(call.data.into())
                .nonce(nonce)
                .gas_limit(REVERT_FRIENDLY_GAS_LIMIT)
                .max_fee_per_gas(GAS_PRICE_UNITS)
                .max_priority_fee_per_gas(0);
            if let Some(value) = call.value {
                tx = tx.value(value);
            }
            let pending = provider.send_transaction(tx).await?;
            hashes.push(*pending.tx_hash());
        }

        let mut outcomes = Vec::with_capacity(hashes.len());
        for hash in hashes {
            let mut mined = None;
            for _ in 0..240 {
                if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
                    mined = Some(receipt);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let receipt =
                mined.ok_or_else(|| eyre!("transaction was not mined within 60s: {hash:#x}"))?;
            outcomes.push(MinedCallOutcome {
                transaction_hash: format!("{:#x}", receipt.transaction_hash),
                success: receipt.status(),
                receipt: serde_json::to_value(receipt)?,
            });
        }
        Ok(outcomes)
    })
}

/// Plain COEN transfer from `key` to `to` (funds a new account).
pub(crate) fn send_value(url: &str, to: Address, key: &str, value: U256) -> Result<String> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let tx = TransactionRequest::default()
            .to(to)
            .value(value)
            .max_fee_per_gas(GAS_PRICE_UNITS)
            .max_priority_fee_per_gas(0);
        let pending = provider.send_transaction(tx).await?;
        let receipt = pending.get_receipt().await?;
        Ok(format!("{:#x}", receipt.transaction_hash))
    })
}

/// Current account balance.
pub(crate) fn balance(url: &str, address: Address) -> Option<U256> {
    let url = url.to_string();
    block_on(async move {
        ProviderBuilder::new()
            .connect_http(url.parse().ok()?)
            .get_balance(address)
            .await
            .ok()
    })
}

/// Current account bytecode.
pub(crate) fn code(url: &str, address: Address) -> Option<Bytes> {
    let url = url.to_string();
    block_on(async move {
        ProviderBuilder::new()
            .connect_http(url.parse().ok()?)
            .get_code_at(address)
            .await
            .ok()
    })
}

/// Current account nonce.
pub(crate) fn nonce(url: &str, address: Address) -> Option<u64> {
    let url = url.to_string();
    block_on(async move {
        ProviderBuilder::new()
            .connect_http(url.parse().ok()?)
            .get_transaction_count(address)
            .await
            .ok()
    })
}

/// Storage slot read used for the ZeroFee schema marker.
pub(crate) fn storage(url: &str, address: Address, slot: U256) -> Option<U256> {
    let url = url.to_string();
    block_on(async move {
        ProviderBuilder::new()
            .connect_http(url.parse().ok()?)
            .get_storage_at(address, slot)
            .await
            .ok()
    })
}

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
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let tx = TransactionRequest::default()
            .to(authority)
            .nonce(tx_nonce)
            .gas_limit(100_000)
            .max_fee_per_gas(GAS_PRICE_UNITS)
            .max_priority_fee_per_gas(0)
            .with_authorization_list(vec![signed]);
        let pending = provider.send_transaction(tx).await?;
        let hash = *pending.tx_hash();
        for _ in 0..20 {
            if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
                return Ok(serde_json::to_value(receipt)?);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Err(eyre!("EIP-7702 transaction was not mined: {hash:#x}"))
    })
}

/// Send the canonical reward call with either the sponsored envelope or a paid
/// priority fee, returning the mined receipt's public JSON representation.
pub(crate) fn send_reward_call(
    url: &str,
    key: &str,
    to: Address,
    priority_fee: u128,
) -> Result<serde_json::Value> {
    let signer: PrivateKeySigner = key.parse().map_err(|e| eyre!("invalid private key: {e}"))?;
    let wallet = EthereumWallet::from(signer);
    let url = url.to_string();
    let data = IAgentReward::claimRewardCall { amount: U256::ZERO }.abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let max_fee = GAS_PRICE_UNITS;
        let tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into())
            .gas_limit(200_000)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(priority_fee);
        let pending = provider.send_transaction(tx).await?;
        let hash = *pending.tx_hash();
        for _ in 0..60 {
            if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
                return Ok(serde_json::to_value(receipt)?);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Err(eyre!("reward transaction was not mined: {hash:#x}"))
    })
}

/// EOA address (`0x`-hex, checksummed) for a private key — pure, no RPC.
pub(crate) fn address_of(key: &str) -> Option<Address> {
    let signer: PrivateKeySigner = key.parse().ok()?;
    Some(signer.address())
}

/// `amount` whole COEN in the chain's six-decimal native units.
pub(crate) fn coen(amount: u64) -> U256 {
    U256::from(amount) * U256::from(1_000_000u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selector(signature: &str) -> [u8; 4] {
        alloy_primitives::keccak256(signature.as_bytes())[..4]
            .try_into()
            .expect("four-byte selector")
    }

    #[test]
    fn address_from_known_key() {
        // Hardhat account #0 — a well-known key→address pair.
        let key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let addr = address_of(key).expect("address");
        assert_eq!(
            format!("{addr:#x}"),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
        assert!(address_of("not-a-key").is_none());
    }

    #[test]
    fn coen_scales_to_base_units() {
        assert_eq!(coen(1), U256::from(1_000_000u64));
        assert_eq!(coen(0), U256::ZERO);
    }

    #[test]
    fn validator_lifecycle_abi_selectors_match_canonical_interfaces() {
        assert_eq!(
            IValidatorSet::validatorByIndexCall::SELECTOR,
            selector("validatorByIndex(uint64)")
        );
        assert_eq!(
            IValidatorSet::getP2pAddressCall::SELECTOR,
            selector("getP2pAddress(address)")
        );
        assert_eq!(
            IValidatorSet::confirmValidatorReadyCall::SELECTOR,
            selector("confirmValidatorReady(bytes)")
        );
        assert_eq!(
            IValidatorSetRaw::activateResharedSetCall::SELECTOR,
            selector("activateResharedSet(address[],bytes32)")
        );
        assert_eq!(
            IStaking::unstakeCall::SELECTOR,
            selector("unstake(uint256)")
        );
        assert_eq!(
            IStaking::unjailValidatorCall::SELECTOR,
            selector("unjailValidator()")
        );
        assert_eq!(
            ISlashIndicator::submitConflictingNotarizeEvidenceCall::SELECTOR,
            selector("submitConflictingNotarizeEvidence(bytes,bytes)")
        );
    }
}
