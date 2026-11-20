use super::*;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
enum Event {
    Observation(engine::Step),
    Prepared,
    Failed,
    Fatal,
}
struct Task {
    state: Arc<Mutex<Harness>>,
    authority: authority::Tracker,
    events: mpsc::UnboundedSender<Event>,
}
impl driver::LifecycleTask for Task {
    type Error = engine::SupervisorError<std::io::Error, std::convert::Infallible>;
    fn is_retryable(error: &Self::Error) -> bool {
        retry::observation(error)
    }
    async fn tick(&mut self) -> Result<engine::Step, Self::Error> {
        let mut state = self.state.lock().await;
        let Harness {
            live,
            journal,
            chain,
            dir,
        } = &mut *state;
        let step = engine::step(
            chain,
            &Signer,
            engine::ResidentSession {
                live,
                journal,
                node_data_dir: dir,
                phase: RenewalUpgradePhaseV1::Idle,
            },
        )
        .await;
        let step = match step {
            Ok(step) => step,
            Err(error) => {
                self.events.send(Event::Failed).unwrap();
                return Err(error);
            }
        };
        self.authority.observe(&state);
        self.events.send(Event::Observation(step.clone())).unwrap();
        Ok(step)
    }
}
pub(super) struct Running {
    pub(super) state: Arc<Mutex<Harness>>,
    pub(super) authority: outbe_tee::TeeAuthorityChannel,
    cancellation: CancellationToken,
    wake: Arc<Notify>,
    events: mpsc::UnboundedReceiver<Event>,
    handle: tokio::task::JoinHandle<()>,
}
impl Running {
    pub(super) fn new(h: Harness, validator: bool) -> Self {
        let state = Arc::new(Mutex::new(h));
        let cancellation = CancellationToken::new();
        let wake = Arc::new(Notify::new());
        let (sender, events) = mpsc::unbounded_channel();
        let tracker = authority::Tracker::new(validator);
        let authority = tracker.channel.clone();
        let mut task = Task {
            state: state.clone(),
            authority: tracker,
            events: sender.clone(),
        };
        let stop = cancellation.clone();
        let finalized = wake.clone();
        let handle = tokio::spawn(async move {
            match driver::prepare(&mut task, &stop, &finalized).await {
                Ok(Some(anchor)) => {
                    assert!(anchor.height > 0);
                    assert!(!anchor.hash.is_zero());
                    sender.send(Event::Prepared).unwrap();
                    driver::run(task, stop, finalized).await;
                }
                Err(driver::PrepareError::Cancelled) => {}
                Err(driver::PrepareError::Observation(_)) => {
                    sender.send(Event::Fatal).unwrap();
                }
                other => panic!("pre-sync failed: {other:?}"),
            }
        });
        Self {
            state,
            authority,
            cancellation,
            wake,
            events,
            handle,
        }
    }
    pub(super) async fn until(
        &mut self,
        predicate: impl Fn(&engine::Step) -> bool,
    ) -> engine::Step {
        loop {
            match self
                .events
                .recv()
                .await
                .expect("driver exited before observation")
            {
                Event::Observation(step) if predicate(&step) => return step,
                _ => {}
            }
        }
    }
    pub(super) async fn change(&self, change: impl FnOnce(&mut Harness)) {
        change(&mut *self.state.lock().await);
        self.wake.notify_one();
    }
    pub(super) async fn finish_join(&mut self) {
        self.until(|s| {
            matches!(
                s,
                engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
            )
        })
        .await;
        self.change(Harness::finalize).await;
        self.until(|s| {
            matches!(
                s,
                engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
            )
        })
        .await;
        while !matches!(self.events.recv().await.unwrap(), Event::Prepared) {}
    }
    pub(super) async fn stop(self) {
        self.cancellation.cancel();
        self.handle.await.unwrap();
    }
}

