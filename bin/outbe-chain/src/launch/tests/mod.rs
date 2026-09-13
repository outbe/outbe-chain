use crate::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

mod admission;
mod bundles;
mod configuration;
mod fixtures;
mod identity;
mod node;
mod shutdown;

use fixtures::{full_node_admission_anchor, ExecutionTeardownSentinel, ThreadDropRecorder};
