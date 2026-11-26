//! Checks of the other validators for the report of the liveness call.
//!
//! The relayer role reads the signed checkpoints of the validators when it
//! delivers a message. Here it reads them one time in each epoch for the
//! report: a validator that does not serve its checkpoint goes to
//! `notServing`. It also reads who delivered one message of each validator: a
//! validator that did not deliver in its turn goes to `notDelivered`.

use std::collections::HashSet;
use std::sync::Arc;

use alloy_eips::BlockId;
use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider};
use alloy_sol_types::SolCall;
use eyre::{eyre, Result};
use futures::future::join_all;

use crate::abi::IMailbox;
use crate::client::eth_call;
use crate::config::ReorgPeriod;
use crate::relayer::deliver::{announced, turn_owner, Chain, Chains, RELAY_TURN_SECS};
use crate::relayer::dispatched;
use crate::relayer::fetch::{Fetcher, Want};
use crate::relayer::message::Message;
use crate::store::{Leaf, Store};
use crate::sync::finalized_block;

/// The most messages that one delivery check reads.
// ponytail: one log read for each message; read block ranges if this is slow
const MAX_DELIVERY_WALK: u32 = 256;

/// The most addresses in the two lists of one report. A free `submitReport`
/// has 1024 bytes of calldata at most (ZeroFee): the call with empty lists
/// has 260 bytes, and each address adds 32 bytes.
pub const MAX_REPORT_ADDRESSES: usize = 23;

/// One active validator.
#[derive(Clone, Copy, Debug)]
pub struct ActiveValidator {
    /// The validator address `V`.
    pub validator: Address,
    /// The key that signs its checkpoints: `hyperlaneSigner(V)`, or `V`.
    pub signer: Address,
}

/// What the report checks need from the relayer role.
pub struct ReportChecks {
    pub chains: Chains,
    pub fetcher: Arc<Fetcher>,
    pub store: Arc<Store>,
}

impl ReportChecks {
    /// The validators of `others` that do not serve the checkpoint at `index`
    /// of `domain` with the root `root`. `others` is the active set without
    /// this validator. The result is in address order.
    ///
    /// An error means that the check did not run: the origin chain did not
    /// answer, or no other validator answers. When none answers, the cause can be
    /// the network of this agent, so the agent does not name them all.
    pub async fn not_serving(
        &self,
        others: &[ActiveValidator],
        domain: u32,
        index: u32,
        message_id: B256,
        root: B256,
    ) -> Result<Vec<Address>> {
        let chain = self
            .chains
            .get(domain)
            .ok_or_else(|| eyre!("domain {domain} is not running"))?;
        let want = Want {
            domain,
            hook: chain.origin.hook,
            index,
            message_id,
        };
        let signers: Vec<Address> = others.iter().map(|other| other.signer).collect();
        let locations =
            announced(chain.provider(), chain.origin.validator_announce, &signers).await?;
        let serves =
            join_all(others.iter().zip(&locations).map(|(other, locations)| {
                self.fetcher.serves(other.signer, locations, &want, root)
            }))
            .await;
        let mut bad: Vec<Address> = others
            .iter()
            .zip(serves)
            .filter(|(_, serves)| !serves)
            .map(|(other, _)| other.validator)
            .collect();
        if others.len() > 1 && bad.len() == others.len() {
            return Err(eyre!(
                "no validator serves the checkpoint at index {index} of domain {domain}"
            ));
        }
        bad.sort_unstable();
        Ok(bad)
    }
}

impl ReportChecks {
    /// The validators that did not deliver a message of `domain` in their
    /// turn. `validators` is the active set, with this validator `me`. The
    /// result is in address order.
    ///
    /// The check starts at the message index `from`, the `deliveryIndex` of
    /// the controller, and goes up to `target`. For each validator it looks
    /// at the first message whose first turn that validator has. This
    /// validator does not check itself.
    ///
    /// An error means that the check did not run.
    pub async fn not_delivered(
        &self,
        validators: &[ActiveValidator],
        me: Address,
        domain: u32,
        from: u32,
        target: u32,
    ) -> Result<Vec<Address>> {
        let origin = self
            .chains
            .get(domain)
            .ok_or_else(|| eyre!("domain {domain} is not running"))?;
        // The time at which a message is final comes from a block count. A
        // chain with a finality tag has no such count: no delivery check.
        let ReorgPeriod::Blocks(reorg) = origin.period else {
            return Ok(Vec::new());
        };
        let active: Vec<Address> = validators.iter().map(|v| v.validator).collect();
        let mut checked = HashSet::from([me]);
        let mut bad = Vec::new();
        let end = target.min(from.saturating_add(MAX_DELIVERY_WALK - 1));
        for index in from..=end {
            if checked.len() >= active.len() {
                break;
            }
            let Some(leaf) = self.store.leaf(domain, index)? else {
                break;
            };
            let Some(owner) = turn_owner(&active, leaf.message_id) else {
                break;
            };
            if !checked.insert(owner) {
                continue;
            }
            if self
                .missed_turn(&origin, reorg, &leaf, owner, validators)
                .await?
            {
                bad.push(owner);
            }
        }
        bad.sort_unstable();
        Ok(bad)
    }

