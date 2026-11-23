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
            engine::Step::Resident {
                jailed: true,
                renewal: None,
                ..
            }
        )
    })
    .await;
    assert_eq!(
        r.authority.snapshot(),
        outbe_tee::TeeAuthorityState::SuspendedJailed
    );
    r.change(|h| h.chain.jailed = false).await;
    r.until(|s| {
        matches!(
            s,
            engine::Step::Resident {
                jailed: false,
                renewal: Some(RenewalStep::NotDue { .. }),
                ..
            }
        )
    })
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
        r.until(|s| {
            matches!(
                s,
                engine::Step::Resident {
                    jailed: false,
                    renewal: Some(RenewalStep::Submitted { .. }),
                    ..
                }
            )
        })
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
    r.until(|s| {
        matches!(
            s,
            engine::Step::Resident {
                jailed: false,
                renewal: Some(RenewalStep::NotDue { .. }),
                ..
            }
        )
    })
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
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::Submitted { .. }),
            ..
        }
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
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::NotDue { .. }),
            ..
        }
    ));
    assert!(Arc::ptr_eq(&live, &h.live));
    assert_eq!(h.sent(), 2);
    h.chain.at.timestamp = h.chain.binding.as_ref().unwrap().valid_until - 3600;
    assert!(matches!(
        h.step().await,
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::Submitted { .. }),
            ..
        }
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
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::NotDue { .. }),
            ..
        }
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
    let fatal: [Error; 9] = [
        RenewalError::UpgradeProofCheckpoint(
            outbe_tee::lifecycle::renewal::UpgradeProofCheckpointError::MissingBaseFee,
        )
        .into(),
        RenewalError::Reconcile(ReconcileError::WrongSender {
            expected: Address::repeat_byte(0x11),
            actual: Address::repeat_byte(0x22),
        })
        .into(),
        EnrollmentError::CompetingLiveBinding.into(),
        EnrollmentError::InvalidConfiguration.into(),
        EnrollmentError::IdentityMismatch.into(),
        EnrollmentError::WrongServiceTransaction.into(),
        RenewalError::BindingMismatch.into(),
        RenewalError::PendingRenewDiverged.into(),
        RenewalError::Reconcile(ReconcileError::FinalizedMutationConflict).into(),
    ];
    for error in fatal {
        assert!(!retry::observation(&error), "{error:?}");
    }
}

#[tokio::test]
async fn engine_step_wrong_sender_fails_before_journal_finish_preserving_bytes() {
    use alloy_consensus::SignableTransaction as _;
    use alloy_primitives::{keccak256, TxKind};
    use outbe_tee::lifecycle::{
        ServiceBindingTargetV1, ServiceTxIntentV1, ServiceTxKindV1, ServiceTxRecordV1,
        ServiceTxStateV1,
    };

    let mut h = Harness::new();
    let other_key = k256::ecdsa::SigningKey::from_slice(&[0x55; 32]).unwrap();
    let other_sender = alloy_primitives::Address::from_private_key(&other_key);
    assert_ne!(other_sender, Signer::address());

    let at = h.chain.at;
    let manifest = h.live.manifest();
    let tx = alloy_consensus::TxEip1559 {
        chain_id: at.chain_id,
        nonce: 0,
        gas_limit: 500_000,
        max_fee_per_gas: 2,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS),
        input: Default::default(),
        ..Default::default()
    };
    let (sig, rec) = other_key
        .sign_prehash_recoverable(tx.signature_hash().as_slice())
        .unwrap();
    let sig = alloy_primitives::Signature::from_signature_and_parity(sig, rec.is_y_odd());
    let mut raw = vec![2];
    alloy_consensus::transaction::RlpEcdsaEncodableTx::rlp_encode_signed(&tx, &sig, &mut raw);
    let record = ServiceTxRecordV1 {
        tx_hash: keccak256(&raw),
        raw_signed_tx: raw.into(),
        sender: other_sender,
        nonce: 0,
        kind: ServiceTxKindV1::Register,
        public_intent: ServiceTxIntentV1 {
            target: ServiceBindingTargetV1 {
                node_id_hash: manifest.node_id.node_id_hash().unwrap(),
                enclave_id: manifest.enclave_id().unwrap(),
                binding_id: B256::repeat_byte(0x41),
                binding_version: 1,
                registration_version: 0,
                renewal_nonce: 0,
                transition_nonce: 0,
                valid_until: at.timestamp + 1000,
                policy_hash: h.chain.policy.policy_hash().unwrap(),
                recipient_x25519: manifest.recipient_x25519.into(),
            },
            candidate: None,
        },
        finalized_checkpoint: at,
        state: ServiceTxStateV1::Pending,
        reconciled_checkpoint: None,
    };
    h.journal.begin(record).unwrap();
    let journal_path = h.dir.join("tee-service-tx-v1/journal.json");
    let before = std::fs::read(&journal_path).unwrap();

    let err = engine::step(
        &h.chain,
        &Signer,
        engine::ResidentSession {
            live: &h.live,
            journal: &mut h.journal,
            node_data_dir: &h.dir,
            phase: RenewalUpgradePhaseV1::Idle,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        engine::SupervisorError::Renewal(RenewalError::Reconcile(
            ReconcileError::WrongSender { expected, actual }
        )) if expected == Signer::address() && actual == other_sender
    ));
    assert!(!retry::observation(&err));
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
}

