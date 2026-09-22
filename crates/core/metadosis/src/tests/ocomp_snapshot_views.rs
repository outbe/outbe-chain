use crate::{api::read_live_ocomp_jobs, schema::MetadosisContract};

use super::with_storage;

#[test]
fn empty_native_aggregate_has_no_live_jobs() {
    with_storage(|storage| {
        assert!(read_live_ocomp_jobs(storage.clone()).unwrap().is_empty());
        assert!(read_live_ocomp_jobs(storage).unwrap().is_empty());
    });
}

#[test]
fn malformed_native_scheduler_is_rejected() {
    with_storage(|storage| {
        let contract = MetadosisContract::new(storage.clone());
        contract
            .ocomp_scheduler
            .write(b"invalid scheduler")
            .unwrap();

        assert!(read_live_ocomp_jobs(storage).is_err());
    });
}

use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    rc::Rc,
};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    error::Result,
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};

use crate::constants::{MAX_ACTIVE_WWDS, MAX_RECORDS_KEPT};

#[derive(Default)]
struct TrackingReader {
    words: HashMap<(Address, U256), U256>,
    allowed: Option<BTreeSet<U256>>,
    forbidden: BTreeSet<U256>,
    reads: Rc<RefCell<Vec<U256>>>,
}

impl StorageReader for TrackingReader {
    fn read_storage(&self, address: Address, key: B256) -> Result<U256> {
        let slot = U256::from_be_bytes(key.0);
        self.reads.borrow_mut().push(slot);
        assert!(
            !self.forbidden.contains(&slot)
                && self
                    .allowed
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(&slot)),
            "view traversed forbidden storage slot {slot} before rejecting native lengths"
        );
        Ok(self
            .words
            .get(&(address, slot))
            .copied()
            .unwrap_or_default())
    }
}

fn scalar_slot(read: impl FnOnce(&MetadosisContract<'_>) -> Result<usize>) -> U256 {
    let reader = TrackingReader::default();
    let reads = reader.reads.clone();
    let mut provider = ReadOnlyStorageProvider::new(reader);
    StorageHandle::enter(&mut provider, |storage| {
        read(&MetadosisContract::new(storage)).unwrap();
    });
    let reads = reads.borrow();
    assert_eq!(reads.len(), 1);
    reads[0]
}

struct NativeSlots {
    active: U256,
    closed: U256,
    scheduler: U256,
    ready: U256,
    response: U256,
}

impl NativeSlots {
    fn discover() -> Self {
        Self {
            active: scalar_slot(|contract| contract.active_wwd.len().map(|n| n as usize)),
            closed: with_storage(|storage| MetadosisContract::new(storage).closed_wwd.base_slot()),
            scheduler: scalar_slot(|contract| contract.ocomp_scheduler.len()),
            ready: scalar_slot(|contract| contract.ocomp_ready_index.len()),
            response: scalar_slot(|contract| contract.ocomp_response_deadline_index.len()),
        }
    }

    fn length_slots(&self) -> BTreeSet<U256> {
        [
            self.active,
            self.closed,
            self.closed + U256::ONE,
            self.scheduler,
            self.ready,
            self.response,
        ]
        .into_iter()
        .collect()
    }
}

fn assert_rejects_before_payload(slots: &NativeSlots, slot: U256, word: U256) {
    let reader = TrackingReader {
        words: [((METADOSIS_ADDRESS, slot), word)].into_iter().collect(),
        allowed: Some(slots.length_slots()),
        ..Default::default()
    };
    let mut provider = ReadOnlyStorageProvider::new(reader);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(read_live_ocomp_jobs(storage).is_err());
    });
}

#[test]
fn oversized_active_count_rejects_before_payload() {
    let slots = NativeSlots::discover();
    assert_rejects_before_payload(&slots, slots.active, U256::from(MAX_ACTIVE_WWDS + 1));
}

#[test]
fn oversized_closed_count_rejects_before_payload() {
    let slots = NativeSlots::discover();
    assert_rejects_before_payload(
        &slots,
        slots.closed + U256::ONE,
        U256::from(MAX_RECORDS_KEPT + 1),
    );
}

