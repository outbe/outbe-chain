//! Storage providers backing the outbe sub-call mechanism.
//!
//! Houses [`CtxStorageProvider`], the [`PrecompileStorageProvider`] impl that
//! the sub-call driver uses
//! to expose journaled access against an [`alloy_evm::eth::EthEvmContext`].
//!
//!
//! [`CtxStorageProvider`] owns a `&'a mut EthEvmContext<DB>` field so that
//! `sub_call(input)` can pass the same
//! `&mut ctx` to `sub_call::run_with_ocomp_context(ctx, ...)`. The outer dispatch path
//! borrows `CtxStorageProvider` through
//! [`outbe_primitives::storage::StorageHandle::with_provider`]. That method uses
//! `Rc<RefCell<&mut dyn PrecompileStorageProvider>>` and releases the inner
//! borrow as soon as the scope ends.

pub mod ctx_provider;

pub(crate) use ctx_provider::CtxStorageProviderConfig;
pub use ctx_provider::{CtxStorageProvider, ReentrancyGuard, ReentrancyStack};
