use super::*;
use crate::{WwdDayType, WwdStatus};
use alloy_sol_types::SolEvent;
use outbe_nod::NodContract;
use outbe_oracle::schema::OracleContract;

mod fixtures;
use fixtures::{
    assert_no_ocomp_job, create_waiting_day, issue_one_tribute, run_start_command,
    seed_missed_offering_day, FixtureTribute,
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