#[test]
fn oversized_scheduler_rejects_before_payload() {
    let slots = NativeSlots::discover();
    let length = 8 + 148 * usize::from(u16::MAX) + 1;
    assert_rejects_before_payload(&slots, slots.scheduler, U256::from(length * 2 + 1));
}

#[test]
fn oversized_ready_index_rejects_before_payload() {
    let slots = NativeSlots::discover();
    let length = 8 + 20 * MAX_RECORDS_KEPT + 1;
    assert_rejects_before_payload(&slots, slots.ready, U256::from(length * 2 + 1));
}

#[test]
fn oversized_response_index_rejects_before_payload() {
    let slots = NativeSlots::discover();
    let length = 8 + 72 * usize::from(u16::MAX) + 1;
    assert_rejects_before_payload(&slots, slots.response, U256::from(length * 2 + 1));
}

#[test]
fn indexed_fsm_length_rejects_before_aggregate_or_fsm_payload() {
    let slots = NativeSlots::discover();
    let day = crate::fixture_kernel::TEST_WWD;
    let fsm_slot = scalar_slot(|contract| contract.ocomp_fsm_states.get_bytes(&day).len());
    for closed in [false, true] {
        for length in [1_usize, 149] {
            let membership = if closed { slots.closed } else { slots.active };
            let count_slot = if closed {
                membership + U256::ONE
            } else {
                membership
            };
            let data_slot = U256::from_be_bytes(keccak256(membership.to_be_bytes::<32>()).0);
            let mut allowed = slots.length_slots();
            allowed.extend([data_slot, fsm_slot]);
            let reader = TrackingReader {
                words: [
                    ((METADOSIS_ADDRESS, count_slot), U256::ONE),
                    ((METADOSIS_ADDRESS, data_slot), U256::from(day.value())),
                    (
                        (METADOSIS_ADDRESS, fsm_slot),
                        U256::from(if length <= 31 {
                            length * 2
                        } else {
                            length * 2 + 1
                        }),
                    ),
                ]
                .into_iter()
                .collect(),
                allowed: Some(allowed),
                ..Default::default()
            };
            let mut provider = ReadOnlyStorageProvider::new(reader);
            StorageHandle::enter(&mut provider, |storage| {
                assert!(read_live_ocomp_jobs(storage).is_err());
            });
        }
    }
}

use outbe_ocomp_protocol::{
    intent::intent_storage_key,
    receipts::desis_request_brief_hash,
    state::{OcompJobRecordV1, OcompJobStatus},
};
use outbe_primitives::{storage::hashmap::HashMapStorageProvider, time::WorldwideDay};

use crate::{
    fixture_kernel::{ActivationFixture, FixtureKernelExt, TEST_WWD},
    reducer::OuterWwdEvent,
};

fn readonly_jobs(provider: &HashMapStorageProvider) -> Result<Vec<(B256, OcompJobRecordV1)>> {
    let reader = TrackingReader {
        words: provider.storage.clone(),
        ..Default::default()
    };
    let mut readonly =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, 1, B256::repeat_byte(17));
    StorageHandle::enter(&mut readonly, read_live_ocomp_jobs)
}

