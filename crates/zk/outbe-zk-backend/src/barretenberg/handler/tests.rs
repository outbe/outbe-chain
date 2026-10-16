use std::sync::{Arc, Barrier};
use std::thread;

use super::{execute, Error, HandlerError, Mutex};

#[test]
fn operations_execute_on_the_calling_thread() {
    let state = Mutex::new(0);
    let caller = thread::current().id();
    let result = execute(&state, |count| {
        *count += 1;
        Ok((*count, thread::current().id()))
    });
    assert_eq!(result, Ok((1, caller)));
}

#[test]
fn concurrent_operations_share_state_without_interleaving() {
    let state = Arc::new(Mutex::new(Vec::new()));
    let start = Arc::new(Barrier::new(9));
    let callers: Vec<_> = (0..8)
        .map(|id| {
            let state = Arc::clone(&state);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                execute(&state, |events| {
                    events.push((id, "setup"));
                    thread::yield_now();
                    events.push((id, "prove"));
                    Ok(id)
                })
            })
        })
        .collect();
    start.wait();
    for (id, caller) in callers.into_iter().enumerate() {
        assert_eq!(caller.join().unwrap(), Ok(id));
    }
    let events = state.lock().unwrap();
    assert_eq!(events.len(), 16);
    for pair in events.chunks_exact(2) {
        assert_eq!(pair[0], (pair[1].0, "setup"));
        assert_eq!(pair[1].1, "prove");
    }
}

#[test]
fn ordinary_command_error_does_not_disable_the_handler() {
    let state = Mutex::new(0);
    let failure = execute(&state, |count| {
        *count += 1;
        Err::<(), _>(Error::Proof("rejected command".into()))
    });
    assert_eq!(
        failure,
        Err(HandlerError::Command("rejected command".into()))
    );
    assert_eq!(execute(&state, |count| Ok(*count)), Ok(1));
}

#[test]
fn panic_disables_later_operations_without_recovering_native_state() {
    let state = Mutex::new(0);
    let failure = execute(&state, |count| -> Result<(), Error> {
        *count += 1;
        panic!("fake native panic");
    });
    assert_eq!(failure, Err(HandlerError::Terminal));
    assert_eq!(execute(&state, |_| Ok(())), Err(HandlerError::Terminal));
}
