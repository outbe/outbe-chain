//! Chain-interaction handle: reads/sends natively via alloy ([`crate::internal::eth`]),
//! governance/tribute sends via `outbe-cli`, and the poll/wait loops that back the
//! scenarios.
//!
//! This is the typed replacement for the `cast`-based RPC readers and the
//! scenario polling helpers used by the lifecycle and update flows.
//! Reads return `Option` - `None` is the analogue of the shell
//! `2>/dev/null || echo dn`. Only governance (`vote`), tribute, `confirm-ready`,
//! and `slash config` still go through `outbe-cli` (the product CLI under test).

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall as _};
use eyre::{ensure, eyre, Result, WrapErr as _};
use outbe_compressed_entities::{PointReadRequestV1, PointReadResultV1, SelectedHeaderV1};
#[cfg(feature = "ocomp-integration")]
use outbe_nod::NodCertifiedGenerationProjection;
#[cfg(feature = "ocomp-integration")]
use outbe_nodfactory::certified_read::active_nod_set;
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::{
    nod_materialization::NodMaterializationHeadV1,
    profile::poc_schema_limits,
    state::{ActiveGenerationV1, OcompJobRecordV1, OcompJobStatus},
    vote::OcompVoteAccountabilityV1,
};
#[cfg(feature = "ocomp-integration")]
use outbe_ocompregistry::precompile::IOcompRegistry;
use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::time::WorldwideDay;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ocomp-integration")]
use crate::internal::eth::{IDesis, INodFactory, IPromisLimit};
use crate::internal::{
    addresses,
    config::Config,
    eth::{
        self, IAgentReward, IGovernance, IL2Registry, IMetadosis, INod, IRadicleRegistry,
        ISlashIndicator, IStaking, ITeeRegistryV1, ITribute, IUpdate, IValidatorSet,
        IValidatorSetRaw, IVote, IZeroFee,
    },
    parse::{self, ScheduledUpdate, VoteStatus},
    shell::Sh,
};
use crate::ocomp_evidence::sha256_hex;
use crate::world::state::FixtureState;
use crate::world::validators::{Operator, Validator};

mod client;
mod compressed_entities;
#[cfg(feature = "ocomp-integration")]
mod desis;
mod finality;
mod governance;
mod l2;
mod materialization;
mod metadosis;
mod ocomp;
mod oracle;
#[cfg(feature = "ocomp-integration")]
mod promis;
mod radicle;
mod tribute;
mod validators;
mod zero_fee;

#[cfg(all(test, feature = "ocomp-integration"))]
#[path = "../rpc_ocomp_observation_tests.rs"]
mod ocomp_observation_tests;
#[cfg(test)]
mod tests;

pub use client::{IOracle, ITributeFactory, Rpc, TxOutcome};
pub use compressed_entities::CompressedEntityAtHeader;
pub use finality::{BlockCommitmentV1, FinalizedCheckpoint};
pub use materialization::NodMaterializationObservationV1;
pub use metadosis::{
    MetadosisWorldwideDayStartedV1, MetadosisWorldwideDayStateV1,
    MetadosisWorldwideDayStatusChangeV1, MetadosisWorldwideDayTerminalReceiptV1,
};
#[cfg(feature = "ocomp-integration")]
pub use ocomp::OcompRequestObservation;
pub use ocomp::{
    OcompCertifiedGenerationV1, OcompPublicActivationV1, OcompPublicJobRequestV1,
    OcompPublicResultVoteTransactionV1, OcompPublicVoteAccountabilityV1,
};
pub use oracle::OracleRateDataV1;
pub use tribute::TributeZkOffer;
pub use validators::{ValidatorP2pAddress, ValidatorRecord};

#[cfg(feature = "ocomp-integration")]
use client::{
    canonical_rpc_log_block_hash, decode_rpc_data_words, parse_rpc_word, rpc_log_block_number,
};
use client::{receipt_has_log, receipt_status, unix_time_millis};

#[cfg(all(test, feature = "ocomp-integration"))]
use materialization::{
    classify_owner_index_result, decode_nod_materialization_progress, MaterializationStallDeadline,
    NodMaterializationProgressV1,
};
#[cfg(all(test, feature = "ocomp-integration"))]
use ocomp::select_ocomp_job_request_log_result;
#[cfg(all(test, feature = "ocomp-integration"))]
use tribute::encode_reward_bearing_tribute_plaintext;
#[cfg(test)]
use zero_fee::zerofee_rollover_wait_budget_secs;
#[cfg(all(test, feature = "ocomp-integration"))]
use zero_fee::SPONSORSHIP_TOPIC;