#[tokio::test]
async fn gas_wait_then_initial_join_before_stake_verifies_and_imports() {
    let mut h = Harness::new();
    h.chain.balance = U256::ZERO;
    let mut r = Running::new(h, true);
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::WaitForGasBalance)
        )
    })
    .await;
    assert_eq!(r.state.lock().await.sent(), 0);
    r.change(|h| h.chain.balance = U256::MAX).await;
    r.finish_join().await;
    assert!(r.authority.snapshot().is_authority_open());
    let state = r.state.lock().await;
    assert_eq!(state.sent(), 1);
    assert!(state.live.admission_anchor().unwrap().is_some());
    assert_eq!(
        B256::from(state.live.network_encryption_public().unwrap()),
        state.chain.public
    );
    assert!(state
        .chain
        .validator_record(state.chain.at, Signer::address())
        .await
        .unwrap()
        .is_none());
    drop(state);
    r.stop().await;
}

#[tokio::test]
async fn cold_jailed_waits_then_active_rejoins_and_opens_authority() {
    let mut h = Harness::new();
    h.admit().await;
    let old = h.live.manifest().enclave_id().unwrap();
    h.live = Arc::new(proof::context(
        12,
        &proof::descriptor(&finality::FinalityCommitteeFixture::new(10)),
    ));
    h.chain.jailed = true;
    let mut r = Running::new(h, true);
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::WaitForUnjail)
        )
    })
    .await;
    assert_eq!(r.state.lock().await.sent(), 1);
    assert!(!r.authority.snapshot().is_authority_open());
    r.change(|h| h.chain.jailed = false).await;
    r.finish_join().await;
    assert!(r.authority.snapshot().is_authority_open());
    let state = r.state.lock().await;
    assert_ne!(state.live.manifest().enclave_id().unwrap(), old);
    assert_eq!(state.chain.binding.as_ref().unwrap().binding_version, 2);
    assert_eq!(state.sent(), 2);
    drop(state);
    r.stop().await;
}

#[tokio::test]
async fn running_jail_preserves_ram_and_reopens_the_same_channel_without_signer_promotion() {
    let mut r = Running::new(Harness::new(), true);
    r.finish_join().await;
    let live = r.state.lock().await.live.clone();
    let public = live.network_encryption_public().unwrap();
    r.change(|h| h.chain.jailed = true).await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::WaitForUnjail)
        )
    })
    .await;
    assert_eq!(
        r.authority.snapshot(),
        outbe_tee::TeeAuthorityState::SuspendedJailed
    );
    r.change(|h| h.chain.jailed = false).await;
    r.until(|s| matches!(s, engine::Step::Renewal(RenewalStep::NotDue { .. })))
        .await;
    assert!(r.authority.snapshot().is_authority_open());
    let state = r.state.lock().await;
    assert!(Arc::ptr_eq(&live, &state.live));
    assert_eq!(state.live.network_encryption_public().unwrap(), public);
    assert_eq!(state.sent(), 1);
    drop(state);
    r.stop().await;
}

#[tokio::test]
async fn background_zero_balance_renewal_runs_for_unstaked_validator_and_keyed_full_node() {
    for validator in [true, false] {
        let mut r = Running::new(Harness::new(), validator);
        r.finish_join().await;
        r.change(|h| {
            h.chain.balance = U256::ZERO;
            h.chain.at.timestamp = h.chain.binding.as_ref().unwrap().valid_until - 3600;
        })
        .await;
        r.until(|s| matches!(s, engine::Step::Renewal(RenewalStep::Submitted { .. })))
            .await;
        let state = r.state.lock().await;
        let record = state.journal.load().unwrap().unwrap();
        assert_eq!(record.kind, ServiceTxKindV1::Renew);
        use alloy_consensus::{transaction::RlpEcdsaDecodableTx, TxEip1559};
        let tx = TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
        assert_eq!(tx.tx().max_priority_fee_per_gas, 0);
        assert_eq!(tx.tx().value, U256::ZERO);
        drop(state);
        r.stop().await;
    }
}

