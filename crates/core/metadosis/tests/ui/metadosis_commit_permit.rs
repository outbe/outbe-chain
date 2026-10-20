use outbe_metadosis::commit::commit_transition;

fn main() {
    let _ = commit_transition::<outbe_primitives::storage::MetadosisCycleLifecycle, ()>;
}
