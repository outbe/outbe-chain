//! Temporary re-exports of `outbe_vote::handlers` and `outbe_update::handlers`
//! until `outbe-chain-iuh.6` deletes `crates/blockchain/evm/src/handlers.rs`.

pub mod vote {
    pub use outbe_vote::handlers::{registry, VoteTargetRegistry};
}

pub mod update {
    pub use outbe_update::handlers::{registry, UpgradeHandlerRegistry};
}
