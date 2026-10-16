use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::{mpsc, Barrier};
use std::thread;
use std::time::Duration;

use super::{Arc, CrsInitError, Envelope, Error, HandlerError, OnceLock, Ordering, Worker};

// Timeouts detect a stuck test; ordering is established by channels and barriers.
fn receive<T>(receiver: &mpsc::Receiver<T>) -> T {
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("worker test stalled")
}

#[test]
fn concurrent_calls_share_one_owner_and_keep_setup_and_prove_atomic() {
    const CALLERS: usize = 8;
    let attempts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let (events_tx, events_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();
    let worker = Arc::new(
        Worker::start(CALLERS, {
            let attempts = Arc::clone(&attempts);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let owner = thread::current().id();
                // This state cannot be sent to another executor thread.
                let configured = Rc::new(Cell::new(None));
                let mut completed = 0;
                Ok(move |request: usize| {
                    assert_eq!(thread::current().id(), owner);
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    configured.set(Some(request));
                    events_tx.send((request, "setup", owner)).unwrap();
                    receive(&continue_rx);
                    assert_eq!(configured.get(), Some(request));
                    events_tx.send((request, "prove", owner)).unwrap();
                    completed += 1;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok((request, owner, completed))
                })
            }
        })
        .unwrap(),
    );
    let start = Arc::new(Barrier::new(CALLERS + 1));
    let (results_tx, results_rx) = mpsc::channel();
    let mut callers = Vec::new();
    for request in 0..CALLERS {
        let worker = Arc::clone(&worker);
        let start = Arc::clone(&start);
        let results_tx = results_tx.clone();
        callers.push(thread::spawn(move || {
            start.wait();
            results_tx.send((request, worker.call(request))).unwrap();
        }));
    }
    start.wait();
    let mut owner = None;
    for _ in 0..CALLERS {
        let (request, phase, thread_id) = receive(&events_rx);
        assert_eq!(phase, "setup");
        assert_ne!(thread_id, thread::current().id());
        assert_eq!(*owner.get_or_insert(thread_id), thread_id);
        continue_tx.send(()).unwrap();
        assert_eq!(receive(&events_rx), (request, "prove", thread_id));
    }
    let mut completions = Vec::new();
    for _ in 0..CALLERS {
        let (request, result) = receive(&results_rx);
        let (response, thread_id, completed) = result.unwrap();
        assert_eq!(response, request);
        assert_eq!(Some(thread_id), owner);
        completions.push(completed);
    }
    for caller in callers {
        caller.join().unwrap();
    }
    completions.sort_unstable();
    assert_eq!(completions, (1..=CALLERS).collect::<Vec<_>>());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn ordinary_proof_error_does_not_stop_the_executor() {
    let worker = Worker::start(1, || {
        let mut calls = 0;
        Ok(move |fail: bool| {
            calls += 1;
            if fail {
                Err(Error::Proof("rejected command".into()))
            } else {
                Ok(calls)
            }
        })
    })
    .unwrap();

    assert_eq!(
        worker.call(true),
        Err(HandlerError::Command("rejected command".into()))
    );
    assert_eq!(worker.call(false), Ok(2));
    assert!(!worker.failed.load(Ordering::Acquire));
}

#[test]
fn initialization_failure_is_cached_once_for_concurrent_and_later_callers() {
    let cache = Arc::new(OnceLock::<Result<Worker<usize, usize>, HandlerError>>::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Barrier::new(9));
    let expected = HandlerError::Initialization(CrsInitError::Failed("fake setup failure".into()));
    let (results_tx, results_rx) = mpsc::channel();
    let mut callers = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let attempts = Arc::clone(&attempts);
        let start = Arc::clone(&start);
        let expected = expected.clone();
        let results_tx = results_tx.clone();
        callers.push(thread::spawn(move || {
            start.wait();
            let result = cache.get_or_init(|| {
                Worker::start(1, move || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err::<fn(usize) -> Result<usize, Error>, _>(expected)
                })
            });
            results_tx.send(result.as_ref().err().cloned()).unwrap();
        }));
    }
    start.wait();
    for _ in 0..8 {
        assert_eq!(receive(&results_rx), Some(expected.clone()));
    }
    for caller in callers {
        caller.join().unwrap();
    }
    let result = cache.get_or_init(|| {
        attempts.fetch_add(1, Ordering::SeqCst);
        Worker::start(1, || Ok(Ok))
    });
    assert_eq!(result.as_ref().err(), Some(&expected));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn initialization_panic_returns_an_error_before_readiness() {
    type Executor = fn(usize) -> Result<usize, Error>;
    let result = Worker::<usize, usize>::start(1, || -> Result<Executor, HandlerError> {
        panic!("fake initialization panic");
    });
    assert!(matches!(
        result,
        Err(HandlerError::Initialization(CrsInitError::Panicked))
    ));
}

