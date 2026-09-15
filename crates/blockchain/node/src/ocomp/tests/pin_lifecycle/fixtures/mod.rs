use super::*;

mod proof_sources;
pub(super) use proof_sources::{DeterministicProofSource, FinalizedFrameDriver};

mod durability;
pub(super) use durability::{FailOnceDurability, FailSync};

pub(super) fn block(number: u64, state_root: B256, marker: u8) -> ConsensusBlock {
    block_extending(number, state_root, B256::ZERO, marker)
}

pub(super) fn block_extending(
    number: u64,
    state_root: B256,
    parent_hash: B256,
    marker: u8,
) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.state_root = state_root;
    block.header.parent_hash = parent_hash;
    block.header.extra_data = Bytes::from(vec![marker]);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block.map_header(OutbeHeader::new)))
}

pub(super) fn candidate(block: &ConsensusBlock) -> CandidatePinV1 {
    candidate_for_intent(block, &production_intent(block.number()))
}

pub(super) fn candidate_for_intent(block: &ConsensusBlock, intent: &JobIntentV1) -> CandidatePinV1 {
    CandidatePinV1 {
        block_number: block.number(),
        block_hash: block.block_hash(),
        state_root: block.header().inner.state_root,
        intent_id: intent.intent_id(&poc_schema_limits()).unwrap(),
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().unwrap(),
    }
}

pub(super) fn fixture_job_id(candidate: CandidatePinV1) -> B256 {
    job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .unwrap()
}

pub(super) fn intent_for_candidate(candidate: CandidatePinV1) -> JobIntentV1 {
    let mut intent = production_intent(candidate.block_number);
    intent.wwd = candidate.wwd;
    intent.activation_preconditions.tribute.wwd = candidate.wwd;
    intent.activation_preconditions.nod.wwd = candidate.wwd;
    intent.activation_preconditions.contributors.worldwide_day = candidate.wwd;
    intent.activation_preconditions.metadosis.wwd = candidate.wwd;
    intent.ce_sealed_root = candidate.ce_sealed_root;
    intent.protocol_bundle_hash = candidate.protocol_bundle_hash;
    assert_eq!(
        intent.intent_id(&poc_schema_limits()).unwrap(),
        candidate.intent_id
    );
    assert_eq!(intent.input_lease_id().unwrap(), candidate.input_lease_id);
    intent
}

pub(super) fn frame_for_block(block: &ConsensusBlock, receipts: Vec<Receipt>) -> FinalizedFrame {
    FinalizedFrame::for_test(
        BlockNumHash::new(block.number(), block.block_hash()),
        block.parent_hash(),
        block.header().inner.state_root,
        Block::default(),
        receipts,
    )
}

pub(super) type CandidateProvider = MockEthProvider<OutbePrimitives, ChainSpec<OutbeHeader>>;

pub(super) struct ProductionCandidateFixture {
    pub(super) request: ConsensusBlock,
    pub(super) candidate: CandidatePinV1,
    pub(super) source: Arc<RethFinalizedInputProofSource<CandidateProvider>>,
    pub(super) receipts: Vec<Receipt>,
}

pub(super) fn production_intent(block_number: u64) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: 42,
        genesis_hash: B256::repeat_byte(1),
        fork_id: B256::repeat_byte(2),
        wwd: 7,
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: B256::repeat_byte(3),
        ce_sealed_root: B256::repeat_byte(4),
        sealed_tribute_collection_key: B256::repeat_byte(5),
        sealed_tribute_collection_root: B256::repeat_byte(6),
        authenticated_day_count: 0,
        authenticated_day_nominal: U256::ZERO,
        pre_admission_envelope_hash: B256::repeat_byte(7),
        source_availability_policy_id: B256::repeat_byte(8),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(1_000),
            previous_vwap: U256::from(90),
            current_vwap: U256::from(100),
            gratis_demand: U256::from(25),
            gratis_supply: U256::from(20),
            lysis_limit_minor: U256::from(300),
            desis_limit_minor: U256::from(700),
            auction_entry_prices: vec![ReferenceEntryPriceV1 {
                reference_currency: outbe_oracle::constants::DAY_TYPE_ISO,
                entry_price_minor: U256::from(95),
                source: AuctionEntryPriceSource::LastClosedDayVwap,
                source_day: 6,
            }],
            request_budget_split_receipt_hash: B256::repeat_byte(9),
        },
        logical_evaluation_height: block_number,
        logical_evaluation_time: 1_000,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: 7,
                source_generation: 1,
                collection_key: B256::repeat_byte(5),
                sealed_collection_root: B256::repeat_byte(6),
                exact_count: 0,
                exact_nominal_total: U256::ZERO,
            },
            nod: NodTargetPreconditionV1 {
                wwd: 7,
                target_generation: 1,
                namespace_root_before: B256::repeat_byte(10),
                max_nod_count: 0,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: 7,
                expected_series_version: 1,
                max_contributor_count: 0,
                max_eligible_nominal_total: U256::ZERO,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: 7,
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: B256::repeat_byte(11),
        result_ocomp_binding_hash: B256::repeat_byte(12),
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    }
}

