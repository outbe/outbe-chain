//! The relayer role: delivers the messages of the origin chains to their
//! destination chains.

pub mod deliver;
pub mod fetch;
pub mod message;
pub mod report;
pub mod retry;
pub mod take_in;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider};
use alloy_sol_types::SolCall;
use eyre::Result;
use tracing::{debug, error, info, warn};

use crate::abi::IValidatorSet;
use crate::client::eth_call;
use crate::errors::RelayError;
use crate::http::health::ValidatorHealth;
use crate::store::{RelayRow, Store};
use crate::sync::{finalized_block, Origin};
use crate::validator::liveness::VALIDATOR_SET_ADDRESS;
use crate::validator::signer::DomainSigner;

use deliver::{
    announced, delivered, is_contract, metadata, multisig_ism, process, turn_position, Chain,
    Chains, RELAY_TURN_SECS,
};
use fetch::{Fetcher, Quorum, Want};
use message::{message_id, Message};
use retry::{backoff, SIGNATURE_FAST_RETRIES};
pub(crate) use take_in::dispatched;
use take_in::TakeIn;

/// Interval of the delivered check of a dropped row.
const DROPPED_CHECK_SECS: u64 = 3600;

/// What the relayer does with a relay row after one look at it.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The message is delivered in a final block: the row goes away.
    Done,
    /// Nothing to do now: another relayer has the turn, or the delivery
    /// waits for a final block.
    Wait,
    /// The attempt failed. `fast`: only the validator signatures are missing.
    Retry { fast: bool, reason: String },
    /// The message can never be delivered: no more attempts.
    Drop(String),
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The rows in the order of the upstream queue: a message with no failed
/// attempt first, by leaf index, then the retries by the time of their next
/// attempt. A retry does not delay a new message.
fn in_queue_order(mut rows: Vec<(u32, RelayRow)>) -> Vec<(u32, RelayRow)> {
    rows.sort_by_key(|(index, row)| match row.attempts {
        0 => (0, 0, *index),
        _ => (1, row.next_attempt, *index),
    });
    rows
}

/// The relayer of one origin domain: takes the messages in and delivers them
/// to their destination chains.
pub struct Relayer {
    pub origin: Origin,
    pub store: Arc<Store>,
    pub chains: Chains,
    pub fetcher: Arc<Fetcher>,
    /// Outbe RPC: the active validators give the delivery order.
    pub outbe: DynProvider,
    /// The key `H`. It pays the gas on each destination chain.
    pub wallet: EthereumWallet,
    /// Validator `V`: the place of this relayer in the delivery order. It
    /// sends only while `V` is an active validator.
    pub validator: Address,
    pub health: Arc<ValidatorHealth>,
    /// The validator sync of this domain: the take-in starts when the sync
    /// is at the head of the chain.
    pub signer: Arc<DomainSigner>,
}

impl Relayer {
    /// Runs forever. An error of a tick is logged and the next tick tries
    /// again: the relayer must never stop the validator role.
    pub async fn run(self, interval: Duration) {
        let domain = self.origin.domain;
        let mut take_in = TakeIn::new(self.origin.clone(), self.store.clone());
        loop {
            let pause = match self.tick(&mut take_in, unix_now()).await {
                Ok(next_attempt) => self.pause(interval, next_attempt),
                Err(error) => {
                    warn!(domain, error = format!("{error:#}"), "relayer tick failed");
                    interval
                }
            };
            tokio::time::sleep(pause).await;
        }
    }

    /// The time to the next tick: the poll interval, or less when a retry is
    /// due earlier. The fast retries of 2 seconds need this.
    fn pause(&self, interval: Duration, next_attempt: Option<u64>) -> Duration {
        match next_attempt {
            Some(at) => interval.min(Duration::from_secs(at.saturating_sub(unix_now()).max(1))),
            None => interval,
        }
    }

