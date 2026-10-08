//! Canonical proof-backed batches for materializing certified NOD generations.

mod protected;
mod verification;
pub use protected::ProtectedNodMaterializationV2;

use alloy_primitives::B256;

use crate::{
    codec::{CanonicalReader, CanonicalWriter},
    error::ProtocolError,
    list::root_hash,
    registry::ListKind,
    result::NodActionV1,
    schema::{impl_top_level_codec, require, wire_struct, NestedCodec, SchemaLimits},
};

pub const MAX_NOD_MATERIALIZATION_ACTIONS: usize = 256;
pub const MAX_NOD_MATERIALIZATION_ROOT_PATH: usize = 32;
const NOD_ACTION_CANONICAL_BYTES: usize = 194;

wire_struct! {
    pub struct NodMaterializationHeadV1 {
        pub queue_sequence: u64,
        pub job_id: B256,
        pub program_semantics_hash: B256,
        pub worldwide_day: u32,
        pub generation: u64,
        pub nod_root: B256,
        pub nod_count: u32,
        pub next_nod_ordinal: u32,
        pub last_progress_height: u64,
    }
    validate = validate_head;
}
impl_top_level_codec!(NodMaterializationHeadV1, NodMaterializationHeadV1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodMaterializationBatchV1 {
    pub queue_sequence: u64,
    pub first_nod_ordinal: u32,
    pub actions: Vec<NodActionV1>,
    pub root_path: Vec<B256>,
}

impl NestedCodec for NodMaterializationBatchV1 {
    fn validate(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(self.queue_sequence != 0, "materialization queue sequence")?;
        require(
            !self.actions.is_empty() && self.actions.len() <= MAX_NOD_MATERIALIZATION_ACTIONS,
            "materialization action count",
        )?;
        require(
            self.root_path.len() <= MAX_NOD_MATERIALIZATION_ROOT_PATH,
            "materialization root path length",
        )?;
        for action in &self.actions {
            <NodActionV1 as NestedCodec>::validate(action, limits)?;
        }
        Ok(())
    }

    fn encode_nested(
        &self,
        output: &mut CanonicalWriter,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        output.write_u64(self.queue_sequence)?;
        output.write_u32(self.first_nod_ordinal)?;
        output.write_vec(
            &self.actions,
            MAX_NOD_MATERIALIZATION_ACTIONS,
            |writer, action| action.encode_nested(writer, limits),
        )?;
        output.write_vec(
            &self.root_path,
            MAX_NOD_MATERIALIZATION_ROOT_PATH,
            |writer, sibling| writer.write_b256(*sibling),
        )
    }

    fn decode_nested(
        input: &mut CanonicalReader<'_>,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            queue_sequence: input.read_u64()?,
            first_nod_ordinal: input.read_u32()?,
            actions: input.read_vec(
                MAX_NOD_MATERIALIZATION_ACTIONS,
                NOD_ACTION_CANONICAL_BYTES,
                |reader| NodActionV1::decode_nested(reader, limits),
            )?,
            root_path: input.read_vec(MAX_NOD_MATERIALIZATION_ROOT_PATH, 32, |reader| {
                reader.read_b256()
            })?,
        })
    }
}
impl_top_level_codec!(NodMaterializationBatchV1, NodMaterializationBatchV1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedNodMaterializationBatchV1 {
    actions: Vec<NodActionV1>,
}

impl VerifiedNodMaterializationBatchV1 {
    #[must_use]
    pub fn actions(&self) -> &[NodActionV1] {
        &self.actions
    }
}

pub fn verify_nod_materialization_batch(
    batch: &NodMaterializationBatchV1,
    head: &NodMaterializationHeadV1,
    configured_subtree_height: u8,
    limits: &SchemaLimits,
) -> Result<VerifiedNodMaterializationBatchV1, ProtocolError> {
    <NodMaterializationBatchV1 as NestedCodec>::validate(batch, limits)?;
    <NodMaterializationHeadV1 as NestedCodec>::validate(head, limits)?;
    let shape = verification::validate_shape(batch, head, configured_subtree_height)?;
    let subtree = verification::subtree_hash(batch, head, &shape, limits)?;
    let hash = verification::root_from_path(batch, &shape, subtree)?;
    let committed = root_hash(
        ListKind::NodActions,
        head.nod_count,
        shape.tree_height,
        hash,
    )?;
    require(committed == head.nod_root, "materialization NOD root")?;
    Ok(VerifiedNodMaterializationBatchV1 {
        actions: batch.actions.clone(),
    })
}

fn validate_head(
    head: &NodMaterializationHeadV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        head.queue_sequence != 0
            && !head.job_id.is_zero()
            && !head.program_semantics_hash.is_zero()
            && head.worldwide_day != 0
            && head.generation != 0
            && !head.nod_root.is_zero()
            && head.nod_count != 0
            && head.next_nod_ordinal < head.nod_count,
        "materialization head authority",
    )
}
