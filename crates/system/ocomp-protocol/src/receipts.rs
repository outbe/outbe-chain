use alloy_primitives::{B256, U256};

use crate::{
    activation::ActivationCallCoreV1,
    error::ProtocolError,
    hash::hash_framed,
    intent::{
        ContributorSeriesReservationV1, DesisBriefReservationV1, NodNamespaceReservationV1,
        PromisDeltaReservationV1, TributePartitionReservationV1,
    },
    registry::HashDomain,
    schema::{
        encode_nested_value, impl_top_level_codec, require, wire_enum_u8, wire_struct, NestedCodec,
        SchemaLimits,
    },
};

wire_enum_u8! {
    pub enum OwnerKind {
        Nod = 1,
        Contributor = 2,
        Tribute = 3,
        Desis = 4,
        Promis = 5,
    }
}

wire_enum_u8! {
    pub enum ActivationOutcome {
        Applied = 1,
        ConflictResolved = 2,
    }
}

wire_struct! {
    pub struct EffectBindingV1 {
        pub intent_id: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub protocol_bundle_hash: B256,
        pub result_digest: B256,
        pub reservation_set_hash: B256,
        pub activation_call_id: B256,
    }
}

wire_struct! {
    pub struct NodBatchReceiptV1 {
        pub binding: EffectBindingV1,
        pub nod_namespace_reservation: NodNamespaceReservationV1,
        pub nod_count: u32,
        pub nod_root: B256,
        pub nod_amount_total: U256,
        pub nod_gratis_consumed: U256,
        pub issued_at: u64,
        pub state_event_digest: B256,
    }
}
impl_top_level_codec!(NodBatchReceiptV1, NodBatchReceiptV1);

wire_struct! {
    pub struct ContributorReceiptV1 {
        pub binding: EffectBindingV1,
        pub contributor_series_reservation: ContributorSeriesReservationV1,
        pub contributor_count: u32,
        pub contributor_root: B256,
        pub eligible_nominal_total: U256,
        pub state_event_digest: B256,
    }
}
impl_top_level_codec!(ContributorReceiptV1, ContributorReceiptV1);

wire_struct! {
    pub struct TributeReceiptV1 {
        pub binding: EffectBindingV1,
        pub tribute_partition_reservation: TributePartitionReservationV1,
        pub sealed_collection_root: B256,
        pub consumed_count: u32,
        pub consumed_nominal_total: U256,
        pub retired_generation: u64,
        pub state_event_digest: B256,
    }
}
impl_top_level_codec!(TributeReceiptV1, TributeReceiptV1);

wire_struct! {
    pub struct DesisReceiptV1 {
        pub binding: EffectBindingV1,
        pub desis_reservation: DesisBriefReservationV1,
        pub brief_hash: B256,
        pub logical_anchor: u64,
        pub accepted_brief_count: u8,
        pub state_event_digest: B256,
    }
}
impl_top_level_codec!(DesisReceiptV1, DesisReceiptV1);

wire_struct! {
    pub struct PromisReceiptV1 {
        pub binding: EffectBindingV1,
        pub promis_reservation: PromisDeltaReservationV1,
        pub accumulator_key: B256,
        pub before_value: U256,
        pub applied_delta: U256,
        pub after_value: U256,
        pub state_event_digest: B256,
    }
}
impl_top_level_codec!(PromisReceiptV1, PromisReceiptV1);

wire_struct! {
    pub struct NodStateEventProjectionV1 {
        pub wwd: u32,
        pub target_generation: u64,
        pub namespace_root_before: B256,
        pub nod_count: u32,
        pub nod_root: B256,
        pub nod_amount_total: U256,
        pub nod_gratis_consumed: U256,
        pub issued_at: u64,
    }
}

wire_struct! {
    pub struct ContributorStateEventProjectionV1 {
        pub series_id: u32,
        pub series_version_before: u64,
        pub series_version_after: u64,
        pub contributor_count: u32,
        pub contributor_root: B256,
        pub eligible_nominal_total: U256,
    }
}

wire_struct! {
    pub struct TributeStateEventProjectionV1 {
        pub wwd: u32,
        pub source_generation: u64,
        pub sealed_collection_root: B256,
        pub consumed_count: u32,
        pub consumed_nominal_total: U256,
        pub retired_generation: u64,
    }
}

wire_struct! {
    pub struct DesisStateEventProjectionV1 {
        pub wwd: u32,
        pub state_version_before: u64,
        pub state_version_after: u64,
        pub brief_hash: B256,
        pub auction_entry_price: U256,
        pub logical_anchor: u64,
        pub accepted_brief_count: u8,
    }
}

wire_struct! {
    pub struct PromisStateEventProjectionV1 {
        pub accumulator_key: B256,
        pub operation_state_version: u64,
        pub before_value: U256,
        pub applied_delta: U256,
        pub after_value: U256,
    }
}

wire_struct! {
    pub struct AggregateActivationReceiptV1 {
        pub binding: EffectBindingV1,
        pub outcome: ActivationOutcome,
        pub nod_receipt_hash: Option<B256>,
        pub contributor_receipt_hash: Option<B256>,
        pub tribute_receipt_hash: Option<B256>,
        pub desis_receipt_hash: Option<B256>,
        pub promis_receipt_hash: Option<B256>,
        pub active_generation_hash: Option<B256>,
        pub effect_commitment: B256,
        pub event_summary_hash: B256,
        pub activated_at_height: u64,
        pub activated_at_time: u64,
    }
    validate = validate_aggregate_receipt;
}
impl_top_level_codec!(AggregateActivationReceiptV1, AggregateActivationReceiptV1);