    /// One pass: takes new messages in, then looks at each row that is due.
    /// Returns the earliest time of a retry that is not due yet.
    async fn tick(&self, take_in: &mut TakeIn, now: u64) -> Result<Option<u64>> {
        let domain = self.origin.domain;
        let chain = self.chains.get(domain).ok_or(RelayError::NoChain(domain))?;
        // The operator sees in the status when `H` has no gas on this chain.
        let signer = self.wallet.default_signer().address();
        match chain.provider().get_balance(signer).await {
            Ok(wei) => self.health.set_balance(domain, wei),
            Err(error) => debug!(domain, %error, "balance read failed"),
        }
        // A take-in error must not stop the rows that are already in the queue.
        let endpoint = (chain.endpoint(), chain.providers.len());
        if let Err(error) = take_in
            .tick(chain.provider(), now, self.signer.synced(), endpoint)
            .await
        {
            warn!(
                domain,
                error = format!("{error:#}"),
                "take-in of new messages failed"
            );
            chain.rotate();
        }
        let rows = in_queue_order(self.store.relay_rows(domain)?);
        // A dropped row is due at a slow rate, only for the delivered check.
        let due = |row: &RelayRow| row.next_attempt <= now;
        if rows.iter().any(|(_, row)| due(row)) {
            let out = eth_call(
                &self.outbe,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::getActiveValidatorsCall {}.abi_encode(),
                BlockId::latest(),
            )
            .await?;
            let active = IValidatorSet::getActiveValidatorsCall::abi_decode_returns(&out)?;
            for (index, row) in rows {
                if !due(&row) {
                    continue;
                }
                let id = message_id(&row.message);
                if row.dropped {
                    self.look_at_dropped(index, row, id, now).await;
                    continue;
                }
                // As upstream, every error counts as a failed attempt, also
                // an RPC error: the relayer does not sort the errors.
                let outcome = match self.look(index, &row, id, now, &active).await {
                    Ok(outcome) => outcome,
                    Err(error) => Outcome::Retry {
                        fast: false,
                        reason: format!("{error:#}"),
                    },
                };
                self.record(index, row, id, outcome)?;
            }
        }
        let now = unix_now();
        Ok(self
            .store
            .relay_rows(domain)?
            .iter()
            .filter(|(_, row)| !row.dropped && row.next_attempt > now)
            .map(|(_, row)| row.next_attempt)
            .min())
    }

    /// A dropped row goes away when someone delivered the message, for
    /// example the operator by hand. The relayer checks this once an hour.
    async fn look_at_dropped(&self, index: u32, mut row: RelayRow, id: B256, now: u64) {
        let domain = self.origin.domain;
        let final_delivery = async {
            let message = Message::decode(&row.message)?;
            let dest = self
                .chains
                .get(message.destination)
                .ok_or(RelayError::NoChain(message.destination))?;
            let last_final = finalized_block(dest.provider(), dest.period).await?;
            let mailbox = dest.origin.mailbox;
            delivered(dest.provider(), mailbox, id, BlockId::number(last_final)).await
        };
        let stored = match final_delivery.await {
            Ok(true) => {
                info!(domain, index, %id, "dropped message is delivered and final");
                self.store.delete_relay(domain, index)
            }
            _ => {
                row.next_attempt = now + DROPPED_CHECK_SECS;
                self.store.put_relay(domain, index, &row)
            }
        };
        if let Err(error) = stored {
            warn!(domain, index, %error, "relay row write failed");
        }
    }

    /// Writes the result of one look into the relay row.
    fn record(&self, index: u32, mut row: RelayRow, id: B256, outcome: Outcome) -> Result<()> {
        let domain = self.origin.domain;
        match outcome {
            Outcome::Done => {
                info!(domain, index, %id, "message is delivered and final");
                self.store.delete_relay(domain, index)
            }
            Outcome::Wait => Ok(()),
            Outcome::Retry { fast, reason } => {
                row.attempts += 1;
                match backoff(row.attempts, fast, id) {
                    // The pause starts when the attempt fails, not when the
                    // tick started: one tick can run for minutes.
                    Some(pause) => {
                        row.next_attempt = unix_now() + pause.as_secs();
                        let (attempts, pause) = (row.attempts, pause.as_secs());
                        // The relayer is often faster than the signatures of
                        // the validators: the fast retries are no warning.
                        if fast && attempts <= SIGNATURE_FAST_RETRIES {
                            debug!(domain, index, %id, attempts, pause, reason, "delivery attempt failed");
                        } else {
                            warn!(domain, index, %id, attempts, pause, reason, "delivery attempt failed");
                        }
                    }
                    None => {
                        row.dropped = true;
                        error!(domain, index, %id, attempts = row.attempts, reason, "message dropped after the last attempt; anyone can still deliver it by hand");
                    }
                }
                self.store.put_relay(domain, index, &row)
            }
            Outcome::Drop(reason) => {
                row.dropped = true;
                error!(domain, index, %id, reason, "message dropped: it can never be delivered");
                self.store.put_relay(domain, index, &row)
            }
        }
    }