fn fresh_process_context(seed: u8) -> Arc<InProcessTeeContext> {
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = proof::descriptor(&genesis);
    Arc::new(proof::context(seed, &descriptor))
}

#[tokio::test]
async fn seam_c1_enrollment_restart_over_in_pool_renew_admits_across_h1_h2_h3() {
    let mut h = Harness::new();
    let mut tracker = authority::Tracker::new(true);
    h.admit().await;
    assert!(tracker.observe(&h).is_authority_open());

    // H1: advance into renewal window so Renew/Pending@N is persisted and sent.
    let lease_until = h.chain.binding.as_ref().unwrap().valid_until;
    h.chain.at.height += 1;
    h.chain.at.hash = B256::repeat_byte(0x21);
    h.chain.at.timestamp = lease_until - h.chain.policy.maximum_lease / 2;
    assert!(matches!(
        h.step().await,
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::Submitted {
                replayed: false,
                ..
            }),
            ..
        }
    ));
    let renew = h.journal.load().unwrap().unwrap();
    assert_eq!(renew.kind, ServiceTxKindV1::Renew);
    assert_eq!(renew.state, ServiceTxStateV1::Pending);
    let journal_path = h.dir.join("tee-service-tx-v1/journal.json");
    let before = std::fs::read(&journal_path).unwrap();
    let sent_before = h.sent();

    // Restart with fresh InProcessTeeContext (!same_process) while Renew@N is still in pool.
    h.live = fresh_process_context(21);
    h.chain.nonces = [renew.nonce, renew.nonce, renew.nonce + 1];
    assert!(matches!(
        h.step().await,
        engine::Step::Enrollment(EnrollmentOutcome::WaitForFinality)
    ));
    assert_eq!(h.sent(), sent_before);
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);

    // H2: Renew@N finalizes -> DiscardLostIdentity -> submits Register@(N+1).
    h.chain.at.height += 1;
    h.chain.at.hash = B256::repeat_byte(0x22);
    h.chain.at.timestamp += 2;
    h.chain.nonces = [renew.nonce + 1; 3];
    h.chain.binding.as_mut().unwrap().renewal_nonce += 1;
    assert!(matches!(
        h.step().await,
        engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
    ));
    let reg = h.journal.load().unwrap().unwrap();
    assert_eq!(reg.kind, ServiceTxKindV1::Register);
    assert_eq!(reg.nonce, renew.nonce + 1);
    assert_eq!(h.sent(), sent_before + 1);

    // H3: Register@(N+1) finalizes -> admitted and authority opens.
    h.finalize();
    assert!(matches!(
        h.step().await,
        engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
    ));
    assert!(h.live.offer_key_ready().unwrap());
    let mut next_tracker = authority::Tracker::new(true);
    assert!(next_tracker.observe(&h).is_authority_open());
}

