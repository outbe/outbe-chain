//! Chain-interaction handle: reads/sends natively via alloy ([`crate::internal::eth`]),
//! governance/tribute sends via `outbe-cli`, and the poll/wait loops that back the
//! scenarios.
//!
//! This is the typed replacement for the `cast`-based RPC readers and the
//! scenario polling helpers used by the lifecycle and update flows.
//! Reads return `Option`. `None` is the analogue of the shell
//! `2>/dev/null || echo dn`. Only governance (`vote`), tribute, `confirm-ready`,
//! and `slash config` still go through `outbe-cli` (the product CLI under test).

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall as _};
use eyre::{ensure, eyre, Result, WrapErr as _};

use outbe_primitives::time::EmissionDate;
use serde::{Deserialize, Serialize};

use crate::internal::eth::{IDesis, IPromisLimit};
use crate::internal::{
    addresses,
    config::Config,
    eth::{
        self, IAgentReward, ICodeRegistry, IGovernance, IL2Registry, IMetadosis, INod,
        ISlashIndicator, IStaking, ITeeRegistryV1, ITribute, IUpdate, IValidatorSet,
        IValidatorSetRaw, IVote, IZeroFee,
    },
    parse::{self, ScheduledUpdate, VoteStatus},
    shell::Sh,
};
use crate::world::state::FixtureState;
use crate::world::validators::{Operator, Validator};

mod client;

mod desis;
mod finality;
mod governance;
mod l2;
mod metadosis;
mod nod;
mod oracle;

mod promis;
mod radicle;
mod state_entities;
mod tribute;
mod validators;
mod zero_fee;

#[cfg(test)]
mod tests;

pub use client::{IOracle, ITributeFactory, Rpc, TxOutcome};
pub use finality::{BlockCommitmentV1, FinalizedCheckpoint};
pub use governance::StablecoinProposal;
pub use metadosis::{
    MetadosisEmissionDayStateV1, MetadosisWorldwideDayStartedV1, MetadosisWorldwideDayStateV1,
    MetadosisWorldwideDayTerminalReceiptV1,
};

pub use oracle::OracleRateDataV1;
pub use tribute::TributeZkOffer;
pub use validators::{ValidatorP2pAddress, ValidatorRecord};

use client::{canonical_rpc_log_block_hash, decode_rpc_data_words, rpc_log_block_number};
use client::{receipt_has_log, receipt_status, unix_time_millis};

#[cfg(test)]
use tribute::encode_reward_bearing_tribute_plaintext;
#[cfg(test)]
use zero_fee::zerofee_rollover_wait_budget_secs;

use tribute::RewardBearingTribute;
pub use tribute::TributeOfferParams;
pub(crate) use tribute::TributeProofInput;

#[cfg(test)]
use nod::classify_owner_index_result;
