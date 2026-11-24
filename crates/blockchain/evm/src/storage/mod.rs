//! Storage providers backing the outbe sub-call mechanism.
//!
//! Houses [`CtxStorageProvider`], the [`PrecompileStorageProvider`] impl that
//! exposes journaled access and sub-call execution against an
//! [`evm2::Evm`].
//!
//! [`CtxStorageProvider`] owns a `&'a mut evm2::Evm<'evm, OutbeEvmTypes>` field so that
//! `sub_call(input)` can pass the same `&mut evm` to `sub_call::run_frame`, then settles
//! against its parent meter. The public standalone `sub_call::run_with_host` is used by
//! direct-driver callers. The outer dispatch path borrows `CtxStorageProvider` through
//! [`outbe_primitives::storage::StorageHandle`].

pub mod ctx_provider;

pub use ctx_provider::CtxStorageProviderConfig;
pub use ctx_provider::{CtxStorageProvider, ReentrancyGuard, ReentrancyStack};