fn encoded_storage_slots(logical_key: B256, encoded: &[u8]) -> Vec<(B256, U256)> {
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    if encoded.len() <= 31 {
        let mut inline = [0_u8; 32];
        inline[..encoded.len()].copy_from_slice(encoded);
        inline[31] = (encoded.len() * 2) as u8;
        return vec![(
            B256::new(base.to_be_bytes::<32>()),
            U256::from_be_bytes(inline),
        )];
    }

    let mut slots = Vec::with_capacity(1 + encoded.len().div_ceil(32));
    slots.push((
        B256::new(base.to_be_bytes::<32>()),
        U256::from(encoded.len() * 2 + 1),
    ));
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    for (index, chunk) in encoded.chunks(32).enumerate() {
        let mut word = [0_u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        slots.push((
            B256::new((data_base + U256::from(index)).to_be_bytes::<32>()),
            U256::from_be_bytes(word),
        ));
    }
    slots
}

pub(super) fn production_candidate_source() -> ProductionCandidateFixture {
    let request = block(100, B256::repeat_byte(0x7a), 0x7b);
    let intent = production_intent(request.number());
    let limits = poc_schema_limits();
    let intent_id = intent.intent_id(&limits).expect("fixture IntentId");
    let record = OcompJobRecordV1 {
        intent: intent.clone(),
        intent_height: request.number(),
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    };
    let encoded = record
        .encode_canonical(&limits)
        .expect("fixture record encoding");
    let logical_key = intent_storage_key(intent_id).expect("fixture intent storage key");

    let chain_spec: ChainSpec<OutbeHeader> = ChainSpecBuilder::mainnet()
        .build()
        .map_header(OutbeHeader::new);
    let provider = MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec);
    provider.add_header(request.block_hash(), request.header().clone());
    provider.add_account(
        METADOSIS_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO)
            .with_bytecode(Bytes::from_static(&[0xef]))
            .extend_storage(encoded_storage_slots(logical_key, &encoded)),
    );
    let activation_preconditions_hash = intent
        .activation_preconditions
        .activation_preconditions_hash(&limits)
        .expect("fixture activation hash");
    let event = IMetadosis::OffchainJobRequested {
        intentId: intent_id,
        wwd: intent.wwd,
        pendingNonce: intent.pending_nonce,
        attempt: intent.attempt,
        activationPreconditionsHash: activation_preconditions_hash,
    };
    let receipts = vec![Receipt {
        tx_type: TxType::Legacy,
        success: true,
        cumulative_gas_used: 1,
        logs: vec![Log {
            address: METADOSIS_ADDRESS,
            data: event.encode_log_data(),
        }],
    }];
    let expected = CandidatePinV1 {
        block_number: request.number(),
        block_hash: request.block_hash(),
        state_root: request.header().inner.state_root,
        intent_id,
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().expect("input lease id"),
    };
    let source = Arc::new(RethFinalizedInputProofSource::new(
        provider,
        FinalizedParentCertStore::new(),
    ));
    ProductionCandidateFixture {
        request,
        candidate: expected,
        source,
        receipts,
    }
}

