mod present_discovery;

// This fixture is test setup only.
// It uses public Registry initialization and native WWD/model capabilities;
// private Metadosis persistence codecs are reproduced only to seed source words.
// Every constructed aggregate must pass the real public native getter.
mod active_canonical;

mod request_locator;

mod local_result;

use super::{with_prepared_owner_storage, CanonicalState, RethReadOnlyView, DAY};
use crate::{snapshot::validation::ocomp::verify_pin_authority, OutbeHeader};
use alloy_consensus::Sealable;
use alloy_primitives::{B256, U256};
use outbe_node::ocomp::retention::{CandidatePinV1, ExportAuthorityV1, PinRecordV1, PinStateV1};
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

use outbe_ocomp_protocol::{
    hash::hash_framed,
    intent::{
        intent_storage_key, ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    profile::poc_schema_limits,
    receipts::{ActivationOutcome, AggregateActivationReceiptV1, EffectBindingV1},
    registry::HashDomain,
    state::{
        LysisTerminalV1, OcompCompletedBindingV1, OcompFinalizedJobV1, OcompJobStatus,
        OcompTerminalOutcome,
    },
    vote::OcompQuorumV1,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    storage::types::{StorageBytes, StorageKey},
};

fn canonical_job(request: &OutbeHeader, completed: bool) -> OcompJobRecordV1 {
    let hash = B256::repeat_byte(11);
    let intent = JobIntentV1 {
        chain_id: 1,
        genesis_hash: hash,
        fork_id: hash,
        wwd: DAY.value(),
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: hash,
        ce_sealed_root: hash,
        sealed_tribute_collection_key: hash,
        sealed_tribute_collection_root: hash,
        authenticated_day_count: 1,
        authenticated_day_nominal: U256::from(10),
        pre_admission_envelope_hash: hash,
        source_availability_policy_id: hash,
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(10),
            previous_vwap: U256::ONE,
            current_vwap: U256::ONE,
            gratis_demand: U256::from(10),
            day_gratis_limit_minor: U256::from(10),
            lysis_limit_minor: U256::from(10),
            desis_limit_minor: U256::ZERO,
            request_limit_split_receipt_hash: hash,
        },
        logical_evaluation_height: request.inner.number,
        logical_evaluation_time: request.inner.timestamp,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: DAY.value(),
                source_generation: 0,
                collection_key: hash,
                sealed_collection_root: hash,
                exact_count: 1,
                exact_nominal_total: U256::from(10),
            },
            nod: NodTargetPreconditionV1 {
                wwd: DAY.value(),
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 1,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: DAY.value(),
                expected_series_version: 0,
                max_contributor_count: 1,
                max_eligible_nominal_total: U256::from(10),
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: DAY.value(),
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 2,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: hash,
        result_ocomp_binding_hash: hash,
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    };
    let limits = poc_schema_limits();
    let intent_id = intent.intent_id(&limits).unwrap();
    let job_id = intent
        .job_id(request.hash_slow(), request.inner.state_root, &limits)
        .unwrap();
    let mut finalized = OcompFinalizedJobV1 {
        job_id,
        finalized_request_block_hash: request.hash_slow(),
        finalized_request_state_root: request.inner.state_root,
        finality_recorded_height: request.inner.number,
        open_height: request.inner.number + 4,
        deadline_height: request.inner.number + 100,
        quorum: None,
    };
    let terminal = if completed {
        let quorum = OcompQuorumV1 {
            member_count: 4,
            quorum_threshold: 3,
            result_digest: hash,
            quorum_height: request.inner.number + 5,
            signer_bitmap: vec![7],
            evidence_hash: hash,
        };
        let receipt = AggregateActivationReceiptV1 {
            binding: EffectBindingV1 {
                intent_id,
                job_id,
                attempt: 0,
                protocol_bundle_hash: hash,
                result_digest: hash,
                activation_preconditions_hash: intent
                    .activation_preconditions
                    .activation_preconditions_hash(&limits)
                    .unwrap(),
                activation_call_id: hash,
            },
            outcome: ActivationOutcome::Applied,
            nod_receipt_hash: Some(hash),
            contributor_receipt_hash: Some(hash),
            tribute_receipt_hash: Some(hash),
            carry_over_receipt_hash: Some(hash),
            request_limit_split_receipt_hash: hash,
            active_generation_hash: Some(hash),
            effect_commitment: hash_framed(HashDomain::Effects, &hash.as_slice().repeat(4))
                .unwrap(),
            event_summary_hash: hash,
            activated_at_height: request.inner.number + 5,
            activated_at_time: 1_005,
        };
        let binding = OcompCompletedBindingV1 {
            job_id,
            activation_call_id: hash,
            result_digest: hash,
            quorum_height: quorum.quorum_height,
            quorum_signer_bitmap: quorum.signer_bitmap.clone(),
            quorum_evidence_hash: quorum.evidence_hash,
            result_evidence_hash: hash,
            terminal_receipt_hash: receipt.terminal_receipt_hash(&limits).unwrap(),
            terminal_receipt: receipt,
        };
        finalized.quorum = Some(quorum);
        Some(LysisTerminalV1 {
            outcome: OcompTerminalOutcome::Completed,
            terminal_height: request.inner.number + 5,
            terminal_time: 1_005,
            completed_binding: Some(binding),
        })
    } else {
        None
    };
    let record = OcompJobRecordV1 {
        intent,
        intent_height: request.inner.number,
        status: if completed {
            OcompJobStatus::Completed
        } else {
            OcompJobStatus::AwaitingFinality
        },
        finalized: Some(finalized),
        terminal,
    };
    record.validate_semantics(&limits).unwrap();
    record
}

