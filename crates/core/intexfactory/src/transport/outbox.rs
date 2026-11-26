//! The outbox of a module: its messages to each target chain, sent in order through
//! `CrosschainBridge.callRemoteWithGasLimit` to `receiveFromOutbe` of TargetRouter.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_core_common::addresses::TARGET_ROUTER_ADDRESS;
use outbe_primitives::error::{PassFailure, PrecompileError, Result};
use outbe_primitives::storage::dsl::{Bytes as StorageBytes, Map};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::CROSSCHAIN_BRIDGE_ADDRESS;

use crate::transport::gas::ica_gas_limit;

/// The wait after a failed send before the next try of that chain.
pub const RETRY_SECONDS: u64 = 300;

/// Gas a send must have left before it is tried, so a call of `flush()` with too little gas stops
/// instead of counting a failed try against a message.
pub const SEND_GAS_RESERVE: u64 = 1_000_000;

sol! {
    struct Call {
        address to;
        uint256 value;
        bytes data;
    }

    function callRemoteWithGasLimit(uint32 domain, Call[] calls, uint256 gasLimit)
        external
        returns (bytes32 messageId);

    function receiveFromOutbe(bytes message) external;
}

/// What the Sent and SendFailed events of a message carry: its type, its day and up to three
/// more fields of that type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    pub msg_type: u8,
    pub emission_day: u32,
    pub fields: [u32; 3],
}

impl Label {
    pub fn new(msg_type: u8, emission_day: u32, fields: [u32; 3]) -> Self {
        Self {
            msg_type,
            emission_day,
            fields,
        }
    }

    fn pack(self) -> U256 {
        U256::from(self.msg_type)
            | (U256::from(self.emission_day) << 8)
            | (U256::from(self.fields[0]) << 40)
            | (U256::from(self.fields[1]) << 72)
            | (U256::from(self.fields[2]) << 104)
    }

    fn unpack(word: U256) -> Self {
        let part = |shift: usize, bits: usize| -> u32 {
            ((word >> shift) & ((U256::from(1u8) << bits) - U256::from(1u8))).to::<u32>()
        };
        Self {
            msg_type: part(0, 8) as u8,
            emission_day: part(8, 32),
            fields: [part(40, 32), part(72, 32), part(104, 32)],
        }
    }
}

/// A message for one target chain: its label, the destination gas of its type and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub chain_id: u32,
    pub label: Label,
    pub message_gas: u64,
    pub payload: Bytes,
}

/// The first message of a chain, as a view shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub label: Label,
    pub failures: u32,
    pub next_try: u64,
}

/// What a try of `flush` did with the first message of a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent {
        chain_id: u32,
        label: Label,
        message_id: B256,
        payload: Bytes,
    },
    Failed {
        chain_id: u32,
        label: Label,
        failures: u32,
        payload: Bytes,
        error_data: Bytes,
    },
}

/// The storage of one outbox. Each module gives it its own fields.
pub struct Outbox<'s> {
    pub head: Map<'s, u32, u32>,
    pub tail: Map<'s, u32, u32>,
    /// `gas | failures << 64 | next_try << 96`.
    pub meta: Map<'s, u64, U256>,
    pub label: Map<'s, u64, U256>,
    pub payload: Map<'s, u64, StorageBytes<'s>>,
}

fn entry_key(chain_id: u32, index: u32) -> u64 {
    (u64::from(chain_id) << 32) | u64::from(index)
}

fn pack_meta(message_gas: u64, failures: u32, next_try: u64) -> U256 {
    U256::from(message_gas) | (U256::from(failures) << 64) | (U256::from(next_try) << 96)
}

fn unpack_meta(word: U256) -> (u64, u32, u64) {
    let low =
        |shift: usize, bits: usize| (word >> shift) & ((U256::from(1u8) << bits) - U256::from(1u8));
    (
        low(0, 64).to::<u64>(),
        low(64, 32).to::<u32>(),
        low(96, 64).to::<u64>(),
    )
}