    /// Checks one message and delivers it when this relayer has the turn.
    /// The steps follow the upstream `prepare`, `submit` and `confirm`.
    async fn look(
        &self,
        index: u32,
        row: &RelayRow,
        id: B256,
        now: u64,
        active: &[Address],
    ) -> Result<Outcome> {
        let message = Message::decode(&row.message)?;
        let Some(dest) = self.chains.get(message.destination) else {
            return Ok(Outcome::Retry {
                fast: false,
                reason: format!("destination domain {} does not run", message.destination),
            });
        };
        // After an error the next look reads from the next endpoint.
        let rotate = |error| {
            dest.rotate();
            error
        };
        let my_turn = self.my_turn(&dest, row, id, now, active);
        if let ControlFlow::Break(outcome) = my_turn.await.map_err(rotate)? {
            return Ok(outcome);
        }
        let ism = self.controller_ism(&dest, &message, &row.message);
        let (validators, threshold) = match ism.await.map_err(rotate)? {
            ControlFlow::Continue(set) => set,
            ControlFlow::Break(outcome) => return Ok(outcome),
        };
        let quorum = self.signed_root(index, id, &validators, threshold);
        let (root, signatures) = match quorum.await? {
            ControlFlow::Continue(signed) => signed,
            ControlFlow::Break(outcome) => return Ok(outcome),
        };
        let metadata = metadata(self.origin.hook, root, index, &signatures);
        let sent = process(&dest, &self.wallet, metadata, &row.message);
        if let Some(hash) = sent.await.map_err(rotate)? {
            self.health
                .record_delivery(self.origin.domain, index, message.destination, hash);
            info!(domain = self.origin.domain, index, %id, destination = message.destination, %hash, "message delivered; waiting for a final block");
        }
        Ok(Outcome::Wait)
    }

    /// The checks before a send. `Break`: the message is delivered, or this
    /// relayer does not have the turn.
    async fn my_turn(
        &self,
        dest: &Chain,
        row: &RelayRow,
        id: B256,
        now: u64,
        active: &[Address],
    ) -> Result<ControlFlow<Outcome>> {
        let mailbox = dest.origin.mailbox;
        let provider = dest.provider();
        let last_final = finalized_block(provider, dest.period).await?;
        // A reorg of the destination can undo a delivery: only a final block
        // ends the work on a message. Upstream waits 10 minutes for this.
        if delivered(provider, mailbox, id, BlockId::number(last_final)).await? {
            return Ok(ControlFlow::Break(Outcome::Done));
        }
        let delivered = delivered(provider, mailbox, id, BlockId::latest()).await?;
        // A validator that is not active does not send: it only removes
        // its rows when another relayer delivers.
        let turn = turn_position(active, self.validator, id)
            .map(|place| row.first_seen + place as u64 * RELAY_TURN_SECS);
        Ok(match turn {
            Some(turn) if !delivered && now >= turn => ControlFlow::Continue(()),
            _ => ControlFlow::Break(Outcome::Wait),
        })
    }

    /// The validators and the threshold of the controller ISM that checks
    /// `message`. `Break`: the message can never be delivered.
    async fn controller_ism(
        &self,
        dest: &Chain,
        message: &Message,
        raw: &[u8],
    ) -> Result<ControlFlow<Outcome, (Vec<Address>, usize)>> {
        let mailbox = dest.origin.mailbox;
        let provider = dest.provider();
        if !is_contract(provider, message.recipient).await? {
            return Ok(ControlFlow::Break(Outcome::Drop(format!(
                "the recipient {} has no code on domain {}",
                message.recipient, message.destination
            ))));
        }
        // A message whose ISM is not the controller ISM is dropped at once.
        // The controller ISM of a domain is fixed: if it changes, the daemon
        // stops the relayer of that domain before this point.
        match multisig_ism(provider, mailbox, message.recipient, raw, dest.ism).await {
            Ok(set) => Ok(ControlFlow::Continue(set)),
            Err(error) if matches!(error.downcast_ref(), Some(RelayError::ForeignIsm { .. })) => {
                Ok(ControlFlow::Break(Outcome::Drop(format!("{error:#}"))))
            }
            Err(error) => Err(error),
        }
    }

    /// The root that the threshold of `validators` signed at `index`, and
    /// their signatures. `Break`: the relayer must try again later.
    async fn signed_root(
        &self,
        index: u32,
        id: B256,
        validators: &[Address],
        threshold: usize,
    ) -> Result<ControlFlow<Outcome, (B256, Vec<[u8; 65]>)>> {
        let domain = self.origin.domain;
        // The daemon replaces the chain entry when the controller changes the
        // ValidatorAnnounce of the domain: read the address at each look.
        let origin_chain = self.chains.get(domain).ok_or(RelayError::NoChain(domain))?;
        let locations = announced(
            origin_chain.provider(),
            origin_chain.origin.validator_announce,
            validators,
        )
        .await
        .inspect_err(|_| origin_chain.rotate())?;
        let want = Want {
            domain,
            hook: self.origin.hook,
            index,
            message_id: id,
        };
        let quorum = self
            .fetcher
            .quorum(validators, &locations, threshold, &want);
        let (fast, reason) = match quorum.await {
            Quorum::Found(root, signatures) => {
                return Ok(ControlFlow::Continue((root, signatures)))
            }
            Quorum::NotYet => (
                true,
                format!("fewer than {threshold} validators serve a signed checkpoint"),
            ),
            Quorum::Reorg(validator) => (
                false,
                format!("validator {validator} reports a reorg of domain {domain}"),
            ),
        };
        Ok(ControlFlow::Break(Outcome::Retry { fast, reason }))
    }
}

#[cfg(test)]
mod tests;