fn stored_job(intent_id: B256, encoded: &[u8]) -> HashMapStorageProvider {
    let mut owner = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut owner, |storage| {
        // Fixed OCM proof mapping base. The snapshot adapter itself never reads
        // Metadosis slots directly or imports its private schema.
        let slot = intent_storage_key(intent_id)
            .unwrap()
            .mapping_slot(U256::from(20));
        StorageBytes::new(slot, METADOSIS_ADDRESS, storage)
            .write(encoded)
            .unwrap();
    });
    owner
}

fn with_job(
    version: u32,
    completed: bool,
    finalized: bool,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView, OcompJobRecordV1),
) {
    with_prepared_owner_storage(
        version,
        400,
        |request| {
            let mut job = canonical_job(request, completed);
            if !finalized {
                assert!(!completed);
                job.finalized = None;
            }
            stored_job(
                job.intent.intent_id(&poc_schema_limits()).unwrap(),
                &job.encode_canonical(&poc_schema_limits()).unwrap(),
            )
        },
        |state, view| {
            let request = view.header(100).unwrap().unwrap();
            let mut job = canonical_job(&request, completed);
            if !finalized {
                job.finalized = None;
            }
            // The native getter reads the job from E, whose state root is
            // independent of the retained request header B.
            assert_ne!(
                request.inner.state_root,
                view.header(400).unwrap().unwrap().inner.state_root
            );
            assert_eq!(
                state
                    .metadosis_job(
                        job.intent.intent_id(&poc_schema_limits()).unwrap(),
                        DAY,
                        None
                    )
                    .unwrap(),
                job
            );
            check(state, view, job);
        },
    );
}

fn candidate(view: &RethReadOnlyView, job: &OcompJobRecordV1) -> CandidatePinV1 {
    let request = view.header(100).unwrap().unwrap();
    CandidatePinV1 {
        block_number: 100,
        block_hash: request.hash_slow(),
        state_root: request.inner.state_root,
        intent_id: job.intent.intent_id(&poc_schema_limits()).unwrap(),
        wwd: job.intent.wwd,
        ce_sealed_root: job.intent.ce_sealed_root,
        protocol_bundle_hash: job.intent.protocol_bundle_hash,
        input_lease_id: job.intent.input_lease_id().unwrap(),
    }
}

fn export() -> ExportAuthorityV1 {
    ExportAuthorityV1 {
        source_generation: 3,
        lease_generation: 4,
        manifest_hash: B256::repeat_byte(55),
    }
}

