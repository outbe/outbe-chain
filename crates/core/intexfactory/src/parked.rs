//! Sweeping the origin router's parked work: a send the relay float could not pay for, and proceeds
//! this factory refused. Both entries are permissionless, so the cycle trigger is the one who pushes.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

use crate::constants::{MAX_PARKED_CALLS_PER_FIRING, MAX_PARKED_FAILURES_PER_FIRING};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS;

/// Cycle-trigger entry: push what the router parked, newest queues last.
pub fn drain(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let mut budget = MAX_PARKED_CALLS_PER_FIRING;
    drain_messages(&storage, &mut budget);
    drain_proceeds(&storage, &mut budget);
    Ok(())
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

fn drain_messages(storage: &StorageHandle<'_>, budget: &mut u32) {
    let factory = IntexFactoryContract::new(storage.clone());
    let Ok(total) = message_count(storage) else {
        return;
    };
    let Ok(start) = factory.parked_message_cursor.read() else {
        return;
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
        let parked = read
            .ok()
            .and_then(|ret| IOriginRouter::parkedMessageCall::abi_decode_returns(&ret).ok());
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

    let _ = factory.parked_message_cursor.write(cursor.head);
}

fn drain_proceeds(storage: &StorageHandle<'_>, budget: &mut u32) {
    let factory = IntexFactoryContract::new(storage.clone());
    let Ok(total) = proceeds_count(storage) else {
        return;
    };
    let Ok(start) = factory.parked_proceeds_cursor.read() else {
        return;
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
        let parked = read
            .ok()
            .and_then(|ret| IOriginRouter::parkedProceedsCall::abi_decode_returns(&ret).ok());
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

    let _ = factory.parked_proceeds_cursor.write(cursor.head);
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
