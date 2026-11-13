use outbe_primitives::storage::{MutationPurposeTag, ScopedMutationPurpose};

struct ForgedPurpose;

impl ScopedMutationPurpose for ForgedPurpose {
    const TAG: MutationPurposeTag = MutationPurposeTag::CycleLifecycle;
}

fn main() {}
