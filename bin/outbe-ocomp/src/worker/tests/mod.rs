use crate::control::poc_schema_limits;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::derive_poseidon_entity_id;
use outbe_lysis::program_v1::phases::{
    output_finalize, AmountRecordV1, AmountRunV1, GratisLeafPrefixV1,
};
use outbe_ocomp_protocol::unit::{PlanCommitmentV1, WorkOutputHeaderV1};
use outbe_primitives::time::WorldwideDay;

use super::{
    require_complete_root_values, require_lease_active, require_plan_binding,
    require_root_reduce_finalized_binding, require_root_reduce_shuffle_population,
    terminal_completion, ExpectedPlanBindingsV1, WorkerError,
};

mod authority;

mod channel;

mod root_reduce;
