use outbe_primitives::storage::CertifiedLysisActivation;

fn clone_capability(capability: CertifiedLysisActivation<'_>) {
    let _ = capability.clone();
}

fn main() {}
