//! Sweeping the origin router's parked work: a send the relay float could not pay for, and proceeds
//! this factory refused. Both entries are permissionless, so the cycle trigger is the one who pushes.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    storage::{dsl::Value, StorageHandle},
};

use crate::constants::{MAX_PARKED_CALLS_PER_FIRING, MAX_PARKED_FAILURES_PER_FIRING};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS;

/// Cycle-trigger entry: push what the router parked, newest queues last.
pub fn drain(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let mut budget = MAX_PARKED_CALLS_PER_FIRING;
    drain_queue::<ParkedMessages>(&storage, &mut budget)?;
    drain_queue::<ParkedProceeds>(&storage, &mut budget)
}

/// Where the next pass starts, and how far the resolved prefix reaches once it ends.
pub(crate) struct Cursor {
    pub(crate) at: u64,
    pub(crate) head: u64,
    prefix_resolved: bool,
    failures: u32,
}

impl Cursor {
    pub(crate) fn new(at: u64) -> Self {
        Self {
            at,
            head: at,
            prefix_resolved: true,
            failures: 0,
        }
    }

    /// An entry that needs nothing more from us. The cursor may pass it for good.
    pub(crate) fn resolved(&mut self) {
        if self.prefix_resolved {
            self.head = self.at.saturating_add(1);
        }
        self.failures = 0;
    }

    /// An entry that stays: the cursor cannot pass it, but the pass walks on.
    pub(crate) fn stuck(&mut self) {
        self.prefix_resolved = false;
        self.failures = self.failures.saturating_add(1);
    }

    pub(crate) fn spent(&self) -> bool {
        self.failures >= MAX_PARKED_FAILURES_PER_FIRING
    }
}

/// One parked router queue: its count, its cursor here, and how to read and push an entry.
trait ParkedQueue {
    const KIND: &'static str;
    fn count(storage: &StorageHandle<'_>) -> Result<u64>;
    fn cursor<'f, 's>(factory: &'f IntexFactoryContract<'s>) -> &'f Value<'s, u64>;
    fn read_call(idx: U256) -> Bytes;
    /// Whether the entry still waits for a push. `None` when the return does not decode.
    fn waiting(ret: &[u8]) -> Option<bool>;
    fn push_call(idx: U256) -> Bytes;
}

struct ParkedMessages;

impl ParkedQueue for ParkedMessages {
    const KIND: &'static str = "message";

    fn count(storage: &StorageHandle<'_>) -> Result<u64> {
        let ret = storage.staticcall(
            ORIGIN_ROUTER_ADDRESS,
            IOriginRouter::parkedMessageCountCall {}.abi_encode().into(),
        )?;
        Ok(to_index(
            IOriginRouter::parkedMessageCountCall::abi_decode_returns(&ret).unwrap_or_default(),
        ))
    }

    fn cursor<'f, 's>(factory: &'f IntexFactoryContract<'s>) -> &'f Value<'s, u64> {
        &factory.parked_message_cursor
    }

    fn read_call(idx: U256) -> Bytes {
        IOriginRouter::parkedMessageCall { idx }.abi_encode().into()
    }

    fn waiting(ret: &[u8]) -> Option<bool> {
        let entry = IOriginRouter::parkedMessageCall::abi_decode_returns(ret).ok()?;
        // An empty payload is an index the router never filled. `sent` is one we
        // already pushed.
        Some(!entry.sent && !entry.payload.is_empty())
    }

    fn push_call(idx: U256) -> Bytes {
        IOriginRouter::resendParkedMessageCall { idx }
            .abi_encode()
            .into()
    }
}

struct ParkedProceeds;

impl ParkedQueue for ParkedProceeds {
    const KIND: &'static str = "proceeds";

    fn count(storage: &StorageHandle<'_>) -> Result<u64> {
        let ret = storage.staticcall(
            ORIGIN_ROUTER_ADDRESS,
            IOriginRouter::parkedProceedsCountCall {}
                .abi_encode()
                .into(),
        )?;
        Ok(to_index(
            IOriginRouter::parkedProceedsCountCall::abi_decode_returns(&ret).unwrap_or_default(),
        ))
    }

    fn cursor<'f, 's>(factory: &'f IntexFactoryContract<'s>) -> &'f Value<'s, u64> {
        &factory.parked_proceeds_cursor
    }

    fn read_call(idx: U256) -> Bytes {
        IOriginRouter::parkedProceedsCall { idx }
            .abi_encode()
            .into()
    }

    fn waiting(ret: &[u8]) -> Option<bool> {
        let entry = IOriginRouter::parkedProceedsCall::abi_decode_returns(ret).ok()?;
        Some(!entry.settled && entry.amount > 0)
    }

    fn push_call(idx: U256) -> Bytes {
        IOriginRouter::distributeParkedProceedsCall { idx }
            .abi_encode()
            .into()
    }
}

fn drain_queue<Q: ParkedQueue>(storage: &StorageHandle<'_>, budget: &mut u32) -> Result<()> {
    let factory = IntexFactoryContract::new(storage.clone());
    let total = match Q::count(storage) {
        Ok(total) => total,
        Err(error) => return skip_unless_node_local(error),
    };
    let start = match Q::cursor(&factory).read() {
        Ok(start) => start,
        Err(error) => return skip_unless_node_local(error),
    };

    let mut cursor = Cursor::new(start);
    while cursor.at < total && *budget > 0 && !cursor.spent() {
        *budget = budget.saturating_sub(1);
        let read = storage.staticcall(ORIGIN_ROUTER_ADDRESS, Q::read_call(U256::from(cursor.at)));
        let read = match read {
            Ok(ret) => Some(ret),
            Err(error) => {
                skip_unless_node_local(error)?;
                None
            }
        };
        match read.and_then(|ret| Q::waiting(&ret)) {
            Some(true) => push_entry::<Q>(storage, &mut cursor)?,
            Some(false) => cursor.resolved(),
            None => cursor.stuck(),
        }
        cursor.at = cursor.at.saturating_add(1);
    }

    Q::cursor(&factory)
        .write(cursor.head)
        .or_else(skip_unless_node_local)
}

fn push_entry<Q: ParkedQueue>(storage: &StorageHandle<'_>, cursor: &mut Cursor) -> Result<()> {
    let idx = cursor.at;
    let pushed = storage.with_checkpoint(|| {
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            Q::push_call(U256::from(idx)),
        )?;
        Ok(())
    });
    match pushed {
        Ok(()) => cursor.resolved(),
        Err(error) if error.sweep_failure() == SweepFailure::Propagate => {
            return Err(error);
        }
        Err(error) => {
            tracing::warn!(target: "outbe::intexfactory", idx, error = ?error, "parked {}: leaving it", Q::KIND);
            cursor.stuck();
        }
    }
    Ok(())
}

/// A failure every node meets leaves the entry for a later pass. A node-local failure fails the
/// block.
fn skip_unless_node_local(error: PrecompileError) -> Result<()> {
    match error.sweep_failure() {
        SweepFailure::Propagate => Err(error),
        SweepFailure::Skip | SweepFailure::Stop => Ok(()),
    }
}

fn to_index(total: U256) -> u64 {
    u64::try_from(total).unwrap_or(u64::MAX)
}
