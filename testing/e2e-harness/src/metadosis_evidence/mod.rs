//! Metadosis-specific adapter over the domain-neutral VerificationLedger.
//!
//! The generic core owns schema and closure mechanics. This module owns only
//! the fixed Metadosis command plan, P0 process-artifact reconstruction, and
//! the rule that assertion status is derived from observed execution facts.

mod container;
mod runner;
mod schema;
mod verify;

pub use container::{run_in_pinned_linux_container, ContainerProvenanceV1};
pub use runner::{run, METADOSIS_P0_NAME, METADOSIS_P0_TARGET, METADOSIS_P0_TEST_ID};
pub use schema::{
    CargoDiscoveryReceiptV1, CommandReceiptV1, MetadosisRunReceiptV1, TestRunReceiptV1,
    METADOSIS_NAMESPACE, METADOSIS_RUN_RECEIPT_KIND, METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
};
pub use verify::{assemble, require_runner_derivation_member, verify};