/// Add another day through the same owner request transition as the activation
/// fixture, stopping before finality so the native scheduler contains both phases.
fn add_awaiting_job(
    fixture: &mut ActivationFixture,
    day: WorldwideDay,
) -> (B256, OcompJobRecordV1) {
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let mut contract = MetadosisContract::new(storage);
        let mut intent = contract
            .ocomp_job_record(fixture.intent_id, &fixture.limits)
            .unwrap()
            .unwrap()
            .intent;
        let mut receipt = fixture.request_receipt.clone();
        intent.wwd = day.value();
        intent.activation_preconditions.tribute.wwd = day.value();
        intent.activation_preconditions.nod.wwd = day.value();
        intent.activation_preconditions.contributors.worldwide_day = day.value();
        intent.activation_preconditions.metadosis.wwd = day.value();
        for price in &mut intent.frozen_metadosis_values.auction_entry_prices {
            price.source_day = day.value();
        }
        receipt.wwd = day.value();
        receipt.auction_entry_prices = intent.frozen_metadosis_values.auction_entry_prices.clone();
        receipt.desis_brief_hash = Some(
            desis_request_brief_hash(
                receipt.protocol_bundle_hash,
                receipt.wwd,
                receipt.desis_limit_minor,
                &receipt.auction_entry_prices,
                receipt.logical_anchor,
            )
            .unwrap(),
        );
        intent
            .frozen_metadosis_values
            .request_limit_split_receipt_hash = receipt.receipt_hash(&fixture.limits).unwrap();
        let frozen = &intent.frozen_metadosis_values;
        contract
            .fixture_create_ready_day(
                day,
                frozen.day_limit,
                frozen.previous_vwap,
                frozen.current_vwap,
            )
            .unwrap();
        contract.active_wwd.insert(day).unwrap();
        contract
            .enqueue_ocomp_ready(day, intent.logical_evaluation_height)
            .unwrap();
        let transition = crate::commit::plan_outer_transition_for_test_fixture(
            &contract,
            day,
            OuterWwdEvent::OcompRequestCommitted,
        )
        .unwrap();
        contract
            .commit_ocomp_request(&transition, &intent, &receipt, &fixture.limits)
            .unwrap();
        let id = intent.intent_id(&fixture.limits).unwrap();
        (
            id,
            contract
                .ocomp_job_record(id, &fixture.limits)
                .unwrap()
                .unwrap(),
        )
    })
}

#[test]
fn awaiting_and_voting_jobs_are_canonical_ordered_and_readonly() {
    let mut fixture = ActivationFixture::new_voting(20, 1_010, true);
    let voting = StorageHandle::enter(&mut fixture.provider, |storage| {
        MetadosisContract::new(storage)
            .ocomp_job_record(fixture.intent_id, &fixture.limits)
            .unwrap()
            .unwrap()
    });
    assert_eq!(voting.status, OcompJobStatus::VotingOpen);
    let awaiting = add_awaiting_job(&mut fixture, WorldwideDay::new(TEST_WWD.value() - 1));
    assert_eq!(awaiting.1.status, OcompJobStatus::AwaitingFinality);
    let expected = vec![awaiting, (fixture.intent_id, voting)];
    let before = fixture.provider.storage.clone();

    // ReadOnlyStorageProvider rejects every write; repeating the public view
    // also verifies deterministic order independent of request insertion order.
    assert_eq!(readonly_jobs(&fixture.provider).unwrap(), expected);
    assert_eq!(readonly_jobs(&fixture.provider).unwrap(), expected);
    assert_eq!(fixture.provider.storage, before);
}

#[test]
fn completed_response_window_survives_without_a_live_job() {
    let mut fixture = ActivationFixture::new(20, 1_010, true);
    fixture.apply().unwrap();
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let contract = MetadosisContract::new(storage);
        let record = contract
            .ocomp_job_record(fixture.intent_id, &fixture.limits)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::Completed);
        assert!(record.finalized.unwrap().quorum.is_some());
        assert!(contract.ocomp_scheduler.is_empty().unwrap());
        assert_eq!(contract.read_response_deadline_index().unwrap().len(), 1);
    });
    assert!(readonly_jobs(&fixture.provider).unwrap().is_empty());
}

#[test]
fn pending_day_omitted_from_scheduler_is_rejected() {
    let mut fixture = ActivationFixture::new_voting(20, 1_010, true);
    StorageHandle::enter(&mut fixture.provider, |storage| {
        MetadosisContract::new(storage)
            .ocomp_scheduler
            .clear()
            .unwrap();
    });
    assert!(readonly_jobs(&fixture.provider).is_err());
}

#[test]
fn scheduler_foreign_day_is_rejected_before_its_fsm_is_even_inspected() {
    let mut fixture = ActivationFixture::new_voting(20, 1_010, true);
    let fsm_slot = scalar_slot(|contract| contract.ocomp_fsm_states.get_bytes(&TEST_WWD).len());
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let contract = MetadosisContract::new(storage);
        assert!(contract.active_wwd.remove(&TEST_WWD).unwrap());
    });
    // An unindexed FSM is untrusted even if its declared payload would be huge.
    fixture
        .provider
        .storage
        .insert((METADOSIS_ADDRESS, fsm_slot), U256::from(20_000_001));
    let reader = TrackingReader {
        words: fixture.provider.storage,
        forbidden: [fsm_slot].into_iter().collect(),
        ..Default::default()
    };
    let mut provider = ReadOnlyStorageProvider::new(reader);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(read_live_ocomp_jobs(storage).is_err());
    });
}

