//! `HyperlaneController` - governance-owned controller of the Hyperlane bridge (`0x...EE14`).
//!
//! The precompile is the owner of every Hyperlane core contract on Outbe
//! (Mailbox, ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
//! StorageMessageIdMultisigIsm) and, through its Interchain Account, of the
//! same contracts on remote chains. It holds no validator set of its own: the
//! ISMs are the source of truth, the controller only forwards owner calls.
//!
//! - `initialize` and `fund` are the only direct write selectors.
//! - Validator rotation, generic local / remote owner calls and table changes
//!   are methods on [`HyperlaneControllerContract`]; the trigger that runs them
//!   (validator vote or another authority) is not wired yet.
//! - Remote dispatch fees (IGP quote) are paid from the controller's own
//!   balance, topped up through `fund`.

pub mod errors;
pub mod precompile;
pub mod schema;

mod runtime;
mod sol_ext;

pub use runtime::{validate_validators, RemoteCall};
pub use schema::HyperlaneControllerContract;

#[cfg(test)]
mod tests;