#[tokio::test]
async fn seam_c1_replace_lost_identity_and_stale_renew_race_admits_across_h1_h2_h3() {
    let mut h = Harness::new();
    h.admit().await;

    // H1: write Renew/Pending@N.
    let lease_until = h.chain.binding.as_ref().unwrap().valid_until;
    h.chain.at.height += 1;
    h.chain.at.hash = B256::repeat_byte(0x31);
    h.chain.at.timestamp = lease_until - h.chain.policy.maximum_lease / 2;
    assert!(matches!(
        h.step().await,
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::Submitted { .. }),
            ..
        }
    ));
    let renew = h.journal.load().unwrap().unwrap();
    assert_eq!(renew.kind, ServiceTxKindV1::Renew);

    // Restart with fresh InProcessTeeContext over unbroadcast/free slot -> ReplaceLostIdentity -> Register@N.
    h.live = fresh_process_context(22);
    h.chain.nonces = [renew.nonce; 3];
    assert!(matches!(
        h.step().await,
        engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
    ));
    let reg_at_n = h.journal.load().unwrap().unwrap();
    assert_eq!(reg_at_n.kind, ServiceTxKindV1::Register);
    assert_eq!(reg_at_n.nonce, renew.nonce);
    assert_ne!(reg_at_n.tx_hash, renew.tx_hash);

    // H2 race: stale peer Renew@N wins on-chain (renewal_nonce + 1, nonce advances to N+1).
    h.chain.at.height += 1;
    h.chain.at.hash = B256::repeat_byte(0x32);
    h.chain.at.timestamp += 2;
    h.chain.binding.as_mut().unwrap().renewal_nonce += 1;
    h.chain.nonces = [renew.nonce + 1; 3];

    // Next tick transitions Register@N -> RebuildRequired and submits Register@(N+1) without CompetingLiveBinding.
    assert!(matches!(
        h.step().await,
        engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
    ));
    let reg_at_next = h.journal.load().unwrap().unwrap();
    assert_eq!(reg_at_next.kind, ServiceTxKindV1::Register);
    assert_eq!(reg_at_next.nonce, renew.nonce + 1);
    assert_ne!(reg_at_next.tx_hash, reg_at_n.tx_hash);
    assert_eq!(
        reg_at_next.public_intent.target.renewal_nonce,
        h.chain.binding.as_ref().unwrap().renewal_nonce
    );

    // H3: Register@(N+1) finalizes -> admitted and authority opens.
    h.finalize();
    assert!(matches!(
        h.step().await,
        engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
    ));
    assert!(h.live.offer_key_ready().unwrap());
    let mut tracker = authority::Tracker::new(true);
    assert!(tracker.observe(&h).is_authority_open());
}

struct ColdSuccessorSeamPort<'a> {
    chain: &'a Chain,
    policy: TeePolicyV1,
    active_upgrade: outbe_primitives::enclave_upgrade::EnclaveUpgradeV1,
    successor: outbe_tee::registry_view::FinalizedStagedSuccessorPolicyV1,
    candidate: Option<LifecycleUpgradeCandidateV1>,
}