pub(super) fn canonical_terminal_fixture(
    candidate: CandidatePinV1,
    status: OcompJobStatus,
) -> OcompJobRecordV1 {
    use outbe_ocomp_protocol::{
        receipts::{ActivationOutcome, AggregateActivationReceiptV1, EffectBindingV1},
        state::{LysisTerminalV1, OcompCompletedBindingV1, OcompTerminalOutcome},
        vote::OcompQuorumV1,
    };
    let intent = production_intent(candidate.block_number);
    let limits = poc_schema_limits();
    let job_id = intent
        .job_id(candidate.block_hash, candidate.state_root, &limits)
        .unwrap();
    let mut finalized = OcompFinalizedJobV1 {
        job_id,
        finalized_request_block_hash: candidate.block_hash,
        finalized_request_state_root: candidate.state_root,
        finality_recorded_height: candidate.block_number + 1,
        open_height: candidate.block_number + 5,
        deadline_height: candidate.block_number + 15,
        quorum: None,
    };
    let mut terminal = LysisTerminalV1 {
        outcome: match status {
            OcompJobStatus::Completed => OcompTerminalOutcome::Completed,
            OcompJobStatus::Expired => OcompTerminalOutcome::Expired,
            OcompJobStatus::Failed => OcompTerminalOutcome::Failed,
            _ => panic!("terminal fixture requires a terminal status"),
        },
        terminal_height: finalized.deadline_height,
        terminal_time: 2_000,
        completed_binding: None,
    };
    if status == OcompJobStatus::Completed {
        let quorum_height = finalized.open_height + 1;
        let digest = B256::repeat_byte(0x93);
        let activation_call_id = B256::repeat_byte(0x94);
        let evidence = B256::repeat_byte(0x95);
        let receipt = AggregateActivationReceiptV1 {
            binding: EffectBindingV1 {
                intent_id: candidate.intent_id,
                job_id,
                attempt: intent.attempt,
                protocol_bundle_hash: intent.protocol_bundle_hash,
                result_digest: digest,
                activation_preconditions_hash: intent
                    .activation_preconditions
                    .activation_preconditions_hash(&limits)
                    .unwrap(),
                activation_call_id,
            },
            outcome: ActivationOutcome::Applied,
            nod_receipt_hash: Some(B256::repeat_byte(0x96)),
            contributor_receipt_hash: Some(B256::repeat_byte(0x97)),
            tribute_receipt_hash: Some(B256::repeat_byte(0x98)),
            carry_over_receipt_hash: Some(B256::repeat_byte(0x99)),
            request_budget_split_receipt_hash: intent
                .frozen_metadosis_values
                .request_budget_split_receipt_hash,
            active_generation_hash: Some(B256::repeat_byte(0x9a)),
            effect_commitment: outbe_ocomp_protocol::hash::hash_framed(
                outbe_ocomp_protocol::registry::HashDomain::Effects,
                [
                    B256::repeat_byte(0x96),
                    B256::repeat_byte(0x97),
                    B256::repeat_byte(0x98),
                    B256::repeat_byte(0x99),
                ]
                .iter()
                .flat_map(|hash| hash.as_slice().iter().copied())
                .collect::<Vec<_>>()
                .as_slice(),
            )
            .unwrap(),
            event_summary_hash: B256::repeat_byte(0x9c),
            activated_at_height: quorum_height,
            activated_at_time: 1_500,
        };
        finalized.quorum = Some(OcompQuorumV1 {
            member_count: 4,
            quorum_threshold: 3,
            result_digest: digest,
            quorum_height,
            signer_bitmap: vec![7],
            evidence_hash: evidence,
        });
        terminal.terminal_height = quorum_height;
        terminal.completed_binding = Some(OcompCompletedBindingV1 {
            job_id,
            activation_call_id,
            result_digest: digest,
            quorum_height,
            quorum_signer_bitmap: vec![7],
            quorum_evidence_hash: evidence,
            result_evidence_hash: evidence,
            terminal_receipt_hash: receipt.terminal_receipt_hash(&limits).unwrap(),
            terminal_receipt: receipt,
        });
    }
    let record = OcompJobRecordV1 {
        intent,
        intent_height: candidate.block_number,
        status,
        finalized: Some(finalized),
        terminal: Some(terminal),
    };
    record.validate_semantics(&limits).unwrap();
    record
}

pub(super) fn ready_record(coordinator: &OcompRetentionCoordinator) -> PinRecordV1 {
    match coordinator.status() {
        RetentionStatus::Ready(record) => record,
        other => panic!("expected ready pin record, got {other:?}"),
    }
}

impl ProductionCandidateFixture {
    pub(super) fn frame(&self) -> FinalizedFrame {
        FinalizedFrame::for_test(
            BlockNumHash::new(self.request.number(), self.request.block_hash()),
            self.request.parent_hash(),
            self.request.header().inner.state_root,
            Block::default(),
            self.receipts.clone(),
        )
    }

    pub(super) fn admit(&self, coordinator: &OcompRetentionCoordinator) {
        let frame = self.frame();
        coordinator
            .reconcile_finalized_frame(&frame, observe_finalized_request(&frame).unwrap())
            .unwrap();
    }
}