    /// True when another validator delivered the message of `leaf` after the
    /// turn of `owner`. Only chain data decides, so every agent gets the
    /// same result:
    /// - the message is ready at the time of the origin block that makes its
    ///   block final;
    /// - the turn of `owner` ends [`RELAY_TURN_SECS`] later;
    /// - the delivery time is the time of the destination block of `process`.
    ///
    /// False when nobody delivered the message in a final block, when
    /// `owner` delivered it, or when an account that is no active validator
    /// delivered it.
    async fn missed_turn(
        &self,
        origin: &Chain,
        reorg: u64,
        leaf: &Leaf,
        owner: Address,
        validators: &[ActiveValidator],
    ) -> Result<bool> {
        let mailbox = origin.origin.mailbox;
        let logs = dispatched(origin.provider(), mailbox, leaf.block, leaf.block);
        let Some(raw) = logs.await?.remove(&leaf.message_id) else {
            return Ok(false);
        };
        let Some(destination) = self.chains.get(Message::decode(&raw)?.destination) else {
            return Ok(false);
        };
        let provider = destination.provider();
        let mailbox = destination.origin.mailbox;
        let block = BlockId::number(finalized_block(provider, destination.period).await?);
        let call = IMailbox::processorCall {
            messageId: leaf.message_id,
        };
        let out = eth_call(provider, mailbox, call.abi_encode(), block).await?;
        let processor = IMailbox::processorCall::abi_decode_returns(&out)?;
        if !by_another_validator(validators, processor, owner) {
            return Ok(false);
        }
        let call = IMailbox::processedAtCall {
            messageId: leaf.message_id,
        };
        let out = eth_call(provider, mailbox, call.abi_encode(), block).await?;
        let processed_at: u64 = IMailbox::processedAtCall::abi_decode_returns(&out)?.to();
        let delivered = block_time(provider, processed_at).await?;
        let ready = block_time(origin.provider(), leaf.block + reorg).await?;
        Ok(after_turn(delivered, ready))
    }
}

/// True when the account `processor` is the signer of an active validator
/// that is not `owner`. False for the zero address (nobody delivered), for
/// the signer of `owner`, and for an account that is no active validator.
fn by_another_validator(
    validators: &[ActiveValidator],
    processor: Address,
    owner: Address,
) -> bool {
    validators
        .iter()
        .find(|v| v.signer == processor)
        .is_some_and(|v| v.validator != owner)
}

/// True when a delivery at the time `delivered` is after the first turn of a
/// message that was ready at the time `ready`.
fn after_turn(delivered: u64, ready: u64) -> bool {
    delivered > ready + RELAY_TURN_SECS
}

/// The time of block `number`.
async fn block_time(provider: &DynProvider, number: u64) -> Result<u64> {
    Ok(provider
        .get_block_by_number(number.into())
        .await?
        .ok_or_else(|| eyre!("block {number} is not found"))?
        .header
        .timestamp)
}

/// Cuts the two lists of a report to [`MAX_REPORT_ADDRESSES`] together. Each
/// list is in address order, so all agents keep the same addresses.
pub fn fit_report(not_serving: &mut Vec<Address>, not_delivered: &mut Vec<Address>) {
    not_serving.truncate(MAX_REPORT_ADDRESSES);
    not_delivered.truncate(MAX_REPORT_ADDRESSES - not_serving.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::IHyperlaneController;

    #[test]
    fn only_another_validator_after_the_turn_is_a_missed_turn() {
        let validator = |n: u8| ActiveValidator {
            validator: Address::repeat_byte(n),
            signer: Address::repeat_byte(n + 0x10),
        };
        let validators = [validator(1), validator(2)];
        let owner = Address::repeat_byte(1);
        // The signer of another active validator.
        assert!(by_another_validator(
            &validators,
            Address::repeat_byte(0x12),
            owner
        ));
        // The owner of the turn, nobody, and an account that is no validator.
        assert!(!by_another_validator(
            &validators,
            Address::repeat_byte(0x11),
            owner
        ));
        assert!(!by_another_validator(&validators, Address::ZERO, owner));
        assert!(!by_another_validator(
            &validators,
            Address::repeat_byte(0x99),
            owner
        ));
        // The validator address is not the signer.
        assert!(!by_another_validator(
            &validators,
            Address::repeat_byte(2),
            owner
        ));

        // The message was ready at 1000: the first turn ends at 1040.
        assert!(!after_turn(1_005, 1_000));
        assert!(!after_turn(1_040, 1_000));
        assert!(after_turn(1_041, 1_000));
    }

    #[test]
    fn a_report_has_the_most_addresses_that_fit() {
        let addresses = |n: u8| (0..n).map(Address::repeat_byte).collect::<Vec<_>>();
        let (mut serving, mut delivered) = (addresses(15), addresses(15));
        fit_report(&mut serving, &mut delivered);
        assert_eq!((serving.len(), delivered.len()), (15, 8));
        let (mut serving, mut delivered) = (addresses(30), addresses(3));
        fit_report(&mut serving, &mut delivered);
        assert_eq!((serving.len(), delivered.len()), (23, 0));
        // The longest call fits in the free calldata of ZeroFee.
        let call = IHyperlaneController::submitReportCall {
            domain: 1,
            index: 1,
            root: B256::ZERO,
            latestIndex: 1,
            notServing: serving,
            notDelivered: delivered,
        };
        assert!(call.abi_encode().len() <= 1024);
    }
}
