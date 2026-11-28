//! The validator role: signs checkpoints, announces the storage location and
//! reports liveness to the HyperlaneController.

pub mod announce;
pub mod liveness;
pub mod signer;
