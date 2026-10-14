//! Typed errors for host operator workflows.

mod onboarding_registry;
mod renewal;
mod rpc_tx;
mod upgrade;

pub use onboarding_registry::{OnboardingError, RegistryError};
pub use renewal::{RenewalError, RenewalJournalError};
pub use rpc_tx::{JournalStorageError, RpcError, StatusError, TxError};
pub use upgrade::UpgradeError;