#[tokio::test]
async fn expired_lease_rejoins_with_fresh_binding_and_reopens_without_renew() {
    let mut r = Running::new(Harness::new(), true);
    r.finish_join().await;
    let live = r.state.lock().await.live.clone();
    let old = r.state.lock().await.chain.binding.clone().unwrap();
    r.change(|h| {
        h.chain.jailed = true;
        h.chain.at.timestamp = old.valid_until;
    })
    .await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::WaitForUnjail)
        )
    })
    .await;
    assert_eq!(r.state.lock().await.sent(), 1);
    r.change(|h| h.chain.jailed = false).await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
        )
    })
    .await;
    let state = r.state.lock().await;
    let intent = state.pending_intent();
    assert_eq!(
        state.journal.load().unwrap().unwrap().kind,
        ServiceTxKindV1::Register
    );
    assert_ne!(intent.binding_id, old.binding_id);
    assert_eq!(intent.binding_version, old.binding_version + 1);
    drop(state);
    r.change(Harness::finalize).await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
        )
    })
    .await;
    assert!(r.authority.snapshot().is_authority_open());
    assert!(Arc::ptr_eq(&live, &r.state.lock().await.live));
    assert_eq!(r.state.lock().await.sent(), 2);
    r.stop().await;
}

#[tokio::test]
async fn external_nonce_displacement_rebuilds_then_reconciles_applied_mutation_without_reset() {
    let mut h = Harness::new();
    h.chain.fail_nonce = true;
    let mut r = Running::new(h, true);
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::WaitForFinality)
        )
    })
    .await;
    let state = r.state.lock().await;
    let live = state.live.clone();
    let first = state.journal.load().unwrap().unwrap();
    drop(state);
    r.change(|h| {
        h.chain.nonces = [1; 3];
        h.chain.fail_nonce = false;
    })
    .await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
        )
    })
    .await;
    let state = r.state.lock().await;
    let next = state.journal.load().unwrap().unwrap();
    assert_eq!(next.nonce, 1);
    assert_ne!(first.tx_hash, next.tx_hash);
    assert!(Arc::ptr_eq(&live, &state.live));
    drop(state);
    r.change(|h| {
        h.finalize();
        h.chain.nonces = [9; 3];
    })
    .await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
        )
    })
    .await;
    while !matches!(r.events.recv().await.unwrap(), Event::Prepared) {}
    r.until(|s| matches!(s, engine::Step::Renewal(RenewalStep::NotDue { .. })))
        .await;
    let state = r.state.lock().await;
    assert_eq!(state.sent(), 2);
    assert!(Arc::ptr_eq(&live, &state.live));
    assert_eq!(
        state.journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::Completed
    );
    drop(state);
    r.stop().await;
}

#[tokio::test]
async fn cancellation_interrupts_unfunded_presync_without_sending() {
    let mut h = Harness::new();
    h.chain.balance = U256::ZERO;
    let mut r = Running::new(h, true);
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::WaitForGasBalance)
        )
    })
    .await;
    assert_eq!(r.state.lock().await.sent(), 0);
    r.stop().await;
}

#[tokio::test]
async fn candidate_preparation_services_source_renewal_and_promoted_binding_needs_no_successor() {
    let mut h = Harness::new();
    h.admit().await;
    h.chain.at.timestamp = h.chain.binding.as_ref().unwrap().valid_until - 3600;
    let mut view = h
        .chain
        .registry_state(
            h.chain.at,
            &outbe_tee::registry_view::NodeBindingSelectorV1::Validator(Signer::address()),
        )
        .await
        .unwrap();
    view.successor_policy = Some(outbe_tee::registry_view::FinalizedStagedSuccessorPolicyV1 {
        finalized_height: h.chain.at.height,
        finalized_hash: h.chain.at.hash,
        proposal_id: U256::from(1),
        policy: view.policy.clone(),
    });
    assert!(!engine::needs_successor(&view).unwrap());
    let submitted = engine::maintain_source(
        &h.chain,
        &Signer,
        engine::ResidentSession {
            live: &h.live,
            journal: &mut h.journal,
            node_data_dir: &h.dir,
            phase: RenewalUpgradePhaseV1::CandidatePrepared,
        },
    )
    .await
    .unwrap();
    assert!(matches!(submitted, Some(RenewalStep::Submitted { .. })));
    assert_eq!(
        h.journal.load().unwrap().unwrap().kind,
        ServiceTxKindV1::Renew
    );
}

