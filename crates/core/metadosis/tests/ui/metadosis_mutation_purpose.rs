use outbe_primitives::storage::{MetadosisMutationPurpose, MetadosisMutationPurposeTag};

struct ForgedPurpose;

impl MetadosisMutationPurpose for ForgedPurpose {
    const TAG: MetadosisMutationPurposeTag = MetadosisMutationPurposeTag::CycleLifecycle;
}

fn main() {}