#[test]
fn missing_or_wrong_canonical_job_is_rejected() {
    for wrong_record in [false, true] {
        let mut fixture = ActivationFixture::new_voting(20, 1_010, true);
        let other = add_awaiting_job(&mut fixture, WorldwideDay::new(TEST_WWD.value() - 1));
        StorageHandle::enter(&mut fixture.provider, |storage| {
            let contract = MetadosisContract::new(storage);
            let key = intent_storage_key(fixture.intent_id).unwrap();
            let bytes = contract.ocomp_job_records.get_bytes(&key);
            if wrong_record {
                bytes
                    .write(&other.1.encode_canonical(&fixture.limits).unwrap())
                    .unwrap();
            } else {
                bytes.clear().unwrap();
            }
        });
        assert!(readonly_jobs(&fixture.provider).is_err());
    }
}

#[test]
fn canonical_terminal_record_cannot_remain_live() {
    let mut completed = ActivationFixture::new(20, 1_010, true);
    completed.apply().unwrap();
    let terminal_record = StorageHandle::enter(&mut completed.provider, |storage| {
        MetadosisContract::new(storage)
            .ocomp_job_record(completed.intent_id, &completed.limits)
            .unwrap()
            .unwrap()
    });
    assert_eq!(terminal_record.status, OcompJobStatus::Completed);
    let mut live = ActivationFixture::new_voting(20, 1_010, true);
    assert_eq!(live.intent_id, completed.intent_id);
    StorageHandle::enter(&mut live.provider, |storage| {
        let mut contract = MetadosisContract::new(storage);
        contract
            .corrupt_ocomp_job_record(
                live.intent_id,
                &terminal_record.encode_canonical(&live.limits).unwrap(),
            )
            .unwrap();
    });
    assert!(readonly_jobs(&live.provider).is_err());
}

#[test]
fn within_bound_malformed_native_indexes_and_fsm_are_rejected() {
    for corruption in ["scheduler", "ready", "response", "fsm"] {
        let mut fixture = ActivationFixture::new_voting(20, 1_010, true);
        StorageHandle::enter(&mut fixture.provider, |storage| {
            let contract = MetadosisContract::new(storage);
            match corruption {
                "scheduler" => contract.ocomp_scheduler.write(&[0; 8]),
                "ready" => contract.ocomp_ready_index.write(&[0; 8]),
                "response" => contract.ocomp_response_deadline_index.write(&[0; 8]),
                "fsm" => contract
                    .ocomp_fsm_states
                    .get_bytes(&TEST_WWD)
                    .write(&[0; 148]),
                _ => unreachable!(),
            }
            .unwrap();
        });
        assert!(readonly_jobs(&fixture.provider).is_err(), "{corruption}");
    }
}

#[test]
fn response_bytes_above_active_capacity_reach_the_native_decoder() {
    let mut fixture = ActivationFixture::new_voting(20, 1_010, true);
    let slots = NativeSlots::discover();
    let payload_slot = U256::from_be_bytes(keccak256(slots.response.to_be_bytes::<32>()).0);
    StorageHandle::enter(&mut fixture.provider, |storage| {
        // Deliberately malformed but within the native u16 count bound. This
        // proves the preflight does not impose the smaller active-WWD cap.
        MetadosisContract::new(storage)
            .ocomp_response_deadline_index
            .write(&vec![0; 8 + 72 * (MAX_ACTIVE_WWDS + 1)])
            .unwrap();
    });
    let reader = TrackingReader {
        words: fixture.provider.storage,
        ..Default::default()
    };
    let reads = reader.reads.clone();
    let mut provider = ReadOnlyStorageProvider::new(reader);
    let result = StorageHandle::enter(&mut provider, read_live_ocomp_jobs);
    assert!(
        matches!(result, Err(outbe_primitives::error::PrecompileError::Fatal(message))
        if message.contains("response index magic/version mismatch"))
    );
    assert!(reads.borrow().contains(&payload_slot));
}