impl LifecycleChainPort for ColdSuccessorSeamPort<'_> {
    type Error = std::io::Error;
    async fn finalized_checkpoint(&self) -> chain::PortResult<LifecycleCheckpointV1> {
        self.chain.finalized_checkpoint().await
    }
    async fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        selector: &outbe_tee::registry_view::NodeBindingSelectorV1,
    ) -> chain::PortResult<LifecycleRegistryStateV1> {
        let mut view = self.chain.registry_state(at, selector).await?;
        view.policy = self.policy.clone();
        view.active_upgrade = Some(self.active_upgrade);
        view.successor_policy = Some(self.successor.clone());
        view.candidate = self.candidate;
        Ok(view)
    }
    async fn pending_upgrade_nonce(
        &self,
        _: LifecycleCheckpointV1,
        _: &outbe_tee::registry_view::NodeBindingSelectorV1,
    ) -> chain::PortResult<u64> {
        Ok(1)
    }
    async fn validator_record(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> chain::PortResult<Option<outbe_primitives::validator_dto::ValidatorRecord>> {
        self.chain.validator_record(at, account).await
    }
    async fn balance(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> chain::PortResult<U256> {
        self.chain.balance(at, account).await
    }
    async fn account_nonce(
        &self,
        account: Address,
        tag: LifecycleNonceTagV1,
    ) -> chain::PortResult<u64> {
        self.chain.account_nonce(account, tag).await
    }
    async fn send_raw_transaction(&self, raw: &[u8]) -> chain::PortResult<B256> {
        self.chain.send_raw_transaction(raw).await
    }
    async fn finalized_receipt(
        &self,
        at: LifecycleCheckpointV1,
        hash: B256,
    ) -> chain::PortResult<Option<LifecycleReceiptV1>> {
        self.chain.finalized_receipt(at, hash).await
    }
    async fn onboarding_proof(
        &self,
        at: LifecycleCheckpointV1,
        hash: B256,
    ) -> chain::PortResult<LifecycleOnboardingProofV1> {
        self.chain.onboarding_proof(at, hash).await
    }
    async fn upgrade_key(
        &self,
        _: LifecycleCheckpointV1,
        _: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
        _: &outbe_tee::upgrade_transfer::UpgradeKeyProofV1,
    ) -> chain::PortResult<LifecycleOnboardingProofV1> {
        Err(LifecyclePortError::Source(std::io::Error::other(
            "not used in cancel/prepare seam",
        )))
    }
}

#[tokio::test]
async fn seam_r3_1_cold_successor_cancels_lost_renew_replays_exact_then_prepares_across_h1_h2() {
    use outbe_tee::lifecycle::successor::{
        cancel_successor_v1, prepare_cold_successor_v1, reconcile_candidate_v1,
        reconcile_successor_v1, SuccessorLeaseV1, SuccessorStep,
    };

    let mut h = Harness::new();
    h.admit().await;

    // H1: write Renew/Pending@N.
    let lease_until = h.chain.binding.as_ref().unwrap().valid_until;
    h.chain.at.height += 1;
    h.chain.at.hash = B256::repeat_byte(0x41);
    h.chain.at.timestamp = lease_until - h.chain.policy.maximum_lease / 2;
    assert!(matches!(
        h.step().await,
        engine::Step::Resident {
            jailed: false,
            renewal: Some(RenewalStep::Submitted { .. }),
            ..
        }
    ));
    let lost_renew = h.journal.load().unwrap().unwrap();
    assert_eq!(lost_renew.kind, ServiceTxKindV1::Renew);

    // Stage strict successor policy + live on-chain candidate at H1.
    let predecessor = h.chain.policy.policy_hash().unwrap();
    let mut successor_policy = h.chain.policy.clone();
    successor_policy.policy_version += 1;
    successor_policy.predecessor_policy_hash = predecessor;
    successor_policy.activation_height = 1;
    let successor_hash = successor_policy.policy_hash().unwrap();
    let active_upgrade = outbe_primitives::enclave_upgrade::EnclaveUpgradeV1 {
        proposal_id: U256::from(1),
        activation_height: 1,
        mrenclave: B256::repeat_byte(88),
        successor_policy_hash: successor_hash,
        predecessor_policy_hash: predecessor,
    };
    let live_candidate = LifecycleUpgradeCandidateV1 {
        context_hash: B256::repeat_byte(0x77),
        valid_until: h.chain.at.timestamp + 3600,
        source_binding_id: h.chain.binding.as_ref().unwrap().binding_id,
        target_hash: B256::repeat_byte(0x78),
        nonce: 1,
    };

    // Restart with cold process (!same_process) over free slot [N; 3].
    let cold_live = fresh_process_context(23);
    h.chain.nonces = [lost_renew.nonce; 3];
    let port_h1 = ColdSuccessorSeamPort {
        chain: &h.chain,
        policy: successor_policy.clone(),
        active_upgrade,
        successor: outbe_tee::registry_view::FinalizedStagedSuccessorPolicyV1 {
            finalized_height: h.chain.at.height,
            finalized_hash: h.chain.at.hash,
            proposal_id: U256::from(1),
            policy: successor_policy.clone(),
        },
        candidate: Some(live_candidate),
    };

    // engine::step recognizes needs_successor == true -> Step::SuccessorRequired.
    assert!(matches!(
        engine::step(
            &port_h1,
            &Signer,
            engine::ResidentSession {
                live: &cold_live,
                journal: &mut h.journal,
                node_data_dir: &h.dir,
                phase: RenewalUpgradePhaseV1::Idle,
            },
        )
        .await
        .unwrap(),
        engine::Step::SuccessorRequired
    ));
    let view_h1 = port_h1
        .registry_state(
            h.chain.at,
            &outbe_tee::registry_view::NodeBindingSelectorV1::Validator(Signer::address()),
        )
        .await
        .unwrap();
    assert_eq!(
        reconcile_candidate_v1(&view_h1, None, None),
        SuccessorStep::CancelRequired {
            context_hash: live_candidate.context_hash,
        }
    );

    // Replace lost Renew@N with operator-owned UpgradeCancel@N.
    let cancel_step = cancel_successor_v1(
        &port_h1,
        &Signer,
        &cold_live,
        &mut h.journal,
        live_candidate.context_hash,
    )
    .await
    .unwrap();
    let cancel = h.journal.load().unwrap().unwrap();
    assert_eq!(cancel.kind, ServiceTxKindV1::UpgradeCancel);
    assert_eq!(cancel.nonce, lost_renew.nonce);
    assert_ne!(cancel.tx_hash, lost_renew.tx_hash);
    assert!(matches!(
        cancel_step,
        SuccessorStep::Submitted {
            transaction_hash,
            replayed: false
        } if transaction_hash == cancel.tx_hash
    ));

    // Next tick before finality replays exact UpgradeCancel@N bytes (ReplayExact).
    let replay_step = reconcile_successor_v1(&port_h1, &Signer, &cold_live, &mut h.journal)
        .await
        .unwrap();
    assert_eq!(
        replay_step,
        SuccessorStep::Submitted {
            transaction_hash: cancel.tx_hash,
            replayed: true,
        }
    );
    assert_eq!(h.journal.load().unwrap().unwrap(), cancel);

    // H2: candidate clears on-chain -> Finalized -> submits UpgradePrepare@(N+1).
    h.chain.at.height += 1;
    h.chain.at.hash = B256::repeat_byte(0x42);
    h.chain.at.timestamp += 2;
    h.chain.nonces = [cancel.nonce + 1; 3];
    let port_h2 = ColdSuccessorSeamPort {
        chain: &h.chain,
        policy: successor_policy.clone(),
        active_upgrade,
        successor: outbe_tee::registry_view::FinalizedStagedSuccessorPolicyV1 {
            finalized_height: h.chain.at.height,
            finalized_hash: h.chain.at.hash,
            proposal_id: U256::from(1),
            policy: successor_policy,
        },
        candidate: None,
    };
    assert_eq!(
        reconcile_successor_v1(&port_h2, &Signer, &cold_live, &mut h.journal)
            .await
            .unwrap(),
        SuccessorStep::Finalized
    );
    let fresh_candidate = fresh_process_context(24);
    let (_, prepared) = prepare_cold_successor_v1(
        &port_h2,
        &Signer,
        &fresh_candidate,
        &mut h.journal,
        SuccessorLeaseV1 {
            binding_id: B256::repeat_byte(0x89),
            valid_until: h.chain.at.timestamp + 7200,
        },
    )
    .await
    .unwrap();
    let prep = h.journal.load().unwrap().unwrap();
    assert_eq!(prep.kind, ServiceTxKindV1::UpgradePrepare);
    assert_eq!(prep.nonce, cancel.nonce + 1);
    assert!(matches!(
        prepared,
        SuccessorStep::Submitted {
            transaction_hash,
            replayed: false
        } if transaction_hash == prep.tx_hash
    ));
}

#[tokio::test]
async fn seam_authority_h1_to_h2_lag_keeps_signer_open_and_retirement_closes_both() {
    let mut h = Harness::new();
    h.admit().await;
    let h1 = h.chain.at.height;
    let binding = h.chain.binding.as_ref().unwrap();
    let (provider, chain, mut identity, hashes) =
        authority_provider::build_tee_admission_mock_provider(
            &[(h1, h.chain.at.timestamp)],
            authority_provider::SeededTeeState {
                valid_until: binding.valid_until,
                has_validator_binding: true,
                validator_binding_mismatch: false,
                validator_jailed: false,
                offer_key: h.chain.public,
            },
        );
    identity.validator = Some(Signer::address());
    identity.reth_p2p_public = h.live.manifest().node_id.reth_p2p_public;
    identity.expected_enclave_id = Some(h.live.manifest().enclave_id().unwrap());
    let node_hash = h.live.manifest().node_id.node_id_hash().unwrap();
    let mut storage = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(676);
    storage.set_block_number(h1);
    outbe_primitives::storage::StorageHandle::enter(&mut storage, |s| {
        let registry = outbe_teeregistry::TeeRegistry::new(s);
        registry
            .v1_node_enclave_id
            .write(&node_hash, identity.expected_enclave_id.unwrap())
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, binding.binding_id)
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, binding.intent_hash)
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node_hash, binding.valid_until)
            .unwrap();
        registry
            .validator_v1_node_hash
            .write(&Signer::address(), node_hash)
            .unwrap();
    });
    let mut accounts = provider.inner.accounts.lock();
    for ((address, slot), value) in &storage.storage {
        let account = accounts
            .get(address)
            .unwrap()
            .clone()
            .extend_storage([(B256::from(slot.to_be_bytes()), *value)]);
        accounts.insert(*address, account);
    }
    drop(accounts);

    let channel = outbe_tee::TeeAuthorityChannel::default();
    let probe = outbe_runtime::LifecycleAuthorityProbe::new(
        h.live.clone(),
        identity,
        *hashes.last().unwrap(),
        channel.clone(),
    )
    .unwrap();
    let protocol = Arc::new(outbe_runtime::OutbeRuntime::new());
    probe.bind_sync_refresh(provider.clone(), Arc::clone(&protocol), chain);
    let state_h1 = probe.refresh(&provider, &protocol, &chain).unwrap();
    assert!(state_h1.is_authority_open());
    assert_eq!(channel.applied_finalized_height(), h1);

    // FinalizationActor observes H2 while Reth provider still only has applied H1.
    let watch = channel.subscribe();
    let h2 = h1 + 1;
    channel.note_observed_finalized_height(h2);
    assert!(watch.ensure_current_for_signer().is_authority_open());
    assert!(channel.snapshot().is_authority_open());
    assert!(!watch.ensure_current().is_authority_open());

    // Retirement closes authority for both strict and signer checks.
    channel.retire_publisher(true);
    assert!(!channel.snapshot().is_authority_open());
    assert!(!watch.ensure_current_for_signer().is_authority_open());
    assert!(!watch.ensure_current().is_authority_open());
}

