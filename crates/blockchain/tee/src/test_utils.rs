//! Shared deterministic test fixtures for TEE protocol records.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{DcapCollateralComponentV1, DcapCollateralKind};

use crate::dcap_protocol::DcapOnboardingContextV1;
use crate::registry_view::RenewalBindingV1;

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

pub fn onboarding_context_fixture(
    seeds: [u8; 9],
    key_epoch: u64,
    network_encryption_epoch: u64,
) -> DcapOnboardingContextV1 {
    let [chain, genesis, intent, node, enclave, binding, policy, recipient, offer] = seeds;
    DcapOnboardingContextV1 {
        chain_id: [chain; 32],
        genesis_hash: B256::repeat_byte(genesis),
        intent_hash: B256::repeat_byte(intent),
        node_id_hash: B256::repeat_byte(node),
        enclave_id: B256::repeat_byte(enclave),
        binding_id: B256::repeat_byte(binding),
        policy_hash: B256::repeat_byte(policy),
        recipient_x25519: [recipient; 32],
        network_encryption_public: [offer; 32],
        key_epoch,
        network_encryption_epoch,
    }
}

pub fn canonical_dcap_collateral_fixture() -> Vec<DcapCollateralComponentV1> {
    (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind],
        })
        .collect()
}

/// Ephemeral loopback Noise-IK development enclave server for cross-crate unit tests.
pub struct MockDevEnclaveServer {
    pub endpoint: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for MockDevEnclaveServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl MockDevEnclaveServer {
    pub fn spawn(
        attestation_pub: [u8; 32],
        handler: impl Fn(crate::protocol::EnclaveRequest) -> crate::protocol::EnclaveResponse
            + Send
            + Sync
            + 'static,
    ) -> Self {
        use std::net::TcpListener;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let endpoint = listener.local_addr().expect("local addr").to_string();
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_loop = Arc::clone(&stop);
        let handler = Arc::new(handler);

        let builder = snow::Builder::new(crate::NOISE_PARAMS.parse().expect("noise params"));
        let kp = builder.generate_keypair().expect("keypair");
        let noise_pub: [u8; 32] = kp.public.as_slice().try_into().expect("32-byte key");
        let noise_priv = kp.private;

        std::thread::spawn(move || {
            while !stop_loop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let h = Arc::clone(&handler);
                        let np = noise_priv.clone();
                        std::thread::spawn(move || {
                            serve_mock_dev_conn(stream, &np, noise_pub, attestation_pub, &*h);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { endpoint, stop }
    }
}

fn serve_mock_dev_conn(
    mut stream: std::net::TcpStream,
    noise_priv: &[u8],
    noise_pub: [u8; 32],
    attestation_pub: [u8; 32],
    handler: &(dyn Fn(crate::protocol::EnclaveRequest) -> crate::protocol::EnclaveResponse
          + Send
          + Sync),
) {
    use crate::codec::{decode_call, decode_request, encode_response, read_frame, write_frame};
    use crate::protocol::{EnclaveRequest, EnclaveResponse};

    let recipient = [0x0B; 32];
    let Ok(qf) = read_frame(&mut stream) else {
        return;
    };
    if !matches!(decode_request(&qf), Ok(EnclaveRequest::GetQuote { .. })) {
        return;
    }
    let mut pre = Vec::with_capacity(96);
    pre.extend_from_slice(&noise_pub);
    pre.extend_from_slice(&recipient);
    pre.extend_from_slice(&attestation_pub);
    let quote = EnclaveResponse::Quote {
        mrenclave: B256::repeat_byte(1),
        mrsigner: B256::repeat_byte(2),
        isv_svn: 1,
        report_data: alloy_primitives::keccak256(&pre),
        recipient_x25519_pub: recipient,
        attestation_pub,
        noise_static_pub: noise_pub,
        quote_body: Vec::new(),
        attestation: "none (mock-dev)".into(),
    };
    if write_frame(&mut stream, &encode_response(&quote).unwrap()).is_err() {
        return;
    }
    let Ok(mut noise) = complete_mock_noise_handshake(&mut stream, noise_priv) else {
        return;
    };
    while let Ok(frame) = read_frame(&mut stream) {
        let mut pt = vec![0u8; frame.len()];
        let Ok(n) = noise.read_message(&frame, &mut pt) else {
            break;
        };
        let req = decode_call(&pt[..n]).expect("decode call").request;
        let resp = if matches!(req, EnclaveRequest::GetPublicKeys) {
            EnclaveResponse::PublicKeys {
                offer_key_ready: true,
                recipient_x25519_pub: recipient,
                attestation_pub,
                noise_static_pub: noise_pub,
                tee_bls_pub: Vec::new(),
                dkg_enc_pub: [0x0D; 32],
                dkg_enc_sig: Vec::new(),
            }
        } else {
            handler(req)
        };
        let plain = encode_response(&resp).expect("encode resp");
        let mut ct = vec![0u8; plain.len() + 64];
        let k = noise.write_message(&plain, &mut ct).expect("encrypt");
        if write_frame(&mut stream, &ct[..k]).is_err() {
            break;
        }
    }
}

fn complete_mock_noise_handshake(
    stream: &mut std::net::TcpStream,
    noise_priv: &[u8],
) -> Result<snow::TransportState, crate::errors::TransportError> {
    let mut hs = snow::Builder::new(crate::NOISE_PARAMS.parse().expect("params"))
        .local_private_key(noise_priv)
        .build_responder()
        .map_err(|e| crate::errors::TransportError::Noise(e.to_string()))?;
    let m1 = crate::codec::read_frame(stream)?;
    let mut buf = [0u8; 1024];
    hs.read_message(&m1, &mut buf)
        .map_err(|e| crate::errors::TransportError::Noise(e.to_string()))?;
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| crate::errors::TransportError::Noise(e.to_string()))?;
    crate::codec::write_frame(stream, &buf[..n])?;
    hs.into_transport_mode()
        .map_err(|e| crate::errors::TransportError::Noise(e.to_string()))
}