#[test]
fn disconnected_transport_is_not_an_invalid_proof_result() {
    let (sender, receiver) = mpsc::sync_channel::<Envelope<usize, usize>>(1);
    drop(receiver);
    let worker = Worker {
        sender,
        failed: Arc::new(super::AtomicBool::new(false)),
    };
    assert_eq!(worker.call(1), Err(HandlerError::RequestDisconnected));

    let (sender, receiver) = mpsc::sync_channel::<Envelope<usize, usize>>(1);
    let worker = Worker {
        sender,
        failed: Arc::new(super::AtomicBool::new(false)),
    };
    let consumer = thread::spawn(move || drop(receive(&receiver)));
    assert_eq!(worker.call(1), Err(HandlerError::ResponseDisconnected));
    consumer.join().unwrap();
}

#[test]
fn bounded_queue_applies_backpressure_then_accepts_the_waiting_call() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = Arc::new(
        Worker::start(1, move || {
            Ok(move |request: usize| {
                entered_tx.send(request).unwrap();
                receive(&release_rx);
                Ok(request * 10)
            })
        })
        .unwrap(),
    );
    let (first_tx, first_rx) = mpsc::channel();
    let first = {
        let worker = Arc::clone(&worker);
        thread::spawn(move || first_tx.send(worker.call(1)).unwrap())
    };
    assert_eq!(receive(&entered_rx), 1);
    let (queued_tx, queued_rx) = mpsc::sync_channel(1);
    assert!(worker
        .sender
        .try_send(Envelope {
            request: 2,
            reply: queued_tx
        })
        .is_ok());
    let (extra_tx, _) = mpsc::sync_channel(1);
    assert!(matches!(
        worker.sender.try_send(Envelope {
            request: 99,
            reply: extra_tx
        }),
        Err(mpsc::TrySendError::Full(_))
    ));
    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (last_tx, last_rx) = mpsc::channel();
    let last = {
        let worker = Arc::clone(&worker);
        thread::spawn(move || {
            started_tx.send(()).unwrap();
            last_tx.send(worker.call(3)).unwrap();
        })
    };
    receive(&started_rx);
    // The first executor is held and the only queue slot contains request 2.
    assert!(matches!(last_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release_tx.send(()).unwrap();
    assert_eq!(receive(&first_rx), Ok(10));
    assert_eq!(receive(&entered_rx), 2);
    assert!(matches!(last_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release_tx.send(()).unwrap();
    assert_eq!(receive(&queued_rx), Ok(20));
    assert_eq!(receive(&entered_rx), 3);
    release_tx.send(()).unwrap();
    assert_eq!(receive(&last_rx), Ok(30));
    first.join().unwrap();
    last.join().unwrap();
}

#[test]
fn panic_releases_current_queued_and_waiting_calls_and_is_terminal() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (panic_tx, panic_rx) = mpsc::channel();
    let worker = Arc::new(
        Worker::<usize, usize>::start(1, {
            let attempts = Arc::clone(&attempts);
            let executions = Arc::clone(&executions);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(move |_| {
                    executions.fetch_add(1, Ordering::SeqCst);
                    entered_tx.send(()).unwrap();
                    receive(&panic_rx);
                    panic!("fake executor panic");
                })
            }
        })
        .unwrap(),
    );
    let (current_tx, current_rx) = mpsc::channel();
    let current = {
        let worker = Arc::clone(&worker);
        thread::spawn(move || current_tx.send(worker.call(1)).unwrap())
    };
    receive(&entered_rx);
    let (queued_tx, queued_rx) = mpsc::sync_channel(1);
    assert!(worker
        .sender
        .try_send(Envelope {
            request: 2,
            reply: queued_tx
        })
        .is_ok());
    let (extra_tx, _) = mpsc::sync_channel(1);
    assert!(matches!(
        worker.sender.try_send(Envelope {
            request: 99,
            reply: extra_tx
        }),
        Err(mpsc::TrySendError::Full(_))
    ));
    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let waiting = {
        let worker = Arc::clone(&worker);
        thread::spawn(move || {
            started_tx.send(()).unwrap();
            waiting_tx.send(worker.call(3)).unwrap();
        })
    };
    receive(&started_rx);
    assert!(matches!(
        waiting_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    panic_tx.send(()).unwrap();
    assert_eq!(receive(&current_rx), Err(HandlerError::Terminal));
    assert_eq!(
        queued_rx.recv_timeout(Duration::from_secs(10)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    assert_eq!(receive(&waiting_rx), Err(HandlerError::Terminal));
    assert!(worker.failed.load(Ordering::Acquire));
    assert_eq!(worker.call(4), Err(HandlerError::Terminal));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    current.join().unwrap();
    waiting.join().unwrap();
}