fn file_backed_context(
    dir: &std::path::Path,
    descriptor: &TrustedNetworkDescriptorV1,
) -> Arc<InProcessTeeContext> {
    use std::os::unix::fs::PermissionsExt as _;
    let host = dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let template = proof::context(11, descriptor);
    let authorization = template.authorization_witness().unwrap();
    let (store, loaded) =
        outbe_tee::node_host::ResidentStateStore::load_or_create(dir, &authorization).unwrap();
    let mut manifest = template.manifest().clone();
    manifest.recipient_x25519 = loaded.keys.tribute_offer_public();
    manifest.attestation_ed25519 = loaded.keys.attestation_pub();
    manifest.noise_responder_x25519 = loaded.keys.noise_public();
    manifest.initialization_challenge = loaded.initialization_challenge;
    Arc::new(
        InProcessTeeContext::new(
            loaded.keys,
            *template.identity(),
            manifest,
            descriptor.clone(),
            outbe_tee::in_process::ProofSourceV1::NoHardware,
        )
        .unwrap()
        .with_restored_state(store, loaded.material, loaded.anchor),
    )
}

fn binding_for_resident(
    live: &InProcessTeeContext,
    policy: &TeePolicyV1,
    valid_until: u64,
) -> outbe_tee::registry_view::RenewalBindingV1 {
    let manifest = live.manifest();
    let mut b = outbe_tee::test_utils::RenewalBindingFixtureV1::new(1).build();
    b.node_id_hash = manifest.node_id.node_id_hash().unwrap();
    b.enclave_id = manifest.enclave_id().unwrap();
    b.binding_id = B256::repeat_byte(0x41);
    b.intent_hash = B256::repeat_byte(0x42);
    b.policy_hash = policy.policy_hash().unwrap();
    b.binding_version = 1;
    b.registration_version = 0;
    b.renewal_nonce = 0;
    b.transition_nonce = 0;
    b.valid_until = valid_until;
    b.recipient_x25519 = manifest.recipient_x25519.into();
    b.attestation_ed25519 = manifest.attestation_ed25519.into();
    b.noise_responder_x25519 = manifest.noise_responder_x25519.into();
    b.node_host_authorization_hash = manifest.node_host_authorization_hash().unwrap();
    b
}