fn stages(
    candidate: CandidatePinV1,
    job: &OcompJobRecordV1,
) -> Vec<(PinRecordV1, bool, Option<ExportAuthorityV1>)> {
    let finalized = job.finalized.as_ref().unwrap();
    let job_id = finalized.job_id;
    let finality_recorded_height = finalized.finality_recorded_height;
    let open_height = finalized.open_height;
    let deadline_height = finalized.deadline_height;
    let mut states = vec![
        (
            PinStateV1::AwaitingJobFinalization { candidate },
            true,
            None,
        ),
        (
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            },
            true,
            None,
        ),
        (
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export: export(),
            },
            true,
            Some(export()),
        ),
    ];
    for authority in [None, Some(export())] {
        // Canonical completion was at 105. Retention observed terminality
        // at 250, after the response deadline of 200; its native evidence
        // window is 64 blocks. This lag does not change job authority.
        states.push((
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation: 3,
                export: authority,
                terminal_height: 250,
                release_height: 314,
            },
            true,
            authority,
        ));
        states.push((
            PinStateV1::GcPending {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation: 3,
                export: authority,
                terminal_height: 250,
                release_height: 314,
            },
            false,
            authority,
        ));
        // Released has no response-window fields. Its observation height
        // is independent of both canonical completion and terminal height.
        states.push((
            PinStateV1::Released {
                candidate,
                job_id,
                source_generation: 3,
                observed_height: 400,
                export: authority,
            },
            false,
            authority,
        ));
    }
    states
        .into_iter()
        .map(|(state, source, authority)| {
            (
                PinRecordV1 {
                    generation: 12,
                    state,
                },
                source,
                authority,
            )
        })
        .collect()
}

#[test]
fn awaiting_pin_binds_current_e_before_and_after_canonical_finalization() {
    for version in [1, 2] {
        for finalized in [false, true] {
            with_job(version, false, finalized, |state, view, job| {
                let candidate = candidate(view, &job);
                let record = PinRecordV1 {
                    generation: 1,
                    state: PinStateV1::AwaitingJobFinalization { candidate },
                };
                let verified =
                    verify_pin_authority(state, view, candidate.block_hash, &record).unwrap();
                assert_eq!(verified.candidate, candidate);
                assert_eq!(verified.job, job);
                assert!(verified.requires_source);
                assert_eq!(verified.export, None);
            });
        }
    }
}

#[test]
fn all_pin_stages_allow_completed_authority_and_preserve_source_obligations() {
    for version in [1, 2] {
        with_job(version, true, true, |state, view, job| {
            assert_eq!(job.status, OcompJobStatus::Completed);
            assert_eq!(job.terminal.as_ref().unwrap().terminal_height, 105);
            let candidate = candidate(view, &job);
            for (record, requires_source, export) in stages(candidate, &job) {
                let verified = verify_pin_authority(state, view, candidate.block_hash, &record)
                    .unwrap_or_else(|error| panic!("{:?}: {error:#}", record.state));
                assert_eq!(verified.candidate, candidate);
                assert_eq!(verified.job, job);
                assert_eq!(
                    verified.requires_source, requires_source,
                    "{:?}",
                    record.state
                );
                assert_eq!(verified.export, export);
            }
        });
    }
}

#[test]
fn every_candidate_identity_component_and_registry_key_bind_exactly() {
    for version in [1, 2] {
        // An unfinalized canonical record forces the verifier to bind B
        // explicitly; the getter's finalized-request branch cannot do it.
        with_job(version, false, false, |state, view, job| {
            let valid = candidate(view, &job);
            let bad_hash = B256::repeat_byte(77);
            let mutations = [
                CandidatePinV1 {
                    block_number: 101,
                    ..valid
                },
                CandidatePinV1 {
                    block_hash: bad_hash,
                    ..valid
                },
                CandidatePinV1 {
                    state_root: bad_hash,
                    ..valid
                },
                CandidatePinV1 {
                    intent_id: bad_hash,
                    ..valid
                },
                CandidatePinV1 {
                    wwd: valid.wwd + 1,
                    ..valid
                },
                CandidatePinV1 {
                    ce_sealed_root: bad_hash,
                    ..valid
                },
                CandidatePinV1 {
                    protocol_bundle_hash: bad_hash,
                    ..valid
                },
                CandidatePinV1 {
                    input_lease_id: bad_hash,
                    ..valid
                },
            ];
            for changed in mutations {
                let record = PinRecordV1 {
                    generation: 1,
                    state: PinStateV1::AwaitingJobFinalization { candidate: changed },
                };
                // Follow a changed block hash with the registry key so the
                // actual canonical-header comparison is also exercised.
                assert!(
                    verify_pin_authority(state, view, changed.block_hash, &record).is_err(),
                    "accepted {changed:?}"
                );
            }
            let record = PinRecordV1 {
                generation: 1,
                state: PinStateV1::AwaitingJobFinalization { candidate: valid },
            };
            assert!(verify_pin_authority(state, view, bad_hash, &record).is_err());
        });
    }
}

