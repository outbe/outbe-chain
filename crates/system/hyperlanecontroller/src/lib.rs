//! `HyperlaneController` - the Hyperlane infrastructure precompile (`0x...EE14`).
//!
//! It keeps the validator set in the ISMs, the liveness of the validator
//! agents, and the domain table. It owns no contract. The owner of every
//! Hyperlane core contract (Mailbox, ProxyAdmin, IGP, gas oracle, ProtocolFee,
//! InterchainAccountRouter, StorageMessageIdMultisigIsm) is `crosschainbridge`
//! (`0x...EE15`), on Outbe directly and on remote chains through its
//! Interchain Account.
//!
//! - It holds no validator set of its own. The ISMs are the source of truth.
//! - `sync` rotates validators through `set_validators_and_threshold`.
//!   Begin-block liveness calls the same rotation.
//! - A write to an ISM or the ICA router is an owner call: the controller
//!   calls `crosschainbridge` like a contract (`callLocal`, `callRemote`).
//! - [`HyperlaneVoteTarget`] adds and removes the Hyperlane chains and sets
//!   the gas data of the IGPs by a validator vote. Permissionless
//!   `applyApproved` runs an approved action.
//! - `getExchangeRateAndGasPrice` is the gas oracle of the Outbe IGP: the
//!   Oracle gas price of the domain in COEN-wei plus `gas_markup_bps`.

pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod schema;
pub mod sol_ext;

mod gas;
mod ism;
mod liveness;
mod runtime;
mod vote_target;

pub use gas::{GAS_MARKUP_BPS, TOKEN_EXCHANGE_RATE_SCALE};
pub use lifecycle::HyperlaneModule;
pub use liveness::{
    checkpoint_digest, SignedCheckpoint, GRACE_BLOCKS, LIVENESS_WINDOW_BLOCKS, MAX_MISSES,
};
pub use runtime::{consensus_threshold, validate_validators, ControllerBootstrap};
pub use schema::HyperlaneControllerContract;
pub use vote_target::HyperlaneVoteTarget;

#[cfg(test)]
mod tests;