async fn run_prepare_once(
    h: Harness,
    validator: bool,
) -> (
    Result<
        Option<driver::PreparedAnchor>,
        driver::PrepareError<engine::SupervisorError<std::io::Error, std::convert::Infallible>>,
    >,
    Arc<Mutex<Harness>>,
    outbe_tee::TeeAuthorityChannel,
) {
    let state = Arc::new(Mutex::new(h));
    let cancellation = CancellationToken::new();
    let wake = Arc::new(Notify::new());
    let (sender, _events) = mpsc::unbounded_channel();
    let tracker = authority::Tracker::new(validator);
    let authority = tracker.channel.clone();
    let mut task = Task {
        state: state.clone(),
        authority: tracker,
        events: sender,
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        driver::prepare(&mut task, &cancellation, &wake),
    )
    .await
    .expect("prepare timed out instead of exiting for restored resident");
    (result, state, authority)
}

#[tokio::test]
async fn restored_founder_and_enrolled_resident_exit_presync_for_not_due_submitted_and_jailed() {
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = proof::descriptor(&genesis);

    // 1. File-restored founder (no Register journal, no anchor.bin), unjailed, NotDue.
    {
        let mut h = Harness::new();
        h.live = file_backed_context(&h.dir, &descriptor);
        let material = outbe_tee::resident::ResidentOfferMaterial::for_test(
            vec![5; 96],
            B256::from(h.live.manifest().chain_id),
            0,
            0,
        )
        .unwrap();
        h.live.activate_for_test(material).unwrap();
        let valid_until = h.chain.at.timestamp + h.chain.policy.maximum_lease;
        h.chain.public = h.live.network_encryption_public().unwrap().into();
        h.chain.binding = Some(binding_for_resident(&h.live, &h.chain.policy, valid_until));

        // Simulate restart restoring founder from resident-state.v1 (OBRESID2 tag 1 has anchor None).
        h.live = file_backed_context(&h.dir, &descriptor);
        assert!(h.live.offer_key_ready().unwrap());
        assert!(h.live.admission_anchor().unwrap().is_none());

        let expected_at = h.chain.at;
        let (result, state, authority) = run_prepare_once(h, true).await;
        let anchor = result.unwrap().expect("founder must exit with anchor");
        assert_eq!(anchor.height, expected_at.height);
        assert_eq!(anchor.hash, expected_at.hash);
        assert_eq!(state.lock().await.sent(), 0);
        assert!(authority.snapshot().is_authority_open());
    }

    // 2. File-restored enrolled resident: NotDue, Submitted, and Jailed.
    for (label, in_renewal_window, jailed) in [
        ("not_due", false, false),
        ("submitted", true, false),
        ("jailed", false, true),
    ] {
        let mut h = Harness::new();
        h.live = file_backed_context(&h.dir, &descriptor);
        h.admit().await;
        let sent_before = h.sent();

        // Simulate restart restoring from resident-state.v1 (OBRESID2 tag 2 restores anchor).
        h.live = file_backed_context(&h.dir, &descriptor);
        assert!(h.live.offer_key_ready().unwrap());
        assert!(h.live.admission_anchor().unwrap().is_some());

        h.chain.jailed = jailed;
        if in_renewal_window {
            let valid_until = h.chain.binding.as_ref().unwrap().valid_until;
            h.chain.at.timestamp = valid_until - h.chain.policy.maximum_lease / 2;
        }
        let expected_at = h.chain.at;
        let (result, state, authority) = run_prepare_once(h, true).await;
        let anchor = result
            .unwrap()
            .unwrap_or_else(|| panic!("{label}: expected PreparedAnchor"));
        assert_eq!(anchor.height, expected_at.height, "{label}");
        assert_eq!(anchor.hash, expected_at.hash, "{label}");
        let state = state.lock().await;
        if in_renewal_window {
            assert_eq!(state.sent(), sent_before + 1, "{label}");
            assert_eq!(
                state.journal.load().unwrap().unwrap().kind,
                ServiceTxKindV1::Renew,
                "{label}"
            );
        } else {
            assert_eq!(state.sent(), sent_before, "{label}");
        }
        if jailed {
            assert_eq!(
                authority.snapshot(),
                outbe_tee::TeeAuthorityState::SuspendedJailed,
                "{label}"
            );
        } else {
            assert!(authority.snapshot().is_authority_open(), "{label}");
        }
    }
}

