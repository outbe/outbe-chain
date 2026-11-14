use outbe_primitives::storage::ScopedMutationLease;

fn main() {
    let _ = std::any::type_name::<ScopedMutationLease<()>>();
}