impl EffectBindingV1 {
    pub fn validate_call(
        &self,
        call: &ActivationCallCoreV1,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        require(
            self.intent_id == call.intent_id
                && self.job_id == call.job_id
                && self.attempt == call.attempt
                && self.protocol_bundle_hash == call.protocol_bundle_hash
                && self.result_digest == call.result_digest
                && self.reservation_set_hash == call.reservation_set_hash
                && self.activation_call_id == call.activation_call_id(limits)?,
            "effect binding activation call",
        )
    }
}

pub(crate) fn owner_state_event_digest<T: NestedCodec>(
    owner: OwnerKind,
    binding: &EffectBindingV1,
    projection: &T,
    limits: &SchemaLimits,
) -> Result<B256, ProtocolError> {
    let binding_bytes = encode_nested_value(binding, limits)?;
    let projection_bytes = encode_nested_value(projection, limits)?;
    let projection_len =
        u32::try_from(projection_bytes.len()).map_err(|_| ProtocolError::IntegerOverflow {
            what: "owner projection byte length",
        })?;
    let mut payload = vec![owner as u8];
    payload.extend_from_slice(&binding_bytes);
    payload.extend_from_slice(&projection_len.to_be_bytes());
    payload.extend_from_slice(&projection_bytes);
    hash_framed(HashDomain::StateEvents, &payload)
}

macro_rules! receipt_hash {
    ($type:ty, $method:ident, $domain:ident) => {
        impl $type {
            pub fn $method(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
                hash_framed(HashDomain::$domain, &self.encode_canonical(limits)?)
            }
        }
    };
}

receipt_hash!(NodBatchReceiptV1, receipt_hash, NodReceipt);
receipt_hash!(ContributorReceiptV1, receipt_hash, ContributorReceipt);
receipt_hash!(TributeReceiptV1, receipt_hash, TributeReceipt);
receipt_hash!(DesisReceiptV1, receipt_hash, DesisReceipt);
receipt_hash!(PromisReceiptV1, receipt_hash, PromisReceipt);

macro_rules! validate_projection_digest {
    ($receipt:ty, $projection:ty, $owner:ident) => {
        impl $receipt {
            pub fn validate_projection(
                &self,
                projection: &$projection,
                limits: &SchemaLimits,
            ) -> Result<(), ProtocolError> {
                require(
                    self.state_event_digest
                        == owner_state_event_digest(
                            OwnerKind::$owner,
                            &self.binding,
                            projection,
                            limits,
                        )?,
                    "receipt state event digest",
                )
            }
        }
    };
}

validate_projection_digest!(NodBatchReceiptV1, NodStateEventProjectionV1, Nod);
validate_projection_digest!(
    ContributorReceiptV1,
    ContributorStateEventProjectionV1,
    Contributor
);
validate_projection_digest!(TributeReceiptV1, TributeStateEventProjectionV1, Tribute);
validate_projection_digest!(DesisReceiptV1, DesisStateEventProjectionV1, Desis);
validate_projection_digest!(PromisReceiptV1, PromisStateEventProjectionV1, Promis);

impl AggregateActivationReceiptV1 {
    pub fn validate_semantics(&self) -> Result<(), ProtocolError> {
        let all_present = self.nod_receipt_hash.is_some()
            && self.contributor_receipt_hash.is_some()
            && self.tribute_receipt_hash.is_some()
            && self.desis_receipt_hash.is_some()
            && self.promis_receipt_hash.is_some()
            && self.active_generation_hash.is_some();
        let all_absent = self.nod_receipt_hash.is_none()
            && self.contributor_receipt_hash.is_none()
            && self.tribute_receipt_hash.is_none()
            && self.desis_receipt_hash.is_none()
            && self.promis_receipt_hash.is_none()
            && self.active_generation_hash.is_none();
        require(
            (self.outcome == ActivationOutcome::Applied && all_present)
                || (self.outcome == ActivationOutcome::ConflictResolved && all_absent),
            "aggregate receipt outcome shape",
        )?;
        let expected = if self.outcome == ActivationOutcome::Applied {
            let (Some(nod), Some(contributor), Some(tribute), Some(desis), Some(promis)) = (
                self.nod_receipt_hash,
                self.contributor_receipt_hash,
                self.tribute_receipt_hash,
                self.desis_receipt_hash,
                self.promis_receipt_hash,
            ) else {
                return Err(ProtocolError::InvalidInvariant(
                    "aggregate receipt effect hashes",
                ));
            };
            let mut payload = Vec::with_capacity(160);
            payload.extend_from_slice(nod.as_slice());
            payload.extend_from_slice(contributor.as_slice());
            payload.extend_from_slice(tribute.as_slice());
            payload.extend_from_slice(desis.as_slice());
            payload.extend_from_slice(promis.as_slice());
            hash_framed(HashDomain::Effects, &payload)?
        } else {
            hash_framed(HashDomain::Effects, &[])?
        };
        require(expected == self.effect_commitment, "effect commitment")
    }

    pub fn terminal_receipt_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics()?;
        hash_framed(HashDomain::TerminalReceipt, &self.encode_canonical(limits)?)
    }
}

pub fn apply_event_summary_hash(owner_digests: [B256; 5]) -> Result<B256, ProtocolError> {
    let mut payload = Vec::with_capacity(160);
    for digest in owner_digests {
        payload.extend_from_slice(digest.as_slice());
    }
    hash_framed(HashDomain::ApplyEventSummary, &payload)
}

fn validate_aggregate_receipt(
    receipt: &AggregateActivationReceiptV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    receipt.validate_semantics()
}
