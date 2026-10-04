//! Input-only fixtures. Golden results must be supplied independently by tests.
use crate::tee::RenewalBindingV1;
use alloy_primitives::B256;

pub struct RenewalBindingFixtureV1 {
    binding: RenewalBindingV1,
}

impl RenewalBindingFixtureV1 {
    pub fn new(seed: u8) -> Self {
        let hash = |offset| B256::repeat_byte(seed.wrapping_add(offset));
        Self {
            binding: RenewalBindingV1 {
                node_id_hash: hash(0),
                enclave_id: hash(1),
                binding_id: hash(2),
                intent_hash: hash(3),
                evidence_hash: hash(4),
                policy_hash: hash(5),
                binding_version: 0,
                registration_version: 0,
                renewal_nonce: 0,
                transition_nonce: 0,
                lease_started_at: 0,
                valid_until: 0,
                collateral_valid_until: 0,
                recipient_x25519: hash(6),
                attestation_ed25519: hash(7),
                noise_responder_x25519: hash(8),
                mrenclave: hash(9),
                mrsigner: hash(10),
                isv_prod_id: 0,
                isv_svn: 0,
                platform_tcb_status: 0,
                verdict_hash: hash(11),
                node_host_authorization_hash: hash(12),
            },
        }
    }

    pub fn versions(mut self, binding: u64, registration: u64) -> Self {
        self.binding.binding_version = binding;
        self.binding.registration_version = registration;
        self
    }

    pub fn nonces(mut self, renewal: u64, transition: u64) -> Self {
        self.binding.renewal_nonce = renewal;
        self.binding.transition_nonce = transition;
        self
    }

    pub fn lease(mut self, started_at: u64, valid_until: u64, collateral_until: u64) -> Self {
        self.binding.lease_started_at = started_at;
        self.binding.valid_until = valid_until;
        self.binding.collateral_valid_until = collateral_until;
        self
    }

    pub fn keys(mut self, seed: u8) -> Self {
        let [first, second, third] = hash_triplet(seed);
        self.binding.recipient_x25519 = first;
        self.binding.attestation_ed25519 = second;
        self.binding.noise_responder_x25519 = third;
        self
    }

    pub fn measurements(mut self, seed: u8) -> Self {
        let [first, second, third] = hash_triplet(seed);
        self.binding.mrenclave = first;
        self.binding.mrsigner = second;
        self.binding.verdict_hash = third;
        self
    }

    pub fn claims(mut self, product_id: u16, svn: u16, tcb_status: u8) -> Self {
        self.binding.isv_prod_id = product_id;
        self.binding.isv_svn = svn;
        self.binding.platform_tcb_status = tcb_status;
        self
    }

    pub fn authorization(mut self, hash: B256) -> Self {
        self.binding.node_host_authorization_hash = hash;
        self
    }

    pub fn build(self) -> RenewalBindingV1 {
        self.binding
    }
}

fn hash_triplet(seed: u8) -> [B256; 3] {
    [seed, seed.wrapping_add(1), seed.wrapping_add(2)].map(B256::repeat_byte)
}
