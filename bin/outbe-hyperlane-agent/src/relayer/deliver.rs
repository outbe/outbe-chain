//! Delivery of one message: the reads on the destination chain, the metadata
//! for the multisig ISM and the `process` transaction.
//!
//! The flow follows hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/relayer/src/msg/pending_message.rs` and
//! `msg/metadata/multisig/message_id_multisig.rs` (Apache-2.0, see
//! `LICENSE-APACHE`), and the Classic submit path of
//! `rust/main/chains/hyperlane-ethereum/src/tx.rs`. Changes:
//! - Only the message-id multisig ISM, also behind routing ISMs.
//! - No cache of the contract reads.
//! - One transaction at a time for each chain, no batch.
//! - No gas price escalation and no gas payment check.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::EthereumWallet;
use alloy_primitives::{keccak256, Address, Bytes, B256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_sol_types::SolCall;
use eyre::Result;
use tracing::{debug, warn};

use crate::abi::{IIsm, IMailbox, IValidatorAnnounce};
use crate::client::{eth_call, send_lock, wait_receipt};
use crate::config::ReorgPeriod;
use crate::errors::RelayError;
use crate::sync::Origin;

/// `IInterchainSecurityModule.Types`.
const ISM_ROUTING: u8 = 1;
const ISM_MESSAGE_ID_MULTISIG: u8 = 5;
/// ISMs in a row that the relayer follows, as upstream `ISM_MAX_DEPTH`.
const ISM_MAX_DEPTH: usize = 13;
/// As upstream `MAX_VALIDATOR_SET_SIZE`.
const MAX_VALIDATOR_SET_SIZE: usize = 50;
/// Gas on top of the estimate, as upstream `GAS_LIMIT_BUFFER`.
const GAS_LIMIT_BUFFER: u64 = 75_000;
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(90);
/// The largest gas estimate of one delivery. The relayer has no gas payment
/// check yet, so `H` pays each delivery. Without a limit, a recipient that
/// uses all the gas of a block makes `H` pay for it, and `H` also pays the
/// liveness transactions. The default gas of `callRemote` is 200,000.
const MAX_DELIVERY_GAS: u64 = 3_000_000;
/// The fee cap of a delivery to Outbe, in base fees. See [`fee_cap`].
const OUTBE_FEE_CAP_BASE_FEES: u128 = 10;

/// The `maxFeePerGas` of a delivery. A transaction pays the base fee of its
/// block, not this cap: a high cap costs nothing while the chain is calm.
///
/// On Outbe the cap is 10 base fees. A delivery that stays in the pool,
/// because the base fee went above its cap, also stops every
/// `submitCheckpoint` behind it: the key has one nonce sequence, and ZeroFee
/// removes the fee of a transaction, not its place in that sequence. Missed
/// checkpoints lead to a jail.
fn fee_cap(estimate: u128, base_fee: u128, priority_fee: u128, outbe: bool) -> u128 {
    match outbe {
        true => estimate.max(
            base_fee
                .saturating_mul(OUTBE_FEE_CAP_BASE_FEES)
                .saturating_add(priority_fee),
        ),
        false => estimate,
    }
}

/// Length of one turn of the delivery order, in seconds: each next relayer
/// of a message waits one more turn before it sends. It is the same value for
/// every validator and every chain: a validator also uses it to decide if
/// another validator did not deliver a message in its turn, and all
/// validators must decide the same.
pub const RELAY_TURN_SECS: u64 = 40;

/// One running domain: the origin of its messages and the destination of the
/// messages of the other domains.
pub struct Chain {
    pub origin: Origin,
    pub providers: Vec<DynProvider>,
    pub period: ReorgPeriod,
    /// `HyperlaneController.ismByDomain` of this domain: the only ISM whose
    /// messages the relayer delivers to this chain.
    pub ism: Address,
    /// This chain is Outbe: the liveness sends `submitCheckpoint` here with
    /// the same key and the same nonce sequence.
    pub outbe: bool,
    /// The provider in use. It moves after an RPC error.
    current: AtomicUsize,
    /// One `process` transaction at a time: the relayers of all origins send
    /// with the same key.
    send: tokio::sync::Mutex<()>,
}

impl Chain {
    pub fn new(
        origin: Origin,
        providers: Vec<DynProvider>,
        period: ReorgPeriod,
        ism: Address,
        outbe: bool,
    ) -> Self {
        Self {
            origin,
            providers,
            period,
            ism,
            outbe,
            current: AtomicUsize::new(0),
            send: tokio::sync::Mutex::new(()),
        }
    }

    pub fn provider(&self) -> &DynProvider {
        &self.providers[self.current.load(Ordering::Relaxed) % self.providers.len()]
    }

    /// The index of the provider in use.
    pub fn endpoint(&self) -> usize {
        self.current.load(Ordering::Relaxed) % self.providers.len()
    }

    /// The next read uses the next endpoint.
    pub fn rotate(&self) {
        self.current.fetch_add(1, Ordering::Relaxed);
    }
}

/// The running domains. The daemon adds and removes them.
#[derive(Clone, Default)]
pub struct Chains(Arc<RwLock<HashMap<u32, Arc<Chain>>>>);

impl Chains {
    pub fn get(&self, domain: u32) -> Option<Arc<Chain>> {
        self.0.read().expect("chains lock").get(&domain).cloned()
    }

    pub fn insert(&self, chain: Arc<Chain>) {
        self.0
            .write()
            .expect("chains lock")
            .insert(chain.origin.domain, chain);
    }

    pub fn retain(&self, keep: impl Fn(u32) -> bool) {
        self.0
            .write()
            .expect("chains lock")
            .retain(|domain, _| keep(*domain));
    }
}

/// The place of `me` in the delivery order of a message: 0 sends at once,
/// each next place waits one more turn. Every relayer calculates the same
/// order from the active validators, so the relayers do not talk. The order
/// is different for each message: the work of a stopped relayer goes to all
/// the others in equal parts. `None`: `me` is not an active validator and
/// has no turn.
pub fn turn_position(active: &[Address], me: Address, message_id: B256) -> Option<usize> {
    if !active.contains(&me) {
        return None;
    }
    let mine = turn_rank(message_id, &me);
    Some(
        active
            .iter()
            .filter(|v| turn_rank(message_id, v) < mine)
            .count(),
    )
}

/// The validator that has the first turn of a message: it sends at once.
pub fn turn_owner(active: &[Address], message_id: B256) -> Option<Address> {
    active
        .iter()
        .copied()
        .min_by_key(|validator| turn_rank(message_id, validator))
}

/// The place of `validator` in the delivery order of a message: a lower value
/// sends earlier.
fn turn_rank(message_id: B256, validator: &Address) -> B256 {
    let mut buf = [0u8; 52];
    buf[..32].copy_from_slice(message_id.as_slice());
    buf[32..].copy_from_slice(validator.as_slice());
    keccak256(buf)
}

pub async fn delivered(
    provider: &DynProvider,
    mailbox: Address,
    message_id: B256,
    block: BlockId,
) -> Result<bool> {
    let call = IMailbox::deliveredCall {
        messageId: message_id,
    };
    let out = eth_call(provider, mailbox, call.abi_encode(), block).await?;
    Ok(IMailbox::deliveredCall::abi_decode_returns(&out)?)
}

/// The validators and the threshold of the ISM that checks `message` on the
/// destination chain. The ISM must be `trusted`, the ISM of the
/// HyperlaneController.
///
/// The recipient contract names its ISM, and the sender chooses the
/// recipient. Without this check, any sender gives the relayer a validator
/// set of its own: the relayer then reads the storage locations that the
/// sender announced, and pays the gas for a message that no Outbe validator
/// checked.
pub async fn multisig_ism(
    provider: &DynProvider,
    mailbox: Address,
    recipient: B256,
    message: &[u8],
    trusted: Address,
) -> Result<(Vec<Address>, usize)> {
    let call = |to: Address, data: Vec<u8>| eth_call(provider, to, data, BlockId::latest());
    let message = Bytes::copy_from_slice(message);
    let recipient = Address::from_word(recipient);
    let out = call(
        mailbox,
        IMailbox::recipientIsmCall { recipient }.abi_encode(),
    )
    .await?;
    let mut ism = IMailbox::recipientIsmCall::abi_decode_returns(&out)?;
    for _ in 0..ISM_MAX_DEPTH {
        let out = call(ism, IIsm::moduleTypeCall {}.abi_encode()).await?;
        match IIsm::moduleTypeCall::abi_decode_returns(&out)? {
            ISM_ROUTING => {
                let route = IIsm::routeCall {
                    message: message.clone(),
                };
                let out = call(ism, route.abi_encode()).await?;
                ism = IIsm::routeCall::abi_decode_returns(&out)?;
            }
            ISM_MESSAGE_ID_MULTISIG => {
                if ism != trusted {
                    return Err(RelayError::ForeignIsm { ism, trusted }.into());
                }
                let read = IIsm::validatorsAndThresholdCall { message };
                let out = call(ism, read.abi_encode()).await?;
                let set = IIsm::validatorsAndThresholdCall::abi_decode_returns(&out)?;
                let count = set.validators.len();
                if count == 0 || count > MAX_VALIDATOR_SET_SIZE || set.threshold == 0 {
                    return Err(RelayError::BadValidatorSet {
                        ism,
                        validators: count,
                        threshold: set.threshold,
                    }
                    .into());
                }
                return Ok((set.validators, usize::from(set.threshold)));
            }
            module => return Err(RelayError::UnsupportedIsm { ism, module }.into()),
        }
    }
    Err(RelayError::UnsupportedIsm {
        ism,
        module: ISM_ROUTING,
    }
    .into())
}

/// True when the recipient has code on the destination chain. A message to
/// an address without code can never be delivered.
pub async fn is_contract(provider: &DynProvider, recipient: B256) -> Result<bool> {
    let code = provider.get_code_at(Address::from_word(recipient)).await?;
    Ok(!code.is_empty())
}

/// The announced storage locations of each validator on the origin chain.
pub async fn announced(
    provider: &DynProvider,
    validator_announce: Address,
    validators: &[Address],
) -> Result<Vec<Vec<String>>> {
    let call = IValidatorAnnounce::getAnnouncedStorageLocationsCall {
        validators: validators.to_vec(),
    };
    let out = eth_call(
        provider,
        validator_announce,
        call.abi_encode(),
        BlockId::latest(),
    )
    .await?;
    Ok(IValidatorAnnounce::getAnnouncedStorageLocationsCall::abi_decode_returns(&out)?)
}

/// The metadata of the message-id multisig ISM:
/// `hook (32) || root (32) || index (4) || signatures (65 each)`.
pub fn metadata(hook: Address, root: B256, index: u32, signatures: &[[u8; 65]]) -> Vec<u8> {
    let mut v = Vec::with_capacity(68 + 65 * signatures.len());
    v.extend_from_slice(hook.into_word().as_slice());
    v.extend_from_slice(root.as_slice());
    v.extend_from_slice(&index.to_be_bytes());
    for signature in signatures {
        v.extend_from_slice(signature);
    }
    v
}

/// Sends `process(metadata, message)` to the destination Mailbox and waits
/// for the receipt. Returns `None` when an earlier transaction of the key is
/// still in the pool or the receipt does not come in time: the caller looks
/// again on its next tick. An error means that the destination rejects the
/// message now.
pub async fn process(
    dest: &Chain,
    wallet: &EthereumWallet,
    metadata: Vec<u8>,
    message: &[u8],
) -> Result<Option<B256>> {
    let _one_per_chain = dest.send.lock().await;
    let provider = dest.provider();
    let from = wallet.default_signer().address();
    let call = IMailbox::processCall {
        metadata: metadata.into(),
        message: Bytes::copy_from_slice(message),
    };
    let tx = TransactionRequest::default()
        .from(from)
        .to(dest.origin.mailbox)
        .input(Bytes::from(call.abi_encode()).into());
    // The dry run immediately before the send: a message that another
    // relayer delivered, or that the ISM rejects, reverts here and costs no gas.
    let gas = provider.estimate_gas(tx.clone()).await?;
    if gas > MAX_DELIVERY_GAS {
        return Err(RelayError::GasAboveLimit {
            gas,
            limit: MAX_DELIVERY_GAS,
        }
        .into());
    }
    let mut tx = tx.gas_limit(gas.saturating_add(GAS_LIMIT_BUFFER));
    // As upstream: a chain with no base fee or a zero base fee gets a legacy
    // transaction with the gas price of the node.
    let base_fee = provider
        .get_fee_history(1, BlockNumberOrTag::Latest, &[])
        .await
        .ok()
        .and_then(|history| history.latest_block_base_fee());
    match base_fee {
        None | Some(0) => tx = tx.gas_price(provider.get_gas_price().await?),
        Some(base_fee) => {
            let fees = provider.estimate_eip1559_fees().await?;
            let priority = fees.max_priority_fee_per_gas;
            tx = tx
                .max_priority_fee_per_gas(priority)
                .max_fee_per_gas(fee_cap(
                    fees.max_fee_per_gas,
                    base_fee,
                    priority,
                    dest.outbe,
                ));
        }
    }
    let lock = send_lock(provider.get_chain_id().await?);
    let sent = {
        let _one_at_a_time = lock.lock().await;
        // ponytail: no gas price escalation; a transaction that stays in the pool stops the deliveries to this chain until a block includes it
        let pending = provider.get_transaction_count(from).pending().await?;
        if pending > provider.get_transaction_count(from).await? {
            debug!(domain = dest.origin.domain, %from, "an earlier transaction is still in the pool; the delivery waits");
            return Ok(None);
        }
        ProviderBuilder::new()
            .wallet(wallet.clone())
            .connect_provider(provider.clone())
            .send_transaction(tx)
            .await?
    };
    let hash = *sent.tx_hash();
    match wait_receipt(provider, hash, RECEIPT_TIMEOUT).await {
        Ok(receipt) if receipt.status() => Ok(Some(hash)),
        Ok(_) => Err(RelayError::Reverted(hash).into()),
        Err(error) => {
            warn!(domain = dest.origin.domain, %hash, %error, "no receipt yet");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_turn_owner_has_position_zero() {
        let active: Vec<Address> = (1..=5).map(Address::repeat_byte).collect();
        for n in 0..20u8 {
            let id = B256::repeat_byte(n);
            let owner = turn_owner(&active, id).unwrap();
            assert_eq!(turn_position(&active, owner, id), Some(0));
        }
        assert_eq!(turn_owner(&[], B256::ZERO), None);
    }

    #[test]
    fn turn_order_is_a_permutation_that_depends_on_the_message() {
        let active: Vec<Address> = (1..=5).map(Address::repeat_byte).collect();
        let places = |id: u8| -> Vec<usize> {
            active
                .iter()
                .map(|v| turn_position(&active, *v, B256::repeat_byte(id)).unwrap())
                .collect()
        };
        let mut sorted = places(1);
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4]);
        // The same order on every relayer, another order for another message.
        assert_eq!(places(1), places(1));
        assert!((2..20).any(|id| places(id) != places(1)));
        // A relayer that is not an active validator has no turn.
        let outsider = Address::repeat_byte(9);
        assert_eq!(turn_position(&active, outsider, B256::ZERO), None);
    }

    #[test]
    fn fee_cap_of_outbe_is_ten_base_fees() {
        // Another chain: the estimate of the node, here 2 base fees + tip.
        assert_eq!(fee_cap(21, 10, 1, false), 21);
        // Outbe: 10 base fees + tip, so the delivery passes a base fee of 100.
        assert_eq!(fee_cap(21, 10, 1, true), 101);
        // A higher estimate stays.
        assert_eq!(fee_cap(500, 10, 1, true), 500);
    }

    #[test]
    fn metadata_layout() {
        let hook = Address::repeat_byte(0x11);
        let m = metadata(hook, B256::repeat_byte(0x22), 7, &[[3; 65], [4; 65]]);
        assert_eq!(m.len(), 68 + 130);
        assert_eq!(&m[..12], &[0; 12]);
        assert_eq!(&m[12..32], hook.as_slice());
        assert_eq!(&m[32..64], &[0x22; 32]);
        assert_eq!(&m[64..68], &[0, 0, 0, 7]);
        assert_eq!(&m[68..133], &[3; 65]);
        assert_eq!(&m[133..], &[4; 65]);
    }
}
