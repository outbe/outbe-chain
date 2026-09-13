use super::*;
use crate::{WwdDayType, WwdStatus};
use alloy_sol_types::SolEvent;
use outbe_nod::NodContract;
use outbe_oracle::schema::OracleContract;
use std::sync::atomic::{AtomicUsize, Ordering};

mod fixtures;
use fixtures::{
    assert_no_ocomp_job, begin_persistent_active_scope, create_waiting_day,
    end_persistent_active_scope, issue_one_tribute_in_scope, run_start_command,
    seed_missed_offering_day, FailOncePartitionLookup, FailSecondPartitionLookup,
};

mod advance;
use advance::run_advance_command;
mod day_limit;
mod genesis;
use genesis::provider_status_events;
mod local_terminal;
mod missed_offering;
mod ocomp_admission;
mod scheduling;
