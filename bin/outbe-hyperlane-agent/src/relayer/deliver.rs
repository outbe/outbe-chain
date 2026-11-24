//! Delivery of one message: the reads on the destination chain, the metadata
//! for the multisig ISM and the `process` transaction.
//!
//! The flow follows hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/relayer/src/msg/pending_message.rs` and
//! `msg/metadata/multisig/message_id_multisig.rs` (Apache-2.0, see
//! `LICENSE-APACHE`). Changes: only the message-id multisig ISM (also behind
//! a routing ISM), one transaction at a time for each chain, no gas price
//! escalation, no batch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{keccak256, Address, Bytes, B256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_sol_types::SolCall;
use eyre::Result;
use tracing::warn;

use crate::abi::{IIsm, IMailbox, IValidatorAnnounce};
use crate::client::{eth_call, wait_receipt};
use crate::config::ReorgPeriod;
use crate::errors::RelayError;
use crate::sync::Origin;

/// `IInterchainSecurityModule.Types`.
const ISM_ROUTING: u8 = 1;
const ISM_MESSAGE_ID_MULTISIG: u8 = 5;
/// Routing ISMs in a row that the relayer follows.
const MAX_ROUTES: usize = 4;
/// Gas on top of the estimate, as upstream `GAS_LIMIT_BUFFER`.
const GAS_LIMIT_BUFFER: u64 = 75_000;
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(90);

/// One running domain: the origin of its messages and the destination of the
/// messages of the other domains.
pub struct Chain {
    pub origin: Origin,
    pub providers: Vec<DynProvider>,
    pub period: ReorgPeriod,
    /// Length of one turn of the delivery order, in seconds.
    pub turn_secs: u64,
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
        turn_secs: u64,
    ) -> Self {
        Self {
            origin,
            providers,
            period,
            turn_secs,
            current: AtomicUsize::new(0),
            send: tokio::sync::Mutex::new(()),
        }
    }

    pub fn provider(&self) -> &DynProvider {
        &self.providers[self.current.load(Ordering::Relaxed) % self.providers.len()]
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
/// the others in equal parts. A relayer that is not an active validator is
/// after all of them.
pub fn turn_position(active: &[Address], me: Address, message_id: B256) -> usize {
    let rank = |validator: &Address| {
        let mut buf = [0u8; 52];
        buf[..32].copy_from_slice(message_id.as_slice());
        buf[32..].copy_from_slice(validator.as_slice());
        keccak256(buf)
    };
    if !active.contains(&me) {
        return active.len();
    }
    let mine = rank(&me);
    active.iter().filter(|v| rank(v) < mine).count()
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
/// destination chain.
pub async fn multisig_ism(
    provider: &DynProvider,
    mailbox: Address,
    recipient: B256,
    message: &[u8],
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
    for _ in 0..MAX_ROUTES {
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
                let read = IIsm::validatorsAndThresholdCall { message };
                let out = call(ism, read.abi_encode()).await?;
                let set = IIsm::validatorsAndThresholdCall::abi_decode_returns(&out)?;
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
    let _one_at_a_time = dest.send.lock().await;
    let provider = dest.provider();
    let from = wallet.default_signer().address();
    // ponytail: no gas price escalation; a transaction that stays in the pool stops the deliveries to this chain until a block includes it
    let pending = provider.get_transaction_count(from).pending().await?;
    if pending > provider.get_transaction_count(from).await? {
        warn!(domain = dest.origin.domain, %from, "an earlier transaction is still in the pool; the delivery waits");
        return Ok(None);
    }
    let call = IMailbox::processCall {
        metadata: metadata.into(),
        message: Bytes::copy_from_slice(message),
    };
    let tx = TransactionRequest::default()
        .from(from)
        .to(dest.origin.mailbox)
        .input(Bytes::from(call.abi_encode()).into());
    let gas = provider.estimate_gas(tx.clone()).await?;
    let sender = ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect_provider(provider.clone());
    let sent = sender
        .send_transaction(tx.gas_limit(gas.saturating_add(GAS_LIMIT_BUFFER)))
        .await?;
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
    fn turn_order_is_a_permutation_that_depends_on_the_message() {
        let active: Vec<Address> = (1..=5).map(Address::repeat_byte).collect();
        let places = |id: u8| -> Vec<usize> {
            active
                .iter()
                .map(|v| turn_position(&active, *v, B256::repeat_byte(id)))
                .collect()
        };
        let mut sorted = places(1);
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4]);
        // The same order on every relayer, another order for another message.
        assert_eq!(places(1), places(1));
        assert!((2..20).any(|id| places(id) != places(1)));
        // A relayer that is not active is after all active validators.
        let outsider = Address::repeat_byte(9);
        assert_eq!(turn_position(&active, outsider, B256::ZERO), 5);
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
