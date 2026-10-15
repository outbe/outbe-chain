//! Per-scenario wall-clock watchdog that tears the run down on a hung scenario.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::env::Environment;
use crate::evidence;

#[derive(Clone)]
pub(crate) struct WatchdogHandle {
    shared: Arc<(Mutex<WatchdogState>, Condvar)>,
}

struct WatchdogState {
    active: Option<(String, Instant, Duration)>,
    stop: bool,
}

pub(crate) struct ScenarioWatchdog {
    handle: WatchdogHandle,
    thread: Option<thread::JoinHandle<()>>,
}

impl ScenarioWatchdog {
    pub(crate) fn start(env: Environment) -> Self {
        let shared = Arc::new((
            Mutex::new(WatchdogState {
                active: None,
                stop: false,
            }),
            Condvar::new(),
        ));
        let handle = WatchdogHandle {
            shared: Arc::clone(&shared),
        };
        let thread = thread::Builder::new()
            .name("outbe-e2e-watchdog".to_owned())
            .spawn(move || watchdog_loop(shared, env))
            .expect("spawn E2E scenario watchdog");
        Self {
            handle,
            thread: Some(thread),
        }
    }

    pub(crate) fn handle(&self) -> WatchdogHandle {
        self.handle.clone()
    }

    pub(crate) fn stop(mut self) {
        self.handle.stop();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join E2E scenario watchdog");
        }
    }
}

impl WatchdogHandle {
    pub(crate) fn arm(&self, scenario: String, timeout: Duration) -> Instant {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("lock E2E watchdog");
        let deadline = Instant::now() + timeout;
        state.active = Some((scenario, deadline, timeout));
        wake.notify_all();
        deadline
    }

    pub(crate) fn disarm(&self) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("lock E2E watchdog");
        state.active = None;
        wake.notify_all();
    }

    fn stop(&self) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("lock E2E watchdog");
        state.stop = true;
        state.active = None;
        wake.notify_all();
    }
}

fn watchdog_loop(shared: Arc<(Mutex<WatchdogState>, Condvar)>, env: Environment) {
    let (state, wake) = &*shared;
    loop {
        let mut state = state.lock().expect("lock E2E watchdog");
        while state.active.is_none() && !state.stop {
            state = wake.wait(state).expect("wait for E2E scenario");
        }
        if state.stop {
            return;
        }
        let (scenario, deadline, timeout) = state.active.clone().expect("active watchdog scenario");
        let now = Instant::now();
        if now < deadline {
            let (next, _) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("wait for E2E scenario deadline");
            drop(next);
            continue;
        }
        state.active = None;
        drop(state);

        eprintln!(
            "outbe-e2e: scenario {scenario:?} exceeded {}s; retaining diagnostics and tearing down",
            timeout.as_secs()
        );
        if let Err(error) = evidence::write_run_timeout(&env, &scenario, timeout) {
            eprintln!("outbe-e2e: could not write timeout evidence: {error:#}");
        }
        crate::shutdown_and_exit_with_code(&env, 1);
    }
}