#[tokio::test]
async fn restored_resident_mismatch_fails_closed_non_retryably() {
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = proof::descriptor(&genesis);

    enum Mismatch {
        OfferKey,
        KeyEpoch,
        OfferEpoch,
        NodeIdHash,
        NodeHostAuth,
        PolicyHash,
        RecipientKey,
    }

    for jailed in [false, true] {
        for case in [
            Mismatch::OfferKey,
            Mismatch::KeyEpoch,
            Mismatch::OfferEpoch,
            Mismatch::NodeIdHash,
            Mismatch::NodeHostAuth,
            Mismatch::PolicyHash,
            Mismatch::RecipientKey,
        ] {
            let mut h = Harness::new();
            h.live = file_backed_context(&h.dir, &descriptor);
            let (key_epoch, offer_epoch) = match case {
                Mismatch::KeyEpoch => (1, 0),
                Mismatch::OfferEpoch => (0, 1),
                _ => (0, 0),
            };
            let material = outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(h.live.manifest().chain_id),
                key_epoch,
                offer_epoch,
            )
            .unwrap();
            h.live.activate_for_test(material).unwrap();
            let valid_until = h.chain.at.timestamp + h.chain.policy.maximum_lease;
            h.chain.public = match case {
                Mismatch::OfferKey => B256::repeat_byte(0xAA),
                _ => h.live.network_encryption_public().unwrap().into(),
            };
            let mut binding = binding_for_resident(&h.live, &h.chain.policy, valid_until);
            match case {
                Mismatch::NodeIdHash => binding.node_id_hash = B256::repeat_byte(0xBB),
                Mismatch::NodeHostAuth => {
                    binding.node_host_authorization_hash = B256::repeat_byte(0xCC);
                }
                Mismatch::PolicyHash => binding.policy_hash = B256::repeat_byte(0xDD),
                Mismatch::RecipientKey => binding.recipient_x25519 = B256::repeat_byte(0xEE),
                Mismatch::OfferKey | Mismatch::KeyEpoch | Mismatch::OfferEpoch => {}
            }
            h.chain.binding = Some(binding);
            h.chain.jailed = jailed;
            h.live = file_backed_context(&h.dir, &descriptor);

            let err = engine::step(
                &h.chain,
                &Signer,
                engine::ResidentSession {
                    live: &h.live,
                    journal: &mut h.journal,
                    node_data_dir: &h.dir,
                    phase: RenewalUpgradePhaseV1::Idle,
                },
            )
            .await
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    engine::SupervisorError::Renewal(RenewalError::BindingMismatch)
                ),
                "jailed={jailed}: unexpected error {err:?}"
            );
            assert!(!retry::observation(&err));

            let (prep, state, _) = run_prepare_once(h, true).await;
            assert!(matches!(prep, Err(driver::PrepareError::Observation(_))));
            assert_eq!(state.lock().await.sent(), 0);
        }
    }
}

