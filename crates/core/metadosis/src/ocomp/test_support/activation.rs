use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackSnapshot {
    storage: HashMap<(Address, U256), U256>,
    events: Vec<Log>,
    ce_work: CeWorkCheckpoint,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSnapshot {
    pub job_status: OcompJobStatus,
    pub worldwide_day_status: u8,
    pub active_job_id: B256,
    pub active_nod_root: B256,
    pub nod: outbe_nod::schema::NodCertifiedGenerationProjection,
    pub contributor: outbe_intex::schema::CertifiedContributorGenerationProjection,
    pub tribute_generation: u64,
    pub tribute_count: u32,
    pub tribute_nominal: U256,
    pub tribute_total_supply: u64,
    pub carry_over: U256,
    pub owner_events: Vec<Log>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationMetadata {
    pub activated_at_height: u64,
    pub activated_at_time: u64,
}

pub struct ActivationFixture {
    pub provider: HashMapStorageProvider,
    pub scope: ExecutionScope,
    pub result: LysisResultV1,
    pub finality: FixedFinality,
    pub intent_id: B256,
    pub limits: SchemaLimits,
    pub request_receipt: RequestLimitSplitReceiptV1,
}

impl ActivationFixture {
    #[must_use]
    pub fn new(current_height: u64, current_time: u64, seed_targets: bool) -> Self {
        Self::new_with_initial_votes(
            current_height,
            current_time,
            seed_targets,
            4,
            2,
            TEST_LOGICAL_TIME,
        )
    }

    /// Request clock distinct from the activation block. Callers that need a
    /// freeze instant set `scheduled_process_time` after this returns.
    #[cfg(test)]
    #[must_use]
    pub fn new_with_request_clock(
        current_height: u64,
        current_time: u64,
        logical_time: u64,
    ) -> Self {
        Self::new_with_initial_votes(current_height, current_time, true, 4, 2, logical_time)
    }

    /// Builds the default production-valid fixture before any validator vote
    /// is recorded, without injecting quorum state.
    #[cfg(test)]
    #[must_use]
    pub fn new_voting(current_height: u64, current_time: u64, seed_targets: bool) -> Self {
        Self::new_voting_with_member_count(current_height, current_time, seed_targets, 4)
    }

    /// Builds a production-valid voting fixture for the requested ValidatorSet
    /// size without introducing an OCOMP-specific membership authority.
    #[cfg(test)]
    #[must_use]
    pub fn new_voting_with_member_count(
        current_height: u64,
        current_time: u64,
        seed_targets: bool,
        member_count: u8,
    ) -> Self {
        Self::new_with_initial_votes(
            current_height,
            current_time,
            seed_targets,
            member_count,
            0,
            TEST_LOGICAL_TIME,
        )
    }

    /// Seeds the Staking-owned bonded balances used by recovery-policy tests.
    #[cfg(test)]
    pub fn seed_ocomp_recovery_stake_for_test(&mut self) {
        StorageHandle::enter(&mut self.provider, |storage| {
            let validators = ValidatorSet::new(storage.clone())
                .registered_validator_addresses()
                .unwrap();
            let bonded = U256::from(1_000u64);
            let staking = outbe_staking::contract::Staking::new(storage.clone());
            staking.config_min_stake.write(bonded).unwrap();
            staking
                .total_staked
                .write(bonded * U256::from(validators.len()))
                .unwrap();
            storage
                .set_balance(STAKING_ADDRESS, bonded * U256::from(validators.len()))
                .unwrap();
            let mut validator_set = ValidatorSet::new(storage.clone());
            for validator in validators {
                staking.stake_amount.write(&validator, bonded).unwrap();
                validator_set
                    .record_stake_increase(validator, bonded, bonded)
                    .unwrap();
            }
        });
    }

    /// Adds the next validator through registration, OCOMP readiness and the
    /// certified-boundary test hook, returning the newly persisted snapshot.
    #[cfg(test)]
    pub fn activate_additional_validator_for_test(
        &mut self,
        index: u8,
    ) -> OcompSnapshotExtensionV1 {
        StorageHandle::enter(&mut self.provider, |storage| {
            let chain_id = storage.chain_id().unwrap();
            let genesis_hash = storage.genesis_hash().unwrap();
            let owner = Address::repeat_byte(0xE0);
            let validator = Address::repeat_byte(0xB0 + index);
            let consensus_pubkey = [0x30 + index; 48];
            let mut validators = ValidatorSet::new(storage.clone());
            assert_eq!(validators.validator_count().unwrap(), u32::from(index));
            validators
                .set_config_max_validators(u32::from(index) + 1)
                .unwrap();
            validators
                .register_validator(owner, validator, &consensus_pubkey)
                .unwrap();
            validators.mark_pending(validator).unwrap();

            let registration = super::committee::registration_for_validator(
                index,
                (validator, consensus_pubkey),
                &super::committee::RegistrationAuthority {
                    chain_id,
                    genesis_hash,
                    limits: &self.limits,
                },
            )
            .unwrap();
            validators
                .confirm_validator_ready(
                    validator,
                    &registration.encode_canonical(&self.limits).unwrap(),
                )
                .unwrap();
            let timestamp: u64 = storage.timestamp().unwrap().try_into().unwrap();
            outbe_validatorset::hooks::transition_epoch(
                storage.clone(),
                timestamp,
                storage.block_number().unwrap(),
            )
            .unwrap();
            let snapshot_key = validators
                .activate_validator_via_boundary_for_test(validator)
                .unwrap();
            read_ocomp_snapshot_extension(storage, snapshot_key)
                .unwrap()
                .unwrap()
        })
    }

    fn new_with_initial_votes(
        current_height: u64,
        current_time: u64,
        seed_targets: bool,
        member_count: u8,
        initial_vote_count: u8,
        logical_time: u64,
    ) -> Self {
        let config = ActivationFixtureConfig {
            current_height,
            current_time,
            seed_targets,
            member_count,
            initial_vote_count,
            logical_time,
        };
        let mut provider = HashMapStorageProvider::new_with_chain_identity(1, hash(17));
        let seed = prepare_activation_seed(&mut provider, &config);
        provider.set_block_number(config.current_height);
        provider.set_timestamp(U256::from(config.current_time));
        let scope = begin_activation_scope(&mut provider);
        StorageHandle::enter(&mut provider, |storage| {
            seed_owner_targets(storage.clone(), config.seed_targets);
            let finalized = seed_requested_job(storage.clone(), &seed, config.seed_targets);
            seed_initial_votes(storage, &scope, &seed, &config, &finalized);
        });
        provider.clear_events(METADOSIS_ADDRESS);
        Self {
            provider,
            scope,
            result: seed.result,
            finality: seed.finality,
            intent_id: seed.intent_id,
            limits: seed.limits,
            request_receipt: seed.request_receipt,
        }
    }

    /// Creates the canonical node-attested vote for one fixture committee
    /// member. The returned bytes still have to pass the production public
    /// dispatch and consensus transition.
    #[must_use]
    pub fn signed_result_vote(&self, validator_index: u8) -> ResultVoteV1 {
        let intent = &self.finality.verified.intent;
        let mut vote = ResultVoteV1 {
            protocol_bundle_hash: intent.protocol_bundle_hash,
            job_id: self.result.job_id,
            attempt: intent.attempt,
            result_validator_set_epoch: intent.result_validator_set_epoch,
            result_committee_set_hash: intent.result_committee_set_hash,
            result_ocomp_binding_hash: intent.result_ocomp_binding_hash,
            ocomp_key_hash: ocomp_key_hash(validator_index),
            key_epoch: 1,
            result: self.result.clone(),
            signature_rs: [0; 64],
        };
        vote.signature_rs = sign(
            &signing_key(validator_index),
            vote.signing_digest(intent, &self.limits).unwrap(),
        );
        vote
    }

    pub fn apply(&mut self) -> PrecompileResult<Bytes> {
        self.provider.enable_lysis_activation_frame();
        self.dispatch_current()
    }

    #[cfg(test)]
    pub(crate) fn apply_with_receipt_fault(
        &mut self,
        fault: ActivationReceiptFault,
    ) -> PrecompileResult<Bytes> {
        ACTIVATION_RECEIPT_FAULT.with(|slot| {
            assert!(
                slot.replace(Some(fault)).is_none(),
                "nested activation receipt fault"
            );
        });
        let result = self.apply();
        ACTIVATION_RECEIPT_FAULT.with(|slot| slot.set(None));
        result
    }

    #[must_use]
    pub fn calldata(&self) -> Bytes {
        Bytes::from(
            outbe_ocomp_protocol::abi::encode_submit_lysis_result_calldata(
                &self.signed_result_vote(2),
                &self.limits,
            )
            .unwrap(),
        )
    }

    pub fn dispatch_current(&mut self) -> PrecompileResult<Bytes> {
        self.dispatch_current_with()
    }

    fn dispatch_current_with(&mut self) -> PrecompileResult<Bytes> {
        let calldata = self.calldata();
        self.provider
            .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::VerifiedResultVote);
        StorageHandle::enter(&mut self.provider, |storage| {
            crate::commands::submit_verified_result_vote(
                storage,
                &self.scope,
                calldata.as_ref(),
                U256::ZERO,
                false,
            )
        })
    }

    pub fn terminal_outcome(&mut self) -> ActivationOutcome {
        StorageHandle::enter(&mut self.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap()
                .terminal
                .unwrap()
                .completed_binding
                .unwrap()
                .terminal_receipt
                .outcome
        })
    }

    pub fn replace_request_receipt(&mut self, receipt: &RequestLimitSplitReceiptV1) {
        let encoded = receipt.encode_canonical(&self.limits).unwrap();
        StorageHandle::enter(&mut self.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_request_limit_receipts
                .get_bytes(&TEST_WWD)
                .write(&encoded)
                .unwrap();
        });
    }

    pub(crate) fn corrupt_request_receipt_mismatch(&mut self) {
        let mut mismatched = self.request_receipt.clone();
        mismatched.logical_anchor = mismatched
            .logical_anchor
            .checked_add(1)
            .expect("fixture logical anchor must admit one mismatch step");
        self.replace_request_receipt(&mismatched);
    }

    pub fn rollback_snapshot(&mut self) -> RollbackSnapshot {
        RollbackSnapshot {
            storage: self.provider.storage.clone(),
            events: self.provider.get_ordered_events().to_vec(),
            ce_work: self.scope.ce_work_checkpoint().unwrap(),
        }
    }

    #[cfg(test)]
    pub fn semantic_snapshot(&mut self) -> super::SemanticSnapshot {
        let owner_events = self
            .provider
            .get_ordered_events()
            .iter()
            .filter(|event| event.address != METADOSIS_ADDRESS)
            .cloned()
            .collect();
        StorageHandle::enter(&mut self.provider, |storage| {
            let contract = MetadosisContract::new(storage.clone());
            let job = contract
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap();
            let active = contract
                .active_lysis_generation(TEST_WWD, &self.limits)
                .unwrap()
                .unwrap();
            let nod = outbe_nod::schema::NodContract::new(storage.clone())
                .ocomp_certified_generation(TEST_WWD)
                .unwrap()
                .unwrap();
            let contributor =
                outbe_intex::api::certified_contributor_generation(&storage, TEST_WWD)
                    .unwrap()
                    .unwrap();
            let tribute = TributeContract::new(storage.clone());
            let admission = tribute.pre_admission_projection(TEST_WWD).unwrap();
            let totals = tribute.get_day_totals(TEST_WWD).unwrap();
            SemanticSnapshot {
                job_status: job.status,
                worldwide_day_status: contract.get_wwd_status(TEST_WWD).unwrap().as_u8(),
                active_job_id: active.job_id,
                active_nod_root: active.nod_root,
                nod,
                contributor,
                tribute_generation: admission.source_generation,
                tribute_count: totals.tribute_count,
                tribute_nominal: totals.tribute_nominal_total_minor,
                tribute_total_supply: tribute.total_supply.read().unwrap(),
                carry_over: outbe_promislimit::PromisLimitContract::new(storage)
                    .get_total_unallocated()
                    .unwrap(),
                owner_events,
            }
        })
    }

    #[cfg(test)]
    pub fn activation_metadata(&mut self) -> ActivationMetadata {
        StorageHandle::enter(&mut self.provider, |storage| {
            let job = MetadosisContract::new(storage)
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap();
            let receipt = &job
                .terminal
                .unwrap()
                .completed_binding
                .unwrap()
                .terminal_receipt;
            ActivationMetadata {
                activated_at_height: receipt.activated_at_height,
                activated_at_time: receipt.activated_at_time,
            }
        })
    }
}

