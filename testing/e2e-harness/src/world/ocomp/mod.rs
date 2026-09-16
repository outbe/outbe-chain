//! Scenario-owned OCOMP process topology.
//!
//! The handle exposes only fixed roles and typed fault operations. It has no
//! method that can insert a JobIntent, result, root or chain state; scenarios
//! must observe those values through the production RPC/control/artifact path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::Result;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ocomp-integration")]
use alloy_consensus::TxEip1559;
#[cfg(feature = "ocomp-integration")]
use alloy_eips::eip2718::Encodable2718 as _;
#[cfg(feature = "ocomp-integration")]
use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
#[cfg(feature = "ocomp-integration")]
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
#[cfg(feature = "ocomp-integration")]
use outbe_chain_constants::GENESIS_CONFIG_KEY;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::config::{
    OcompForkInstallClassification, OcompForkInstallV1, OcompRequestProfile,
};
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay};
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::{WwdDayType, WwdStatus};
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::{
    activation::SignOncePurpose,
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::BoundedBytes,
    profile::{CapacityProfileV1, ProtocolBundleV1},
    registry::{FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID},
    vote::{ResultVoteSigningSubjectV1, ResultVoteV1},
    PreparedVoteTransactionV1,
};
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::time::WorldwideDay;
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, ORACLE_ADDRESS, TRIBUTE_ADDRESS, VALIDATOR_SET_ADDRESS},
    signer::OutbeEvmSigner,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    OutbeHeader,
};
#[cfg(feature = "ocomp-integration")]
use std::fs::{self, File, OpenOptions};
#[cfg(feature = "ocomp-integration")]
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
#[cfg(feature = "ocomp-integration")]
use std::net::{SocketAddr, TcpStream};
#[cfg(feature = "ocomp-integration")]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
#[cfg(feature = "ocomp-integration")]
use std::process::{Command, Stdio};
#[cfg(feature = "ocomp-integration")]
use std::str::FromStr as _;
#[cfg(feature = "ocomp-integration")]
use std::sync::Arc;
#[cfg(feature = "ocomp-integration")]
use std::thread::sleep;
#[cfg(feature = "ocomp-integration")]
use std::time::{Duration, Instant};

use crate::internal::config::Config;
#[cfg(feature = "ocomp-integration")]
use crate::internal::config::E2E_ORACLE_VOTE_PERIOD_BLOCKS;
use crate::internal::proc::ChildGuard;
use crate::ocomp_evidence::{
    CorrelatedTributeFixtureV1, CorrelationError, JobIntentCorrelationV1,
    PublicTributeCorrelationV1, TributeCorrelationBuilder, ValidatorSourceCorrelationV1,
};

mod evidence;
mod fixtures;
mod identity;
mod processes;
#[cfg(test)]
mod tests;
mod topology;

pub use evidence::{
    OcompFaultRecordV1, OcompForkMismatchEvidenceV1, OcompForkRestartEvidenceV1,
    OcompProcessRecordV1, OcompScenarioTopologyV1,
};

pub use fixtures::genesis::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX;

pub use identity::{OcompLaunchIdentityEvidenceV1, OcompProcessFault, OcompProcessRole};

pub use topology::OcompTopology;

#[cfg(feature = "ocomp-integration")]
pub use fixtures::bundles::{
    OcompMeasurementForkV1, OcompMismatchedForkManifestV1, OCOMP_MEASUREMENT_ACTIVATION_HEIGHT,
};

#[cfg(feature = "ocomp-integration")]
pub use fixtures::membership::OcompDynamicMembershipForkV1;

#[cfg(feature = "ocomp-integration")]
pub use identity::OcompLaunchIdentityV1;

#[cfg(feature = "ocomp-integration")]
pub use processes::readiness::OcompRuntimeCountsV1;

#[cfg(any(test, feature = "ocomp-integration"))]
pub(crate) use processes::restart::OcompNodeFacingResumePlan;

#[cfg(feature = "ocomp-integration")]
pub(crate) use evidence::OcompCanonicalArtifactProof;

#[cfg(feature = "ocomp-integration")]
pub(crate) use fixtures::genesis::{
    OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS, OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS,
    OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE, OCOMP_PUBLIC_TRIBUTE_AMOUNT_MICRO,
};

#[cfg(feature = "ocomp-integration")]
pub(crate) use fixtures::membership::{
    stage_direct_joiner_domain_material, OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS,
    OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS, OCOMP_TEST_EPOCH_LENGTH_BLOCKS,
};

use evidence::unix_time_millis;

use topology::OwnedProcess;

#[cfg(feature = "ocomp-integration")]
use evidence::OcompArtifactPhase;

#[cfg(feature = "ocomp-integration")]
use fixtures::bundles::{
    installed_protocol_bundle_hashes, publish_bundle_catalog_entry, publish_exact_file,
    replace_json_atomically,
};

#[cfg(feature = "ocomp-integration")]
use fixtures::delegates::{
    measurement_founder_registrations, measurement_signing_key, ocomp_evm_private_key,
};

#[cfg(feature = "ocomp-integration")]
use fixtures::genesis::{
    apply_measurement_gas_envelope, capacity_tribute_private_keys, clear_seeded_metadosis_days,
    find_alloc_address_key, fund_capacity_tribute_accounts, genesis_chain_id, parse_hex_word,
    parse_outbe_chain_spec, parse_storage_word, schedule_public_measurement_day,
    seed_capacity_operator_l2_registrations, seed_fresh_metadosis_oracle_input,
};

#[cfg(feature = "ocomp-integration")]
use fixtures::membership::schedule_public_recovery_day;

#[cfg(feature = "ocomp-integration")]
use processes::readiness::tail_file;

#[cfg(all(test, feature = "ocomp-integration"))]
use topology::OcompDomain;

#[cfg(feature = "ocomp-integration")]
#[cfg(test)]
use processes::launch::{
    configure_release_layout, configure_snapshot_exporter_command,
    configure_snapshot_exporter_projection, OCOMP_BASE_PATH_ENV, OCOMP_VALIDATOR_INDEX_ENV,
};

#[cfg(feature = "ocomp-integration")]
#[cfg(test)]
use processes::readiness::{
    ensure_supervisor_status_ready, fetch_snapshot_exporter_status, SupervisorWorkerStatusV1,
};
