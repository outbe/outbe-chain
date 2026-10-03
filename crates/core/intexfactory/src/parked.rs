//! Sweeping the origin router's parked work: a send the relay float could not pay for, and proceeds
//! this factory refused. Both entries are permissionless, so the cycle trigger is the one who pushes.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    storage::StorageHandle,
};

use crate::constants::{MAX_PARKED_CALLS_PER_FIRING, MAX_PARKED_FAILURES_PER_FIRING};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS;

/// Cycle-trigger entry: push what the router parked, newest queues last.
pub fn drain(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let mut budget = MAX_PARKED_CALLS_PER_FIRING;
    drain_messages(&storage, &mut budget)?;
    drain_proceeds(&storage, &mut budget)
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

    /// An entry that needs nothing more from us; the cursor may pass it for good.
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

fn drain_messages(storage: &StorageHandle<'_>, budget: &mut u32) -> Result<()> {
    let factory = IntexFactoryContract::new(storage.clone());
    let total = match message_count(storage) {
        Ok(total) => total,
        Err(error) => return skip_unless_node_local(error),
    };
    let start = match factory.parked_message_cursor.read() {
        Ok(start) => start,
        Err(error) => return skip_unless_node_local(error),
    };

    let mut cursor = Cursor::new(start);
    while cursor.at < total && *budget > 0 && !cursor.spent() {
        *budget = budget.saturating_sub(1);
        let read = storage.staticcall(
            ORIGIN_ROUTER_ADDRESS,
            IOriginRouter::parkedMessageCall {
                idx: U256::from(cursor.at),
            }
            .abi_encode()
            .into(),
        );
        let read = match read {
            Ok(ret) => Some(ret),
            Err(error) => {
                skip_unless_node_local(error)?;
                None
            }
        };
        let parked =
            read.and_then(|ret| IOriginRouter::parkedMessageCall::abi_decode_returns(&ret).ok());
        match parked {
            // An empty payload is an index the router never filled; `sent` is one we already pushed.
            Some(entry) if !entry.sent && !entry.payload.is_empty() => {
                let idx = cursor.at;
                let sent = storage.with_checkpoint(|| {
                    storage.call(
                        ORIGIN_ROUTER_ADDRESS,
                        U256::ZERO,
                        IOriginRouter::resendParkedMessageCall {
                            idx: U256::from(idx),
                        }
                        .abi_encode()
                        .into(),
                    )?;
                    Ok(())
                });
                match sent {
                    Ok(()) => cursor.resolved(),
                    Err(error) if error.sweep_failure() == SweepFailure::Propagate => {
                        return Err(error);
                    }
                    Err(error) => {
                        tracing::warn!(target: "outbe::intexfactory", idx, error = ?error, "parked message: leaving it");
                        cursor.stuck();
                    }
                }
            }
            Some(_) => cursor.resolved(),
            None => cursor.stuck(),
        }
        cursor.at = cursor.at.saturating_add(1);
    }

    factory
        .parked_message_cursor
        .write(cursor.head)
        .or_else(skip_unless_node_local)
}

fn drain_proceeds(storage: &StorageHandle<'_>, budget: &mut u32) -> Result<()> {
    let factory = IntexFactoryContract::new(storage.clone());
    let total = match proceeds_count(storage) {
        Ok(total) => total,
        Err(error) => return skip_unless_node_local(error),
    };
    let start = match factory.parked_proceeds_cursor.read() {
        Ok(start) => start,
        Err(error) => return skip_unless_node_local(error),
    };

    let mut cursor = Cursor::new(start);
    while cursor.at < total && *budget > 0 && !cursor.spent() {
        *budget = budget.saturating_sub(1);
        let read = storage.staticcall(
            ORIGIN_ROUTER_ADDRESS,
            IOriginRouter::parkedProceedsCall {
                idx: U256::from(cursor.at),
            }
            .abi_encode()
            .into(),
        );
        let read = match read {
            Ok(ret) => Some(ret),
            Err(error) => {
                skip_unless_node_local(error)?;
                None
            }
        };
        let parked =
            read.and_then(|ret| IOriginRouter::parkedProceedsCall::abi_decode_returns(&ret).ok());
        match parked {
            Some(entry) if !entry.settled && entry.amount > 0 => {
                let idx = cursor.at;
                let settled = storage.with_checkpoint(|| {
                    storage.call(
                        ORIGIN_ROUTER_ADDRESS,
                        U256::ZERO,
                        IOriginRouter::distributeParkedProceedsCall {
                            idx: U256::from(idx),
                        }
                        .abi_encode()
                        .into(),
                    )?;
                    Ok(())
                });
                match settled {
                    Ok(()) => cursor.resolved(),
                    Err(error) if error.sweep_failure() == SweepFailure::Propagate => {
                        return Err(error);
                    }
                    Err(error) => {
                        tracing::warn!(target: "outbe::intexfactory", idx, error = ?error, "parked proceeds: leaving it");
                        cursor.stuck();
                    }
                }
            }
            Some(_) => cursor.resolved(),
            None => cursor.stuck(),
        }
        cursor.at = cursor.at.saturating_add(1);
    }

    factory
        .parked_proceeds_cursor
        .write(cursor.head)
        .or_else(skip_unless_node_local)
}

/// A failure every node meets leaves the entry for a later pass; a node-local one fails the block.
fn skip_unless_node_local(error: PrecompileError) -> Result<()> {
    match error.sweep_failure() {
        SweepFailure::Propagate => Err(error),
        SweepFailure::Skip | SweepFailure::Stop => Ok(()),
    }
}

fn message_count(storage: &StorageHandle<'_>) -> Result<u64> {
    let ret = storage.staticcall(
        ORIGIN_ROUTER_ADDRESS,
        IOriginRouter::parkedMessageCountCall {}.abi_encode().into(),
    )?;
    Ok(to_index(
        IOriginRouter::parkedMessageCountCall::abi_decode_returns(&ret).unwrap_or_default(),
    ))
}

fn proceeds_count(storage: &StorageHandle<'_>) -> Result<u64> {
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

fn to_index(total: U256) -> u64 {
    u64::try_from(total).unwrap_or(u64::MAX)
}