impl Outbox<'_> {
    pub fn push(&self, message: Message) -> Result<()> {
        let index = self.tail.read(&message.chain_id)?;
        let key = entry_key(message.chain_id, index);
        self.meta
            .write(&key, pack_meta(message.message_gas, 0, 0))?;
        self.label.write(&key, message.label.pack())?;
        self.payload.bytes(&key).write(&message.payload)?;
        self.tail.write(&message.chain_id, index + 1)
    }

    pub fn len(&self, chain_id: u32) -> Result<u32> {
        Ok(self.tail.read(&chain_id)? - self.head.read(&chain_id)?)
    }

    pub fn head_of(&self, chain_id: u32) -> Result<Option<Head>> {
        let index = self.head.read(&chain_id)?;
        if index >= self.tail.read(&chain_id)? {
            return Ok(None);
        }
        let key = entry_key(chain_id, index);
        let (_, failures, next_try) = unpack_meta(self.meta.read(&key)?);
        Ok(Some(Head {
            label: Label::unpack(self.label.read(&key)?),
            failures,
            next_try,
        }))
    }

    fn pop(&self, chain_id: u32, index: u32) -> Result<()> {
        let key = entry_key(chain_id, index);
        self.meta.clear(&key)?;
        self.label.clear(&key)?;
        self.payload.bytes(&key).clear()?;
        let next = index + 1;
        if next == self.tail.read(&chain_id)? {
            self.head.write(&chain_id, 0)?;
            self.tail.write(&chain_id, 0)
        } else {
            self.head.write(&chain_id, next)
        }
    }

    /// The messages that wait for `chain_id`, first one first.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn messages(&self, chain_id: u32) -> Result<Vec<Message>> {
        let mut messages = Vec::new();
        for index in self.head.read(&chain_id)?..self.tail.read(&chain_id)? {
            let key = entry_key(chain_id, index);
            messages.push(Message {
                chain_id,
                label: Label::unpack(self.label.read(&key)?),
                message_gas: unpack_meta(self.meta.read(&key)?).0,
                payload: self.payload.bytes(&key).read()?.into(),
            });
        }
        Ok(messages)
    }

    /// The first message of a chain as the views of the modules show it.
    pub fn head_view(&self, chain_id: u32) -> Result<HeadView> {
        let head = self.head_of(chain_id)?;
        Ok(HeadView {
            msg_type: head.map_or(0, |h| h.label.msg_type),
            emission_day: head.map_or(0, |h| h.label.emission_day),
            pending: self.len(chain_id)?,
            failures: head.map_or(0, |h| h.failures),
            next_try: head.map_or(0, |h| h.next_try),
        })
    }

    /// Sends the due messages of each chain in order, at most `budget` sends in all. A chain stops
    /// at its first failed send, which waits [`RETRY_SECONDS`]. Each message is sent, removed and
    /// reported as one unit: gas that runs out inside it rolls back only that message and ends the
    /// call, and the next call resumes it. A node-local error fails the call.
    pub fn flush(
        &self,
        storage: &StorageHandle<'_>,
        chains: &[u32],
        now: u64,
        mut budget: u32,
        mut on_outcome: impl FnMut(Outcome) -> Result<()>,
    ) -> Result<()> {
        for &chain_id in chains {
            while budget > 0 {
                let step = storage.with_checkpoint(|| {
                    Ok(match self.try_head(storage, chain_id, now)? {
                        Try::Idle => Step::Idle,
                        Try::OutOfGas => Step::OutOfGas,
                        Try::Done(outcome) => {
                            let failed = matches!(outcome, Outcome::Failed { .. });
                            on_outcome(outcome)?;
                            Step::Done { failed }
                        }
                    })
                });
                match step {
                    Ok(Step::Idle) => break,
                    Ok(Step::OutOfGas) => return Ok(()),
                    Ok(Step::Done { failed }) => {
                        budget -= 1;
                        if failed {
                            break;
                        }
                    }
                    Err(error) if error.pass_failure() == PassFailure::Stop => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// Sends the first message of a chain when it is due.
    fn try_head(&self, storage: &StorageHandle<'_>, chain_id: u32, now: u64) -> Result<Try> {
        let index = self.head.read(&chain_id)?;
        if index >= self.tail.read(&chain_id)? {
            return Ok(Try::Idle);
        }
        let key = entry_key(chain_id, index);
        let (message_gas, failures, next_try) = unpack_meta(self.meta.read(&key)?);
        if next_try > now {
            return Ok(Try::Idle);
        }
        if storage
            .metered_gas_remaining()?
            .is_some_and(|left| left < SEND_GAS_RESERVE)
        {
            return Ok(Try::OutOfGas);
        }
        let label = Label::unpack(self.label.read(&key)?);
        let payload: Bytes = self.payload.bytes(&key).read()?.into();
        let error = match storage.with_checkpoint(|| send(storage, chain_id, message_gas, &payload))
        {
            Ok(message_id) => {
                self.pop(chain_id, index)?;
                return Ok(Try::Done(Outcome::Sent {
                    chain_id,
                    label,
                    message_id,
                    payload,
                }));
            }
            Err(error) => error,
        };
        match error.pass_failure() {
            PassFailure::Propagate => Err(error),
            PassFailure::Stop => Ok(Try::OutOfGas),
            PassFailure::Skip => {
                let failures = failures.saturating_add(1);
                self.meta
                    .write(&key, pack_meta(message_gas, failures, now + RETRY_SECONDS))?;
                Ok(Try::Done(Outcome::Failed {
                    chain_id,
                    label,
                    failures,
                    payload,
                    error_data: revert_data(error),
                }))
            }
        }
    }
}

enum Try {
    Idle,
    OutOfGas,
    Done(Outcome),
}

enum Step {
    Idle,
    OutOfGas,
    Done { failed: bool },
}

/// What the `outboxHead` views of the modules return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadView {
    pub msg_type: u8,
    pub emission_day: u32,
    pub pending: u32,
    pub failures: u32,
    pub next_try: u64,
}

impl From<HeadView> for (u8, u32, u32, u32, u64) {
    fn from(head: HeadView) -> Self {
        (
            head.msg_type,
            head.emission_day,
            head.pending,
            head.failures,
            head.next_try,
        )
    }
}

/// The begin-block step of a module: it sub-calls `flush()` of `module`, so the module is the
/// caller of CrosschainBridge. Only a node-local error fails the block; any other failure leaves
/// the messages for the next block.
pub fn run_flush_step(
    storage: &StorageHandle<'_>,
    module: Address,
    flush_call: Bytes,
) -> Result<()> {
    match storage.call(module, U256::ZERO, flush_call) {
        Err(error)
            if error.pass_failure() == PassFailure::Propagate
                || matches!(error, PrecompileError::Fatal(_)) =>
        {
            Err(error)
        }
        Err(error) => {
            tracing::warn!(target: "outbe::intex", %module, %error, "outbox flush skipped");
            Ok(())
        }
        Ok(_) => Ok(()),
    }
}

fn send(
    storage: &StorageHandle<'_>,
    chain_id: u32,
    message_gas: u64,
    payload: &Bytes,
) -> Result<B256> {
    let call = callRemoteWithGasLimitCall {
        domain: chain_id,
        calls: vec![Call {
            to: TARGET_ROUTER_ADDRESS,
            value: U256::ZERO,
            data: receiveFromOutbeCall {
                message: payload.clone(),
            }
            .abi_encode()
            .into(),
        }],
        gasLimit: U256::from(ica_gas_limit(message_gas)),
    };
    let returned = storage.call(
        CROSSCHAIN_BRIDGE_ADDRESS,
        U256::ZERO,
        call.abi_encode().into(),
    )?;
    callRemoteWithGasLimitCall::abi_decode_returns(&returned)
        .map_err(|_| PrecompileError::RevertBytes(returned))
}

fn revert_data(error: PrecompileError) -> Bytes {
    match error {
        PrecompileError::RevertBytes(bytes) => bytes,
        other => Bytes::from(other.to_string().into_bytes()),
    }
}
