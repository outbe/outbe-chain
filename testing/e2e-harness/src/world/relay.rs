//! Carry Hyperlane messages between the committee chain and the target chain.
//!
//! `MockRelayMailbox` records a dispatch and emits it instead of delivering it
//! inline, because across two chains the peer cannot be a live contract
//! reference. This is the other half: a pump that reads those events on one
//! chain and calls `deliver` on the other, in both directions, the way a real
//! relayer would.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{sleep, JoinHandle};
use std::time::Duration;

use alloy_primitives::{Address, FixedBytes, U256};
use alloy_sol_types::{sol, SolEvent as _};
use eyre::{eyre, Result};

use crate::internal::eth;

sol! {
    interface IRelayMailbox {
        event Dispatched(
            bytes32 indexed messageId,
            uint32 indexed destinationDomain,
            bytes32 sender,
            bytes32 recipient,
            bytes message
        );
        function deliver(uint32 origin, bytes32 sender, bytes32 recipient, bytes message) external;
    }
}

/// One end of the route: where to read dispatches and who this chain is.
#[derive(Clone, Debug)]
pub struct RelayEnd {
    pub url: String,
    pub mailbox: Address,
    pub domain: u32,
}

/// How often the pump looks for new dispatches. A real relayer is asynchronous;
/// scenarios assert arrival with a deadline rather than assuming it is instant.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A running pump. Dropping it stops the thread.
pub struct Relay {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Relay {
    /// Start carrying messages both ways between `a` and `b`.
    pub fn start(a: RelayEnd, b: RelayEnd, sender_key: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("e2e-relay".to_owned())
            .spawn(move || {
                // Each direction keeps its own cursor: a message already carried
                // must not be delivered twice, which the inbound side would
                // acknowledge and drop but which would hide a real duplicate.
                let mut carried_a = 0usize;
                let mut carried_b = 0usize;
                while !flag.load(Ordering::Relaxed) {
                    carried_a += carry(&a, &b, &sender_key, carried_a);
                    carried_b += carry(&b, &a, &sender_key, carried_b);
                    sleep(POLL_INTERVAL);
                }
            })
            .expect("spawn the e2e relay");
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Deliver every dispatch on `from` past `already` that is addressed to `to`.
/// Returns how many were carried this round.
fn carry(from: &RelayEnd, to: &RelayEnd, sender_key: &str, already: usize) -> usize {
    let Ok(dispatches) = read_dispatches(from, to.domain) else {
        return 0;
    };
    let mut carried = 0;
    for dispatch in dispatches.into_iter().skip(already) {
        if deliver(to, from.domain, &dispatch, sender_key).is_err() {
            // Stop at the first failure so the cursor never runs ahead of what
            // actually landed; the next round retries from the same message.
            break;
        }
        carried += 1;
    }
    carried
}

struct Dispatch {
    sender: FixedBytes<32>,
    recipient: FixedBytes<32>,
    message: Vec<u8>,
}

fn read_dispatches(end: &RelayEnd, destination: u32) -> Result<Vec<Dispatch>> {
    let topic0 = IRelayMailbox::Dispatched::SIGNATURE_HASH;
    let destination_topic = format!("0x{:064x}", destination);
    let logs = eth::raw_json_with_params(
        &end.url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{:?}", end.mailbox),
            "topics": [format!("{topic0:?}"), serde_json::Value::Null, destination_topic],
        }]),
    )
    .ok_or_else(|| eyre!("relay could not read dispatches"))?;

    let entries = logs
        .as_array()
        .ok_or_else(|| eyre!("relay got no dispatch array"))?;
    entries.iter().map(decode_dispatch).collect()
}

fn decode_dispatch(entry: &serde_json::Value) -> Result<Dispatch> {
    let data = entry
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre!("dispatch log carries no data"))?;
    let bytes = alloy_primitives::hex::decode(data.trim_start_matches("0x"))
        .map_err(|error| eyre!("undecodable dispatch data: {error}"))?;
    let decoded = IRelayMailbox::Dispatched::abi_decode_data(&bytes)
        .map_err(|error| eyre!("undecodable dispatch event: {error}"))?;
    Ok(Dispatch {
        sender: decoded.0,
        recipient: decoded.1,
        message: decoded.2.to_vec(),
    })
}

fn deliver(to: &RelayEnd, origin: u32, dispatch: &Dispatch, sender_key: &str) -> Result<()> {
    eth::send_call(
        &to.url,
        to.mailbox,
        sender_key,
        &IRelayMailbox::deliverCall {
            origin,
            sender: dispatch.sender,
            recipient: dispatch.recipient,
            message: dispatch.message.clone().into(),
        },
        None::<U256>,
    )?;
    Ok(())
}
