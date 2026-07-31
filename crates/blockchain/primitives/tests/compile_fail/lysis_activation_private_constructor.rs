use alloy_primitives::B256;
use outbe_primitives::storage::CertifiedLysisActivation;

fn main() {
    let _ = CertifiedLysisActivation::new(B256::ZERO);
}
