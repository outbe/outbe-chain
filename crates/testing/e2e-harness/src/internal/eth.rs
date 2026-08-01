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

use alloy_eips::{eip7702::Authorization, BlockId, BlockNumberOrTag};
use alloy_network::{EthereumWallet, TransactionBuilder7702};
use alloy_primitives::{Address, Bytes, TxHash, B256, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall};
use eyre::{eyre, Result};
use tokio::runtime::Runtime;

/// Legacy-style gas price used by the old `cast send --gas-price` calls (1 gwei).
const GAS_PRICE_WEI: u128 = 1_000_000_000;

// Precompile ABI surface the harness reads/writes. Signatures mirror the
// `cast call`/`cast send` strings they replace (and `bin/outbe-cli/src/abi.rs`).
sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IValidatorSet {
        event ValidatorDeactivated(address indexed validator, uint64 atHeight);
        function validatorByAddress(address v) external view returns (
            address addr, bytes pubkey, uint256 stake, uint8 status,
            uint64 slashCount, uint64 missedBlocks, uint64 missedVotes,
            uint64 blocksProposed, uint64 joined, uint64 deactivated,
            uint64 unbondEnd, bool hasShare);
        function isConsensusParticipant(address v) external view returns (bool);
        function activeValidatorCount() external view returns (uint32);
        function activeConsensusCount() external view returns (uint32);
        function getEpochNumber() external view returns (uint256);
        function deactivateValidator(address v) external;
        function registerValidator(address v, bytes pubkey, bytes sig) external;
        function setP2pAddress(address v, uint8 kind, bytes addr) external;
        function setDelegate(uint8 role, address delegate) external;
        function getDelegate(address validator, uint8 role) external view returns (address);
        function resolveValidator(uint8 role, address signer) external view returns (address);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IUpdate {
        // Returned as a single (dynamic) struct, not flat values — the `bytes`
        // member makes struct-return and multi-return ABI-encode differently.
        struct ScheduledUpdate {
            uint256 proposalId;
            uint32 version;
            uint64 activationHeight;
            bytes info;
            uint8 status;
        }
        function getActiveVersion() external view returns (uint32);
        function getScheduledUpdate(uint256 id) external view returns (ScheduledUpdate memory);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IGovernance {
        struct Oip {
            uint256 id;
            uint8 status;
            address author;
            uint64 createdBlock;
            uint64 updatedBlock;
            bytes32 textHash;
            string text;
        }
        struct Gip {
            uint256 id;
            uint8 status;
            address author;
            uint64 createdBlock;
            uint64 updatedBlock;
            bytes32 textHash;
            string text;
        }
        function getOip(uint256 id) external view returns (Oip memory);
        function getGip(uint256 id) external view returns (Gip memory);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface ITeeRegistry {
        function isBootstrapped() external view returns (bool);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IL2Registry {
        function registerNetwork(uint64 chainId, address l1Address, bytes publicKey) external;
        function setZkEnabled(uint64 chainId, bool enabled) external;
        function getNetwork(uint64 chainId) external view returns (
            address l1Address, bytes publicKey, bool zkEnabled);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface ITribute {
        function totalSupply() external view returns (uint256);
        function getTributesByOwner(address owner) external view returns (bytes[] memory);
        function getTributesByDay(uint32 worldwideDay) external view returns (bytes[] memory);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface INod {
        struct CertifiedGenerationData {
            bool exists;
            uint32 worldwideDay;
            uint64 generation;
            bytes32 nodRoot;
            bytes32 bucketRoot;
            bytes32 outputManifestRoot;
            uint32 tributeCount;
            uint32 nodCount;
            uint32 bucketCount;
            uint256 nodAmountTotal;
            uint256 nodGratisConsumed;
            uint64 issuedAt;
        }
        function totalSupply() external view returns (uint256);
        function certifiedGeneration(uint32 worldwideDay)
            external
            view
            returns (CertifiedGenerationData memory);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IMetadosis {
        function getOffchainJob(bytes32 intentId)
            external
            view
            returns (bytes memory ocompJobRecordV1);
        function submitLysisResult(bytes calldata resultVoteV1) external;
        function getOffchainVoteAccountability(bytes32 jobId)
            external
            view
            returns (bytes memory ocompVoteAccountabilityV1);
        function getActiveLysisGeneration(uint32 wwd)
            external
            view
            returns (bytes memory activeGenerationV1);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IDesis {
        function getAuctionStage(uint32 worldwideDay) external view returns (uint8);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IStaking {
        function stake(address v, uint256 amount) external payable;
        function claimUnbonded() external;
        function getStake(address validator) external view returns (uint256);
        function getTotalStaked() external view returns (uint256);
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IWorldwideDay {
        function getWorldwideDay(uint32 day) external view returns (
            uint8 f0, uint8 f1, uint64 f2, uint64 f3, uint64 f4,
            uint64 f5, uint64 f6, uint256 f7, uint256 f8);
        function getWorldwideDaysByStatus(uint8 status) external view returns (uint32[] memory);
    }
    interface IZeroFee {
        function authorizeSponsorship(address signer) external view returns (bool);
        function getCounter(address signer) external view returns (uint32 day, uint32 count);
    }
    interface IAgentReward {
        function claimReward(uint256 agentId) external;
    }
}

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IVote.sol"
);

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoinFactory.sol"
);

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoinPolicyRegistry.sol"
);

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoin.sol"
);

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

/// `eth_call` a view function and decode its return, or `None` on any transport /
/// decode error (the analogue of the old `cast … 2>/dev/null`).
pub(crate) fn read_call<C: SolCall>(url: &str, to: Address, call: &C) -> Option<C::Return>
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
        // Pin state and block context to the canonical head. Omitting the block
        // tag lets some RPC implementations execute against `pending`, whose
        // timestamp can cross a UTC boundary before a block is canonical.
        let out = provider.call(tx).block(BlockId::latest()).await.ok()?;
        C::abi_decode_returns(&out).ok()
    })
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

/// `eth_call` raw calldata and return the revert payload the node reports, or
/// `None` when the call succeeds or fails without revert data. This surfaces
/// the machine-readable rejection bytes that transaction receipts omit.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn call_revert_data(url: &str, to: Address, data: Vec<u8>) -> Option<Vec<u8>> {
    let url = url.to_string();
    block_on(async move {
        let provider = ProviderBuilder::new().connect_http(url.parse().ok()?);
        let tx = TransactionRequest::default()
            .to(to)
            .input(Bytes::from(data).into());
        match provider.call(tx).block(BlockId::latest()).await {
            Ok(_) => None,
            Err(error) => {
                let payload = error.as_error_resp()?;
                let raw = payload.data.as_ref()?.get().trim_matches('"').to_owned();
                hex::decode(raw.trim_start_matches("0x")).ok()
            }
        }
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
            .max_fee_per_gas(GAS_PRICE_WEI)
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
            .max_fee_per_gas(GAS_PRICE_WEI)
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
            .max_fee_per_gas(GAS_PRICE_WEI)
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
            .max_fee_per_gas(GAS_PRICE_WEI)
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
    let data = IAgentReward::claimRewardCall {
        agentId: U256::ZERO,
    }
    .abi_encode();
    block_on(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url.parse()?);
        let max_fee = GAS_PRICE_WEI;
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

/// `amount` whole COEN in the chain's 18-decimal native base units.
pub(crate) fn coen(amount: u64) -> U256 {
    U256::from(amount) * U256::from(1_000_000_000_000_000_000u128)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(coen(1), U256::from(1_000_000_000_000_000_000u128));
        assert_eq!(coen(0), U256::ZERO);
    }
}