#[tokio::test]
async fn finalized_renewal_keeps_resident_admission_and_schedules_the_next_renewal() {
    let mut h = Harness::new();
    h.admit().await;
    let live = h.live.clone();
    h.chain.at.timestamp = h.chain.binding.as_ref().unwrap().valid_until - 3600;
    assert!(matches!(
        h.step().await,
        engine::Step::Renewal(RenewalStep::Submitted { .. })
    ));
    let renewal = h.pending_intent();
    let binding = h.chain.binding.as_mut().unwrap();
    binding.intent_hash = renewal.intent_hash().unwrap();
    binding.renewal_nonce = renewal.renewal_nonce;
    binding.registration_version = renewal.registration_version;
    binding.valid_until = renewal.requested_valid_until;
    h.chain.nonces = [2; 3];
    h.chain.at.height += 1;
    h.chain.at.timestamp += 1;
    assert!(
        engine::source_renewal_settled(&h.chain, &Signer, &h.live, &mut h.journal)
            .await
            .unwrap()
    );
    assert!(matches!(
        h.step().await,
        engine::Step::Renewal(RenewalStep::NotDue { .. })
    ));
    assert!(Arc::ptr_eq(&live, &h.live));
    assert_eq!(h.sent(), 2);
    h.chain.at.timestamp = h.chain.binding.as_ref().unwrap().valid_until - 3600;
    assert!(matches!(
        h.step().await,
        engine::Step::Renewal(RenewalStep::Submitted { .. })
    ));
    assert_eq!(
        h.journal.load().unwrap().unwrap().kind,
        ServiceTxKindV1::Renew
    );
}

#[tokio::test]
async fn presync_retries_port_failures_without_replacing_resident_or_register() {
    let mut h = Harness::new();
    let live = h.live.clone();
    h.chain.checkpoint_failure = true;
    let mut r = Running::new(h, true);
    assert!(matches!(r.events.recv().await, Some(Event::Failed)));
    {
        let state = r.state.lock().await;
        assert_eq!(state.sent(), 0);
        assert!(state.journal.load().unwrap().is_none());
        assert!(Arc::ptr_eq(&live, &state.live));
    }
    r.change(|h| h.chain.checkpoint_failure = false).await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
        )
    })
    .await;
    let record = r.state.lock().await.journal.load().unwrap().unwrap();
    r.change(|h| {
        h.finalize();
        h.chain.proof_failure = true;
    })
    .await;
    assert!(matches!(r.events.recv().await, Some(Event::Failed)));
    {
        let state = r.state.lock().await;
        assert_eq!(state.sent(), 1);
        assert!(Arc::ptr_eq(&live, &state.live));
        assert!(!live.offer_key_ready().unwrap());
        assert_eq!(
            state.journal.load().unwrap().unwrap().raw_signed_tx,
            record.raw_signed_tx
        );
    }
    r.change(|h| h.chain.proof_failure = false).await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
        )
    })
    .await;
    while !matches!(r.events.recv().await.unwrap(), Event::Prepared) {}
    {
        let state = r.state.lock().await;
        assert!(Arc::ptr_eq(&live, &state.live));
        assert!(live.offer_key_ready().unwrap());
        assert!(live.admission_anchor().unwrap().is_some());
        assert_eq!(state.sent(), 1);
        assert_eq!(
            state.journal.load().unwrap().unwrap().state,
            ServiceTxStateV1::Completed
        );
    }
    r.stop().await;
}