struct ActivationFixtureConfig {
    current_height: u64,
    current_time: u64,
    seed_targets: bool,
    member_count: u8,
    initial_vote_count: u8,
    logical_time: u64,
}
struct ActivationSeed {
    limits: SchemaLimits,
    bundle: ProtocolBundleV1,
    bundle_hash: B256,
    request_receipt: RequestLimitSplitReceiptV1,
    intent: JobIntentV1,
    intent_id: B256,
    request_state_root: B256,
    result: LysisResultV1,
    finality: FixedFinality,
}
fn prepare_activation_seed(
    provider: &mut HashMapStorageProvider,
    config: &ActivationFixtureConfig,
) -> ActivationSeed {
    assert!(config.member_count > 0);
    assert!(config.initial_vote_count < config.member_count);
    let limits = poc_schema_limits();
    let bundle = bundle();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let snapshot = StorageHandle::enter(provider, |storage| {
        seed_validator_snapshot(storage, &limits, config.member_count)
    });
    let request_receipt = request_receipt(bundle_hash, config.logical_time);
    let request_receipt_hash = request_receipt.receipt_hash(&limits).unwrap();
    let intent = intent(
        bundle_hash,
        &snapshot,
        request_receipt_hash,
        config.logical_time,
    );
    let intent_id = intent.intent_id(&limits).unwrap();
    let proof = finality_proof(&intent, &limits);
    let request_state_root = hash(98);
    let job_id = intent
        .job_id(
            proof.parent_accounting.finalized_block_hash,
            request_state_root,
            &limits,
        )
        .unwrap();
    let result = result(bundle_hash, job_id, &limits, config.logical_time);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let finality = FixedFinality {
        expected,
        verified: VerifiedFinalizedIntentV1 {
            intent: intent.clone(),
            intent_id,
            intent_storage_key: intent_storage_key(intent_id).unwrap(),
            job_id,
            request: FinalizedRequestBindingV1 {
                block_number: 9,
                block_hash: hash(46),
                state_root: request_state_root,
            },
        },
        calls: Arc::new(AtomicUsize::new(0)),
    };

    ActivationSeed {
        limits,
        bundle,
        bundle_hash,
        request_receipt,
        intent,
        intent_id,
        request_state_root,
        result,
        finality,
    }
}
fn seed_owner_targets(storage: StorageHandle<'_>, seed_targets: bool) {
    let nod = NodContract::new(storage.clone());
    nod.ocomp_materialization_head_sequence.write(1).unwrap();
    nod.ocomp_materialization_tail_sequence.write(1).unwrap();

    if seed_targets {
        let tribute = TributeContract::new(storage.clone());
        tribute.ocomp_profile_ready.write(true).unwrap();
        tribute.total_supply.write(2).unwrap();
        let mut totals = DayTotals::with_key(TEST_WWD);
        totals.initialized = true;
        totals.is_sealed = true;
        totals.tribute_count = 2;
        totals.tribute_nominal_total_minor = U256::from(1_000);
        tribute.day_totals.create(&totals).unwrap();
        let mut admission = DayPreAdmission::with_key(TEST_WWD);
        admission.initialized = true;
        admission.is_sealed = true;
        admission.sealed_collection_root = hash(31);
        admission.sealed_tribute_count = 2;
        admission.sealed_tribute_nominal_total_minor = U256::from(1_000);
        admission.source_generation = 0;
        tribute.day_pre_admission.create(&admission).unwrap();
    }
}
fn seed_requested_job(
    storage: StorageHandle<'_>,
    seed: &ActivationSeed,
    seed_targets: bool,
) -> outbe_ocomp_protocol::state::OcompFinalizedJobV1 {
    let limits = &seed.limits;
    let bundle = &seed.bundle;
    let bundle_hash = seed.bundle_hash;
    let request_receipt = &seed.request_receipt;
    let intent = &seed.intent;
    let intent_id = seed.intent_id;
    let request_state_root = seed.request_state_root;
    let mut contract = MetadosisContract::new(storage.clone());
    if seed_targets {
        contract.initialize_ocomp_pre_admission(TEST_WWD).unwrap();
    }
    contract
        .worldwide_days
        .create(&WorldwideDayRecord {
            wwd: TEST_WWD,
            status: status::READY,
            day_type: day_type::GREEN,
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            // Nod issued_at is this freeze clock. It equals the request logical
            // time in this fixture, so activation-height checks are not also a
            // freeze-versus-request check. That split lives in its own test.
            scheduled_process_time: TEST_LOGICAL_TIME,
            metadosis_limit_minor: U256::from(100),
            previous_vwap: U256::from(8),
            current_vwap: U256::from(10),
        })
        .unwrap();
    contract.active_wwd.insert(TEST_WWD).unwrap();
    contract
        .enqueue_ocomp_ready(TEST_WWD, TEST_REQUEST_HEIGHT)
        .unwrap();
    let profile = OcompRequestProfile {
        chain_id: 1,
        genesis_hash: hash(17),
        fork_id: hash(21),
        protocol_bundle_hash: bundle_hash,
        correctness_profile_id: hash(12),
        capacity_profile: capacity_profile(),
        source_availability_policy_id: hash(44),
    };
    seed_registry_authority(
        &storage,
        &outbe_ocompregistry::OcompProtocolAuthorityV1 {
            request_profile: profile,
            protocol_bundle: bundle.clone(),
        },
        limits,
    )
    .unwrap();
    let outer_transition = crate::commit::plan_outer_transition_for_test_fixture(
        &contract,
        TEST_WWD,
        crate::reducer::OuterWwdEvent::OcompRequestCommitted,
    )
    .unwrap();
    outbe_ocompregistry::OcompRegistry::new(storage.clone())
        .pin_lineage(intent_id, limits)
        .unwrap();
    contract
        .commit_ocomp_request(&outer_transition, intent, request_receipt, limits)
        .unwrap();
    let finalized = contract
        .record_ocomp_finality(
            intent_id,
            hash(46),
            request_state_root,
            TEST_REQUEST_HEIGHT,
            capacity_profile().result_deadline_blocks,
            limits,
        )
        .unwrap();
    assert_eq!(
        finalized.open_height,
        TEST_REQUEST_HEIGHT + RESULT_VOTE_MIN_FINALITY_DEPTH
    );
    contract
        .open_due_ocomp_voting(finalized.open_height, limits)
        .unwrap();
    finalized
}
fn seed_initial_votes(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    seed: &ActivationSeed,
    config: &ActivationFixtureConfig,
    finalized: &outbe_ocomp_protocol::state::OcompFinalizedJobV1,
) {
    let mut contract = MetadosisContract::new(storage);
    for index in 0..config.initial_vote_count {
        let vote = signed_result_vote_for_intent(&seed.intent, &seed.result, index, &seed.limits);
        contract
            .record_ocomp_result_vote(
                &vote,
                finalized.open_height + u64::from(index),
                scope,
                &seed.limits,
            )
            .unwrap();
    }
}
