//! Lifecycle tests: tally, begin-block hooks, slash window, OCOMP atomicity.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::ValidatorLifecycle;

use crate::schema::{OracleContract, SCALE_1E18};

use super::common::*;

mod arithmetic;
mod hooks;
mod ocomp;
mod tally;
