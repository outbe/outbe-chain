use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use crate::*;

#[path = "launch_fixtures.rs"]
mod fixtures;

use fixtures::{full_node_admission_anchor, ExecutionTeardownSentinel, ThreadDropRecorder};

#[path = "admission_gate_tests.rs"]
mod admission_gate;

#[path = "admission_provider.rs"]
mod admission_provider;

#[path = "admission_binding_tests.rs"]
mod admission_binding;

#[path = "configuration_tests.rs"]
mod configuration;

#[path = "identity_tests.rs"]
mod identity;

#[path = "shutdown_tests.rs"]
mod shutdown;

#[cfg(feature = "snapshot-integration")]
#[path = "state_sync_tests.rs"]
mod state_sync;