#[tokio::test]
async fn unadmitted_jailed_never_exits_presync_as_prepared_anchor() {
    let mut h = Harness::new();
    h.chain.jailed = true;
    let step = h.step().await;
    assert!(matches!(
        step,
        engine::Step::Enrollment(EnrollmentOutcome::WaitForUnjail)
    ));

    let state = Arc::new(Mutex::new(h));
    let cancellation = CancellationToken::new();
    let wake = Arc::new(Notify::new());
    let (sender, mut events) = mpsc::unbounded_channel();
    let tracker = authority::Tracker::new(true);
    let mut task = Task {
        state: state.clone(),
        authority: tracker,
        events: sender,
    };
    let cancel_task = cancellation.clone();
    let handle = tokio::spawn(async move { driver::prepare(&mut task, &cancel_task, &wake).await });
    assert!(matches!(
        events.recv().await,
        Some(Event::Observation(engine::Step::Enrollment(
            EnrollmentOutcome::WaitForUnjail
        )))
    ));
    cancellation.cancel();
    assert!(matches!(
        handle.await.unwrap(),
        Err(driver::PrepareError::Cancelled)
    ));
    assert_eq!(state.lock().await.sent(), 0);
}

#[tokio::test]
async fn restored_resident_with_missing_anchor_for_current_register_reingests() {
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = proof::descriptor(&genesis);
    let mut h = Harness::new();
    h.live = file_backed_context(&h.dir, &descriptor);
    h.admit().await;

    // Simulate crash between activate (resident-state.v1 written) and anchor.bin write.
    let anchor_path = h.dir.join("tee-live-join-admission-v1/anchor.bin");
    assert!(anchor_path.exists());
    std::fs::remove_file(&anchor_path).unwrap();

    // Restart restoring from resident-state.v1 (OBRESID2 tag 2 restores anchor).
    h.live = file_backed_context(&h.dir, &descriptor);
    assert!(h.live.offer_key_ready().unwrap());
    assert!(h.live.admission_anchor().unwrap().is_some());

    let step = h.step().await;
    assert!(
        matches!(
            step,
            engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
        ),
        "missing anchor.bin for current Register must re-ingest, got {step:?}"
    );
    let restored_anchor = load_live_join_admission_anchor_v1(&h.dir, &h.live)
        .unwrap()
        .expect("re-ingest must persist anchor.bin");
    assert_eq!(
        restored_anchor.intent_hash,
        h.chain.binding.as_ref().unwrap().intent_hash
    );

    // Subsequent restart with anchor.bin present exits pre-sync as Step::Resident.
    h.live = file_backed_context(&h.dir, &descriptor);
    let expected_at = h.chain.at;
    let (prep, _, _) = run_prepare_once(h, true).await;
    let anchor = prep
        .unwrap()
        .expect("expected PreparedAnchor after re-ingest");
    assert_eq!(anchor.height, expected_at.height);
    assert_eq!(anchor.hash, expected_at.hash);
}