#[tokio::test]
async fn completed_register_steady_state_never_queries_a_receipt() {
    let mut h = Harness::new();
    h.admit().await;
    h.chain
        .receipt_calls
        .store(0, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        h.step().await,
        engine::Step::Renewal(RenewalStep::NotDue { .. })
    ));
    assert_eq!(
        h.chain
            .receipt_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(h.sent(), 1);
}

#[tokio::test]
async fn cancellation_interrupts_retry_wait_and_fatal_journal_stops_presync() {
    let mut h = Harness::new();
    h.chain.checkpoint_failure = true;
    let mut r = Running::new(h, true);
    assert!(matches!(r.events.recv().await, Some(Event::Failed)));
    r.stop().await;

    let h = Harness::new();
    std::fs::write(
        h.dir.join("tee-service-tx-v1/journal.json"),
        b"invalid journal",
    )
    .unwrap();
    let mut r = Running::new(h, true);
    assert!(matches!(r.events.recv().await, Some(Event::Failed)));
    assert!(matches!(r.events.recv().await, Some(Event::Fatal)));
    assert_eq!(r.state.lock().await.sent(), 0);
    r.handle.await.unwrap();
}

#[tokio::test]
async fn registration_binding_id_uses_lifecycle_domain_and_versioned_identity() {
    let h = Harness::new();
    let view = h
        .chain
        .registry_state(
            h.chain.at,
            &outbe_tee::registry_view::NodeBindingSelectorV1::Validator(Signer::address()),
        )
        .await
        .unwrap();
    let mut identity = Vec::new();
    identity.extend_from_slice(h.live.manifest().node_id.node_id_hash().unwrap().as_slice());
    identity.extend_from_slice(h.live.manifest().enclave_id().unwrap().as_slice());
    identity.extend_from_slice(view.policy.policy_hash().unwrap().as_slice());
    identity.extend_from_slice(&1_u64.to_be_bytes());
    identity.extend_from_slice(&0_u64.to_be_bytes());
    let expected = alloy_primitives::keccak256(
        [b"outbe/tee/lifecycle-binding/v1".as_slice(), &identity].concat(),
    );
    let previous = alloy_primitives::keccak256(
        [b"outbe/tee/bootstrap-binding/v1".as_slice(), &identity].concat(),
    );
    let actual = engine::binding_id(&h.live, &view).unwrap();
    assert_eq!(actual, expected);
    assert_ne!(actual, previous);
}

#[test]
fn retry_classification_preserves_fatal_enrollment_and_identity_errors() {
    type Error = engine::SupervisorError<std::io::Error, std::convert::Infallible>;
    let transient: [Error; 4] = [
        LifecyclePortError::Source(std::io::Error::from(std::io::ErrorKind::TimedOut)).into(),
        EnrollmentError::Port(LifecyclePortError::Source(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset,
        )))
        .into(),
        EnrollmentError::Reconcile(ReconcileError::CheckpointMismatch).into(),
        RenewalError::Reconcile(ReconcileError::CheckpointMismatch).into(),
    ];
    for error in transient {
        assert!(retry::observation(&error), "{error:?}");
    }
    let fatal: [Error; 7] = [
        EnrollmentError::LiveRejoinForbiddenInMode {
            mode: AttestationMode::DcapRequired,
        }
        .into(),
        EnrollmentError::CompetingLiveBinding.into(),
        EnrollmentError::InvalidConfiguration.into(),
        EnrollmentError::IdentityMismatch.into(),
        EnrollmentError::WrongServiceTransaction.into(),
        RenewalError::BindingMismatch.into(),
        RenewalError::Reconcile(ReconcileError::FinalizedMutationConflict).into(),
    ];
    for error in fatal {
        assert!(!retry::observation(&error), "{error:?}");
    }
}