#[test]
fn finalized_job_and_each_persisted_response_window_field_must_match_current_e() {
    for version in [1, 2] {
        with_job(version, true, true, |state, view, job| {
            let candidate = candidate(view, &job);
            for (valid, _, _) in stages(candidate, &job) {
                if matches!(valid.state, PinStateV1::AwaitingJobFinalization { .. }) {
                    continue;
                }
                for field in 0..4 {
                    let mut changed = valid;
                    match &mut changed.state {
                        PinStateV1::Finalized {
                            job_id,
                            finality_recorded_height,
                            open_height,
                            deadline_height,
                            ..
                        }
                        | PinStateV1::Exported {
                            job_id,
                            finality_recorded_height,
                            open_height,
                            deadline_height,
                            ..
                        }
                        | PinStateV1::Terminal {
                            job_id,
                            finality_recorded_height,
                            open_height,
                            deadline_height,
                            ..
                        }
                        | PinStateV1::GcPending {
                            job_id,
                            finality_recorded_height,
                            open_height,
                            deadline_height,
                            ..
                        } => match field {
                            0 => *job_id = B256::repeat_byte(88),
                            1 => *finality_recorded_height += 1,
                            2 => *open_height += 1,
                            _ => *deadline_height += 1,
                        },
                        PinStateV1::Released { job_id, .. } => {
                            if field != 0 {
                                continue;
                            }
                            *job_id = B256::repeat_byte(88);
                        }
                        PinStateV1::AwaitingJobFinalization { .. } => unreachable!(),
                    }
                    assert!(
                        verify_pin_authority(state, view, candidate.block_hash, &changed).is_err(),
                        "accepted {changed:?}"
                    );
                }
            }
        });
    }
}

#[test]
fn local_finalized_pin_cannot_supply_finalization_absent_from_current_e() {
    for version in [1, 2] {
        with_job(version, false, false, |state, view, job| {
            let candidate = candidate(view, &job);
            let finalized_job = canonical_job(&view.header(100).unwrap().unwrap(), false);
            for (record, _, _) in stages(candidate, &finalized_job) {
                if !matches!(record.state, PinStateV1::AwaitingJobFinalization { .. }) {
                    assert!(
                        verify_pin_authority(state, view, candidate.block_hash, &record).is_err(),
                        "accepted {:?}",
                        record.state
                    );
                }
            }
        });
    }
}

#[test]
fn canonical_finalized_job_must_bind_the_retained_native_request_header() {
    for version in [1, 2] {
        with_prepared_owner_storage(
            version,
            400,
            |request| {
                let mut other_request = request.clone();
                other_request.inner.timestamp += 1;
                let job = canonical_job(&other_request, false);
                stored_job(
                    job.intent.intent_id(&poc_schema_limits()).unwrap(),
                    &job.encode_canonical(&poc_schema_limits()).unwrap(),
                )
            },
            |state, view| {
                let job = canonical_job(&view.header(100).unwrap().unwrap(), false);
                let candidate = candidate(view, &job);
                let record = PinRecordV1 {
                    generation: 1,
                    state: PinStateV1::AwaitingJobFinalization { candidate },
                };
                assert!(verify_pin_authority(state, view, candidate.block_hash, &record).is_err());
            },
        );
    }
}
#[test]
fn unfinalized_canonical_intent_height_must_equal_candidate_height() {
    for version in [1, 2] {
        with_prepared_owner_storage(
            version,
            400,
            |request| {
                let mut job = canonical_job(request, false);
                job.finalized = None;
                job.intent_height = 101;
                stored_job(
                    job.intent.intent_id(&poc_schema_limits()).unwrap(),
                    &job.encode_canonical(&poc_schema_limits()).unwrap(),
                )
            },
            |state, view| {
                let job = canonical_job(&view.header(100).unwrap().unwrap(), false);
                let candidate = candidate(view, &job);
                assert_eq!(
                    state
                        .metadosis_job(candidate.intent_id, DAY, None)
                        .unwrap()
                        .intent_height,
                    101
                );
                let record = PinRecordV1 {
                    generation: 1,
                    state: PinStateV1::AwaitingJobFinalization { candidate },
                };
                assert!(verify_pin_authority(state, view, candidate.block_hash, &record).is_err());
            },
        );
    }
}
