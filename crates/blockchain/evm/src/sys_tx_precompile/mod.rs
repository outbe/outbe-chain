//! System-transaction orchestration precompile.
//!
//! The precompile is called through `transact_system_call` with
//! `SYSTEM_ADDRESS` as the EVM caller. It decodes a versioned
//! [`SystemTx`](crate::system_tx::SystemTx) payload and routes to
//! the system tx body. Thus runtime events are emitted through the
//! EVM journal and become receipt-visible.

use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

mod boundary;
mod context;
mod cycle;

#[cfg(any(test, feature = "test-hooks", feature = "test-utils"))]
pub use context::block_runtime_context_from_storage;
pub use context::current_preloaded_system_tx_context;
pub use context::record_observed_runtime_block_context;
#[cfg(any(test, feature = "test-hooks"))]
pub use context::take_observed_runtime_block_contexts;
pub use context::{with_preloaded_system_tx_context, PreloadedSystemTxContext};

#[cfg(any(test, feature = "test-hooks"))]
pub use cycle::enforce_tee_lease_deadlines;
pub use cycle::{enforce_enclave_upgrade_deadline, EnclaveUpgradeDeadline};

/// Test hook read by the runtime system-transaction dispatch.
/// Production builds return `None`.
pub fn forced_dirty_system_precompile_revert() -> Option<(Address, U256, U256)> {
    #[cfg(any(test, feature = "test-hooks"))]
    {
        FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| *cell.borrow())
    }
    #[cfg(not(any(test, feature = "test-hooks")))]
    {
        None
    }
}

#[cfg(any(test, feature = "test-hooks", feature = "test-utils"))]
pub use boundary::run_boundary_outcome;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Immutable chain-manifest authorities for system-transaction execution.
pub(crate) struct SystemTxRuntime<'a> {
    pub(crate) host: &'a dyn crate::EvmRuntimeHost,
    pub(crate) tee_attestation_v1:
        &'a crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
}

pub(crate) fn dispatch_with_runtime(
    storage: StorageHandle,
    runtime: SystemTxRuntime<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    let SystemTxRuntime {
        host,
        tee_attestation_v1,
    } = runtime;
    host.dispatch_system_tx(storage, data, caller, value, tee_attestation_v1)
}

#[cfg(any(test, feature = "test-hooks"))]
std::thread_local! {
    static FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT: std::cell::RefCell<Option<(Address, U256, U256)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn with_forced_dirty_system_precompile_revert<R>(
    address: Address,
    slot: U256,
    value: U256,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
    }

    FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| {
        *cell.borrow_mut() = Some((address, slot, value));
    });
    let _reset = Reset;
    f()
}
